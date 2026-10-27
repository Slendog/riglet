//! Triangle binding for mesh deformers (E6), accelerated with a uniform
//! grid. [`bind_point_brute`] is the reference definition; [`TriangleGrid`]
//! must return exactly the same triangle for every point.
//!
//! Worst-case cost is still points × triangles (for example, many thin
//! triangles crossing one region that holds no point). The grid counts its
//! work (candidate triangle tests plus grid cells visited) so callers can
//! stop at a budget (`Limits::binding_work_budget`). The count depends only
//! on the input, never on timing or platform.

use crate::geometry::{self, add, dot, scale, sub};
use crate::model::Vec2;

/// Barycentric tolerance for "on an edge counts as inside".
const INSIDE_EPSILON: f32 = 1e-6;
/// Upper bound on grid cells per axis.
const MAX_CELLS_PER_AXIS: usize = 1024;

#[derive(Debug, Clone, Copy, PartialEq)]
struct Aabb {
    min: Vec2,
    max: Vec2,
}

impl Aabb {
    fn of(t: [Vec2; 3]) -> Self {
        let min = [t[0][0].min(t[1][0]).min(t[2][0]), t[0][1].min(t[1][1]).min(t[2][1])];
        let max = [t[0][0].max(t[1][0]).max(t[2][0]), t[0][1].max(t[1][1]).max(t[2][1])];
        // Inflate so every point that passes the barycentric tolerance is
        // inside the box, including float error at large coordinates.
        let extent = (max[0] - min[0]).max(max[1] - min[1]);
        let magnitude = min[0].abs().max(min[1].abs()).max(max[0].abs()).max(max[1].abs());
        let pad = 1e-5 * (extent + magnitude);
        Aabb { min: [min[0] - pad, min[1] - pad], max: [max[0] + pad, max[1] + pad] }
    }

    fn contains(&self, p: Vec2) -> bool {
        p[0] >= self.min[0] && p[0] <= self.max[0] && p[1] >= self.min[1] && p[1] <= self.max[1]
    }

    fn union(self, o: Aabb) -> Aabb {
        Aabb {
            min: [self.min[0].min(o.min[0]), self.min[1].min(o.min[1])],
            max: [self.max[0].max(o.max[0]), self.max[1].max(o.max[1])],
        }
    }

    fn distance_sq(&self, p: Vec2) -> f32 {
        let dx = (self.min[0] - p[0]).max(0.0).max(p[0] - self.max[0]);
        let dy = (self.min[1] - p[1]).max(0.0).max(p[1] - self.max[1]);
        dx * dx + dy * dy
    }
}

fn segment_distance_sq(p: Vec2, a: Vec2, b: Vec2) -> f32 {
    let ab = sub(b, a);
    let len_sq = dot(ab, ab);
    let t = if len_sq > 0.0 { (dot(sub(p, a), ab) / len_sq).clamp(0.0, 1.0) } else { 0.0 };
    let d = sub(p, add(a, scale(ab, t)));
    dot(d, d)
}

fn triangle_distance_sq(t: [Vec2; 3], p: Vec2) -> f32 {
    segment_distance_sq(p, t[0], t[1]).min(segment_distance_sq(p, t[1], t[2])).min(segment_distance_sq(p, t[2], t[0]))
}

/// Containment test shared by the reference and the grid.
fn contains(t: [Vec2; 3], bounds: &Aabb, p: Vec2) -> Option<bool> {
    let l = geometry::barycentric(t, p)?;
    Some(bounds.contains(p) && l.iter().all(|&x| x >= -INSIDE_EPSILON))
}

/// Reference definition (E6): the lowest-index triangle containing `p`,
/// else the nearest triangle, ties to the lowest index. Degenerate
/// triangles are skipped. O(triangles); used by tests to check the grid.
pub fn bind_point_brute(points: &[Vec2], indices: &[u32], p: Vec2) -> u32 {
    let mut best = (0u32, f32::INFINITY);
    for tri in 0..indices.len() / 3 {
        let t = geometry::triangle(points, indices, tri);
        match contains(t, &Aabb::of(t), p) {
            None => continue,
            Some(true) => return tri as u32,
            Some(false) => {}
        }
        let d = triangle_distance_sq(t, p);
        if d < best.1 {
            best = (tri as u32, d);
        }
    }
    best.0
}

/// Uniform grid over a mesh's triangles. Each cell lists, in ascending
/// index order, the triangles whose bounding box overlaps it.
#[derive(Debug, Clone)]
pub struct TriangleGrid {
    triangles: Vec<[Vec2; 3]>,
    boxes: Vec<Aabb>,
    bounds: Aabb,
    cells: usize,
    cell_size: Vec2,
    /// CSR layout: cell `c` lists `entries[offsets[c]..offsets[c + 1]]`.
    offsets: Vec<u32>,
    entries: Vec<u32>,
    /// Per-triangle query stamp, to visit each triangle once per query.
    stamps: Vec<u32>,
    query: u32,
    work: u64,
}

impl TriangleGrid {
    pub fn new(points: &[Vec2], indices: &[u32]) -> Self {
        let count = indices.len() / 3;
        let triangles: Vec<[Vec2; 3]> = (0..count).map(|t| geometry::triangle(points, indices, t)).collect();
        let boxes: Vec<Aabb> = triangles.iter().map(|&t| Aabb::of(t)).collect();
        let bounds = boxes.iter().copied().reduce(Aabb::union).unwrap_or(Aabb { min: [0.0; 2], max: [0.0; 2] });

        // Start at about one cell per triangle; halve the resolution until
        // the total (triangle, cell) pairs fit the build budget. At one cell
        // the pair count equals the triangle count, so this always ends.
        let budget = 16 * count as u64 + 65_536;
        let mut cells = ((count as f64).sqrt().ceil() as usize).clamp(1, MAX_CELLS_PER_AXIS);
        let mut grid = TriangleGrid {
            triangles,
            boxes,
            bounds,
            cells,
            cell_size: [1.0, 1.0],
            offsets: Vec::new(),
            entries: Vec::new(),
            stamps: vec![0; count],
            query: 0,
            work: 0,
        };
        loop {
            grid.set_resolution(cells);
            let pairs: u64 = grid.boxes.iter().map(|b| grid.cell_count(b)).sum();
            if pairs <= budget || cells == 1 {
                grid.fill();
                return grid;
            }
            cells = (cells / 2).max(1);
        }
    }

    fn set_resolution(&mut self, cells: usize) {
        self.cells = cells;
        let size = |lo: f32, hi: f32| {
            let s = (hi - lo) / cells as f32;
            if s > 0.0 && s.is_finite() { s } else { 1.0 }
        };
        self.cell_size = [size(self.bounds.min[0], self.bounds.max[0]), size(self.bounds.min[1], self.bounds.max[1])];
    }

    fn axis_cell(&self, v: f32, axis: usize) -> usize {
        let (min, size) =
            (self.bounds.min.get(axis).copied().unwrap_or(0.0), self.cell_size.get(axis).copied().unwrap_or(1.0));
        let c = ((v - min) / size).floor();
        // NaN casts to 0; the clamp keeps every point in range.
        (c.max(0.0) as usize).min(self.cells - 1)
    }

    fn cell_range(&self, b: &Aabb) -> (usize, usize, usize, usize) {
        (
            self.axis_cell(b.min[0], 0),
            self.axis_cell(b.max[0], 0),
            self.axis_cell(b.min[1], 1),
            self.axis_cell(b.max[1], 1),
        )
    }

    fn cell_count(&self, b: &Aabb) -> u64 {
        let (x0, x1, y0, y1) = self.cell_range(b);
        ((x1 - x0 + 1) * (y1 - y0 + 1)) as u64
    }

    fn fill(&mut self) {
        let n = self.cells * self.cells;
        let mut counts = vec![0u32; n + 1];
        for b in &self.boxes {
            let (x0, x1, y0, y1) = self.cell_range(b);
            for y in y0..=y1 {
                for x in x0..=x1 {
                    if let Some(c) = counts.get_mut(y * self.cells + x + 1) {
                        *c += 1;
                    }
                }
            }
        }
        for i in 1..counts.len() {
            let prev = counts.get(i - 1).copied().unwrap_or(0);
            if let Some(c) = counts.get_mut(i) {
                *c += prev;
            }
        }
        let mut cursor = counts.clone();
        let mut entries = vec![0u32; counts.last().copied().unwrap_or(0) as usize];
        for (tri, b) in self.boxes.iter().enumerate() {
            let (x0, x1, y0, y1) = self.cell_range(b);
            for y in y0..=y1 {
                for x in x0..=x1 {
                    if let Some(slot) = cursor.get_mut(y * self.cells + x)
                        && let Some(e) = entries.get_mut(*slot as usize)
                    {
                        *e = tri as u32;
                        *slot += 1;
                    }
                }
            }
        }
        self.offsets = counts;
        self.entries = entries;
    }

    fn cell(&self, x: usize, y: usize) -> &[u32] {
        let c = y * self.cells + x;
        let (Some(&a), Some(&b)) = (self.offsets.get(c), self.offsets.get(c + 1)) else { return &[] };
        self.entries.get(a as usize..b as usize).unwrap_or(&[])
    }

    fn cell_box(&self, x: usize, y: usize) -> Aabb {
        let min =
            [self.bounds.min[0] + x as f32 * self.cell_size[0], self.bounds.min[1] + y as f32 * self.cell_size[1]];
        Aabb { min, max: [min[0] + self.cell_size[0], min[1] + self.cell_size[1]] }
    }

    /// Work done by all [`TriangleGrid::bind`] calls so far: candidate
    /// triangle tests plus grid cells visited. Deterministic for a given
    /// mesh and query sequence.
    pub fn work(&self) -> u64 {
        self.work
    }

    /// Distance from `p` to the grid cells at Chebyshev ring `ring` or
    /// farther from cell `(cx, cy)`: those cells lie in at most four
    /// rectangles (left, right, top, bottom of the inner square). `None`
    /// when no such cell exists.
    fn ring_lower_bound(&self, cx: usize, cy: usize, ring: usize, p: Vec2) -> Option<f32> {
        let n = self.cells;
        let x = |i: usize| self.bounds.min[0] + i as f32 * self.cell_size[0];
        let y = |j: usize| self.bounds.min[1] + j as f32 * self.cell_size[1];
        let rect = |x0: usize, x1: usize, y0: usize, y1: usize| Aabb { min: [x(x0), y(y0)], max: [x(x1), y(y1)] };
        let mut regions = Vec::with_capacity(4);
        if cx + ring < n {
            regions.push(rect(cx + ring, n, 0, n));
        }
        if cx >= ring {
            regions.push(rect(0, cx + 1 - ring, 0, n));
        }
        if cy + ring < n {
            regions.push(rect(0, n, cy + ring, n));
        }
        if cy >= ring {
            regions.push(rect(0, n, 0, cy + 1 - ring));
        }
        regions.iter().map(|r| r.distance_sq(p).sqrt()).reduce(f32::min)
    }

    /// Same result as [`bind_point_brute`].
    pub fn bind(&mut self, p: Vec2) -> u32 {
        if self.triangles.is_empty() {
            return 0;
        }
        let (cx, cy) = (self.axis_cell(p[0], 0), self.axis_cell(p[1], 1));

        // Containment: the lowest-index containing triangle overlaps p's
        // cell, and cell lists are in ascending index order.
        if self.bounds.contains(p) {
            let mut tests = 0u64;
            let mut found = None;
            for &tri in self.cell(cx, cy) {
                tests += 1;
                let (Some(&t), Some(b)) = (self.triangles.get(tri as usize), self.boxes.get(tri as usize)) else {
                    continue;
                };
                if contains(t, b, p) == Some(true) {
                    found = Some(tri);
                    break;
                }
            }
            self.work = self.work.saturating_add(tests);
            if let Some(tri) = found {
                return tri;
            }
        }

        // Nearest: visit rings of cells around p's cell until no unvisited
        // cell can be closer than the best triangle so far.
        self.query = self.query.wrapping_add(1);
        if self.query == 0 {
            self.stamps.fill(0);
            self.query = 1;
        }
        let mut best = (u32::MAX, f32::INFINITY);
        for ring in 0..self.cells {
            let Some(lower) = self.ring_lower_bound(cx, cy, ring, p) else { break };
            // Small margin so float error never prunes a true tie.
            if best.0 != u32::MAX && lower * (1.0 - 1e-4) > best.1.sqrt() {
                break;
            }
            for (x, y) in ring_cells(cx, cy, ring, self.cells) {
                self.work = self.work.saturating_add(1);
                if best.0 != u32::MAX && self.cell_box(x, y).distance_sq(p) * (1.0 - 1e-4) > best.1 {
                    continue;
                }
                self.visit_cell(x, y, p, &mut best);
            }
        }
        if best.0 == u32::MAX { 0 } else { best.0 }
    }

    fn visit_cell(&mut self, x: usize, y: usize, p: Vec2, best: &mut (u32, f32)) {
        let c = y * self.cells + x;
        let (Some(&a), Some(&b)) = (self.offsets.get(c), self.offsets.get(c + 1)) else { return };
        for k in a..b {
            let Some(&tri) = self.entries.get(k as usize) else { continue };
            let Some(stamp) = self.stamps.get_mut(tri as usize) else { continue };
            if *stamp == self.query {
                continue;
            }
            *stamp = self.query;
            self.work = self.work.saturating_add(1);
            let Some(&t) = self.triangles.get(tri as usize) else { continue };
            if geometry::barycentric(t, p).is_none() {
                continue;
            }
            let d = triangle_distance_sq(t, p);
            if d < best.1 || (d == best.1 && tri < best.0) {
                *best = (tri, d);
            }
        }
    }
}

/// Cells at Chebyshev distance exactly `ring` from `(cx, cy)`, inside an
/// `n` x `n` grid. Visits only the ring's perimeter.
fn ring_cells(cx: usize, cy: usize, ring: usize, n: usize) -> impl Iterator<Item = (usize, usize)> {
    let (cx, cy, r, n) = (cx as i64, cy as i64, ring as i64, n as i64);
    let inside = move |(x, y): (i64, i64)| (0..n).contains(&x) && (0..n).contains(&y);
    let rows = (cx - r..=cx + r).flat_map(move |x| {
        let bottom = if r > 0 { Some((x, cy + r)) } else { None };
        std::iter::once((x, cy - r)).chain(bottom)
    });
    let cols = (cy - r + 1..cy + r).flat_map(move |y| [(cx - r, y), (cx + r, y)]);
    rows.chain(cols).filter(move |&c| inside(c)).map(|(x, y)| (x as usize, y as usize))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefers_lowest_index_on_shared_edge() {
        // Two triangles sharing the edge (1,0)-(0,1).
        let pts = [[0.0, 0.0], [1.0, 0.0], [0.0, 1.0], [1.0, 1.0]];
        let idx = [1, 3, 2, 0, 1, 2];
        let mut grid = TriangleGrid::new(&pts, &idx);
        for (p, want) in [([0.5, 0.5], 0), ([0.1, 0.1], 1), ([-1.0, -1.0], 1), ([0.9, 0.9], 0)] {
            assert_eq!(bind_point_brute(&pts, &idx, p), want, "brute {p:?}");
            assert_eq!(grid.bind(p), want, "grid {p:?}");
        }
    }

    #[test]
    fn ring_cells_visit_each_cell_once() {
        let mut seen = std::collections::HashSet::new();
        for ring in 0..6 {
            for c in ring_cells(2, 3, ring, 6) {
                assert!(seen.insert(c), "{c:?} visited twice");
            }
        }
        assert_eq!(seen.len(), 36);
    }

    #[test]
    fn empty_mesh_binds_to_zero() {
        assert_eq!(TriangleGrid::new(&[], &[]).bind([1.0, 2.0]), 0);
    }
}
