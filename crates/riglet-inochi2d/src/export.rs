//! Riglet -> Inochi2D (`.inp`) export. Design: `docs/design/m3-inochi2d.md`
//! sections 5 and 5a.
//!
//! The importer restructures the tree (rotation deformers for transforms,
//! holder groups for parts with children, MeshGroup reach, baked static
//! transforms), so export first groups Riglet nodes back into Inochi2D
//! nodes by id (`inochi:<uuid>` plus a suffix) and places each one where
//! the import recorded it (`parentUUID`, `childIndex`). Native Riglet nodes
//! become new nodes. Raw Inochi2D fields kept in `extensions.inochi2d` are
//! written back; everything Riglet models is written from the Riglet data,
//! so edits survive. Every lossy step emits a `ConversionWarning`.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use riglet_format::buffer;
use riglet_format::model::*;
use riglet_format::{Document, Limits, Rig};
use serde_json::{Map, Value, json};

use crate::InochiError;
use crate::container::{self, Container, Encoding, RawTexture};
use crate::import::{Affine, EXT, floats, tint_screen_from_riglet};

/// The exported file and what was lost on the way.
#[derive(Debug)]
pub struct Export {
    pub bytes: Vec<u8>,
    pub warnings: Vec<ConversionWarning>,
}

/// Inochi2D's "no texture" slot value.
const NO_TEXTURE: u64 = 4_294_967_295;

/// One Inochi2D node, assembled from the Riglet nodes that represent it.
#[derive(Debug, Default, Clone)]
struct Entry {
    uuid: u32,
    /// Topmost Riglet member (pre-order), for name, visibility and order.
    outer: Id,
    transform: Option<Id>,
    drawable: Option<Id>,
    mesh: Option<Id>,
    /// Holder group, plain node group, or composite group.
    group: Option<Id>,
    /// Raw Inochi2D fields recorded on import.
    ext: Option<Map<String, Value>>,
    /// Inochi2D parent and position among its siblings.
    parent: Option<u32>,
    child_index: Option<u64>,
    order: usize,
    visible: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Member {
    Transform,
    Drawable,
    Mesh,
    Group,
}

/// `inochi:<uuid>[:suffix]`: the uuid and what the Riglet node is of it;
/// `None` for importer artifacts (`:bake`, `:rigid` frames) and natives.
fn imported(id: &str, node: &Node) -> Option<Result<(u32, Member), ()>> {
    let rest = id.strip_prefix("inochi:")?;
    let (head, suffix) = rest.split_once(':').unwrap_or((rest, ""));
    let uuid: u32 = head.parse().ok()?;
    if suffix.ends_with("rigid") || suffix == "bake" {
        return Some(Err(()));
    }
    let member = match (suffix, node) {
        ("transform", _) => Member::Transform,
        ("part", _) => Member::Drawable,
        ("mesh", _) => Member::Mesh,
        ("", Node::Drawable(_)) => Member::Drawable,
        ("", Node::MeshDeformer(_)) => Member::Mesh,
        ("", Node::Rotation(_)) => Member::Transform,
        ("", _) => Member::Group,
        _ => return None,
    };
    Some(Ok((uuid, member)))
}

fn ext_of(base: &NodeBase) -> Option<&Map<String, Value>> {
    base.extensions.as_ref()?.get(EXT)?.as_object()
}

/// Riglet blend mode -> Inochi2D name (R4 group (a) maps 1:1).
fn blend_name(mode: BlendMode) -> &'static str {
    match mode {
        BlendMode::Normal => "Normal",
        BlendMode::Multiply => "Multiply",
        // AddGlow is additive with normal alpha; exact over opaque pixels.
        BlendMode::Additive | BlendMode::AddGlow => "AddGlow",
        BlendMode::Screen => "Screen",
        BlendMode::Lighten => "Lighten",
        BlendMode::ColorDodge => "ColorDodge",
        BlendMode::Subtract => "Subtract",
        BlendMode::Exclusion => "Exclusion",
        BlendMode::Inverse => "Inverse",
        BlendMode::DestinationIn => "DestinationIn",
        BlendMode::ClipToLower => "ClipToLower",
        BlendMode::SliceFromLower => "SliceFromLower",
    }
}

// ---------- sampling keyform sets ----------

/// One keyform set's value at given parameter values (E3), per property:
/// deltas for "add" sets, factors for "multiply" sets.
#[derive(Debug, Clone, Default, PartialEq)]
struct Sample {
    positions: Option<Vec<Vec2>>,
    opacity: Option<f32>,
    draw_order: Option<f32>,
    angle: Option<f32>,
    origin: Option<Vec2>,
    scale: Option<f32>,
    scale_xy: Option<Vec2>,
    multiply_color: Option<[f32; 3]>,
    screen_color: Option<[f32; 3]>,
}

/// (key index, weight) taps on one axis, as E3 (and the nearest key for
/// draw order).
fn taps(keys: &[f32], v: f32, interp: AxisInterpolation) -> (Vec<(usize, f32)>, usize) {
    let n = keys.len();
    let first = keys.first().copied().unwrap_or(0.0);
    let last = keys.last().copied().unwrap_or(0.0);
    if n <= 1 || v <= first {
        return (vec![(0, 1.0)], 0);
    }
    if v >= last {
        return (vec![(n - 1, 1.0)], n - 1);
    }
    let i = keys.partition_point(|&k| k <= v).saturating_sub(1).min(n - 2);
    let (k0, k1) = (keys.get(i).copied().unwrap_or(first), keys.get(i + 1).copied().unwrap_or(last));
    let t = (v - k0) / (k1 - k0);
    let nearest = if t > 0.5 { i + 1 } else { i };
    match interp {
        _ if t <= 0.0 => (vec![(i, 1.0)], nearest),
        AxisInterpolation::Step => (vec![(i, 1.0)], i),
        AxisInterpolation::Linear => (vec![(i, 1.0 - t), (i + 1, t)], nearest),
        AxisInterpolation::Cubic => {
            let w = riglet_format::eval::cubic_weights(t);
            let idx = [i.saturating_sub(1), i, i + 1, (i + 2).min(n - 1)];
            (idx.into_iter().zip(w).collect(), nearest)
        }
    }
}

fn add_opt<const N: usize>(acc: &mut Option<[f32; N]>, v: Option<[f32; N]>, missing: f32, w: f32) {
    let v = v.unwrap_or([missing; N]);
    let a = acc.get_or_insert([0.0; N]);
    for (a, v) in a.iter_mut().zip(v) {
        *a += v * w;
    }
}

struct Sampler<'a> {
    files: &'a BTreeMap<String, Vec<u8>>,
    cache: HashMap<String, Vec<Vec2>>,
}

impl Sampler<'_> {
    fn positions(&mut self, r: &BufferRef) -> Option<Vec<Vec2>> {
        let key = format!("{}:{}:{}", r.file, r.byte_offset, r.count);
        if let Some(p) = self.cache.get(&key) {
            return Some(p.clone());
        }
        let p = buffer::read_vec2(self.files, r, u64::MAX).ok()?;
        self.cache.insert(key, p.clone());
        Some(p)
    }

    /// E3 at the values `at(axis)`.
    fn sample(&mut self, set: &KeyformSet, at: &dyn Fn(&ParamAxis) -> f32) -> Sample {
        let multiply = set.combine == Some(Combine::Multiply);
        let missing = if multiply { 1.0 } else { 0.0 };
        let mut per_axis = Vec::new();
        let mut stride = 1usize;
        let mut nearest = 0usize;
        for axis in &set.axes {
            let (t, near) = taps(&axis.keys, at(&axis.param), axis.interpolation);
            nearest += near * stride;
            per_axis.push((stride, t));
            stride = stride.saturating_mul(axis.keys.len());
        }
        let has = |f: &dyn Fn(&Keyform) -> bool| set.keyforms.iter().any(f);
        let mut out = Sample::default();
        let mut digit = vec![0usize; per_axis.len()];
        loop {
            let (mut index, mut w) = (0usize, 1.0f32);
            for ((s, t), &d) in per_axis.iter().zip(&digit) {
                let (k, wk) = t.get(d).copied().unwrap_or((0, 0.0));
                index += k * s;
                w *= wk;
            }
            if let Some(kf) = set.keyforms.get(index)
                && w != 0.0
            {
                if let Some(p) = kf.positions.as_ref().and_then(|r| self.positions(r)) {
                    let acc = out.positions.get_or_insert_with(|| vec![[0.0; 2]; p.len()]);
                    for (a, d) in acc.iter_mut().zip(&p) {
                        a[0] += d[0] * w;
                        a[1] += d[1] * w;
                    }
                }
                let scalar = |acc: &mut Option<f32>, v: Option<f32>, present: bool| {
                    if present {
                        *acc.get_or_insert(0.0) += v.unwrap_or(missing) * w;
                    }
                };
                scalar(&mut out.opacity, kf.opacity, has(&|k| k.opacity.is_some()));
                scalar(&mut out.angle, kf.angle, has(&|k| k.angle.is_some()));
                scalar(&mut out.scale, kf.scale, has(&|k| k.scale.is_some()));
                if has(&|k| k.origin.is_some()) {
                    add_opt(&mut out.origin, kf.origin, missing, w);
                }
                if has(&|k| k.scale_xy.is_some()) {
                    add_opt(&mut out.scale_xy, kf.scale_xy, missing, w);
                }
                if has(&|k| k.multiply_color.is_some()) {
                    add_opt(&mut out.multiply_color, kf.multiply_color, missing, w);
                }
                if has(&|k| k.screen_color.is_some()) {
                    add_opt(&mut out.screen_color, kf.screen_color, missing, w);
                }
            }
            let mut done = true;
            for ((_, t), d) in per_axis.iter().zip(digit.iter_mut()) {
                *d += 1;
                if *d < t.len() {
                    done = false;
                    break;
                }
                *d = 0;
            }
            if done {
                break;
            }
        }
        // E3: draw order is never interpolated (nearest grid point).
        if has(&|k| k.draw_order.is_some()) {
            out.draw_order = Some(set.keyforms.get(nearest).and_then(|k| k.draw_order).unwrap_or(0.0));
        }
        out
    }
}

// ---------- exporter ----------

struct Exporter<'a> {
    doc: &'a Document,
    sampler: Sampler<'a>,
    warnings: Vec<ConversionWarning>,
    entries: BTreeMap<u32, Entry>,
    /// Riglet node id -> the entry it belongs to.
    owner_of: HashMap<Id, u32>,
    used_uuids: HashSet<u32>,
    /// Per Riglet parameter id: uuid, normalized axis points per component.
    params: Vec<(Id, u32, [Vec<f32>; 2])>,
    textures: Vec<RawTexture>,
    texture_slot: HashMap<Id, usize>,
    /// Bake (static transforms folded into vertices on import) per entry.
    bakes: HashMap<u32, Affine>,
    /// Draw-order bindings per entry: (entry, parameter, -delta grid,
    /// interpolation), turned into zSort bindings by `z_sort`.
    z_bindings: Vec<(u32, u32, Value, String)>,
}

fn warn(w: &mut Vec<ConversionWarning>, code: &str, object: Option<&str>, message: impl Into<String>) {
    w.push(ConversionWarning { code: code.into(), object_id: object.map(Into::into), message: message.into() });
}

/// A stable fresh uuid for a native Riglet id (FNV-1a), avoiding `used`.
fn fresh_uuid(id: &str, used: &mut HashSet<u32>) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for b in id.bytes() {
        h = (h ^ u32::from(b)).wrapping_mul(0x0100_0193);
    }
    while h == 0 || h == u32::MAX || used.contains(&h) {
        h = h.wrapping_add(1);
    }
    used.insert(h);
    h
}

impl<'a> Exporter<'a> {
    fn warn(&mut self, code: &str, object: Option<&str>, message: impl Into<String>) {
        warn(&mut self.warnings, code, object, message);
    }

    fn node(&self, id: &str) -> Option<&'a Node> {
        self.doc.model.nodes.get(id)
    }

    /// Groups Riglet nodes into entries, in Riglet pre-order.
    fn collect(&mut self) {
        let model = &self.doc.model;
        for id in model.nodes.keys() {
            if let Some(Ok((u, _))) = model.nodes.get(id).and_then(|n| imported(id, n)) {
                self.used_uuids.insert(u);
            }
        }
        let mut order = 0usize;
        let mut stack: Vec<(Id, Option<u32>)> = vec![(model.root.clone(), None)];
        while let Some((id, parent_entry)) = stack.pop() {
            let Some(node) = model.nodes.get(&id) else { continue };
            let base = node.base();
            let here = if id == model.root {
                None
            } else {
                match imported(&id, node) {
                    Some(Err(())) => parent_entry,
                    Some(Ok((u, member))) => {
                        self.add_member(u, member, &id, base, parent_entry, &mut order);
                        Some(u)
                    }
                    None => {
                        let u = fresh_uuid(&id, &mut self.used_uuids);
                        let member = match node {
                            Node::Drawable(_) => Member::Drawable,
                            Node::MeshDeformer(_) => Member::Mesh,
                            Node::Rotation(_) => Member::Transform,
                            Node::Warp(_) => {
                                warn(
                                    &mut self.warnings,
                                    "WARP_UNSUPPORTED",
                                    Some(&id),
                                    "warp deformers aren't exported yet; written as a plain node, its children undeformed",
                                );
                                Member::Group
                            }
                            Node::Group(_) => Member::Group,
                        };
                        self.add_member(u, member, &id, base, parent_entry, &mut order);
                        Some(u)
                    }
                }
            };
            for c in base.children.iter().rev() {
                stack.push((c.clone(), here));
            }
        }
    }

    fn add_member(
        &mut self,
        u: u32,
        member: Member,
        id: &str,
        base: &NodeBase,
        parent: Option<u32>,
        order: &mut usize,
    ) {
        self.owner_of.insert(id.into(), u);
        let e = self.entries.entry(u).or_insert_with(|| {
            *order += 1;
            Entry { uuid: u, outer: id.into(), order: *order, visible: true, parent, ..Default::default() }
        });
        e.visible &= base.visible;
        let slot = match member {
            Member::Transform => &mut e.transform,
            Member::Drawable => &mut e.drawable,
            Member::Mesh => &mut e.mesh,
            Member::Group => &mut e.group,
        };
        slot.get_or_insert_with(|| id.into());
        if let Some(x) = ext_of(base)
            && x.contains_key("uuid")
            && e.ext.is_none()
        {
            e.ext = Some(x.clone());
            if let Some(p) = x.get("parentUUID").and_then(Value::as_u64) {
                e.parent = u32::try_from(p).ok();
            }
            e.child_index = x.get("childIndex").and_then(Value::as_u64);
        }
    }

    fn textures(&mut self) {
        // Imported slots first, at their original index (raw bytes of
        // undecodable ones restored); then every other texture.
        let files = &self.doc.files;
        let mut slots: BTreeMap<usize, RawTexture> = BTreeMap::new();
        for (name, data) in files.range("extensions/inochi2d/textures/".to_owned()..) {
            let Some(rest) = name.strip_prefix("extensions/inochi2d/textures/") else { break };
            if let Some((i, enc)) = rest.split_once(".raw")
                && let (Ok(i), Ok(enc)) = (i.parse::<usize>(), enc.parse::<u8>())
            {
                slots.insert(i, RawTexture { encoding: Encoding::from_byte(enc), data: data.clone() });
            }
        }
        for t in &self.doc.model.textures {
            if let Some(i) = t.id.strip_prefix("tex").and_then(|i| i.parse::<usize>().ok())
                && let Some(png) = files.get(&t.file)
            {
                slots.insert(i, RawTexture { encoding: Encoding::Png, data: png.clone() });
                self.texture_slot.insert(t.id.clone(), i);
            }
        }
        let mut next = slots.keys().next_back().map_or(0, |i| i + 1);
        for t in &self.doc.model.textures {
            if self.texture_slot.contains_key(&t.id) || t.id.starts_with("inochi:placeholder") {
                continue;
            }
            if let Some(png) = files.get(&t.file) {
                slots.insert(next, RawTexture { encoding: Encoding::Png, data: png.clone() });
                self.texture_slot.insert(t.id.clone(), next);
                next += 1;
            }
        }
        let count = slots.keys().next_back().map_or(0, |i| i + 1);
        self.textures = (0..count)
            .map(|i| slots.remove(&i).unwrap_or(RawTexture { encoding: Encoding::Png, data: Vec::new() }))
            .collect();
    }

    /// The importer writes Inochi2D's Nearest as a step axis with a key at
    /// every midpoint whose value is the next key's. Returns the original
    /// keys when axis `ai` of `set` has that shape.
    fn nearest_keys(&mut self, set: &KeyformSet, ai: usize) -> Option<Vec<f32>> {
        let axis = set.axes.get(ai)?;
        let k = &axis.keys;
        if axis.interpolation != AxisInterpolation::Step || k.len() < 3 || k.len() % 2 == 0 {
            return None;
        }
        for i in (1..k.len()).step_by(2) {
            let (a, m, b) = (*k.get(i - 1)?, *k.get(i)?, *k.get(i + 1)?);
            if (m - (a + b) / 2.0).abs() > 1e-5 * (b - a).abs().max(1.0) {
                return None;
            }
            // Same value at the midpoint and the next key, for every value
            // of the other axes (their keys).
            let others: Vec<(usize, f32)> = set
                .axes
                .iter()
                .enumerate()
                .filter(|(j, _)| *j != ai)
                .flat_map(|(j, a)| a.keys.iter().map(move |&x| (j, x)))
                .collect();
            let at = |v: f32, other: Option<(usize, f32)>| {
                move |p: &ParamAxis| {
                    let j = set.axes.iter().position(|a| &a.param == p).unwrap_or(usize::MAX);
                    if j == ai {
                        v
                    } else if let Some((oj, ov)) = other.filter(|(oj, _)| *oj == j) {
                        let _ = oj;
                        ov
                    } else {
                        set.axes.get(j).and_then(|a| a.keys.first()).copied().unwrap_or(0.0)
                    }
                }
            };
            let checks: Vec<Option<(usize, f32)>> =
                if others.is_empty() { vec![None] } else { others.into_iter().map(Some).collect() };
            for o in checks {
                if self.sampler.sample(set, &at(m, o)) != self.sampler.sample(set, &at(b, o)) {
                    return None;
                }
            }
        }
        Some(k.iter().step_by(2).copied().collect())
    }

    fn params(&mut self) {
        // Axis points per component: the union of every set's keys on it
        // (normalized; Nearest-shaped step axes contribute their original
        // keys only), plus the range ends.
        let mut keys: HashMap<(Id, u8), BTreeSet<u32>> = HashMap::new();
        let sets: Vec<KeyformSet> =
            self.doc.model.nodes.values().flat_map(|n| n.base().keyform_sets.iter().cloned()).collect();
        for set in &sets {
            for (ai, a) in set.axes.iter().enumerate() {
                let own = self.nearest_keys(set, ai).unwrap_or_else(|| a.keys.clone());
                let e = keys.entry((a.param.parameter.clone(), a.param.component)).or_default();
                for k in own {
                    e.insert(k.to_bits());
                }
            }
        }
        for p in &self.doc.model.parameters {
            let uuid = match p.id.strip_prefix("inochi:").and_then(|u| u.parse::<u32>().ok()) {
                Some(u) => u,
                None => fresh_uuid(&p.id, &mut self.used_uuids),
            };
            let comps = if p.kind == ParamKind::TwoD { 2 } else { 1 };
            let mut axis = [vec![0.0], vec![0.0]];
            for (c, ax) in axis.iter_mut().enumerate().take(comps) {
                let (lo, hi) = (p.min.get(c).copied().unwrap_or(0.0), p.max.get(c).copied().unwrap_or(1.0));
                let mut pts: Vec<f32> = keys
                    .get(&(p.id.clone(), c as u8))
                    .into_iter()
                    .flatten()
                    .map(|&b| ((f32::from_bits(b) - lo) / (hi - lo)).clamp(0.0, 1.0))
                    .chain([0.0, 1.0])
                    .collect();
                pts.sort_by(f32::total_cmp);
                pts.dedup_by(|a, b| (*a - *b).abs() < 1e-6);
                *ax = pts;
            }
            self.params.push((p.id.clone(), uuid, axis));
        }
    }

    /// The Inochi2D transform of an entry: the recorded one when the Riglet
    /// rotation still matches it, else derived from the rotation.
    fn transform(&self, e: &Entry) -> Value {
        let raw = e.ext.as_ref().and_then(|x| x.get("transform")).cloned();
        let Some(Node::Rotation(r)) = e.transform.as_deref().and_then(|t| self.node(t)) else {
            return raw.unwrap_or_else(|| json!({"trans": [0, 0, 0], "rot": [0, 0, 0], "scale": [1, 1]}));
        };
        let (t, rot, s) = raw_trs(raw.as_ref());
        let sxy = r.rest.scale.xy();
        let folded = [s[0] * rot[1].cos(), s[1] * rot[0].cos()];
        let same = (r.rest.origin[0] - t[0]).abs() < 1e-4
            && (r.rest.origin[1] - t[1]).abs() < 1e-4
            && (r.rest.angle - rot[2].to_degrees()).abs() < 1e-4
            && (sxy[0] - folded[0]).abs() < 1e-5
            && (sxy[1] - folded[1]).abs() < 1e-5;
        if same && let Some(raw) = raw {
            return raw;
        }
        // Edited (or native): x/y rotations can't be kept with a changed
        // scale, so they fold into the scale (S6).
        json!({
            "trans": [r.rest.origin[0], r.rest.origin[1], t[2]],
            "rot": [0.0, 0.0, r.rest.angle.to_radians()],
            "scale": [sxy[0], sxy[1]],
        })
    }
}

/// (trans, rot, scale) of a raw Inochi2D transform.
fn raw_trs(raw: Option<&Value>) -> ([f32; 3], [f32; 3], Vec2) {
    let get = |k: &str, d: f32| {
        let f = floats(raw.and_then(|t| t.get(k)));
        [0, 1, 2].map(|i| f.get(i).copied().unwrap_or(d))
    };
    let s = get("scale", 1.0);
    (get("trans", 0.0), get("rot", 0.0), [s[0], s[1]])
}

/// The importer's bake for a node's own transform (S6 fold included).
fn trs_affine(transform: &Value) -> Affine {
    let (t, r, s) = raw_trs(Some(transform));
    Affine::trs([t[0], t[1]], r[2], [s[0] * r[1].cos(), s[1] * r[0].cos()])
}

/// Exports a document as an Inochi2D puppet (`.inp`).
pub fn export(doc: &Document) -> Result<Export, InochiError> {
    let rig = Rig::prepare(doc, &Limits::default()).map_err(|issues| {
        InochiError::InvalidModel(issues.iter().map(ToString::to_string).collect::<Vec<_>>().join("; "))
    })?;
    let mut ex = Exporter {
        doc,
        sampler: Sampler { files: &doc.files, cache: HashMap::new() },
        warnings: Vec::new(),
        entries: BTreeMap::new(),
        owner_of: HashMap::new(),
        used_uuids: HashSet::new(),
        params: Vec::new(),
        textures: Vec::new(),
        texture_slot: HashMap::new(),
        bakes: HashMap::new(),
        z_bindings: Vec::new(),
    };
    ex.collect();
    ex.textures();
    ex.params();
    let mut nodes = ex.nodes(&rig)?;
    let params = ex.bindings(&mut nodes)?;
    let root = ex.tree(nodes);

    let model_ext = doc.model.extensions.as_ref().and_then(|e| e.get(EXT)).and_then(Value::as_object);
    let pick = |k: &str, d: Value| model_ext.and_then(|m| m.get(k)).cloned().unwrap_or(d);
    if !doc.model.motions.is_empty() && model_ext.is_none_or(|m| !m.contains_key("animations")) {
        ex.warn("MOTIONS_DROPPED", None, "motions aren't exported yet");
    }
    if !doc.model.physics.is_empty() {
        ex.warn("PHYSICS_DROPPED", None, "physics rigs aren't exported yet");
    }
    let puppet = json!({
        "meta": pick("meta", json!({"name": "", "version": "v0.8.7"})),
        "physics": pick("physics", json!({"pixelsPerMeter": 1000, "gravity": 9.8})),
        "nodes": root,
        "param": params,
        "automation": pick("automation", json!([])),
        "animations": pick("animations", json!({})),
    });
    let mut extensions = Vec::new();
    for (name, data) in doc.files.range("extensions/inochi2d/ext/".to_owned()..) {
        let Some(n) = name.strip_prefix("extensions/inochi2d/ext/") else { break };
        extensions.push((n.to_owned(), data.clone()));
    }
    let json = serde_json::to_vec(&puppet).map_err(|e| InochiError::Json(e.to_string()))?;
    let bytes = container::write(&Container { json, textures: std::mem::take(&mut ex.textures), extensions })?;
    Ok(Export { bytes, warnings: ex.warnings })
}

impl Exporter<'_> {
    /// Builds every entry's JSON (without children), and the bakes.
    fn nodes(&mut self, rig: &Rig) -> Result<BTreeMap<u32, Map<String, Value>>, InochiError> {
        let _ = rig;
        // Bakes top-down in Inochi2D order: a node without a rotation
        // deformer had its transform folded into its own and its
        // children's vertices (only under MeshGroups; elsewhere identity).
        let order = self.inochi_order();
        let mut out = BTreeMap::new();
        for u in order {
            let Some(e) = self.entries.get(&u).cloned() else { continue };
            let parent_bake = e.parent.and_then(|p| self.bakes.get(&p)).copied().unwrap_or(Affine::IDENTITY);
            let transform = self.transform(&e);
            let bake = if e.transform.is_some() { Affine::IDENTITY } else { parent_bake.then(trs_affine(&transform)) };
            self.bakes.insert(u, bake);
            let mut n = e.ext.clone().unwrap_or_default();
            for k in ["parentUUID", "childIndex"] {
                n.remove(k);
            }
            let outer = self.node(&e.outer);
            n.insert("uuid".into(), u.into());
            n.insert("name".into(), outer.map_or("", |o| o.base().name.as_str()).into());
            n.insert("enabled".into(), e.visible.into());
            n.insert("transform".into(), transform);
            n.entry("lockToRoot").or_insert(false.into());
            let ty = if let Some(t) = n.get("type").and_then(Value::as_str) {
                t.to_owned()
            } else if e.mesh.is_some() {
                "MeshGroup".into()
            } else if e.drawable.is_some() {
                "Part".into()
            } else if matches!(
                e.group.as_deref().and_then(|g| self.node(g)),
                Some(Node::Group(GroupNode { composite: true, .. }))
            ) {
                "Composite".into()
            } else {
                "Node".into()
            };
            n.insert("type".into(), ty.into());
            let inverse = bake.inverse().unwrap_or(Affine::IDENTITY);
            if let Some(Node::Drawable(d)) = e.drawable.as_deref().and_then(|i| self.node(i)) {
                self.part(&mut n, d, inverse);
            }
            if let Some(Node::MeshDeformer(m)) = e.mesh.as_deref().and_then(|i| self.node(i)) {
                let pos = self.vec2s(&m.mesh.positions)?;
                let idx = buffer::read_indices(&self.doc.files, &m.mesh.indices, u64::MAX).unwrap_or_default();
                let verts: Vec<f32> = pos.iter().flat_map(|p| inverse.point(*p)).collect();
                let uvs = vec![0.0f32; verts.len()];
                n.insert("mesh".into(), json!({"verts": verts, "uvs": uvs, "indices": idx, "origin": [0, 0]}));
                if let Some(d) = m.deltas {
                    n.insert("dynamic_deformation".into(), (d == MeshDeltas::Mapped).into());
                }
            }
            if let Some(Node::Group(g)) = e.group.as_deref().and_then(|i| self.node(i))
                && g.composite
            {
                n.insert("blend_mode".into(), blend_name(g.blend_mode.unwrap_or(BlendMode::Normal)).into());
                n.insert("opacity".into(), g.rest.opacity.into());
                n.insert("masks".into(), self.masks(g.masks.as_deref().unwrap_or(&[])));
                n.entry("tint").or_insert(json!([1, 1, 1]));
                n.entry("screenTint").or_insert(json!([0, 0, 0]));
            }
            out.insert(u, n);
        }
        Ok(out)
    }

    fn vec2s(&self, r: &BufferRef) -> Result<Vec<Vec2>, InochiError> {
        buffer::read_vec2(&self.doc.files, r, u64::MAX).map_err(|e| InochiError::InvalidModel(e.to_string()))
    }

    fn masks(&self, masks: &[MaskRef]) -> Value {
        Value::Array(
            masks
                .iter()
                .filter_map(|m| {
                    let source = self.owner_of.get(&m.source)?;
                    let mode = match m.mode {
                        MaskMode::Clip => "Mask",
                        MaskMode::Invert => "DodgeMask",
                    };
                    Some(json!({"source": source, "mode": mode}))
                })
                .collect(),
        )
    }

    fn part(&mut self, n: &mut Map<String, Value>, d: &DrawableNode, inverse: Affine) {
        let pos = buffer::read_vec2(&self.doc.files, &d.mesh.positions, u64::MAX).unwrap_or_default();
        let uvs = buffer::read_vec2(&self.doc.files, &d.mesh.uvs, u64::MAX).unwrap_or_default();
        let idx = buffer::read_indices(&self.doc.files, &d.mesh.indices, u64::MAX).unwrap_or_default();
        let verts: Vec<f32> = pos.iter().flat_map(|p| inverse.point(*p)).collect();
        let uv: Vec<f32> = uvs.iter().flatten().copied().collect();
        n.insert("mesh".into(), json!({"verts": verts, "uvs": uv, "indices": idx, "origin": [0, 0]}));
        let mut slots: Vec<Value> = n.get("textures").and_then(Value::as_array).cloned().unwrap_or_default();
        slots.resize(3, NO_TEXTURE.into());
        let albedo = self.texture_slot.get(&d.texture).map_or(NO_TEXTURE, |&i| i as u64);
        // An undecodable imported texture keeps its original slot.
        let keep = slots.first().and_then(Value::as_u64).filter(|_| d.texture.starts_with("inochi:placeholder"));
        if let Some(first) = slots.first_mut() {
            *first = keep.unwrap_or(albedo).into();
        }
        n.insert("textures".into(), Value::Array(slots));
        n.insert("blend_mode".into(), blend_name(d.blend_mode).into());
        if d.blend_mode == BlendMode::Additive {
            self.warn(
                "BLEND_MODE_APPROXIMATED",
                Some(&d.base.id),
                "additive exported as AddGlow: its alpha is As + Ad(1 - As), not Ad (same over opaque pixels)",
            );
        }
        n.insert("opacity".into(), d.rest.opacity.into());
        let (m, s) = tint_screen_from_riglet(d.rest.multiply_color, d.rest.screen_color);
        n.insert("tint".into(), json!(m));
        n.insert("screenTint".into(), json!(s));
        n.insert("masks".into(), self.masks(&d.masks));
        if let Some(t) = d.mask_threshold {
            n.insert("mask_threshold".into(), t.into());
        } else {
            n.entry("mask_threshold").or_insert(0.5.into());
        }
    }

    /// Entries in Inochi2D tree order (parents first).
    fn inochi_order(&self) -> Vec<u32> {
        let children = self.children();
        let mut out = Vec::new();
        let mut stack: Vec<u32> = children.get(&None).cloned().unwrap_or_default().into_iter().rev().collect();
        while let Some(u) = stack.pop() {
            out.push(u);
            if let Some(c) = children.get(&Some(u)) {
                stack.extend(c.iter().rev());
            }
        }
        out
    }

    /// Children per Inochi2D parent, sorted by recorded index then order.
    fn children(&self) -> HashMap<Option<u32>, Vec<u32>> {
        let mut m: HashMap<Option<u32>, Vec<u32>> = HashMap::new();
        for e in self.entries.values() {
            let parent = e.parent.filter(|p| self.entries.contains_key(p));
            m.entry(parent).or_default().push(e.uuid);
        }
        for list in m.values_mut() {
            list.sort_by_key(|u| self.entries.get(u).map(|e| (e.child_index.unwrap_or(u64::MAX), e.order)));
        }
        m
    }

    /// Nests the node JSON; a missing single root gets a synthetic Node.
    fn tree(&mut self, mut nodes: BTreeMap<u32, Map<String, Value>>) -> Value {
        let children = self.children();
        fn build(
            u: u32,
            nodes: &mut BTreeMap<u32, Map<String, Value>>,
            children: &HashMap<Option<u32>, Vec<u32>>,
        ) -> Value {
            let mut n = nodes.remove(&u).unwrap_or_default();
            let kids: Vec<Value> =
                children.get(&Some(u)).into_iter().flatten().map(|&c| build(c, nodes, children)).collect();
            n.insert("children".into(), Value::Array(kids));
            Value::Object(n)
        }
        let tops = children.get(&None).cloned().unwrap_or_default();
        match tops.as_slice() {
            [single] if self.entries.get(single).is_some_and(|e| e.drawable.is_none()) => {
                build(*single, &mut nodes, &children)
            }
            _ => {
                let kids: Vec<Value> = tops.iter().map(|&u| build(u, &mut nodes, &children)).collect();
                let u = fresh_uuid("riglet:root", &mut self.used_uuids);
                json!({
                    "uuid": u, "name": "Root", "type": "Node", "enabled": true, "zsort": 0.0,
                    "transform": {"trans": [0, 0, 0], "rot": [0, 0, 0], "scale": [1, 1]},
                    "lockToRoot": false, "children": kids
                })
            }
        }
    }
}

// ---------- draw order ----------

impl Exporter<'_> {
    /// Inochi2D accumulates zSort down the tree (a node's sort is its
    /// parent's plus its own, and a zSort binding moves the whole subtree).
    /// Rest: each drawable and composite gets the zsort that puts it at
    /// -drawOrder; plain nodes keep their recorded zsort. Bindings: per
    /// distinct draw-order set, emitted where a drawable's wanted share
    /// differs from what it inherits (exact; the importer's per-member
    /// copies of one binding fold back into one).
    fn z_sort(&mut self, nodes: &mut BTreeMap<u32, Map<String, Value>>, bindings: &mut HashMap<u32, Vec<Value>>) {
        let z_bindings = std::mem::take(&mut self.z_bindings);
        let children = self.children();
        let order = self.inochi_order();
        let target = |e: &Entry| -> Option<f32> {
            match e.drawable.as_deref().or(e.group.as_deref()).and_then(|i| self.node(i)) {
                Some(Node::Drawable(d)) => Some(-d.rest.draw_order),
                Some(Node::Group(g)) if g.composite => Some(-g.rest.draw_order.unwrap_or(0.0)),
                _ => None,
            }
        };
        let mut acc: HashMap<u32, f32> = HashMap::new();
        for &u in &order {
            let Some(e) = self.entries.get(&u) else { continue };
            let parent = e.parent.and_then(|p| acc.get(&p)).copied().unwrap_or(0.0);
            let own = match target(e) {
                Some(t) => t - parent,
                None => e.ext.as_ref().and_then(|x| x.get("zsort")).and_then(Value::as_f64).map_or(0.0, |z| z as f32),
            };
            acc.insert(u, parent + own);
            if let Some(n) = nodes.get_mut(&u) {
                n.insert("zsort".into(), own.into());
            }
        }
        // Bindings: key = (parameter, values, interpolation).
        let mut keys: Vec<(u32, String, String)> = Vec::new();
        let mut wants: HashMap<(u32, String, String), HashSet<u32>> = HashMap::new();
        for (u, p, values, interp) in z_bindings {
            let k = (p, values.to_string(), interp);
            if !keys.contains(&k) {
                keys.push(k.clone());
            }
            wants.entry(k).or_default().insert(u);
        }
        for k in keys {
            let want = wants.get(&k).cloned().unwrap_or_default();
            let Ok(values) = serde_json::from_str::<Value>(&k.1) else { continue };
            let negated = negate(&values);
            // (node, inherited multiplicity), top-down.
            let mut stack: Vec<(u32, i32)> = children.get(&None).into_iter().flatten().map(|&u| (u, 0)).collect();
            while let Some((u, inherited)) = stack.pop() {
                let Some(e) = self.entries.get(&u) else { continue };
                let mut here = inherited;
                if target(e).is_some() {
                    let wanted = i32::from(want.contains(&u));
                    let mut diff = wanted - inherited;
                    while diff != 0 {
                        let v = if diff > 0 { &values } else { &negated };
                        bindings.entry(k.0).or_default().push(json!({
                            "node": u, "param_name": "zSort", "values": v, "interpolate_mode": k.2
                        }));
                        diff -= diff.signum();
                    }
                    here = wanted;
                }
                // A zSort binding moves a composite as one unit; its
                // contents keep their order inside it.
                let composite = matches!(
                    e.group.as_deref().and_then(|g| self.node(g)),
                    Some(Node::Group(GroupNode { composite: true, .. }))
                );
                for &c in children.get(&Some(u)).into_iter().flatten() {
                    stack.push((c, if composite { 0 } else { here }));
                }
            }
        }
    }
}

fn negate(v: &Value) -> Value {
    match v {
        Value::Array(a) => Value::Array(a.iter().map(negate).collect()),
        Value::Number(n) => n.as_f64().map_or(Value::Null, |x| (-x).into()),
        other => other.clone(),
    }
}

// ---------- bindings ----------

/// What a set animates, per Inochi2D binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Target {
    Drawable,
    Transform,
    Mesh,
    Composite,
}

impl Exporter<'_> {
    /// Parameters with their bindings, and the zSort of every node.
    fn bindings(&mut self, nodes: &mut BTreeMap<u32, Map<String, Value>>) -> Result<Vec<Value>, InochiError> {
        let mut bindings: HashMap<u32, Vec<Value>> = HashMap::new();
        let entries: Vec<Entry> = self.entries.values().cloned().collect();
        let param_of: HashMap<Id, usize> = self.params.iter().enumerate().map(|(i, p)| (p.0.clone(), i)).collect();
        let defaults: HashMap<Id, Vec<f32>> =
            self.doc.model.parameters.iter().map(|p| (p.id.clone(), p.default.clone())).collect();
        for e in &entries {
            let members = [
                (e.drawable.clone(), Target::Drawable),
                (e.transform.clone(), Target::Transform),
                (e.mesh.clone(), Target::Mesh),
                (e.group.clone(), Target::Composite),
            ];
            for (id, target) in members {
                let Some(id) = id else { continue };
                let Some(node) = self.node(&id) else { continue };
                for set in &node.base().keyform_sets {
                    self.set_bindings(e, node, target, set, &param_of, &defaults, &mut bindings);
                }
            }
        }
        self.z_sort(nodes, &mut bindings);
        let model = &self.doc.model;
        let mut out = Vec::new();
        for (p, (id, uuid, axis)) in model.parameters.iter().zip(&self.params) {
            let mut v =
                p.extensions.as_ref().and_then(|e| e.get(EXT)).and_then(Value::as_object).cloned().unwrap_or_default();
            let vec2 = p.kind == ParamKind::TwoD;
            let two = |x: &[f32], d: f32| [x.first().copied().unwrap_or(d), x.get(1).copied().unwrap_or(0.0)];
            v.insert("uuid".into(), (*uuid).into());
            v.insert("name".into(), p.name.clone().into());
            v.insert("is_vec2".into(), vec2.into());
            v.insert("min".into(), json!(two(&p.min, 0.0)));
            v.insert("max".into(), json!(two(&p.max, 1.0)));
            v.insert("defaults".into(), json!(two(&p.default, 0.0)));
            v.insert("axis_points".into(), json!(axis));
            v.entry("merge_mode").or_insert("Additive".into());
            let idx = param_of.get(id).copied().unwrap_or(0);
            v.insert("bindings".into(), Value::Array(bindings.remove(&(idx as u32)).unwrap_or_default()));
            out.push(Value::Object(v));
        }
        Ok(out)
    }

    #[allow(clippy::too_many_arguments)]
    fn set_bindings(
        &mut self,
        e: &Entry,
        node: &Node,
        target: Target,
        set: &KeyformSet,
        param_of: &HashMap<Id, usize>,
        defaults: &HashMap<Id, Vec<f32>>,
        bindings: &mut HashMap<u32, Vec<Value>>,
    ) {
        let multiply = set.combine == Some(Combine::Multiply);
        let ps: BTreeSet<usize> = set.axes.iter().filter_map(|a| param_of.get(&a.param.parameter).copied()).collect();
        let default_at = |a: &ParamAxis| {
            defaults.get(&a.parameter).and_then(|d| d.get(usize::from(a.component))).copied().unwrap_or(0.0)
        };
        let base = self.sampler.sample(set, &default_at);
        if ps.len() > 1 {
            self.warn(
                "KEYFORM_INTERACTION_DROPPED",
                Some(&node.base().id),
                "a keyform set over several parameters is split into one binding per parameter through the \
                 defaults (exact at the default pose and along each parameter alone)",
            );
        }
        for (k, &pi) in ps.iter().enumerate() {
            let Some((pid, _, axis)) = self.params.get(pi).cloned() else { continue };
            let Some(param) = self.doc.model.parameters.get(pi) else { continue };
            let vec2 = param.kind == ParamKind::TwoD;
            let (lo, hi) = (param.min.clone(), param.max.clone());
            let key = |c: usize, t: f32| {
                let (l, h) = (lo.get(c).copied().unwrap_or(0.0), hi.get(c).copied().unwrap_or(1.0));
                l + t * (h - l)
            };
            let ys: Vec<f32> = if vec2 { axis[1].clone() } else { vec![0.0] };
            let mut grid: Vec<Vec<Sample>> = Vec::new();
            for &x in &axis[0] {
                let mut col = Vec::new();
                for &y in &ys {
                    let at = |a: &ParamAxis| {
                        if a.parameter == pid {
                            if a.component == 0 { key(0, x) } else { key(1, y) }
                        } else {
                            default_at(a)
                        }
                    };
                    let mut s = self.sampler.sample(set, &at);
                    if k > 0 {
                        s = remove_base(&s, &base, multiply);
                    }
                    col.push(s);
                }
                grid.push(col);
            }
            let interp = self.interp(set, &pid, &axis, vec2, &node.base().id);
            self.emit(e, node, target, multiply, &grid, interp, pi as u32, bindings);
        }
    }

    /// Inochi2D interpolation for the axes of `pid`: exact for linear; cubic
    /// only when its keys are the parameter's axis points; step as Nearest.
    fn interp(&mut self, set: &KeyformSet, pid: &str, axis: &[Vec<f32>; 2], vec2: bool, object: &str) -> &'static str {
        let idx: Vec<usize> =
            (0..set.axes.len()).filter(|&i| set.axes.get(i).is_some_and(|a| a.param.parameter == pid)).collect();
        let axes: Vec<KeyAxis> = idx.iter().filter_map(|&i| set.axes.get(i).cloned()).collect();
        let param = self.doc.model.parameters.iter().find(|p| p.id == pid).cloned();
        let mut own_keys = Vec::new();
        for &i in &idx {
            let a = set.axes.get(i).cloned();
            let k = self.nearest_keys(set, i).or_else(|| a.as_ref().map(|a| a.keys.clone())).unwrap_or_default();
            own_keys.push((a.map_or(0, |a| a.param.component), k));
        }
        let same_keys = own_keys.iter().all(|(c, keys)| {
            let c = usize::from(*c);
            let (lo, hi) = param
                .as_ref()
                .map_or((0.0, 1.0), |p| (p.min.get(c).copied().unwrap_or(0.0), p.max.get(c).copied().unwrap_or(1.0)));
            let norm: Vec<f32> = keys.iter().map(|k| (k - lo) / (hi - lo)).collect();
            axis.get(c)
                .is_some_and(|ax| ax.len() == norm.len() && ax.iter().zip(&norm).all(|(x, y)| (x - y).abs() < 1e-5))
        }) && axes.len() == if vec2 { 2 } else { 1 };
        let nearest_shaped = idx.iter().all(|&i| self.nearest_keys(set, i).is_some());
        let all = |m: AxisInterpolation| axes.iter().all(|a| a.interpolation == m);
        if all(AxisInterpolation::Linear) {
            "Linear"
        } else if all(AxisInterpolation::Cubic) && same_keys {
            "Cubic"
        } else if all(AxisInterpolation::Step) {
            // Exact when the axis is the importer's Nearest shape over the
            // parameter's axis points.
            if !(nearest_shaped && same_keys) {
                self.warn("STEP_AS_NEAREST", Some(object), "step axis exported as Nearest (switches at midpoints)");
            }
            "Nearest"
        } else {
            self.warn("CUBIC_RESAMPLED", Some(object), "cubic or mixed axes resampled as linear at the axis points");
            "Linear"
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn emit(
        &mut self,
        e: &Entry,
        node: &Node,
        target: Target,
        multiply: bool,
        grid: &[Vec<Sample>],
        interp: &str,
        param: u32,
        bindings: &mut HashMap<u32, Vec<Value>>,
    ) {
        let first = grid.first().and_then(|c| c.first()).cloned().unwrap_or_default();
        let mut push = |name: &str, values: Value| {
            bindings.entry(param).or_default().push(json!({
                "node": e.uuid, "param_name": name, "values": values, "interpolate_mode": interp
            }));
        };
        let scalars = |f: &dyn Fn(&Sample) -> f32| -> Value {
            Value::Array(grid.iter().map(|col| Value::Array(col.iter().map(|s| f(s).into()).collect())).collect())
        };
        let id = node.base().id.clone();
        if first.positions.is_some() {
            let inverse = self.bakes.get(&e.uuid).and_then(|b| b.inverse()).unwrap_or(Affine::IDENTITY);
            let values: Value = Value::Array(
                grid.iter()
                    .map(|col| {
                        Value::Array(
                            col.iter()
                                .map(|s| {
                                    let d: Vec<Value> = s
                                        .positions
                                        .iter()
                                        .flatten()
                                        .map(|p| {
                                            let v = inverse.vector(*p);
                                            json!([v[0], v[1]])
                                        })
                                        .collect();
                                    Value::Array(d)
                                })
                                .collect(),
                        )
                    })
                    .collect(),
            );
            if matches!(target, Target::Drawable | Target::Mesh) {
                push("deform", values);
            }
        }
        match target {
            Target::Transform => {
                if first.origin.is_some() && !multiply {
                    push("transform.t.x", scalars(&|s| s.origin.map_or(0.0, |o| o[0])));
                    push("transform.t.y", scalars(&|s| s.origin.map_or(0.0, |o| o[1])));
                }
                if first.angle.is_some() && !multiply {
                    push("transform.r.z", scalars(&|s| s.angle.unwrap_or(0.0).to_radians()));
                }
                if first.scale.is_some() || first.scale_xy.is_some() {
                    // Inochi2D scale bindings multiply: an additive delta d
                    // on rest scale r becomes the factor (r + d) / r.
                    let rest = match node {
                        Node::Rotation(r) => r.rest.scale.xy(),
                        _ => [1.0; 2],
                    };
                    let factor = |s: &Sample, k: usize| {
                        let u = s.scale.unwrap_or(if multiply { 1.0 } else { 0.0 });
                        let xy =
                            s.scale_xy.map_or(if multiply { 1.0 } else { 0.0 }, |v| v.get(k).copied().unwrap_or(0.0));
                        if multiply {
                            u * xy
                        } else {
                            let r = rest.get(k).copied().unwrap_or(1.0);
                            if r == 0.0 { 1.0 } else { (r + u + xy) / r }
                        }
                    };
                    push("transform.s.x", scalars(&|s| factor(s, 0)));
                    push("transform.s.y", scalars(&|s| factor(s, 1)));
                }
            }
            Target::Drawable | Target::Composite => {
                if first.opacity.is_some() {
                    let rest = match node {
                        Node::Drawable(d) => d.rest.opacity,
                        Node::Group(g) => g.rest.opacity,
                        _ => 1.0,
                    };
                    if !multiply && rest == 0.0 {
                        self.warn("MULTIPLICATIVE_BINDINGS", Some(&id), "opacity delta on rest opacity 0 dropped");
                    } else {
                        push(
                            "opacity",
                            scalars(&|s| {
                                let o = s.opacity.unwrap_or(if multiply { 1.0 } else { 0.0 });
                                if multiply { o } else { (rest + o) / rest }
                            }),
                        );
                    }
                }
                if (first.multiply_color.is_some() || first.screen_color.is_some())
                    && let Node::Drawable(d) = node
                {
                    self.colors(d, multiply, grid, &mut push);
                }
            }
            Target::Mesh => {}
        }
        if first.draw_order.is_some() {
            // Collected into zSort bindings by `z_sort`.
            let values = scalars(&|s| -s.draw_order.unwrap_or(0.0));
            self.z_bindings.push((e.uuid, param, values, interp.to_owned()));
        }
    }

    /// Tint and screen bindings from a color set, through the inverse
    /// color-order conversion at every grid point (exact at the keys).
    fn colors(&mut self, d: &DrawableNode, multiply: bool, grid: &[Vec<Sample>], push: &mut dyn FnMut(&str, Value)) {
        let (rm, rs) = (d.rest.multiply_color, d.rest.screen_color);
        let (m0, s0) = tint_screen_from_riglet(rm, rs);
        let at = |s: &Sample| {
            let mut m = rm;
            let mut sc = rs;
            for (k, (mk, sk)) in m.iter_mut().zip(sc.iter_mut()).enumerate() {
                let dm =
                    s.multiply_color.map_or(if multiply { 1.0 } else { 0.0 }, |c| c.get(k).copied().unwrap_or(0.0));
                let ds = s.screen_color.map_or(0.0, |c| c.get(k).copied().unwrap_or(0.0));
                if multiply {
                    *mk = (*mk * dm).clamp(0.0, 1.0);
                } else {
                    *mk = (*mk + dm).clamp(0.0, 1.0);
                    *sk = (*sk + ds).clamp(0.0, 1.0);
                }
            }
            tint_screen_from_riglet(m, sc)
        };
        for (k, ch) in ["r", "g", "b"].iter().enumerate() {
            let m0k = m0.get(k).copied().unwrap_or(1.0);
            let s0k = s0.get(k).copied().unwrap_or(0.0);
            let tint: Value = Value::Array(
                grid.iter()
                    .map(|col| {
                        Value::Array(
                            col.iter()
                                .map(|s| {
                                    let (m, _) = at(s);
                                    let mk = m.get(k).copied().unwrap_or(1.0);
                                    (if m0k == 0.0 { 1.0 } else { mk / m0k }).into()
                                })
                                .collect(),
                        )
                    })
                    .collect(),
            );
            let screen: Value = Value::Array(
                grid.iter()
                    .map(|col| {
                        Value::Array(
                            col.iter()
                                .map(|s| {
                                    let (_, sc) = at(s);
                                    (sc.get(k).copied().unwrap_or(0.0) - s0k).into()
                                })
                                .collect(),
                        )
                    })
                    .collect(),
            );
            push(&format!("tint.{ch}"), tint);
            push(&format!("screenTint.{ch}"), screen);
        }
    }
}

/// Splitting a multi-parameter set: every slice but the first is relative
/// to the value at the defaults (deltas subtract, factors divide).
fn remove_base(s: &Sample, base: &Sample, multiply: bool) -> Sample {
    let op = |a: f32, b: f32| if multiply { if b == 0.0 { 1.0 } else { a / b } } else { a - b };
    let f = |a: Option<f32>, b: Option<f32>| a.map(|a| op(a, b.unwrap_or(if multiply { 1.0 } else { 0.0 })));
    fn arr<const N: usize>(
        a: Option<[f32; N]>,
        b: Option<[f32; N]>,
        op: &dyn Fn(f32, f32) -> f32,
        miss: f32,
    ) -> Option<[f32; N]> {
        let b = b.unwrap_or([miss; N]);
        a.map(|a| {
            let mut o = a;
            for (o, b) in o.iter_mut().zip(b) {
                *o = op(*o, b);
            }
            o
        })
    }
    let miss = if multiply { 1.0 } else { 0.0 };
    Sample {
        positions: s.positions.as_ref().map(|p| {
            let b = base.positions.clone().unwrap_or_default();
            p.iter()
                .enumerate()
                .map(|(i, v)| {
                    let q = b.get(i).copied().unwrap_or([0.0; 2]);
                    [v[0] - q[0], v[1] - q[1]]
                })
                .collect()
        }),
        opacity: f(s.opacity, base.opacity),
        draw_order: s.draw_order.map(|a| a - base.draw_order.unwrap_or(0.0)),
        angle: f(s.angle, base.angle),
        origin: arr(s.origin, base.origin, &op, miss),
        scale: f(s.scale, base.scale),
        scale_xy: arr(s.scale_xy, base.scale_xy, &op, miss),
        multiply_color: arr(s.multiply_color, base.multiply_color, &op, miss),
        screen_color: arr(s.screen_color, base.screen_color, &|a, b| a - b, 0.0),
    }
}
