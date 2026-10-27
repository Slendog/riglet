//! Mesh-deformer binding (E6): the grid must match the reference exactly,
//! and adversarial meshes must not make loading slow.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

mod common;

use std::time::{Duration, Instant};

use common::*;
use riglet_format::bind::{TriangleGrid, bind_point_brute};
use riglet_format::{Limits, Rig, validate};

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn unit(&mut self) -> f32 {
        (self.next() >> 40) as f32 / (1u64 << 24) as f32
    }

    fn range(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (hi - lo) * self.unit()
    }
}

fn check_equivalent(points: &[[f32; 2]], indices: &[u32], queries: &[[f32; 2]]) {
    let mut grid = TriangleGrid::new(points, indices);
    for &q in queries {
        assert_eq!(grid.bind(q), bind_point_brute(points, indices, q), "query {q:?}");
    }
}

#[test]
fn grid_matches_reference_on_random_soups() {
    let mut rng = Rng(0x1234_5678_9ABC_DEF1);
    for case in 0..40 {
        let tris = 1 + (rng.next() % 300) as usize;
        let scale = [1.0, 100.0, 10_000.0][case % 3];
        let mut points = Vec::new();
        for _ in 0..tris * 3 {
            points.push([rng.range(-scale, scale), rng.range(-scale, scale)]);
        }
        let indices: Vec<u32> = (0..points.len() as u32).collect();
        let mut queries: Vec<[f32; 2]> =
            (0..400).map(|_| [rng.range(-3.0 * scale, 3.0 * scale), rng.range(-3.0 * scale, 3.0 * scale)]).collect();
        // Vertices and edge midpoints exercise the shared-edge rule.
        queries.extend(points.iter().take(60));
        queries.extend(points.chunks_exact(2).take(60).map(|c| [(c[0][0] + c[1][0]) / 2.0, (c[0][1] + c[1][1]) / 2.0]));
        check_equivalent(&points, &indices, &queries);
    }
}

#[test]
fn grid_matches_reference_on_a_regular_mesh() {
    // 40 x 40 quads, two triangles each, with shared edges everywhere.
    let n = 40;
    let mut points = Vec::new();
    for y in 0..=n {
        for x in 0..=n {
            points.push([x as f32 * 2.5, y as f32 * 2.5]);
        }
    }
    let mut indices = Vec::new();
    for y in 0..n {
        for x in 0..n {
            let i = (y * (n + 1) + x) as u32;
            let s = n as u32 + 1;
            indices.extend([i, i + 1, i + s, i + 1, i + s + 1, i + s]);
        }
    }
    let mut queries = Vec::new();
    for y in -10..=110 {
        for x in -10..=110 {
            queries.push([x as f32, y as f32]);
        }
    }
    check_equivalent(&points, &indices, &queries);
}

#[test]
fn grid_handles_flat_and_far_inputs() {
    // Every triangle in a thin horizontal band: zero-height grid extent
    // must not break the cell size.
    let points = [[0.0, 0.0], [10.0, 0.0], [5.0, 1e-3], [20.0, 0.0], [30.0, 0.0], [25.0, 1e-3]];
    let indices = [0, 1, 2, 3, 4, 5];
    let queries = [[5.0, 0.0], [15.0, 0.0], [-1e30, 5.0], [1e30, -1e30], [f32::NAN, 0.0], [25.0, 100.0]];
    let mut grid = TriangleGrid::new(&points, &indices);
    for q in queries {
        if q[0].is_nan() {
            // No ordering for NaN distances; only require no panic.
            let _ = grid.bind(q);
        } else {
            assert_eq!(grid.bind(q), bind_point_brute(&points, &indices, q), "query {q:?}");
        }
    }
}

/// Needle triangles that all cross one center region, plus points placed in
/// the gaps between needles. Every grid cell overlaps every needle box, so
/// this is the grid's worst case: about points × triangles tests.
fn needle_rig(needles: usize, points: usize) -> Builder {
    let mut b = Builder::new();
    let (l, w) = (1000.0f32, 1e-3f32);
    let mut pos = Vec::new();
    let mut idx = Vec::new();
    for i in 0..needles {
        let a = std::f32::consts::PI * i as f32 / needles as f32;
        let (s, c) = a.sin_cos();
        let (u, n) = ([c, s], [-s, c]);
        let base = pos.len() as u16;
        pos.push([l * u[0] + w * n[0], l * u[1] + w * n[1]]);
        pos.push([l * u[0] - w * n[0], l * u[1] - w * n[1]]);
        pos.push([-l * u[0], -l * u[1]]);
        idx.extend([base, base + 1, base + 2]);
    }
    b.mesh_deformer("m", "root", &pos, &idx);
    let child: Vec<[f32; 2]> = (0..points)
        .map(|i| {
            // Midway between two needles, on a circle of radius 100.
            let a = std::f32::consts::PI * (i as f32 + 0.5) / points as f32;
            [100.0 * a.cos(), 100.0 * a.sin()]
        })
        .collect();
    b.drawable("d", "m", &child);
    b
}

#[test]
fn adversarial_binding_over_the_cap_is_rejected_quickly() {
    // 21 845 needles (65 535 vertices, the per-mesh limit) and 65 536 child
    // points: 1.4e9 pairs, far above the default cap.
    let b = needle_rig(21_845, 65_536);
    let doc = b.document();
    let start = Instant::now();
    let issues = validate(&doc, &Limits::default());
    let elapsed = start.elapsed();
    assert!(issues.iter().any(|i| i.code == "LIMIT_MESH_BINDING"), "{issues:?}");
    assert!(elapsed < Duration::from_secs(5), "rejection took {elapsed:?}");
}

/// Times binding of the needle rig at `needles` x `points` pairs.
fn time_needles(needles: usize, points: usize) -> Duration {
    let b = needle_rig(needles, points);
    let doc = b.document();
    let start = Instant::now();
    let rig = Rig::prepare(&doc, &Limits::default());
    let elapsed = start.elapsed();
    assert!(rig.is_ok(), "{:?}", rig.err());
    elapsed
}

#[test]
fn adversarial_binding_below_the_cap_finishes_in_budget() {
    // Debug builds are slow, so this runs a scaled-down worst case
    // (4 194 304 pairs). The full-cap case runs in release in CI (below).
    let elapsed = time_needles(2048, 2048);
    assert!(elapsed < Duration::from_secs(20), "binding took {elapsed:?}");
}

/// Worst case at the default cap (2^28 pairs). Run with
/// `cargo test --release --test binding -- --ignored`.
#[test]
#[ignore = "release only: cargo test --release --test binding -- --ignored"]
fn adversarial_binding_at_the_cap_finishes_in_budget() {
    let elapsed = time_needles(16_384, 16_384);
    eprintln!("binding at the cap took {elapsed:?}");
    assert!(elapsed < Duration::from_secs(30), "binding took {elapsed:?}");
}

#[test]
fn regular_mesh_binding_is_fast() {
    // A realistic large case: 20 000 triangles, 65 536 child points.
    let mut b = Builder::new();
    let n = 100usize;
    let mut pos = Vec::new();
    for y in 0..=n {
        for x in 0..=n {
            pos.push([x as f32 * 5.0, y as f32 * 5.0]);
        }
    }
    let mut idx = Vec::new();
    for y in 0..n {
        for x in 0..n {
            let i = (y * (n + 1) + x) as u16;
            let s = n as u16 + 1;
            idx.extend([i, i + 1, i + s, i + 1, i + s + 1, i + s]);
        }
    }
    b.mesh_deformer("m", "root", &pos, &idx);
    let child: Vec<[f32; 2]> = (0..65_536).map(|i| [(i % 256) as f32 * 1.95, (i / 256) as f32 * 1.95]).collect();
    b.drawable("d", "m", &child);
    let doc = b.document();
    let limits = Limits { max_mesh_binding_pairs: u64::MAX, ..Limits::default() };
    let start = Instant::now();
    assert!(Rig::prepare(&doc, &limits).is_ok());
    let elapsed = start.elapsed();
    assert!(elapsed < Duration::from_secs(10), "binding took {elapsed:?}");
}
