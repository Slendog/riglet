//! Rig evaluator: parameter values -> deformed vertices, opacity and draw
//! order (spec E1–E7).

use std::borrow::Cow;
use std::collections::HashMap;

use crate::bind::TriangleGrid;
use crate::geometry::{self, Mat2};
use crate::model::{AxisInterpolation, Rgb, Vec2};
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
    /// Effective opacity (own × ancestors), E4.
    pub opacity: f32,
    pub draw_order: f32,
    pub multiply_color: Rgb,
    pub screen_color: Rgb,
    /// False when the node or any ancestor is hidden (model visibility).
    pub visible: bool,
}

/// Caller-owned evaluation output. Reuse one per rig across frames: buffers
/// keep their capacity, so steady-state evaluation reuses allocations.
#[derive(Debug, Clone, Default)]
pub struct EvalOutput {
    /// One entry per drawable, in node pre-order. The slot for a drawable
    /// never changes for a given rig, so renderers can key GPU buffers on it.
    pub drawables: Vec<DrawableFrame>,
    /// Indices into `drawables`, bottom first (E4).
    pub draw_order: Vec<usize>,
    scratch: Scratch,
}

impl EvalOutput {
    pub fn new() -> Self {
        Self::default()
    }

    /// Drawables in draw order, bottom first.
    pub fn in_draw_order(&self) -> impl Iterator<Item = &DrawableFrame> {
        self.draw_order.iter().filter_map(|&i| self.drawables.get(i))
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

/// Position of `v` on one axis (E3): lower key index, weight toward the
/// next key, and the key used for draw order.
fn axis_position(keys: &[f32], v: f32, interpolation: AxisInterpolation) -> (usize, f32, usize) {
    let n = keys.len();
    let first = keys.first().copied().unwrap_or(0.0);
    let last = keys.last().copied().unwrap_or(0.0);
    if n <= 1 || v <= first {
        return (0, 0.0, 0);
    }
    if v >= last {
        return (n - 1, 0.0, n - 1);
    }
    let i = keys.partition_point(|&k| k <= v).saturating_sub(1).min(n - 2);
    let (k0, k1) = (keys.get(i).copied().unwrap_or(first), keys.get(i + 1).copied().unwrap_or(last));
    match interpolation {
        AxisInterpolation::Step => (i, 0.0, i),
        AxisInterpolation::Linear => {
            let t = (v - k0) / (k1 - k0);
            // Draw order uses the nearest key; ties go to the lower key.
            let nearest = if t > 0.5 { i + 1 } else { i };
            (i, t, nearest)
        }
    }
}

/// Sum of keyform deltas for one node.
#[derive(Debug, Clone, Default)]
struct Accum {
    positions: Vec<Vec2>,
    opacity: f32,
    draw_order: f32,
    angle: f32,
    origin: Vec2,
    scale: f32,
    multiply_color: Rgb,
    screen_color: Rgb,
}

impl Accum {
    /// Zeroes every delta, keeping the positions buffer's capacity.
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

/// E3: multilinear blend inside one set (draw order: nearest grid point).
fn eval_set(set: &RigSet, values: &[[f32; 2]], acc: &mut Accum) {
    let mut base = 0usize;
    let mut stride = 1usize;
    let mut nearest = 0usize;
    let mut branching: Vec<(usize, f32)> = Vec::new();
    for axis in &set.axes {
        let v = values.get(axis.param).and_then(|p| p.get(axis.component)).copied().unwrap_or(0.0);
        let (i, t, near) = axis_position(&axis.keys, v, axis.interpolation);
        base += i * stride;
        nearest += near * stride;
        if t > 0.0 {
            branching.push((stride, t));
        }
        stride = stride.saturating_mul(axis.keys.len());
    }

    let corners = 1u64.checked_shl(branching.len() as u32).unwrap_or(u64::MAX);
    for mask in 0..corners {
        let mut index = base;
        let mut w = 1.0f32;
        for (bit, &(s, t)) in branching.iter().enumerate() {
            if mask & (1 << bit) != 0 {
                index += s;
                w *= t;
            } else {
                w *= 1.0 - t;
            }
        }
        if w == 0.0 {
            continue;
        }
        let Some(kf) = set.keyforms.get(index) else { continue };
        if let Some(p) = &kf.positions {
            for (a, d) in acc.positions.iter_mut().zip(p) {
                a[0] += d[0] * w;
                a[1] += d[1] * w;
            }
        }
        acc.opacity += kf.opacity.unwrap_or(0.0) * w;
        acc.angle += kf.angle.unwrap_or(0.0) * w;
        acc.scale += kf.scale.unwrap_or(0.0) * w;
        add_scaled(&mut acc.origin, kf.origin, w);
        add_scaled(&mut acc.multiply_color, kf.multiply_color, w);
        add_scaled(&mut acc.screen_color, kf.screen_color, w);
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
    scale: f32,
}

fn rest_state(node: &RigNode) -> DeformerState {
    match &node.kind {
        RigKind::Warp { points, .. } => DeformerState { points: points.clone(), ..Default::default() },
        RigKind::MeshDeformer { positions, .. } | RigKind::Drawable { positions, .. } => {
            DeformerState { points: positions.clone(), ..Default::default() }
        }
        RigKind::Rotation { origin, angle, scale } => {
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
/// cap on total binding work (points × triangles).
struct Binder<'g> {
    bindings: Bindings,
    grids: HashMap<usize, TriangleGrid>,
    rest: &'g [DeformerState],
    nodes: &'g [RigNode],
    work: u64,
    cap: u64,
    exceeded: bool,
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
                let mesh = key.1;
                let RigKind::MeshDeformer { indices, .. } = &binder.nodes.get(mesh)?.kind else { return None };
                let triangles = (indices.len() / 3) as u64;
                binder.work = binder.work.saturating_add((pts.len() as u64).saturating_mul(triangles));
                if binder.work > binder.cap {
                    binder.exceeded = true;
                    return None;
                }
                let rest = binder.rest.get(mesh).map_or(&[][..], |s| s.points.as_slice());
                let grid = binder.grids.entry(mesh).or_insert_with(|| TriangleGrid::new(rest, indices));
                let tris: Vec<u32> = pts.iter().map(|&p| grid.bind(p)).collect();
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

/// E1: maps `pts` (in `node`'s parent space) up through every ancestor
/// deformer to model space.
fn map_up(
    nodes: &[RigNode],
    current: &[DeformerState],
    rest: &[DeformerState],
    node: usize,
    pts: &mut [Vec2],
    bind: &mut BindMode<'_, '_>,
) {
    let mut owner = nodes.get(node).and_then(|n| n.owner);
    while let Some(d) = owner {
        let (Some(dn), Some(cur)) = (nodes.get(d), current.get(d)) else { return };
        let rest_d = rest.get(d);
        match &dn.kind {
            RigKind::Warp { rows, cols, .. } => {
                for p in pts.iter_mut() {
                    *p = geometry::warp(&cur.points, *rows, *cols, *p).0;
                }
                owner = dn.owner;
            }
            RigKind::MeshDeformer { indices, .. } => {
                let rest_pts = rest_d.map_or(&[][..], |r| r.points.as_slice());
                if let Some(tris) = bind.get((node, d), pts) {
                    for (p, &tri) in pts.iter_mut().zip(tris.iter()) {
                        let tri = tri as usize;
                        let r = geometry::triangle(rest_pts, indices, tri);
                        let c = geometry::triangle(&cur.points, indices, tri);
                        *p = geometry::triangle_map(r, c, *p);
                    }
                }
                owner = dn.owner;
            }
            RigKind::Rotation { .. } => {
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
                let s = cur.scale * d_scale;
                for p in pts.iter_mut() {
                    *p = geometry::add(base, geometry::scale(geometry::rotate(*p, cur.angle + d_angle), s));
                }
                owner = next;
            }
            RigKind::Group { .. } | RigKind::Drawable { .. } => owner = dn.owner,
        }
    }
}

/// Rest geometry of every node, computed once per rig.
pub(crate) fn rest_states(nodes: &[RigNode]) -> Vec<DeformerState> {
    nodes.iter().map(rest_state).collect()
}

/// Binding work exceeded `Limits::max_mesh_binding_pairs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BindingLimitExceeded;

/// Derives E6 bindings by walking every node's rest points up the tree at
/// rest. At rest, mesh deformers are the identity, so the walk needs no
/// bindings of its own.
pub(crate) fn compute_bindings(
    nodes: &[RigNode],
    rest: &[DeformerState],
    max_pairs: u64,
) -> Result<Bindings, BindingLimitExceeded> {
    let has_mesh = nodes.iter().any(|n| matches!(n.kind, RigKind::MeshDeformer { .. }));
    if !has_mesh {
        return Ok(Bindings::new());
    }
    let mut binder = Binder {
        bindings: Bindings::new(),
        grids: HashMap::new(),
        rest,
        nodes,
        work: 0,
        cap: max_pairs,
        exceeded: false,
    };
    for (i, node) in nodes.iter().enumerate() {
        if let Some(mut pts) = node.kind.rest_points() {
            map_up(nodes, rest, rest, i, &mut pts, &mut BindMode::Compute(&mut binder));
            if binder.exceeded {
                return Err(BindingLimitExceeded);
            }
        }
    }
    Ok(binder.bindings)
}

impl Rig {
    /// Evaluates the rig at `params` into `out` (E1–E7), reusing `out`'s
    /// buffers. Parameters missing from `params` use their defaults.
    pub fn evaluate(&self, params: &ParamValues, out: &mut EvalOutput) {
        let nodes = &self.nodes;
        let rest = &self.rest_states;
        let Scratch { values, current, opacity, visible, accum } = &mut out.scratch;

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

        let mut slot = 0usize;
        for ((node, state), rest_state) in nodes.iter().zip(current.iter_mut()).zip(rest) {
            let point_count = if matches!(node.kind, RigKind::Rotation { .. }) { 0 } else { rest_state.points.len() };
            accum.reset(point_count);
            for set in &node.sets {
                eval_set(set, values, accum);
            }

            // Parents precede children in pre-order.
            let (parent_opacity, parent_visible) = node.parent.map_or((1.0, true), |p| {
                (opacity.get(p).copied().unwrap_or(1.0), visible.get(p).copied().unwrap_or(true))
            });
            let own_opacity = (node.opacity + accum.opacity).clamp(0.0, 1.0);
            opacity.push(own_opacity * parent_opacity);
            visible.push(node.visible && parent_visible);

            state.points.clear();
            state.points.extend(rest_state.points.iter().zip(&accum.positions).map(|(r, d)| geometry::add(*r, *d)));
            state.origin = geometry::add(rest_state.origin, accum.origin);
            state.angle = rest_state.angle + accum.angle;
            state.scale = (rest_state.scale + accum.scale).max(MIN_SCALE);

            if let RigKind::Drawable { draw_order, multiply_color, screen_color, .. } = &node.kind {
                let clamp3 = |mut rest: Rgb, d: Rgb| {
                    for (r, d) in rest.iter_mut().zip(d) {
                        *r = (*r + d).clamp(0.0, 1.0);
                    }
                    rest
                };
                if let Some(frame) = out.drawables.get_mut(slot) {
                    frame.draw_order = draw_order + accum.draw_order;
                    frame.multiply_color = clamp3(*multiply_color, accum.multiply_color);
                    frame.screen_color = clamp3(*screen_color, accum.screen_color);
                }
                slot += 1;
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
            map_up(nodes, current, rest, node, &mut frame.positions, &mut bind);
        }

        // Stable sort over pre-order slots: ties keep tree order (E4).
        out.draw_order.clear();
        out.draw_order.extend(0..out.drawables.len());
        let drawables = &out.drawables;
        out.draw_order.sort_by(|&a, &b| {
            let key = |i: usize| drawables.get(i).map_or(0.0, |d| d.draw_order);
            key(a).total_cmp(&key(b))
        });
    }
}
