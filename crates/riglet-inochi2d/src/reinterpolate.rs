//! Fills the unset points of an Inochi2D parameter binding grid, exactly as
//! Inochi2D 0.8.7 does (`ParameterBindingImpl.reInterpolate` in
//! `source/inochi2d/core/param/binding.d`), so imported bindings evaluate
//! the same. Also the uniform Catmull-Rom `cubic` from inmath
//! (`inmath/interpolate.d`), used to resample cubic bindings.
//!
//! This file is a PORT of that code. Its notices:
//!
//! Inochi2D: Copyright (c) 2020, Inochi2D Project. Distributed under the
//! 2-Clause BSD License:
//!
//!   Redistribution and use in source and binary forms, with or without
//!   modification, are permitted provided that the following conditions
//!   are met:
//!   1. Redistributions of source code must retain the above copyright
//!      notice, this list of conditions and the following disclaimer.
//!   2. Redistributions in binary form must reproduce the above copyright
//!      notice, this list of conditions and the following disclaimer in the
//!      documentation and/or other materials provided with the distribution.
//!
//!   THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS
//!   "AS IS" AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT
//!   LIMITED TO, THE IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A
//!   PARTICULAR PURPOSE ARE DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT
//!   HOLDER OR CONTRIBUTORS BE LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL,
//!   SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT
//!   LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES; LOSS OF USE,
//!   DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY
//!   THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT
//!   (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE
//!   OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.
//!
//! inmath `cubic`: Copyright (c) 2011, David Herberth; 2021, Inochi2D
//! Project. MIT License: permission is hereby granted, free of charge, to
//! any person obtaining a copy of this software and associated
//! documentation files, to deal in the Software without restriction,
//! subject to including the above copyright notice and this permission
//! notice in all copies or substantial portions of the Software. THE
//! SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND.

/// A value a binding grid holds: a scalar, or one deform (per-vertex
/// deltas). Needs the vector-space operations the algorithm uses.
pub trait Value: Clone {
    fn lerp(&self, other: &Self, t: f32) -> Self;
    fn add(&self, other: &Self) -> Self;
    fn sub(&self, other: &Self) -> Self;
    fn scale(&self, s: f32) -> Self;
}

impl Value for f32 {
    fn lerp(&self, other: &Self, t: f32) -> Self {
        self * (1.0 - t) + other * t
    }
    fn add(&self, other: &Self) -> Self {
        self + other
    }
    fn sub(&self, other: &Self) -> Self {
        self - other
    }
    fn scale(&self, s: f32) -> Self {
        self * s
    }
}

impl Value for Vec<[f32; 2]> {
    fn lerp(&self, other: &Self, t: f32) -> Self {
        self.iter().zip(other).map(|(a, b)| [a[0] * (1.0 - t) + b[0] * t, a[1] * (1.0 - t) + b[1] * t]).collect()
    }
    fn add(&self, other: &Self) -> Self {
        self.iter().zip(other).map(|(a, b)| [a[0] + b[0], a[1] + b[1]]).collect()
    }
    fn sub(&self, other: &Self) -> Self {
        self.iter().zip(other).map(|(a, b)| [a[0] - b[0], a[1] - b[1]]).collect()
    }
    fn scale(&self, s: f32) -> Self {
        self.iter().map(|a| [a[0] * s, a[1] * s]).collect()
    }
}

/// inmath `cubic`: uniform Catmull-Rom through p1..p2 at t in [0, 1].
pub fn cubic<T: Value>(p0: &T, p1: &T, p2: &T, p3: &T, t: f32) -> T {
    let a = p0.scale(-0.5).add(&p1.scale(1.5)).add(&p2.scale(-1.5)).add(&p3.scale(0.5));
    let b = p0.add(&p1.scale(-2.5)).add(&p2.scale(2.0)).add(&p3.scale(-0.5));
    let c = p0.scale(-0.5).add(&p2.scale(0.5));
    a.scale(t * t * t).add(&b.scale(t * t)).add(&c.scale(t)).add(p1)
}

/// A binding grid `values[x][y]` with `set[x][y]`, over the parameter's
/// normalized axis points (`axis[0]` for x, `axis[1]` for y; a 1D
/// parameter has one y point).
pub struct Grid<'a, T> {
    pub values: &'a mut [Vec<T>],
    pub set: &'a [Vec<bool>],
    pub axis: [&'a [f32]; 2],
}

/// Error: the grid's shape doesn't match its axes, or the algorithm didn't
/// complete (Inochi2D asserts here; we fail cleanly).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReinterpolateError;

/// Fills every unset point. Returns `Ok(false)` when no point is set (the
/// caller clears the binding, as Inochi2D does).
pub fn reinterpolate<T: Value>(g: Grid<'_, T>) -> Result<bool, ReinterpolateError> {
    let x_count = g.axis[0].len();
    let y_count = g.axis[1].len();
    if g.values.len() != x_count
        || g.set.len() != x_count
        || g.values.iter().any(|c| c.len() != y_count)
        || g.set.iter().any(|c| c.len() != y_count)
    {
        return Err(ReinterpolateError);
    }
    let mut st = State {
        values: g.values,
        valid: g.set.to_vec(),
        newly_set: vec![vec![false; y_count]; x_count],
        distance: vec![vec![0.0; y_count]; x_count],
        commit: Vec::new(),
        axis: g.axis,
        y_major: false,
        x_count,
        y_count,
    };
    let total = x_count * y_count;
    let mut valid_count = st.valid.iter().flatten().filter(|v| **v).count();
    if valid_count == 0 {
        return Ok(false);
    }
    // Each round commits at least one point, so `total` rounds suffice; the
    // cap turns Inochi2D's "should never happen" into a clean error.
    for _ in 0..=total {
        for (x, y) in std::mem::take(&mut st.commit) {
            if let Some(v) = st.valid.get_mut(x).and_then(|c| c.get_mut(y))
                && !*v
            {
                *v = true;
                valid_count += 1;
            }
        }
        if valid_count == total {
            return Ok(true);
        }
        for col in &mut st.newly_set {
            col.iter_mut().for_each(|v| *v = false);
        }
        st.interpolate_1d_2d(false);
        st.interpolate_1d_2d(true);
        if !st.commit.is_empty() {
            continue;
        }
        st.extrapolate_corners();
        if !st.commit.is_empty() {
            continue;
        }
        st.extend_and_intersect(false);
        st.extend_and_intersect(true);
        if st.commit.is_empty() {
            break;
        }
    }
    Err(ReinterpolateError)
}

struct State<'a, 'v, T> {
    values: &'v mut [Vec<T>],
    valid: Vec<Vec<bool>>,
    newly_set: Vec<Vec<bool>>,
    distance: Vec<Vec<f32>>,
    commit: Vec<(usize, usize)>,
    axis: [&'a [f32]; 2],
    y_major: bool,
    x_count: usize,
    y_count: usize,
}

impl<T: Value> State<'_, '_, T> {
    /// (major, minor) -> (x, y).
    fn xy(&self, maj: usize, min: usize) -> (usize, usize) {
        if self.y_major { (min, maj) } else { (maj, min) }
    }
    fn major_count(&self) -> usize {
        if self.y_major { self.y_count } else { self.x_count }
    }
    fn minor_count(&self) -> usize {
        if self.y_major { self.x_count } else { self.y_count }
    }
    fn is_valid(&self, maj: usize, min: usize) -> bool {
        let (x, y) = self.xy(maj, min);
        self.valid.get(x).and_then(|c| c.get(y)).copied().unwrap_or(false)
    }
    fn is_newly_set(&self, maj: usize, min: usize) -> bool {
        let (x, y) = self.xy(maj, min);
        self.newly_set.get(x).and_then(|c| c.get(y)).copied().unwrap_or(false)
    }
    fn get(&self, maj: usize, min: usize) -> Option<T> {
        let (x, y) = self.xy(maj, min);
        self.values.get(x).and_then(|c| c.get(y)).cloned()
    }
    fn get_distance(&self, maj: usize, min: usize) -> f32 {
        let (x, y) = self.xy(maj, min);
        self.distance.get(x).and_then(|c| c.get(y)).copied().unwrap_or(0.0)
    }
    /// The minor axis's point positions (x points when y-major).
    fn axis_point(&self, idx: usize) -> f32 {
        let axis = if self.y_major { self.axis[0] } else { self.axis[1] };
        axis.get(idx).copied().unwrap_or(0.0)
    }
    fn set(&mut self, maj: usize, min: usize, val: T, distance: f32) {
        let (x, y) = self.xy(maj, min);
        if let Some(slot) = self.values.get_mut(x).and_then(|c| c.get_mut(y)) {
            *slot = val;
        }
        if let Some(d) = self.distance.get_mut(x).and_then(|c| c.get_mut(y)) {
            *d = distance;
        }
        if let Some(n) = self.newly_set.get_mut(x).and_then(|c| c.get_mut(y)) {
            *n = true;
        }
        self.commit.push((x, y));
    }
    fn interp(&self, maj: usize, left: usize, mid: usize, right: usize) -> Option<T> {
        let (l, m, r) = (self.axis_point(left), self.axis_point(mid), self.axis_point(right));
        let off = (m - l) / (r - l);
        Some(self.get(maj, left)?.scale(1.0 - off).add(&self.get(maj, right)?.scale(off)))
    }

    fn interpolate_1d_2d(&mut self, second_pass: bool) {
        self.y_major = second_pass;
        let mut intersections = false;
        for i in 0..self.major_count() {
            let cnt = self.minor_count();
            let mut l = 0;
            while l < cnt && !self.is_valid(i, l) {
                l += 1;
            }
            if l >= cnt {
                continue;
            }
            loop {
                while l + 1 < cnt && self.is_valid(i, l + 1) {
                    l += 1;
                }
                if l + 1 >= cnt {
                    break;
                }
                let mut r = l + 1;
                while r < cnt && !self.is_valid(i, r) {
                    r += 1;
                }
                if r >= cnt {
                    break;
                }
                for m in (l + 1)..r {
                    let Some(val) = self.interp(i, l, m, r) else { continue };
                    if second_pass && self.is_newly_set(i, m) {
                        if !intersections {
                            self.commit.clear();
                        }
                        if let Some(prev) = self.get(i, m) {
                            self.set(i, m, val.add(&prev).scale(0.5), 0.0);
                        }
                        intersections = true;
                    }
                    if !intersections {
                        self.set(i, m, val, 0.0);
                    }
                }
                l = r;
            }
        }
    }

    fn extrapolate_corners(&mut self) {
        if self.y_count <= 1 || self.x_count <= 1 {
            return;
        }
        let v = |s: &Self, x: usize, y: usize| s.valid.get(x).and_then(|c| c.get(y)).copied().unwrap_or(false);
        for x in 0..self.x_count - 1 {
            for y in 0..self.y_count - 1 {
                let (a, b, c, d) = (v(self, x, y), v(self, x + 1, y), v(self, x, y + 1), v(self, x + 1, y + 1));
                let corner = if a && b && c && !d {
                    Some((x, y, x + 1, y + 1))
                } else if a && b && !c && d {
                    Some((x + 1, y, x, y + 1))
                } else if a && !b && c && d {
                    Some((x, y + 1, x + 1, y))
                } else if !a && b && c && d {
                    Some((x + 1, y + 1, x, y))
                } else {
                    None
                };
                if let Some((bx, by, tx, ty)) = corner {
                    let cell = |x: usize, y: usize| self.values.get(x).and_then(|c| c.get(y)).cloned();
                    // base + (values[tx][by] - base) + (values[bx][ty] - base)
                    if let (Some(base), Some(dx), Some(dy)) = (cell(bx, by), cell(tx, by), cell(bx, ty)) {
                        let val = base.add(&dx.sub(&base)).add(&dy.sub(&base));
                        if let Some(slot) = self.values.get_mut(tx).and_then(|c| c.get_mut(ty)) {
                            *slot = val;
                        }
                        self.commit.push((tx, ty));
                    }
                }
            }
        }
    }

    fn extend_and_intersect(&mut self, second_pass: bool) {
        self.y_major = second_pass;
        let mut intersections = false;
        for i in 0..self.major_count() {
            let cnt = self.minor_count();
            let mut j = 0;
            while j < cnt && !self.is_valid(i, j) {
                j += 1;
            }
            if j >= cnt {
                continue;
            }
            let Some(val) = self.get(i, j) else { continue };
            let origin = self.axis_point(j);
            for k in 0..j {
                self.set_or_average(i, k, val.clone(), origin, second_pass, &mut intersections);
            }
            let mut j = cnt - 1;
            while j < cnt && !self.is_valid(i, j) {
                match j.checked_sub(1) {
                    Some(n) => j = n,
                    None => break,
                }
            }
            let Some(val) = self.get(i, j) else { continue };
            let origin = self.axis_point(j);
            for k in (j + 1)..cnt {
                self.set_or_average(i, k, val.clone(), origin, second_pass, &mut intersections);
            }
        }
    }

    fn set_or_average(&mut self, maj: usize, min: usize, val: T, origin: f32, second: bool, intersections: &mut bool) {
        let min_dist = (self.axis_point(min) - origin).abs();
        if second && self.is_newly_set(maj, min) {
            if !*intersections {
                self.commit.clear();
            }
            let maj_dist = self.get_distance(maj, min);
            let frac = min_dist / (min_dist + maj_dist * maj_dist / min_dist);
            if let Some(prev) = self.get(maj, min) {
                self.set(maj, min, val.scale(1.0 - frac).add(&prev.scale(frac)), 0.0);
            }
            *intersections = true;
        }
        if !*intersections {
            self.set(maj, min, val, min_dist);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(values: &mut [Vec<f32>], set: &[Vec<bool>], ax: &[f32], ay: &[f32]) -> bool {
        reinterpolate(Grid { values, set, axis: [ax, ay] }).unwrap()
    }

    #[test]
    fn fills_1d_gaps_linearly_and_extends_edges() {
        let mut v = vec![vec![0.0], vec![0.0], vec![10.0], vec![0.0]];
        let set = vec![vec![false], vec![true], vec![true], vec![false]];
        // x axis points 0, 0.25, 0.5, 1: ends extend the nearest set value.
        let mut values = v.clone();
        values[1][0] = 2.0;
        assert!(run(&mut values, &set, &[0.0, 0.25, 0.5, 1.0], &[0.0]));
        assert_eq!(values, vec![vec![2.0], vec![2.0], vec![10.0], vec![10.0]]);
        // A gap between set points is interpolated by axis position.
        v = vec![vec![0.0], vec![0.0], vec![8.0]];
        let set = vec![vec![true], vec![false], vec![true]];
        assert!(run(&mut v, &set, &[0.0, 0.25, 1.0], &[0.0]));
        assert_eq!(v[1][0], 2.0);
    }

    #[test]
    fn extrapolates_a_missing_corner() {
        // 2x2 with (1,1) unset: base + dX + dY.
        let mut v = vec![vec![1.0, 3.0], vec![2.0, 0.0]];
        let set = vec![vec![true, true], vec![true, false]];
        assert!(run(&mut v, &set, &[0.0, 1.0], &[0.0, 1.0]));
        assert_eq!(v[1][1], 4.0);
    }

    #[test]
    fn empty_grid_reports_no_points() {
        let mut v = vec![vec![0.0; 2]; 2];
        let set = vec![vec![false; 2]; 2];
        assert!(!run(&mut v, &set, &[0.0, 1.0], &[0.0, 1.0]));
    }

    #[test]
    fn shape_mismatch_is_an_error() {
        let mut v = vec![vec![0.0]];
        let set = vec![vec![true]];
        assert!(reinterpolate(Grid { values: &mut v, set: &set, axis: [&[0.0, 1.0], &[0.0]] }).is_err());
    }

    #[test]
    fn every_pattern_of_a_3x3_grid_completes() {
        // All 511 non-empty set patterns must fill completely.
        for mask in 1u32..512 {
            let set: Vec<Vec<bool>> =
                (0..3).map(|x| (0..3).map(|y| mask & (1 << (x * 3 + y)) != 0).collect()).collect();
            let mut v: Vec<Vec<f32>> = (0..3).map(|x| (0..3).map(|y| (x * 3 + y) as f32).collect()).collect();
            assert!(run(&mut v, &set, &[0.0, 0.5, 1.0], &[0.0, 0.3, 1.0]), "mask {mask:#b}");
            assert!(v.iter().flatten().all(|f| f.is_finite()), "mask {mask:#b}: {v:?}");
        }
    }

    #[test]
    fn cubic_passes_through_its_middle_points() {
        assert_eq!(cubic(&0.0f32, &1.0, &2.0, &3.0, 0.0), 1.0);
        assert_eq!(cubic(&0.0f32, &1.0, &2.0, &3.0, 1.0), 2.0);
        assert!((cubic(&0.0f32, &1.0, &2.0, &3.0, 0.5) - 1.5).abs() < 1e-6);
    }
}
