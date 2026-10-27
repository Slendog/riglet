//! Rig evaluator: parameter values -> deformed vertices, opacity and draw
//! order (spec E1–E7).

use std::borrow::Cow;
use std::collections::HashMap;

use crate::bind::TriangleGrid;
use crate::geometry::{self, Mat2};
use crate::model::{AxisInterpolation, Combine, MeshParent, Rgb, Vec2};
use crate::rig::{Rig, RigKind, RigNode, RigParam, RigSet};

/// Mesh-deformer bindings (E6): `(node, mesh deformer)` -> bound triangle
/// per point of `node`'s point set.
pub(crate) type Bindings = HashMap<(usize, usize), Vec<u32>>;

/// Minimum evaluated rotation-deformer scale (E4).
pub const MIN_SCALE: f32 = 1e-6;
/// Below this rest-Jacobian determinant the warp frame falls back to
/// dθ = 0, ds = 1 (E5).
const MIN_REST_DET: f32 = 1e-12;

/// Raw parameter values, one `[component 0, component 1]` pair per
/// parameter (component 1 is ignored for 1d parameters).
#[derive(Debug, Clone, PartialEq)]
pub struct ParamValues {
    pub values: Vec<[f32; 2]>,
}

impl ParamValues {
    pub fn defaults(rig: &Rig) -> Self {
        ParamValues { values: rig.params.iter().map(|p| p.default).collect() }
    }

    /// Sets a parameter by id. Returns false for an unknown id.
    pub fn set(&mut self, rig: &Rig, id: &str, value: [f32; 2]) -> bool {
        match rig.param_index(id).and_then(|i| self.values.get_mut(i)) {
            Some(slot) => {
                *slot = value;
                true
            }
            None => false,
        }
    }
}

/// Evaluated state of one drawable, in model space.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DrawableFrame {
    /// Node index in [`Rig::nodes`].
    pub node: usize,
    pub positions: Vec<Vec2>,
    /// Opacity to draw with: own × ancestors up to, not including, the
    /// nearest composite (R6). Equals E4's effective opacity outside
    /// composites.
    pub opacity: f32,
    pub draw_order: f32,
    pub multiply_color: Rgb,
    pub screen_color: Rgb,
    /// False when the node or any ancestor is hidden (model visibility).
    pub visible: bool,
}

/// A member of a draw scope (the root or a composite, E4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Member {
    /// Index into [`EvalOutput::drawables`].
    Drawable(usize),
    /// Index into [`EvalOutput::composites`].
    Composite(usize),
}

/// Evaluated state of one composite group (R6).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CompositeFrame {
    /// Node index in [`Rig::nodes`].
    pub node: usize,
    /// Layer opacity: own × ancestors up to, not including, the next
    /// enclosing composite (R6 rule 3).
    pub opacity: f32,
    pub draw_order: f32,
    /// False when the group or any ancestor is hidden.
    pub visible: bool,
    /// This composite's scope, bottom first.
    pub members: Vec<Member>,
}

/// Caller-owned evaluation output. Reuse one per rig across frames: buffers
/// keep their capacity, so steady-state evaluation reuses allocations.
#[derive(Debug, Clone, Default)]
pub struct EvalOutput {
    /// One entry per drawable, in node pre-order. The slot for a drawable
    /// never changes for a given rig, so renderers can key GPU buffers on it.
    pub drawables: Vec<DrawableFrame>,
    /// One entry per composite group, in node pre-order (stable slots).
    pub composites: Vec<CompositeFrame>,
    /// The top scope (everything outside composites), bottom first.
    pub members: Vec<Member>,
    scratch: Scratch,
}

impl EvalOutput {
    pub fn new() -> Self {
        Self::default()
    }

    /// Every drawable in final draw order, bottom first, with composite
    /// contents expanded in place.
    pub fn in_draw_order(&self) -> Vec<&DrawableFrame> {
        let mut out = Vec::with_capacity(self.drawables.len());
        // Explicit stack: composite nesting depth is untrusted.
        let mut stack: Vec<std::slice::Iter<'_, Member>> = vec![self.members.iter()];
        while let Some(top) = stack.last_mut() {
            match top.next() {
                None => {
                    stack.pop();
                }
                Some(Member::Drawable(i)) => out.extend(self.drawables.get(*i)),
                Some(Member::Composite(i)) => {
                    if let Some(c) = self.composites.get(*i) {
                        stack.push(c.members.iter());
                    }
                }
            }
        }
        out
    }

    /// The evaluated drawable for node `node`, if it is a drawable.
    pub fn drawable(&self, node: usize) -> Option<&DrawableFrame> {
        self.drawables.iter().find(|d| d.node == node)
    }
}

#[derive(Debug, Clone, Default)]
struct Scratch {
    values: Vec<[f32; 2]>,
    current: Vec<DeformerState>,
    opacity: Vec<f32>,
    visible: Vec<bool>,
    accum: Accum,
    /// A drawable's points without its own deltas (S7 `deltas: added`).
    without_deltas: Vec<Vec2>,
}

/// E2: clamp, or wrap for repeat parameters. Non-finite input means default.
fn normalize(p: &RigParam, raw: [f32; 2]) -> [f32; 2] {
    let mut out = p.default;
    for c in 0..p.kind.components() {
        let (Some(&v), Some(&lo), Some(&hi), Some(slot)) = (raw.get(c), p.min.get(c), p.max.get(c), out.get_mut(c))
        else {
            continue;
        };
        if !v.is_finite() {
            continue;
        }
        *slot = if p.repeat {
            let w = lo + (v - lo).rem_euclid(hi - lo);
            if w >= hi { lo } else { w }
        } else {
            v.clamp(lo, hi)
        };
    }
    out
}

/// Where `v` falls on one axis (E3): up to four (key index, weight) taps
/// with distinct indices, and the key used for draw order.
#[derive(Debug, Clone, Copy)]
struct AxisTaps {
    taps: [(usize, f32); 4],
    len: usize,
    nearest: usize,
}

impl AxisTaps {
    fn one(i: usize) -> AxisTaps {
        AxisTaps { taps: [(i, 1.0); 4], len: 1, nearest: i }
    }

    fn taps(&self) -> &[(usize, f32)] {
        self.taps.get(..self.len).unwrap_or(&[])
    }

    /// Adds `w` to key `i`, merging repeated indices (clamped cubic ends).
    fn add(&mut self, i: usize, w: f32) {
        let len = self.len;
        if let Some(t) = self.taps.iter_mut().take(len).find(|t| t.0 == i) {
            t.1 += w;
        } else if let Some(t) = self.taps.get_mut(len) {
            *t = (i, w);
            self.len += 1;
        }
    }
}

/// Uniform Catmull-Rom weights for p0..p3 at t (E3 cubic; Inochi2D's
/// inmath `cubic`).
pub fn cubic_weights(t: f32) -> [f32; 4] {
    let (t2, t3) = (t * t, t * t * t);
    [-0.5 * t3 + t2 - 0.5 * t, 1.5 * t3 - 2.5 * t2 + 1.0, -1.5 * t3 + 2.0 * t2 + 0.5 * t, 0.5 * t3 - 0.5 * t2]
}

fn axis_taps(keys: &[f32], v: f32, interpolation: AxisInterpolation) -> AxisTaps {
    let n = keys.len();
    let first = keys.first().copied().unwrap_or(0.0);
    let last = keys.last().copied().unwrap_or(0.0);
    if n <= 1 || v <= first {
        return AxisTaps::one(0);
    }
    if v >= last {
        return AxisTaps::one(n - 1);
    }
    let i = keys.partition_point(|&k| k <= v).saturating_sub(1).min(n - 2);
    let (k0, k1) = (keys.get(i).copied().unwrap_or(first), keys.get(i + 1).copied().unwrap_or(last));
    let t = (v - k0) / (k1 - k0);
    // Draw order uses the nearest key on linear and cubic axes; ties go to
    // the lower key.
    let nearest = if t > 0.5 { i + 1 } else { i };
    if interpolation == AxisInterpolation::Step || t <= 0.0 {
        return AxisTaps::one(i);
    }
    let mut a = AxisTaps { taps: [(0, 0.0); 4], len: 0, nearest };
    match interpolation {
        AxisInterpolation::Step => {}
        AxisInterpolation::Linear => {
            a.add(i, 1.0 - t);
            a.add(i + 1, t);
        }
        AxisInterpolation::Cubic => {
            let [w0, w1, w2, w3] = cubic_weights(t);
            a.add(i.saturating_sub(1), w0);
            a.add(i, w1);
            a.add(i + 1, w2);
            a.add((i + 2).min(n - 1), w3);
        }
    }
    a
}

/// Sum of keyform deltas ("add" sets) and product of factors ("multiply"
/// sets, S2) for one node.
#[derive(Debug, Clone)]
struct Accum {
    positions: Vec<Vec2>,
    opacity: f32,
    draw_order: f32,
    angle: f32,
    origin: Vec2,
    scale: f32,
    scale_xy: Vec2,
    multiply_color: Rgb,
    screen_color: Rgb,
    opacity_factor: f32,
    scale_factor: Vec2,
    multiply_color_factor: Rgb,
}

impl Default for Accum {
    fn default() -> Self {
        Accum {
            positions: Vec::new(),
            opacity: 0.0,
            draw_order: 0.0,
            angle: 0.0,
            origin: [0.0; 2],
            scale: 0.0,
            scale_xy: [0.0; 2],
            multiply_color: [0.0; 3],
            screen_color: [0.0; 3],
            opacity_factor: 1.0,
            scale_factor: [1.0; 2],
            multiply_color_factor: [1.0; 3],
        }
    }
}

impl Accum {
    /// Zeroes every delta and resets every factor to 1, keeping the
    /// positions buffer's capacity.
    fn reset(&mut self, point_count: usize) {
        let positions = std::mem::take(&mut self.positions);
        *self = Accum { positions, ..Default::default() };
        self.positions.clear();
        self.positions.resize(point_count, [0.0; 2]);
    }
}

fn add_scaled<const N: usize>(acc: &mut [f32; N], delta: Option<[f32; N]>, w: f32) {
    if let Some(d) = delta {
        for (a, d) in acc.iter_mut().zip(d) {
            *a += d * w;
        }
    }
}

/// E3: per-axis blend inside one set (linear: multilinear; cubic:
/// Catmull-Rom; draw order: nearest grid point).
/// "add" sets add their interpolated deltas; "multiply" sets multiply in
/// their interpolated factors, a missing property counting as 1 (S2).
fn eval_set(set: &RigSet, values: &[[f32; 2]], acc: &mut Accum) {
    let multiply = set.combine == Combine::Multiply;
    let (mut opacity_f, mut scale_f, mut scale_xy_f, mut color_f) = (0.0f32, 0.0f32, [0.0f32; 2], [0.0f32; 3]);
    let mut nearest = 0usize;
    let mut stride = 1usize;
    let mut axes: Vec<(usize, AxisTaps)> = Vec::with_capacity(set.axes.len());
    for axis in &set.axes {
        let v = values.get(axis.param).and_then(|p| p.get(axis.component)).copied().unwrap_or(0.0);
        let taps = axis_taps(&axis.keys, v, axis.interpolation);
        nearest += taps.nearest * stride;
        axes.push((stride, taps));
        stride = stride.saturating_mul(axis.keys.len());
    }

    // Every combination of one tap per axis (an odometer over the axes); at
    // most the grid size, since taps per axis <= keys per axis.
    let mut digit = vec![0usize; axes.len()];
    loop {
        let mut index = 0usize;
        let mut w = 1.0f32;
        for ((s, taps), &d) in axes.iter().zip(&digit) {
            let (k, wk) = taps.taps().get(d).copied().unwrap_or((0, 0.0));
            index += k * s;
            w *= wk;
        }
        if w != 0.0
            && let Some(kf) = set.keyforms.get(index)
        {
            if multiply {
                opacity_f += kf.opacity.unwrap_or(1.0) * w;
                scale_f += kf.scale.unwrap_or(1.0) * w;
                add_scaled(&mut scale_xy_f, Some(kf.scale_xy.unwrap_or([1.0; 2])), w);
                add_scaled(&mut color_f, Some(kf.multiply_color.unwrap_or([1.0; 3])), w);
            } else {
                if let Some(p) = &kf.positions {
                    for (a, d) in acc.positions.iter_mut().zip(p) {
                        a[0] += d[0] * w;
                        a[1] += d[1] * w;
                    }
                }
                acc.opacity += kf.opacity.unwrap_or(0.0) * w;
                acc.angle += kf.angle.unwrap_or(0.0) * w;
                acc.scale += kf.scale.unwrap_or(0.0) * w;
                add_scaled(&mut acc.scale_xy, kf.scale_xy, w);
                add_scaled(&mut acc.origin, kf.origin, w);
                add_scaled(&mut acc.multiply_color, kf.multiply_color, w);
                add_scaled(&mut acc.screen_color, kf.screen_color, w);
            }
        }
        let mut done = true;
        for ((_, taps), d) in axes.iter().zip(digit.iter_mut()) {
            *d += 1;
            if *d < taps.len {
                done = false;
                break;
            }
            *d = 0;
        }
        if done {
            break;
        }
    }
    if multiply {
        acc.opacity_factor *= opacity_f;
        for (a, (f, xy)) in acc.scale_factor.iter_mut().zip(std::iter::repeat(scale_f).zip(scale_xy_f)) {
            *a *= f * xy;
        }
        for (a, f) in acc.multiply_color_factor.iter_mut().zip(color_f) {
            *a *= f;
        }
        return;
    }
    if let Some(kf) = set.keyforms.get(nearest) {
        acc.draw_order += kf.draw_order.unwrap_or(0.0);
    }
}

/// Current (or rest) geometry of a deformer, in its parent space.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct DeformerState {
    points: Vec<Vec2>,
    origin: Vec2,
    angle: f32,
    /// Per axis (S1).
    scale: Vec2,
}

fn rest_state(node: &RigNode) -> DeformerState {
    match &node.kind {
        RigKind::Warp { points, .. } => DeformerState { points: points.clone(), ..Default::default() },
        RigKind::MeshDeformer { positions, .. } | RigKind::Drawable { positions, .. } => {
            DeformerState { points: positions.clone(), ..Default::default() }
        }
        RigKind::Rotation { origin, angle, scale, .. } => {
            DeformerState { origin: *origin, angle: *angle, scale: *scale, ..Default::default() }
        }
        _ => DeformerState::default(),
    }
}

enum BindMode<'a, 'g> {
    Compute(&'a mut Binder<'g>),
    Lookup(&'a Bindings),
}

/// Builds bindings at load, with one triangle grid per mesh deformer and a
/// budget on total binding work (see [`TriangleGrid::work`]).
struct Binder<'g> {
    bindings: Bindings,
    grids: HashMap<usize, TriangleGrid>,
    rest: &'g [DeformerState],
    nodes: &'g [RigNode],
    budget: u64,
    exceeded: bool,
}

impl Binder<'_> {
    fn total_work(&self) -> u64 {
        self.grids.values().map(TriangleGrid::work).fold(0, u64::saturating_add)
    }
}

impl<'a> BindMode<'a, '_> {
    /// Bound triangle per point of `pts` for the `(node, mesh deformer)` pair.
    fn get(&mut self, key: (usize, usize), pts: &[Vec2]) -> Option<Cow<'a, [u32]>> {
        match self {
            BindMode::Lookup(b) => {
                let b: &'a Bindings = b;
                b.get(&key).map(|v| Cow::Borrowed(v.as_slice()))
            }
            BindMode::Compute(binder) => {
                if let Some(done) = binder.bindings.get(&key) {
                    return Some(Cow::Owned(done.clone()));
                }
                let mesh = key.1;
                let RigKind::MeshDeformer { indices, .. } = &binder.nodes.get(mesh)?.kind else { return None };
                let rest = binder.rest.get(mesh).map_or(&[][..], |s| s.points.as_slice());
                let mut used = binder.total_work();
                let grid = binder.grids.entry(mesh).or_insert_with(|| TriangleGrid::new(rest, indices));
                let mut tris = Vec::with_capacity(pts.len());
                for &p in pts {
                    let before = grid.work();
                    tris.push(grid.bind(p));
                    used = used.saturating_add(grid.work() - before);
                    // Checked per point: one query costs at most a few
                    // passes over one mesh, so overshoot stays small.
                    if used > binder.budget {
                        binder.exceeded = true;
                        return None;
                    }
                }
                binder.bindings.insert(key, tris.clone());
                Some(Cow::Owned(tris))
            }
        }
    }
}

/// Rotation delta (degrees) and scale factor of a frame Jacobian relative to
/// its rest Jacobian (E5).
fn frame_change(j: Mat2, j0: Mat2) -> (f32, f32) {
    let (d, d0) = (geometry::det(j), geometry::det(j0));
    match (geometry::polar_rotation(j), geometry::polar_rotation(j0)) {
        (Some(r), Some(r0)) if d0.abs() >= MIN_REST_DET => ((r - r0).to_degrees(), (d.abs() / d0.abs()).sqrt()),
        _ => (0.0, 1.0),
    }
}

/// Applies `f` to every point of `pts` and, when present, of `pts0`.
fn for_both(pts: &mut [Vec2], pts0: &mut Option<&mut [Vec2]>, f: impl Fn(Vec2) -> Vec2) {
    for p in pts.iter_mut() {
        *p = f(*p);
    }
    if let Some(p0) = pts0 {
        for p in p0.iter_mut() {
            *p = f(*p);
        }
    }
}

/// E1: maps `pts` (in `node`'s parent space) up through every ancestor
/// deformer to model space. `pts0`, when given, are the same points without
/// the node's own position deltas; mesh deformers with S7 `deltas: added`
/// map those and add the difference (E6), and they travel along.
fn map_up(
    nodes: &[RigNode],
    current: &[DeformerState],
    rest: &[DeformerState],
    node: usize,
    pts: &mut [Vec2],
    mut pts0: Option<&mut [Vec2]>,
    bind: &mut BindMode<'_, '_>,
) {
    let mut owner = nodes.get(node).and_then(|n| n.owner);
    // S7: the points came through a pointwise or translate rotation
    // deformer (triangles are walked each frame), and a translate one's
    // index and origin, carried up to the mesh deformer.
    let mut walk = false;
    // (translate deformer, its current origin, its origin without its own
    // deltas), both carried up in the same frames.
    let mut translate: Option<(usize, Vec2, Vec2)> = None;
    while let Some(d) = owner {
        let (Some(dn), Some(cur)) = (nodes.get(d), current.get(d)) else { return };
        let rest_d = rest.get(d);
        match &dn.kind {
            RigKind::Warp { rows, cols, .. } => {
                for_both(pts, &mut pts0, |p| geometry::warp(&cur.points, *rows, *cols, p).0);
                if let Some((_, o, o0)) = &mut translate {
                    *o = geometry::warp(&cur.points, *rows, *cols, *o).0;
                    *o0 = geometry::warp(&cur.points, *rows, *cols, *o0).0;
                }
                owner = dn.owner;
            }
            RigKind::MeshDeformer { indices, deltas_added, neighbors, .. } => {
                let rest_pts = rest_d.map_or(&[][..], |r| r.points.as_slice());
                let map = |p: Vec2, tri: u32| {
                    let tri = tri as usize;
                    let r = geometry::triangle(rest_pts, indices, tri);
                    let c = geometry::triangle(&cur.points, indices, tri);
                    geometry::triangle_map(r, c, p)
                };
                // S7 walk: from the rest-bound triangle toward `p`.
                let find = |p: Vec2, bound: u32| {
                    if walk { walk_triangle(rest_pts, indices, neighbors, bound, p) } else { bound }
                };
                if let Some((r, o_cur, o_rest)) = translate.take() {
                    // S7 translate: everything moves by the displacement at
                    // the translate deformer's origin (without its own
                    // deltas when this mesh deformer adds deltas).
                    let o = if *deltas_added { o_rest } else { o_cur };
                    if let Some(tri) = bind.get((r, d), &[o]).and_then(|t| t.first().copied()) {
                        let shift = geometry::sub(map(o, find(o, tri)), o);
                        for_both(pts, &mut pts0, |p| geometry::add(p, shift));
                    }
                } else if let Some(tris) = bind.get((node, d), pts) {
                    match pts0.as_deref_mut() {
                        // S7 added: T(p0) + (p - p0); p0 keeps excluding the
                        // deltas; the lookup uses p0 (the point without them).
                        Some(p0) if *deltas_added => {
                            for ((p, q), &tri) in pts.iter_mut().zip(p0.iter_mut()).zip(tris.iter()) {
                                let mapped = map(*q, find(*q, tri));
                                *p = geometry::add(mapped, geometry::sub(*p, *q));
                                *q = mapped;
                            }
                        }
                        p0 => {
                            if let Some(p0) = p0 {
                                for ((p, q), &tri) in pts.iter().zip(p0.iter_mut()).zip(tris.iter()) {
                                    *q = map(*q, find(*p, tri));
                                }
                            }
                            for (p, &tri) in pts.iter_mut().zip(tris.iter()) {
                                *p = map(*p, find(*p, tri));
                            }
                        }
                    }
                }
                walk = false;
                owner = dn.owner;
            }
            RigKind::Rotation { mesh_parent, .. } => {
                let parent = dn.owner.and_then(|o| nodes.get(o).map(|n| (o, n)));
                let (base, d_angle, d_scale, next) = match parent {
                    Some((w, wn @ RigNode { kind: RigKind::Warp { rows, cols, .. }, .. })) => {
                        let cur_w = current.get(w).map_or(&[][..], |s| s.points.as_slice());
                        let rest_w = rest.get(w).map_or(&[][..], |s| s.points.as_slice());
                        let (o, j) = geometry::warp(cur_w, *rows, *cols, cur.origin);
                        let (_, j0) = geometry::warp(rest_w, *rows, *cols, cur.origin);
                        let (da, ds) = frame_change(j, j0);
                        (o, da, ds, wn.owner)
                    }
                    // S7 pointwise: map into the mesh deformer's space as if
                    // it were a group; the mesh deformer then maps each
                    // point through its own walked triangle (E6).
                    Some((_, RigNode { kind: RigKind::MeshDeformer { .. }, .. }))
                        if *mesh_parent == MeshParent::Pointwise =>
                    {
                        walk = true;
                        (cur.origin, 0.0, 1.0, dn.owner)
                    }
                    // S7 translate: rigid up to the nearest mesh deformer,
                    // which then shifts everything by its displacement at
                    // this origin (it may be above pointwise deformers).
                    _ if *mesh_parent == MeshParent::Translate => {
                        walk = true;
                        let o0 = rest_d.map_or(cur.origin, |r| r.origin);
                        translate = Some((d, cur.origin, o0));
                        (cur.origin, 0.0, 1.0, dn.owner)
                    }
                    Some((m, mn @ RigNode { kind: RigKind::MeshDeformer { indices, .. }, .. })) => {
                        let cur_m = current.get(m).map_or(&[][..], |s| s.points.as_slice());
                        let rest_m = rest.get(m).map_or(&[][..], |s| s.points.as_slice());
                        let tri = bind.get((d, m), &[cur.origin]).and_then(|t| t.first().copied());
                        match tri {
                            Some(tri) => {
                                let r = geometry::triangle(rest_m, indices, tri as usize);
                                let c = geometry::triangle(cur_m, indices, tri as usize);
                                let o = geometry::triangle_map(r, c, cur.origin);
                                let identity = [[1.0, 0.0], [0.0, 1.0]];
                                let (da, ds) =
                                    geometry::triangle_jacobian(r, c).map_or((0.0, 1.0), |j| frame_change(j, identity));
                                (o, da, ds, mn.owner)
                            }
                            None => (cur.origin, 0.0, 1.0, mn.owner),
                        }
                    }
                    _ => (cur.origin, 0.0, 1.0, dn.owner),
                };
                // E5: origin + ds * R(angle + dθ) * diag(sx, sy) * p.
                let [sx, sy] = cur.scale;
                let angle = cur.angle + d_angle;
                let frame = |p: Vec2| {
                    let scaled = [p[0] * sx, p[1] * sy];
                    geometry::add(base, geometry::scale(geometry::rotate(scaled, angle), d_scale))
                };
                for_both(pts, &mut pts0, frame);
                // A translate origin set below travels with the points (this
                // deformer's own origin is already in its parent space).
                if let Some((t, o, o0)) = &mut translate
                    && *t != d
                {
                    *o = frame(*o);
                    *o0 = frame(*o0);
                }
                owner = next;
            }
            RigKind::Group { .. } | RigKind::Drawable { .. } => owner = dn.owner,
        }
    }
}

/// Walk limit for S7 lookups (E6).
pub const WALK_STEPS: usize = 8;

/// S7 (E6): from triangle `start`, cross the edge opposite the most
/// negative barycentric coordinate of `p` until a triangle contains `p`
/// (tolerance 1e-6), the mesh boundary or a degenerate triangle is reached,
/// or after WALK_STEPS moves. O(WALK_STEPS) per point.
fn walk_triangle(rest: &[Vec2], indices: &[u32], neighbors: &[[u32; 3]], start: u32, p: Vec2) -> u32 {
    let mut t = start;
    for _ in 0..WALK_STEPS {
        let Some(l) = geometry::barycentric(geometry::triangle(rest, indices, t as usize), p) else { return t };
        let (k, &m) = l.iter().enumerate().min_by(|a, b| a.1.total_cmp(b.1)).unwrap_or((0, &0.0));
        if m >= -1e-6 {
            return t;
        }
        match neighbors.get(t as usize).and_then(|n| n.get(k)).copied() {
            Some(n) if n != u32::MAX => t = n,
            _ => return t,
        }
    }
    t
}

fn adds_deltas_above(nodes: &[RigNode], node: usize) -> bool {
    let mut owner = nodes.get(node).and_then(|n| n.owner);
    while let Some(o) = owner {
        let Some(n) = nodes.get(o) else { return false };
        if matches!(n.kind, RigKind::MeshDeformer { deltas_added: true, .. }) {
            return true;
        }
        owner = n.owner;
    }
    false
}

/// Rest geometry of every node, computed once per rig.
pub(crate) fn rest_states(nodes: &[RigNode]) -> Vec<DeformerState> {
    nodes.iter().map(rest_state).collect()
}

/// Binding work exceeded `Limits::binding_work_budget`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BindingLimitExceeded;

/// Derives E6 bindings by walking every node's rest points up the tree at
/// rest. At rest, mesh deformers are the identity, so the walk needs no
/// bindings of its own.
/// Returns the bindings and the binding work done.
pub(crate) fn compute_bindings(
    nodes: &[RigNode],
    rest: &[DeformerState],
    budget: u64,
) -> Result<(Bindings, u64), BindingLimitExceeded> {
    let has_mesh = nodes.iter().any(|n| matches!(n.kind, RigKind::MeshDeformer { .. }));
    if !has_mesh {
        return Ok((Bindings::new(), 0));
    }
    let mut binder = Binder { bindings: Bindings::new(), grids: HashMap::new(), rest, nodes, budget, exceeded: false };
    for (i, node) in nodes.iter().enumerate() {
        if let Some(mut pts) = node.kind.rest_points() {
            map_up(nodes, rest, rest, i, &mut pts, None, &mut BindMode::Compute(&mut binder));
            if binder.exceeded {
                return Err(BindingLimitExceeded);
            }
        }
    }
    let work = binder.total_work();
    Ok((binder.bindings, work))
}

impl Rig {
    /// Evaluates the rig at `params` into `out` (E1–E7), reusing `out`'s
    /// buffers. Parameters missing from `params` use their defaults.
    pub fn evaluate(&self, params: &ParamValues, out: &mut EvalOutput) {
        let nodes = &self.nodes;
        let rest = &self.rest_states;
        let Scratch { values, current, opacity, visible, accum, without_deltas } = &mut out.scratch;

        values.clear();
        values.extend(
            self.params
                .iter()
                .enumerate()
                .map(|(i, p)| normalize(p, params.values.get(i).copied().unwrap_or(p.default))),
        );
        current.resize_with(nodes.len(), DeformerState::default);
        opacity.clear();
        visible.clear();
        out.drawables.resize_with(self.drawables.len(), DrawableFrame::default);
        out.composites.resize_with(self.composites.len(), CompositeFrame::default);

        let mut slot = 0usize;
        let mut composite_slot = 0usize;
        for ((node, state), rest_state) in nodes.iter().zip(current.iter_mut()).zip(rest) {
            let point_count = if matches!(node.kind, RigKind::Rotation { .. }) { 0 } else { rest_state.points.len() };
            accum.reset(point_count);
            for set in &node.sets {
                eval_set(set, values, accum);
            }

            // Parents precede children in pre-order. Opacity stops at a
            // composite parent: the composite applies it to the layer (R6).
            let (parent_opacity, parent_visible) = node.parent.map_or((1.0, true), |p| {
                let parent_is_composite =
                    matches!(nodes.get(p).map(|n| &n.kind), Some(RigKind::Group { composite: true, .. }));
                let o = if parent_is_composite { 1.0 } else { opacity.get(p).copied().unwrap_or(1.0) };
                (o, visible.get(p).copied().unwrap_or(true))
            });
            let own_opacity = ((node.opacity + accum.opacity) * accum.opacity_factor).clamp(0.0, 1.0);
            opacity.push(own_opacity * parent_opacity);
            visible.push(node.visible && parent_visible);

            state.points.clear();
            state.points.extend(rest_state.points.iter().zip(&accum.positions).map(|(r, d)| geometry::add(*r, *d)));
            state.origin = geometry::add(rest_state.origin, accum.origin);
            state.angle = rest_state.angle + accum.angle;
            // E4/S1/S2: per axis (rest + scale + scaleXY) * factors.
            for (k, out) in state.scale.iter_mut().enumerate() {
                let rest = rest_state.scale.get(k).copied().unwrap_or(1.0);
                let delta = accum.scale + accum.scale_xy.get(k).copied().unwrap_or(0.0);
                let factor = accum.scale_factor.get(k).copied().unwrap_or(1.0);
                // E4: |scale| >= MIN_SCALE, keeping the sign (a negative scale
                // mirrors; 0 counts as positive).
                let v = (rest + delta) * factor;
                *out = if v < 0.0 { v.min(-MIN_SCALE) } else { v.max(MIN_SCALE) };
            }

            if let RigKind::Drawable { draw_order, multiply_color, screen_color, .. } = &node.kind {
                let clamp3 = |mut rest: Rgb, d: Rgb| {
                    for (r, d) in rest.iter_mut().zip(d) {
                        *r = (*r + d).clamp(0.0, 1.0);
                    }
                    rest
                };
                if let Some(frame) = out.drawables.get_mut(slot) {
                    frame.draw_order = draw_order + accum.draw_order;
                    let mut multiply = accum.multiply_color;
                    for ((m, f), r) in multiply.iter_mut().zip(accum.multiply_color_factor).zip(multiply_color) {
                        // (rest + delta) * factor; clamp3 adds rest back.
                        *m = (r + *m) * f - r;
                    }
                    frame.multiply_color = clamp3(*multiply_color, multiply);
                    frame.screen_color = clamp3(*screen_color, accum.screen_color);
                }
                slot += 1;
            }
            if let RigKind::Group { composite: true, draw_order, .. } = &node.kind {
                if let Some(frame) = out.composites.get_mut(composite_slot) {
                    frame.draw_order = draw_order + accum.draw_order;
                }
                composite_slot += 1;
            }
        }

        let mut bind = BindMode::Lookup(&self.bindings);
        for (frame, &node) in out.drawables.iter_mut().zip(&self.drawables) {
            frame.node = node;
            frame.opacity = opacity.get(node).copied().unwrap_or(0.0);
            frame.visible = visible.get(node).copied().unwrap_or(false);
            frame.positions.clear();
            if let Some(state) = current.get(node) {
                frame.positions.extend_from_slice(&state.points);
            }
            // S7: track the points without their own deltas only when a
            // mesh deformer above adds them.
            let mut track = None;
            if adds_deltas_above(nodes, node) {
                without_deltas.clear();
                if let Some(r) = rest.get(node) {
                    without_deltas.extend_from_slice(&r.points);
                }
                track = Some(without_deltas.as_mut_slice());
            }
            map_up(nodes, current, rest, node, &mut frame.positions, track, &mut bind);
        }

        for (frame, &node) in out.composites.iter_mut().zip(&self.composites) {
            frame.node = node;
            frame.opacity = opacity.get(node).copied().unwrap_or(0.0);
            frame.visible = visible.get(node).copied().unwrap_or(false);
            frame.members.clear();
        }

        // Scopes (E4): members in pre-order, then a stable sort by draw
        // order, so ties keep tree order.
        out.members.clear();
        for (index, node_scope) in self.scope.iter().enumerate() {
            let Some(Some(member)) = self.slot.get(index).copied() else { continue };
            let list = match node_scope {
                None => &mut out.members,
                Some(c) => match out.composites.get_mut(*c) {
                    Some(frame) => &mut frame.members,
                    None => continue,
                },
            };
            list.push(member);
        }
        let (drawables, composites) = (&out.drawables, &mut out.composites);
        let key = |m: &Member, composites: &[CompositeFrame]| match *m {
            Member::Drawable(i) => drawables.get(i).map_or(0.0, |d| d.draw_order),
            Member::Composite(i) => composites.get(i).map_or(0.0, |c| c.draw_order),
        };
        let mut members = std::mem::take(&mut out.members);
        members.sort_by(|a, b| key(a, composites).total_cmp(&key(b, composites)));
        out.members = members;
        for i in 0..composites.len() {
            let mut list = composites.get_mut(i).map(|c| std::mem::take(&mut c.members)).unwrap_or_default();
            list.sort_by(|a, b| key(a, composites).total_cmp(&key(b, composites)));
            if let Some(c) = composites.get_mut(i) {
                c.members = list;
            }
        }
    }
}
