//! Inochi2D puppet JSON + textures -> Riglet document (design doc
//! sections 4 and 0). Section/row references in comments point there.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Cursor;

use riglet_format::buffer::BufferWriter;
use riglet_format::model::*;
use riglet_format::{Document, EvalOutput, Limits, ParamValues, Rig};
use serde_json::{Map, Value};

use crate::container::{self, Encoding};
use crate::reinterpolate::{self, Grid};
use crate::{InochiError, tga};

/// Import options.
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// Reader limits for the container, the JSON and the result.
    pub limits: Limits,
    /// The file is an Inochi Creator project (`.inx`) rather than `.inp`.
    pub inx: bool,
}

/// An imported model and every conversion warning (also recorded in the
/// document's provenance).
#[derive(Debug, Clone)]
pub struct Import {
    pub document: Document,
    pub warnings: Vec<ConversionWarning>,
}

const BUFFER_FILE: &str = "buffers/inochi2d.bin";
const EXT: &str = "inochi2d";

// ---------- JSON helpers (tolerant: missing or mistyped = default) ----------

fn num(v: &Value, k: &str) -> Option<f32> {
    v.get(k).and_then(Value::as_f64).map(|f| f as f32).filter(|f| f.is_finite())
}

fn uuid(v: &Value, k: &str) -> Option<u32> {
    v.get(k).and_then(Value::as_u64).and_then(|u| u32::try_from(u).ok())
}

fn string<'a>(v: &'a Value, k: &str) -> Option<&'a str> {
    v.get(k).and_then(Value::as_str)
}

fn flag(v: &Value, k: &str, default: bool) -> bool {
    v.get(k).and_then(Value::as_bool).unwrap_or(default)
}

fn floats(v: Option<&Value>) -> Vec<f32> {
    v.and_then(Value::as_array)
        .map(|a| a.iter().map(|x| x.as_f64().map_or(0.0, |f| f as f32)).collect())
        .unwrap_or_default()
}

fn vec3(v: &Value, k: &str, default: [f32; 3]) -> [f32; 3] {
    let f = floats(v.get(k));
    let mut out = default;
    for (o, x) in out.iter_mut().zip(f) {
        if x.is_finite() {
            *o = x;
        }
    }
    out
}

fn pairs(flat: &[f32]) -> Option<Vec<Vec2>> {
    let (chunks, rest) = flat.as_chunks::<2>();
    rest.is_empty().then(|| chunks.to_vec())
}

/// The node's JSON without its bulky or fully-mapped parts, for
/// `extensions` (export restores the rest from here).
fn ext_copy(v: &Value) -> Value {
    let mut m = v.as_object().cloned().unwrap_or_default();
    for k in ["children", "mesh"] {
        m.remove(k);
    }
    Value::Object(m)
}

fn node_id(uuid: u32) -> Id {
    format!("inochi:{uuid}")
}

// ---------- 2D affine for baked static transforms ----------

#[derive(Debug, Clone, Copy, PartialEq)]
struct Affine {
    /// Columns: x axis, y axis, translation.
    m: [[f32; 2]; 3],
}

impl Affine {
    const IDENTITY: Affine = Affine { m: [[1.0, 0.0], [0.0, 1.0], [0.0, 0.0]] };

    /// T * R * S (Inochi2D node transform, rotation in radians).
    fn trs(t: Vec2, rz: f32, s: Vec2) -> Affine {
        let (sin, cos) = rz.sin_cos();
        Affine { m: [[cos * s[0], sin * s[0]], [-sin * s[1], cos * s[1]], t] }
    }

    fn then(self, inner: Affine) -> Affine {
        let lin = |v: Vec2| [self.m[0][0] * v[0] + self.m[1][0] * v[1], self.m[0][1] * v[0] + self.m[1][1] * v[1]];
        let [ix, iy, it] = inner.m;
        let t = lin(it);
        Affine { m: [lin(ix), lin(iy), [t[0] + self.m[2][0], t[1] + self.m[2][1]]] }
    }

    fn point(self, p: Vec2) -> Vec2 {
        [
            self.m[0][0] * p[0] + self.m[1][0] * p[1] + self.m[2][0],
            self.m[0][1] * p[0] + self.m[1][1] * p[1] + self.m[2][1],
        ]
    }

    fn vector(self, v: Vec2) -> Vec2 {
        [self.m[0][0] * v[0] + self.m[1][0] * v[1], self.m[0][1] * v[0] + self.m[1][1] * v[1]]
    }

    fn inverse(self) -> Option<Affine> {
        let [[a, b], [c, d], [tx, ty]] = self.m;
        let det = a * d - b * c;
        if det.abs() < 1e-12 || !det.is_finite() {
            return None;
        }
        let x = [d / det, -b / det];
        let y = [-c / det, a / det];
        let inv = Affine { m: [x, y, [0.0, 0.0]] };
        let t = inv.vector([tx, ty]);
        Some(Affine { m: [x, y, [-t[0], -t[1]]] })
    }
}

// ---------- state ----------

/// Where an Inochi2D node landed in the Riglet tree.
#[derive(Debug, Clone, Default)]
struct Mapped {
    ty: String,
    /// Rotation deformer for the node transform, if any.
    transform: Option<Id>,
    drawable: Option<Id>,
    mesh_deformer: Option<Id>,
    composite: Option<Id>,
    /// Static transform baked into vertices (under a MeshGroup).
    bake: Option<Affine>,
    children: Vec<u32>,
    vertex_count: usize,
    /// Rest rotation about x and y (radians) and the stored scale, for
    /// transform.r.x / r.y bindings (S6).
    rot3: [f32; 2],
    rest_scale: Vec2,
    texture: Option<usize>,
}

struct TextureInfo {
    id: Id,
    partial_alpha: bool,
}

struct ParamInfo {
    id: Id,
    vec2: bool,
    /// Keys per component, in parameter units.
    keys: [Vec<f32>; 2],
    /// Normalized axis points, as stored.
    axis: [Vec<f32>; 2],
}

/// (masked node id, is composite, sources by uuid).
type PendingMask = (Id, bool, Vec<(u32, MaskMode)>);

/// (json, parent frame, accumulated zSort, mesh-group bake, parent uuid,
/// MeshGroup reach).
type WalkItem<'v> = (&'v Value, Id, f32, Option<Affine>, Option<u32>, Option<Reach>);

/// How the nearest MeshGroup reaches a node. Inochi2D's `MeshGroup.setGroup`
/// filters a child and recurses only through drawables and composites with
/// `propagate_meshgroup`; any other node is "blocked": not deformed (with
/// `translate_children`, only its translation moves), and its subtree isn't
/// reached at all, not even by outer MeshGroups.
#[derive(Debug, Clone)]
struct Reach {
    /// A frame with the same coordinates as the current one but outside
    /// every reaching mesh deformer, once `chain` is replicated under it.
    outside: Id,
    /// Rotation deformers created inside the mesh deformers since `outside`;
    /// a blocked node is placed under rigid copies of them ("twins").
    chain: Vec<Id>,
    translate_children: bool,
    /// A node on the way is hidden (its visibility must carry over).
    hidden: bool,
    /// The reaching MeshGroup's mesh deformer, when it is not dynamic.
    static_mesh: Option<Id>,
}

struct Importer<'a> {
    limits: &'a Limits,
    nodes: BTreeMap<Id, Node>,
    buf: BufferWriter,
    files: BTreeMap<String, Vec<u8>>,
    textures: Vec<Texture>,
    texture_info: Vec<TextureInfo>,
    params: Vec<Parameter>,
    param_info: HashMap<u32, ParamInfo>,
    mapped: HashMap<u32, Mapped>,
    warnings: Vec<ConversionWarning>,
    /// Masks to resolve after the walk.
    pending_masks: Vec<PendingMask>,
    mask_thresholds: HashMap<u32, f32>,
    /// Nodes whose transform is animated (need a rotation deformer).
    animated_transform: HashSet<u32>,
    /// Nodes with deform bindings.
    deformed: HashSet<u32>,
    placeholder: HashMap<&'static str, Id>,
    /// (rotation deformer, its rigid twin outside the mesh deformers): the
    /// twin gets a copy of the original's keyform sets after the bindings.
    twins: Vec<(Id, Id)>,
    /// Per mesh deformer: the bake of its MeshGroup (Riglet coordinates =
    /// bake applied to the MeshGroup's local coordinates).
    mesh_bakes: HashMap<Id, Affine>,
    static_deform_warned: HashSet<Id>,
    /// Per node and axis (x = 0, y = 1): how many rest values and bindings
    /// rotate it, and the largest angle (radians).
    rot3_used: HashMap<u32, BTreeMap<usize, (u32, f32)>>,
}

fn warn(warnings: &mut Vec<ConversionWarning>, code: &str, object: Option<&str>, message: impl Into<String>) {
    warnings.push(ConversionWarning { code: code.into(), object_id: object.map(Into::into), message: message.into() });
}

fn base(id: &str, name: &str, parent: Option<&str>, visible: bool) -> NodeBase {
    NodeBase {
        id: id.into(),
        name: name.into(),
        parent: parent.map(Into::into),
        children: Vec::new(),
        visible,
        keyform_sets: Vec::new(),
        extensions: None,
    }
}

fn set_ext(base: &mut NodeBase, value: Value) {
    let mut m = base.extensions.take().unwrap_or_default();
    m.insert(EXT.into(), value);
    base.extensions = Some(m);
}

/// Inochi2D blend mode name -> Riglet (R4, design section 4a): its
/// fixed-function modes as on its legacy GL path, where LinearDodge is the
/// same as Screen. `None` = needs destination reads (unsupported).
fn blend(name: &str) -> Option<BlendMode> {
    Some(match name {
        "Normal" => BlendMode::Normal,
        "Multiply" => BlendMode::Multiply,
        "Screen" | "LinearDodge" => BlendMode::Screen,
        "Lighten" => BlendMode::Lighten,
        "ColorDodge" => BlendMode::ColorDodge,
        "AddGlow" => BlendMode::AddGlow,
        "Subtract" => BlendMode::Subtract,
        "Exclusion" => BlendMode::Exclusion,
        "Inverse" => BlendMode::Inverse,
        "DestinationIn" => BlendMode::DestinationIn,
        "ClipToLower" => BlendMode::ClipToLower,
        "SliceFromLower" => BlendMode::SliceFromLower,
        _ => return None,
    })
}

impl Importer<'_> {
    fn warn(&mut self, code: &str, object: Option<&str>, message: impl Into<String>) {
        warn(&mut self.warnings, code, object, message);
    }

    fn insert(&mut self, node: Node) -> Result<(), InochiError> {
        let id = node.base().id.clone();
        if self.nodes.len() as u64 >= self.limits.max_nodes {
            return Err(InochiError::Limit { what: "node count", limit: self.limits.max_nodes });
        }
        if let Some(p) = node.base().parent.clone()
            && let Some(parent) = self.nodes.get_mut(&p)
        {
            parent.base_mut().children.push(id.clone());
        }
        if self.nodes.insert(id.clone(), node).is_some() {
            return Err(InochiError::Puppet(format!("duplicate node id {id}")));
        }
        Ok(())
    }

    fn group(&mut self, id: &str, name: &str, parent: &str, visible: bool) -> Result<(), InochiError> {
        self.insert(Node::Group(GroupNode {
            base: base(id, name, Some(parent), visible),
            composite: false,
            blend_mode: None,
            masks: None,
            rest: GroupRest { opacity: 1.0, draw_order: None },
        }))
    }

    /// A 1x1 placeholder texture (created once per kind).
    fn placeholder(&mut self, kind: &'static str, rgba: [u8; 4]) -> Id {
        if let Some(id) = self.placeholder.get(kind) {
            return id.clone();
        }
        let id = format!("placeholder_{kind}");
        let file = format!("textures/{id}.png");
        self.files.insert(file.clone(), encode_png(1, 1, &rgba));
        self.textures.push(Texture { id: id.clone(), file, width: 1, height: 1, premultiplied_alpha: true });
        self.placeholder.insert(kind, id.clone());
        id
    }

    // ----- textures -----

    fn textures(&mut self, raw: Vec<container::RawTexture>) -> Result<(), InochiError> {
        let max = self.limits.max_texture_dimension;
        for (i, t) in raw.into_iter().enumerate() {
            let id = format!("tex{i}");
            let file = format!("textures/{i}.png");
            let is_png = t.data.get(..8) == Some(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A][..]);
            // Inochi2D sniffs PNG/TGA and ignores the encoding byte.
            let decoded = if t.data.is_empty() {
                Err("empty texture".to_owned())
            } else if is_png {
                decode_png(&t.data, max).map_err(|e| e.to_string())
            } else if t.encoding == Encoding::Bc7 {
                Err("BC7 textures can't be decoded".to_owned())
            } else {
                tga::decode(&t.data, max).map(|img| (img.width, img.height, img.rgba)).map_err(|e| e.to_string())
            };
            match decoded {
                Ok((width, height, rgba)) => {
                    let partial_alpha = rgba.as_chunks::<4>().0.iter().any(|&[.., a]| a != 0 && a != 255);
                    let bytes = if is_png { t.data } else { encode_png(width, height, &rgba) };
                    self.files.insert(file.clone(), bytes);
                    self.textures.push(Texture { id: id.clone(), file, width, height, premultiplied_alpha: true });
                    self.texture_info.push(TextureInfo { id, partial_alpha });
                }
                Err(reason) => {
                    let code = if t.data.is_empty() { "TEXTURE_EMPTY" } else { "TEXTURE_UNDECODABLE" };
                    // Keep the raw bytes so export can write them back.
                    if !t.data.is_empty() {
                        self.files.insert(format!("extensions/inochi2d/textures/{i}.raw{}", t.encoding.byte()), t.data);
                    }
                    let placeholder = self.placeholder("magenta", [255, 0, 255, 255]);
                    self.warn(code, Some(&id), format!("texture {i}: {reason}; using a placeholder"));
                    self.texture_info.push(TextureInfo { id: placeholder, partial_alpha: false });
                }
            }
        }
        Ok(())
    }

    // ----- parameters -----

    fn params(&mut self, list: &[Value]) -> Result<(), InochiError> {
        for p in list {
            let Some(uuid) = uuid(p, "uuid") else {
                self.warn("PARAMETER_INVALID", None, "parameter without a uuid skipped");
                continue;
            };
            let id = node_id(uuid);
            let name = string(p, "name").unwrap_or("").to_owned();
            let vec2 = flag(p, "is_vec2", false);
            let n = if vec2 { 2 } else { 1 };
            let (min, max, defaults) = (floats(p.get("min")), floats(p.get("max")), floats(p.get("defaults")));
            let axes = p.get("axis_points").and_then(Value::as_array).cloned().unwrap_or_default();
            let mut keys = [Vec::new(), Vec::new()];
            let mut axis = [vec![0.0], vec![0.0]];
            let mut ok = true;
            let (mut pmin, mut pmax, mut pdef) = (vec![0.0; n], vec![1.0; n], vec![0.0; n]);
            let slots = keys.iter_mut().zip(&mut axis).zip(pmin.iter_mut().zip(&mut pmax).zip(&mut pdef));
            for (c, ((key, ax), ((smin, smax), sdef))) in slots.enumerate() {
                let (lo, hi) = (min.get(c).copied().unwrap_or(0.0), max.get(c).copied().unwrap_or(1.0));
                if !(lo < hi && lo.is_finite() && hi.is_finite()) {
                    ok = false;
                    break;
                }
                let pts = floats(axes.get(c));
                if pts.is_empty()
                    || pts.iter().zip(pts.iter().skip(1)).any(|(a, b)| a >= b)
                    || !pts.iter().all(|x| x.is_finite())
                {
                    ok = false;
                    break;
                }
                *key = pts.iter().map(|t| lo + t * (hi - lo)).collect();
                *ax = pts;
                *smin = lo;
                *smax = hi;
                *sdef = defaults.get(c).copied().unwrap_or(lo).clamp(lo, hi);
            }
            if !ok {
                self.warn(
                    "PARAMETER_INVALID",
                    Some(&id),
                    format!("parameter `{name}` has an invalid range or axis points; skipped"),
                );
                continue;
            }
            let mut ext = Map::new();
            for k in ["merge_mode", "parentUUID"] {
                if let Some(v) = p.get(k) {
                    ext.insert(k.into(), v.clone());
                }
            }
            let mut extensions = Map::new();
            if !ext.is_empty() {
                extensions.insert(EXT.into(), Value::Object(ext));
            }
            self.params.push(Parameter {
                id: id.clone(),
                name,
                kind: if vec2 { ParamKind::TwoD } else { ParamKind::OneD },
                min: pmin,
                max: pmax,
                default: pdef,
                repeat: None,
                group: None,
                extensions: (!extensions.is_empty()).then_some(extensions),
            });
            self.param_info.insert(uuid, ParamInfo { id, vec2, keys, axis });
        }
        Ok(())
    }

    // ----- node tree -----

    fn walk(&mut self, root: &Value) -> Result<(), InochiError> {
        // Root: Riglet's root group; the Inochi2D root's transform (rare)
        // becomes a rotation deformer under it.
        self.insert(Node::Group(GroupNode {
            base: base("root", "root", None, true),
            composite: false,
            blend_mode: None,
            masks: None,
            rest: GroupRest { opacity: 1.0, draw_order: None },
        }))?;
        let mut stack: Vec<WalkItem> = vec![(root, "root".into(), 0.0, None, None, None)];
        while let Some((v, frame, acc, bake, parent_uuid, reach)) = stack.pop() {
            let Some(uuid) = uuid(v, "uuid") else {
                self.warn("NODE_INVALID", None, "node without a uuid skipped (with its subtree)");
                continue;
            };
            if self.mapped.contains_key(&uuid) {
                return Err(InochiError::Puppet(format!("duplicate node uuid {uuid}")));
            }
            let ty = string(v, "type").unwrap_or("Node").to_owned();
            let name = string(v, "name").unwrap_or("").to_owned();
            let visible = flag(v, "enabled", true);
            let acc = acc + num(v, "zsort").unwrap_or(0.0);
            let id = node_id(uuid);
            if let Some(p) = parent_uuid.and_then(|p| self.mapped.get_mut(&p)) {
                p.children.push(uuid);
            }

            let known = ["Node", "Tmp", "Part", "Mask", "Composite", "MeshGroup", "SimplePhysics", "Camera"];
            if !known.contains(&ty.as_str()) {
                self.warn(
                    "NODE_TYPE_UNSUPPORTED",
                    Some(&id),
                    format!("node type `{ty}` (`{name}`) kept in extensions with its subtree"),
                );
                self.keep_unknown(&frame, v);
                continue;
            }
            if ty == "Camera" {
                self.keep_unknown(&frame, v);
                continue;
            }

            // Transform (section 4, nodes).
            let tr = v.get("transform").cloned().unwrap_or(Value::Null);
            let t = vec3(&tr, "trans", [0.0; 3]);
            let r = vec3(&tr, "rot", [0.0; 3]);
            let sf = floats(tr.get("scale"));
            let rest_scale = [sf.first().copied().unwrap_or(1.0), sf.get(1).copied().unwrap_or(1.0)];
            // Rotations about x and y (S6): projected orthographically,
            // Inochi2D's T * Rz Ry Rx * S is R(rz) * [[cos ry, sin ry sin rx],
            // [0, cos rx]] * S, so each alone is a (signed) per-axis scale.
            let s = [rest_scale[0] * r[1].cos(), rest_scale[1] * r[0].cos()];
            for (axis, a) in [r[0], r[1]].into_iter().enumerate() {
                if a != 0.0 {
                    self.note_rot3(uuid, axis, a.abs());
                }
            }
            if flag(v, "lockToRoot", false) {
                self.warn("LOCK_TO_ROOT_UNSUPPORTED", Some(&id), "lockToRoot is not supported (kept in extensions)");
            }
            let identity = t[0] == 0.0 && t[1] == 0.0 && r[2] == 0.0 && s == [1.0, 1.0];
            let animated = self.animated_transform.contains(&uuid);
            let mut m = Mapped { ty: ty.clone(), rot3: [r[0], r[1]], rest_scale, ..Default::default() };

            // MeshGroup reach (see `Reach`). A blocked node without
            // translate_children moves outside the mesh deformers (exact);
            // with it, it gets a rigid rotation deformer inside them.
            let propagates = matches!(ty.as_str(), "Part" | "Mask" | "MeshGroup")
                || (ty == "Composite" && flag(v, "propagate_meshgroup", false));
            let (frame, visible, reached, translated) = match &reach {
                Some(r) if !propagates && r.translate_children => (frame, visible, false, true),
                Some(r) if !propagates => (self.outside_frame(r)?, visible && !r.hidden, false, false),
                Some(_) => (frame, visible, true, false),
                None => (frame, visible, false, false),
            };

            if reached
                && self.deformed.contains(&uuid)
                && let Some(mesh) = reach.as_ref().and_then(|r| r.static_mesh.clone())
                && self.static_deform_warned.insert(mesh.clone())
            {
                self.warn(
                    "MESHGROUP_DEFORM_APPROXIMATED",
                    Some(&mesh),
                    "a MeshGroup without dynamic_deformation adds its children's own deformation after its map; \
                     Riglet maps the deformed positions (E6)",
                );
            }

            // Under a MeshGroup, static transforms are baked into vertices
            // (exact); animated ones need a rigid rotation deformer.
            let mut created: Vec<Id> = Vec::new();
            let (frame, bake) = match bake {
                Some(outer) if !animated && !translated => {
                    (frame, Some(outer.then(Affine::trs([t[0], t[1]], r[2], s))))
                }
                Some(outer) => {
                    if translated {
                        self.warn(
                            "MESHGROUP_TRANSLATE_APPROXIMATED",
                            Some(&id),
                            "Inochi2D's MeshGroup only translates this node (translate_children); Riglet also \
                             rotates and scales it with the mesh around its origin",
                        );
                    } else if reached {
                        self.warn(
                            "MESHGROUP_TRANSFORM_APPROXIMATED",
                            Some(&id),
                            "an animated transform under a MeshGroup is mapped rigidly; Inochi2D deforms each vertex",
                        );
                    }
                    // The baked static ancestors become one static rotation
                    // deformer, so this transform and its bindings stay in
                    // their own local frame.
                    let parent = if outer == Affine::IDENTITY {
                        frame
                    } else {
                        let bid = format!("{id}:bake");
                        self.bake_frame(&bid, &name, &frame, outer)?;
                        created.push(bid.clone());
                        bid
                    };
                    let rid = format!("{id}:transform");
                    self.rotation(&rid, &name, &parent, visible, t, r[2], s)?;
                    m.transform = Some(rid.clone());
                    created.push(rid.clone());
                    (rid, Some(Affine::IDENTITY))
                }
                None if identity && !animated => (frame, None),
                None => {
                    let rid = format!("{id}:transform");
                    self.rotation(&rid, &name, &frame, visible, t, r[2], s)?;
                    m.transform = Some(rid.clone());
                    (rid, None)
                }
            };
            m.bake = bake;
            // Visibility applies once, on the outermost node created.
            let inner_visible = visible || m.transform.is_some();

            let children: Vec<&Value> =
                v.get("children").and_then(Value::as_array).map(|a| a.iter().collect()).unwrap_or_default();
            let child_frame;
            let mut child_bake = bake;
            // Reached nodes pass the reach on (blocked ones end it).
            let mut child_reach = match &reach {
                Some(r) if reached => {
                    let mut r = r.clone();
                    r.chain.extend(created.iter().cloned());
                    r.hidden |= !visible;
                    Some(r)
                }
                _ => None,
            };
            match ty.as_str() {
                "Part" | "Mask" => {
                    let has_children = !children.is_empty();
                    // Part with children: a frame group holds the drawable first,
                    // then the children (V2: drawables have no children).
                    let (holder, did) = if has_children && m.transform.is_none() {
                        self.group(&id, &name, &frame, inner_visible)?;
                        (id.clone(), format!("{id}:part"))
                    } else if m.transform.is_some() {
                        (frame.clone(), format!("{id}:part"))
                    } else {
                        (frame.clone(), id.clone())
                    };
                    let visible_here = if holder == frame && m.transform.is_none() { inner_visible } else { true };
                    if self.drawable(
                        v,
                        uuid,
                        &did,
                        &name,
                        &holder,
                        visible_here && (ty != "Mask"),
                        acc,
                        bake,
                        &mut m,
                    )? {
                        m.drawable = Some(did);
                    }
                    child_frame = holder;
                }
                "Composite" => {
                    let cid = id.clone();
                    let opacity = num(v, "opacity").unwrap_or(1.0).clamp(0.0, 1.0);
                    let mode = string(v, "blend_mode").unwrap_or("Normal");
                    let blend_mode = blend(mode).unwrap_or_else(|| {
                        warn(
                            &mut self.warnings,
                            "BLEND_MODE_UNSUPPORTED",
                            Some(&cid),
                            format!("blend mode `{mode}` drawn as normal"),
                        );
                        BlendMode::Normal
                    });
                    let tint = vec3(v, "tint", [1.0; 3]);
                    let screen = vec3(v, "screenTint", [0.0; 3]);
                    if tint != [1.0; 3] || screen != [0.0; 3] {
                        self.warn(
                            "COMPOSITE_TINT_DROPPED",
                            Some(&cid),
                            "composite tint/screenTint is not supported (kept in extensions)",
                        );
                    }
                    if flag(v, "propagate_meshgroup", false) {
                        self.warn(
                            "PROPAGATE_MESHGROUP_UNSUPPORTED",
                            Some(&cid),
                            "propagate_meshgroup is not supported",
                        );
                    }
                    // The composite wraps its transform, so its masks and
                    // blend apply to the finished layer.
                    let parent = match &m.transform {
                        Some(rid) => self.nodes.get(rid).and_then(|n| n.base().parent.clone()).unwrap_or(frame.clone()),
                        None => frame.clone(),
                    };
                    let mut cbase = base(&cid, &name, Some(&parent), visible);
                    set_ext(&mut cbase, ext_copy(v));
                    self.insert(Node::Group(GroupNode {
                        base: cbase,
                        composite: true,
                        blend_mode: (blend_mode != BlendMode::Normal).then_some(blend_mode),
                        masks: None,
                        rest: GroupRest { opacity, draw_order: Some(-acc) },
                    }))?;
                    if let Some(rid) = m.transform.clone() {
                        self.reparent(&rid, &cid);
                    }
                    self.collect_masks(v, &cid, true);
                    m.composite = Some(cid.clone());
                    child_frame = m.transform.clone().unwrap_or(cid);
                }
                "MeshGroup" => {
                    if flag(v, "dynamic_deformation", false) {
                        self.warn(
                            "MESHGROUP_DYNAMIC_APPROXIMATED",
                            Some(&id),
                            "dynamic_deformation: Inochi2D looks every deformed vertex up again each frame; Riglet \
                             binds vertices to triangles at rest (E6)",
                        );
                    }
                    let mid = if m.transform.is_some() { format!("{id}:mesh") } else { id.clone() };
                    if let Some(count) = self.mesh_deformer(v, &mid, &name, &frame, inner_visible, bake)? {
                        m.mesh_deformer = Some(mid.clone());
                        m.vertex_count = count;
                        // Children are in the MeshGroup's local frame: the
                        // same bake as its own mesh (identity unless it is
                        // a static transform under another MeshGroup).
                        child_bake = Some(bake.unwrap_or(Affine::IDENTITY));
                        let (outside, mut chain) = match (&reach, reached) {
                            (Some(r), true) => (r.outside.clone(), r.chain.clone()),
                            _ => (frame.clone(), Vec::new()),
                        };
                        if reached {
                            chain.extend(created.iter().cloned());
                        }
                        child_reach = Some(Reach {
                            outside,
                            chain,
                            translate_children: flag(v, "translate_children", false),
                            hidden: reach.as_ref().is_some_and(|r| reached && r.hidden) || !visible,
                            static_mesh: (!flag(v, "dynamic_deformation", false)).then(|| mid.clone()),
                        });
                        child_frame = mid;
                    } else {
                        self.group(&mid, &name, &frame, inner_visible)?;
                        child_frame = mid;
                    }
                }
                _ => {
                    // Node, Tmp, SimplePhysics: a plain frame.
                    if ty == "SimplePhysics" {
                        self.warn(
                            "SIMPLE_PHYSICS_UNSUPPORTED",
                            Some(&id),
                            "SimplePhysics is kept in extensions; Riglet does not simulate it",
                        );
                    }
                    if m.transform.is_none() {
                        self.group(&id, &name, &frame, visible)?;
                        child_frame = id.clone();
                    } else {
                        child_frame = frame.clone();
                    }
                }
            }
            // Raw fields for export (on the outermost Riglet node).
            let outer = m.transform.clone().or(m.composite.clone()).unwrap_or_else(|| match ty.as_str() {
                "Part" | "Mask" if children.is_empty() => m.drawable.clone().unwrap_or(id.clone()),
                "MeshGroup" => m.mesh_deformer.clone().unwrap_or(id.clone()),
                _ => id.clone(),
            });
            if m.composite.is_none()
                && let Some(n) = self.nodes.get_mut(&outer)
            {
                set_ext(n.base_mut(), ext_copy(v));
            }
            self.mapped.insert(uuid, m);
            // Push children in reverse so they pop in file order.
            for c in children.iter().rev() {
                stack.push((c, child_frame.clone(), acc, child_bake, Some(uuid), child_reach.clone()));
            }
        }
        Ok(())
    }

    /// Inochi2D looks a point up in a rest-pose bitmask: the cell of
    /// `floor(p)` holds the triangle containing that integer corner (the
    /// highest index wins), and a point outside the rest bounds or in an
    /// empty cell stays undeformed. Riglet binds every point (E6) and
    /// extrapolates. Warn once per MeshGroup whose reached drawables have
    /// points Inochi2D leaves undeformed at rest.
    fn check_mesh_group_coverage(&mut self) {
        let mut findings = Vec::new();
        // One snapshot of the buffer for every lookup below.
        let mut files = BTreeMap::new();
        self.buf.clone().finish(&mut files);
        let vec2s = |r: &BufferRef| riglet_format::buffer::read_vec2(&files, r, u64::MAX).ok();
        for (id, node) in &self.nodes {
            let Node::MeshDeformer(m) = node else { continue };
            let Some(to_local) = self.mesh_bakes.get(id).copied().unwrap_or(Affine::IDENTITY).inverse() else {
                continue;
            };
            let Some(points) = vec2s(&m.mesh.positions) else { continue };
            let Some(indices) = riglet_format::buffer::read_indices(&files, &m.mesh.indices, u64::MAX).ok() else {
                continue;
            };
            let local: Vec<Vec2> = points.iter().map(|&p| to_local.point(p)).collect();
            let tris: Vec<[Vec2; 3]> =
                (0..indices.len() / 3).map(|t| riglet_format::geometry::triangle(&local, &indices, t)).collect();
            let (mut lo, mut hi) = ([f32::MAX; 2], [f32::MIN; 2]);
            for p in &local {
                lo = [lo[0].min(p[0]), lo[1].min(p[1])];
                hi = [hi[0].max(p[0]), hi[1].max(p[1])];
            }
            let (lo, hi) = ([lo[0].floor(), lo[1].floor()], [hi[0].ceil(), hi[1].ceil()]);
            let sign = |p: Vec2, a: Vec2, b: Vec2| (p[0] - b[0]) * (a[1] - b[1]) - (a[0] - b[0]) * (p[1] - b[1]);
            let in_tri = |p: Vec2, t: [Vec2; 3]| {
                let d = [sign(p, t[0], t[1]), sign(p, t[1], t[2]), sign(p, t[2], t[0])];
                !(d.iter().any(|&x| x < 0.0) && d.iter().any(|&x| x > 0.0))
            };
            let hit = |p: Vec2| {
                let inside = lo[0] <= p[0] && p[0] < hi[0] && lo[1] <= p[1] && p[1] < hi[1];
                let corner = [p[0].floor(), p[1].floor()];
                inside && tris.iter().any(|&t| in_tri(corner, t))
            };
            // Drawables reached through groups only (rotation deformers map
            // their points rigidly, see MESHGROUP_TRANSFORM_APPROXIMATED).
            let mut outside = 0usize;
            let mut stack: Vec<&Id> = m.base.children.iter().collect();
            while let Some(c) = stack.pop() {
                match self.nodes.get(c) {
                    Some(Node::Drawable(d)) => {
                        if let Some(pts) = vec2s(&d.mesh.positions) {
                            outside += pts.iter().filter(|&&p| !hit(to_local.point(p))).count();
                        }
                    }
                    Some(Node::Group(g)) if !g.composite => stack.extend(g.base.children.iter()),
                    _ => {}
                }
            }
            if outside > 0 {
                findings.push((id.clone(), outside));
            }
        }
        for (id, outside) in findings {
            self.warn(
                "MESHGROUP_OUTSIDE_POINTS",
                Some(&id),
                format!(
                    "{outside} child points miss the MeshGroup's rest bitmask: Inochi2D leaves them undeformed, \
                     Riglet extrapolates"
                ),
            );
        }
    }

    /// A static rotation deformer equal to `b` (rotation and signed per-axis
    /// scale, S6; a shear can't be expressed and is approximated).
    fn bake_frame(&mut self, id: &str, name: &str, parent: &str, b: Affine) -> Result<(), InochiError> {
        let [x, y, t] = b.m;
        let angle = x[1].atan2(x[0]);
        let (sin, cos) = angle.sin_cos();
        let sx = x[0].hypot(x[1]);
        let sy = -sin * y[0] + cos * y[1];
        let shear = cos * y[0] + sin * y[1];
        if shear.abs() > 1e-4 * sx.max(sy.abs()) {
            self.warn(
                "MESHGROUP_TRANSFORM_APPROXIMATED",
                Some(id),
                "static transforms above this node combine to a shear, approximated",
            );
        }
        self.rotation(id, name, parent, true, [t[0], t[1], 0.0], angle, [sx, sy])
    }

    /// The frame for a blocked node: `r.outside` with rigid twins of the
    /// rotation deformers in `r.chain` (created once per original).
    fn outside_frame(&mut self, r: &Reach) -> Result<Id, InochiError> {
        let mut parent = r.outside.clone();
        for rid in &r.chain {
            let tid = format!("{rid}:rigid");
            if !self.nodes.contains_key(&tid) {
                let Some(Node::Rotation(orig)) = self.nodes.get(rid).cloned() else { continue };
                let mut b = base(&tid, &orig.base.name, Some(&parent), orig.base.visible);
                set_ext(&mut b, serde_json::json!({ "twinOf": rid }));
                self.insert(Node::Rotation(RotationDeformerNode { base: b, rest: orig.rest }))?;
                self.twins.push((rid.clone(), tid.clone()));
            }
            parent = tid;
        }
        Ok(parent)
    }

    fn copy_twin_sets(&mut self) {
        for (orig, twin) in &self.twins {
            let sets = self.nodes.get(orig).map(|n| n.base().keyform_sets.clone()).unwrap_or_default();
            if let Some(n) = self.nodes.get_mut(twin) {
                n.base_mut().keyform_sets = sets;
            }
        }
    }

    /// Records a rotation about x or y; true when the axis already had one.
    fn note_rot3(&mut self, uuid: u32, axis: usize, angle: f32) -> bool {
        let e = self.rot3_used.entry(uuid).or_default().entry(axis).or_insert((0, 0.0));
        e.0 += 1;
        e.1 = e.1.max(angle);
        e.0 > 1
    }

    /// After the bindings: rotation about both x and y shears (dropped);
    /// under such a node a z translation moves x and y by sin(angle) * z in
    /// Inochi2D (orthographic projection after the 3D rotation), which
    /// Riglet's 2D frames don't model.
    fn check_rot3(&mut self, root: &Value) {
        let used = std::mem::take(&mut self.rot3_used);
        for (uuid, axes) in &used {
            if axes.len() == 2 {
                self.warn(
                    "TRANSFORM_3D_ROTATION_DROPPED",
                    Some(&node_id(*uuid)),
                    "rotation about both x and y shears; mapped as per-axis scales without the shear",
                );
            }
        }
        let mut stack: Vec<(&Value, Option<(u32, f32)>)> = vec![(root, None)];
        let mut depth: BTreeMap<u32, f32> = BTreeMap::new();
        while let Some((v, under)) = stack.pop() {
            let Some(uuid) = uuid(v, "uuid") else { continue };
            if let Some((top, sin)) = under {
                let tz = v.get("transform").map_or(0.0, |t| vec3(t, "trans", [0.0; 3])[2]);
                if tz != 0.0 {
                    let e = depth.entry(top).or_default();
                    *e = e.max(sin * tz.abs());
                }
            }
            let own = used
                .get(&uuid)
                .map(|a| a.values().map(|x| x.1.min(std::f32::consts::FRAC_PI_2).sin()).fold(0.0, f32::max));
            // The strongest x/y rotation above (nested ones combine in 3D,
            // which this only bounds).
            let next = match (under, own) {
                (Some(u), Some(sin)) if sin <= u.1 => Some(u),
                (_, Some(sin)) => Some((uuid, sin)),
                (u, None) => u,
            };
            for c in v.get("children").and_then(Value::as_array).into_iter().flatten() {
                stack.push((c, next));
            }
        }
        for (uuid, shift) in depth {
            if shift > 0.01 {
                self.warn(
                    "TRANSFORM_3D_DEPTH_DROPPED",
                    Some(&node_id(uuid)),
                    format!(
                        "z translations below this x/y-rotated node shift points by up to {shift:.2} px in Inochi2D"
                    ),
                );
            }
        }
    }

    fn reparent(&mut self, id: &str, new_parent: &str) {
        let Some(old) = self.nodes.get(id).and_then(|n| n.base().parent.clone()) else { return };
        if let Some(p) = self.nodes.get_mut(&old) {
            p.base_mut().children.retain(|c| c != id);
        }
        if let Some(n) = self.nodes.get_mut(id) {
            n.base_mut().parent = Some(new_parent.into());
        }
        if let Some(p) = self.nodes.get_mut(new_parent) {
            p.base_mut().children.push(id.into());
        }
    }

    fn keep_unknown(&mut self, frame: &str, v: &Value) {
        if let Some(n) = self.nodes.get_mut(frame) {
            let b = n.base_mut();
            let mut m = b.extensions.take().unwrap_or_default();
            let entry = m.entry(EXT.to_owned()).or_insert_with(|| Value::Object(Map::new()));
            if let Some(obj) = entry.as_object_mut() {
                let list = obj.entry("unsupportedChildren").or_insert_with(|| Value::Array(Vec::new()));
                if let Some(a) = list.as_array_mut() {
                    a.push(v.clone());
                }
            }
            b.extensions = Some(m);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn rotation(
        &mut self,
        id: &str,
        name: &str,
        parent: &str,
        visible: bool,
        t: [f32; 3],
        rz: f32,
        s: Vec2,
    ) -> Result<(), InochiError> {
        let scale = if s[0] == s[1] { Scale::Uniform(s[0]) } else { Scale::PerAxis(s) };
        self.insert(Node::Rotation(RotationDeformerNode {
            base: base(id, name, Some(parent), visible),
            rest: RotationRest { origin: [t[0], t[1]], angle: rz.to_degrees(), scale, opacity: 1.0 },
        }))
    }

    /// Reads `mesh`: positions (vert - origin, baked), uvs, indices.
    fn mesh(&mut self, v: &Value, id: &str, bake: Option<Affine>) -> Option<(Vec<Vec2>, Vec<Vec2>, Vec<u16>)> {
        let mesh = v.get("mesh")?;
        let verts = pairs(&floats(mesh.get("verts")));
        let uvs = pairs(&floats(mesh.get("uvs")));
        let origin = floats(mesh.get("origin"));
        let origin = [origin.first().copied().unwrap_or(0.0), origin.get(1).copied().unwrap_or(0.0)];
        let indices: Option<Vec<u16>> = mesh
            .get("indices")
            .and_then(Value::as_array)
            .map(|a| a.iter().map(|i| i.as_u64().and_then(|i| u16::try_from(i).ok())).collect())
            .unwrap_or(Some(Vec::new()));
        let (Some(verts), Some(indices)) = (verts, indices) else {
            self.warn("MESH_INVALID", Some(id), "mesh data is malformed; node skipped");
            return None;
        };
        let uvs = uvs.unwrap_or_default();
        let n = verts.len();
        if n as u64 > self.limits.max_vertices_per_mesh
            || (uvs.len() != n && !uvs.is_empty())
            || indices.len() % 3 != 0
            || indices.iter().any(|&i| usize::from(i) >= n)
        {
            self.warn("MESH_INVALID", Some(id), "mesh vertex/uv/index counts don't match; node skipped");
            return None;
        }
        let bake = bake.unwrap_or(Affine::IDENTITY);
        let positions = verts.iter().map(|p| bake.point([p[0] - origin[0], p[1] - origin[1]])).collect();
        let uvs = if uvs.is_empty() { vec![[0.0, 0.0]; n] } else { uvs };
        Some((positions, uvs, indices))
    }

    #[allow(clippy::too_many_arguments)]
    fn drawable(
        &mut self,
        v: &Value,
        uuid: u32,
        id: &str,
        name: &str,
        parent: &str,
        visible: bool,
        acc: f32,
        bake: Option<Affine>,
        m: &mut Mapped,
    ) -> Result<bool, InochiError> {
        let Some((positions, uvs, indices)) = self.mesh(v, id, bake) else { return Ok(false) };
        if positions.is_empty() {
            self.warn("MESH_EMPTY", Some(id), "part has an empty mesh; nothing to draw");
            return Ok(false);
        }
        m.vertex_count = positions.len();
        // Textures [albedo, emissive, bump]; 4294967295 = none.
        let slots: Vec<Option<usize>> = v
            .get("textures")
            .and_then(Value::as_array)
            .map(|a| a.iter().map(|t| t.as_u64().filter(|&x| x != u64::from(u32::MAX)).map(|x| x as usize)).collect())
            .unwrap_or_default();
        if slots.iter().skip(1).any(Option::is_some) {
            self.warn("EMISSIVE_BUMP_NOT_RENDERED", Some(id), "emissive/bump textures are kept but not rendered");
        }
        let is_mask_node = m.ty == "Mask";
        let texture = match slots.first().copied().flatten() {
            _ if is_mask_node => self.placeholder("white", [255, 255, 255, 255]),
            Some(i) => match self.texture_info.get(i) {
                Some(t) => {
                    m.texture = Some(i);
                    t.id.clone()
                }
                None => {
                    self.warn("TEXTURE_MISSING", Some(id), format!("texture slot {i} does not exist"));
                    self.placeholder("transparent", [0, 0, 0, 0])
                }
            },
            None => self.placeholder("transparent", [0, 0, 0, 0]),
        };
        let mode = string(v, "blend_mode").unwrap_or("Normal");
        let blend_mode = blend(mode).unwrap_or_else(|| {
            warn(
                &mut self.warnings,
                "BLEND_MODE_UNSUPPORTED",
                Some(id),
                format!("blend mode `{mode}` drawn as normal"),
            );
            BlendMode::Normal
        });
        self.mask_thresholds.insert(uuid, num(v, "mask_threshold").unwrap_or(0.5).clamp(0.0, 1.0));
        let mesh = DrawableMesh {
            positions: self.buf.push_vec2(&positions),
            uvs: self.buf.push_vec2(&uvs),
            indices: self.buf.push_u16(&indices),
        };
        self.insert(Node::Drawable(DrawableNode {
            base: base(id, name, Some(parent), visible),
            texture,
            mesh,
            blend_mode,
            culling: false,
            masks: Vec::new(),
            mask_threshold: None,
            rest: DrawableRest {
                opacity: num(v, "opacity").unwrap_or(1.0).clamp(0.0, 1.0),
                draw_order: -acc,
                multiply_color: vec3(v, "tint", [1.0; 3]).map(|c| c.clamp(0.0, 1.0)),
                screen_color: vec3(v, "screenTint", [0.0; 3]).map(|c| c.clamp(0.0, 1.0)),
            },
        }))?;
        self.collect_masks(v, id, false);
        Ok(true)
    }

    /// Returns the vertex count, or `None` when there is no usable mesh.
    fn mesh_deformer(
        &mut self,
        v: &Value,
        id: &str,
        name: &str,
        parent: &str,
        visible: bool,
        bake: Option<Affine>,
    ) -> Result<Option<usize>, InochiError> {
        let Some((positions, _, indices)) = self.mesh(v, id, bake) else { return Ok(None) };
        self.mesh_bakes.insert(id.into(), bake.unwrap_or(Affine::IDENTITY));
        // Riglet rejects zero-area rest triangles (V7); Inochi2D's
        // rasterized lookup never selects them, so dropping them is exact.
        let mut kept = Vec::with_capacity(indices.len());
        let mut dropped = 0;
        for tri in indices.as_chunks::<3>().0 {
            let [a, b, c] = tri.map(|i| positions.get(usize::from(i)).copied().unwrap_or([0.0; 2]));
            let area = ((b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])) / 2.0;
            if area.abs() < 1e-9 {
                dropped += 1;
            } else {
                kept.extend_from_slice(tri);
            }
        }
        if dropped > 0 {
            self.warn(
                "MESHGROUP_DEGENERATE_TRIANGLES",
                Some(id),
                format!("{dropped} zero-area triangles dropped (they never bind)"),
            );
        }
        if positions.is_empty() || kept.is_empty() {
            self.warn("MESH_EMPTY", Some(id), "MeshGroup has no usable triangles; treated as a group");
            return Ok(None);
        }
        let count = positions.len();
        let mesh = DeformerMesh { positions: self.buf.push_vec2(&positions), indices: self.buf.push_u16(&kept) };
        self.insert(Node::MeshDeformer(MeshDeformerNode {
            base: base(id, name, Some(parent), visible),
            mesh,
            rest: OpacityRest { opacity: 1.0 },
        }))?;
        Ok(Some(count))
    }

    fn collect_masks(&mut self, v: &Value, id: &str, composite: bool) {
        let mut list = Vec::new();
        if let Some(masks) = v.get("masks").and_then(Value::as_array) {
            for m in masks {
                let mode = if string(m, "mode") == Some("DodgeMask") { MaskMode::Invert } else { MaskMode::Clip };
                if let Some(s) = uuid(m, "source") {
                    list.push((s, mode));
                }
            }
        } else if let Some(by) = v.get("masked_by").and_then(Value::as_array) {
            // Legacy keys.
            let mode = if string(v, "mask_mode") == Some("DodgeMask") { MaskMode::Invert } else { MaskMode::Clip };
            list.extend(by.iter().filter_map(|s| s.as_u64().and_then(|u| u32::try_from(u).ok())).map(|s| (s, mode)));
        }
        if !list.is_empty() {
            self.pending_masks.push((id.into(), composite, list));
        }
    }

    fn resolve_masks(&mut self) {
        for (target, composite, list) in std::mem::take(&mut self.pending_masks) {
            // Inochi2D's stencil is order-dependent: exact when every Mask
            // comes before every DodgeMask.
            let first_invert = list.iter().position(|(_, m)| *m == MaskMode::Invert);
            if first_invert.is_some_and(|i| list.iter().skip(i).any(|(_, m)| *m == MaskMode::Clip)) {
                self.warn(
                    "MASK_ORDER",
                    Some(&target),
                    "a Mask follows a DodgeMask; Riglet's mask combination is order-independent",
                );
            }
            let mut refs = Vec::new();
            for (source, mode) in list {
                let Some(src) = self.mapped.get(&source).and_then(|m| m.drawable.clone()) else {
                    self.warn(
                        "MASK_SOURCE_MISSING",
                        Some(&target),
                        format!("mask source {source} is not a drawable part; ignored"),
                    );
                    continue;
                };
                if src == target {
                    continue;
                }
                let threshold = self.mask_thresholds.get(&source).copied().unwrap_or(0.5);
                if let Some(Node::Drawable(d)) = self.nodes.get_mut(&src) {
                    d.mask_threshold = Some(threshold);
                }
                refs.push(MaskRef { source: src, mode });
            }
            match self.nodes.get_mut(&target) {
                Some(Node::Drawable(d)) if !composite => d.masks = refs,
                Some(Node::Group(g)) if composite && !refs.is_empty() => g.masks = Some(refs),
                _ => {}
            }
        }
    }
}

// ---------- bindings ----------

/// One binding grid, reinterpolated: values[x][y].
enum Values {
    Scalar(Vec<Vec<f32>>),
    Deform(Vec<Vec<Vec<Vec2>>>),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Interp {
    Linear,
    Nearest,
    Cubic,
}

/// Expands keys for one axis: Nearest -> step axis with a key at every
/// midpoint (ties go up, like Inochi2D). Linear and Cubic map 1:1 (cubic
/// axes, S5, are Inochi2D's formula).
/// Returns (keys, source position per key: index + fraction).
fn expand_axis(keys: &[f32], interp: Interp) -> (Vec<f32>, Vec<(usize, f32)>) {
    let mut out = Vec::new();
    let mut src = Vec::new();
    for (i, &k) in keys.iter().enumerate() {
        out.push(k);
        src.push((i, 0.0));
        let Some(&next) = keys.get(i + 1) else { break };
        match interp {
            Interp::Linear | Interp::Cubic => {}
            Interp::Nearest => {
                out.push((k + next) / 2.0);
                src.push((i + 1, 0.0));
            }
        }
    }
    (out, src)
}

impl Importer<'_> {
    fn scan_animated_transforms(&mut self, params: &[Value]) {
        for p in params {
            for b in p.get("bindings").and_then(Value::as_array).into_iter().flatten() {
                let name = string(b, "param_name").unwrap_or("");
                let Some(u) = uuid(b, "node") else { continue };
                if name.starts_with("transform.") {
                    self.animated_transform.insert(u);
                } else if name == "deform" {
                    self.deformed.insert(u);
                }
            }
        }
    }

    fn bindings(&mut self, params: &[Value]) -> Result<(), InochiError> {
        // Tint/screen bindings are collected per part and converted together.
        let mut colors: HashMap<u32, Vec<(u32, String, Values, Interp)>> = HashMap::new();
        for p in params {
            let Some(puuid) = uuid(p, "uuid") else { continue };
            if !self.param_info.contains_key(&puuid) {
                continue;
            }
            for b in p.get("bindings").and_then(Value::as_array).into_iter().flatten() {
                let Some(nuuid) = uuid(b, "node") else { continue };
                let name = string(b, "param_name").unwrap_or("").to_owned();
                let interp = match string(b, "interpolate_mode") {
                    Some("Nearest") => Interp::Nearest,
                    Some("Cubic") => Interp::Cubic,
                    _ => Interp::Linear,
                };
                let Some(target) = self.mapped.get(&nuuid).cloned() else {
                    self.warn("BINDING_INVALID", None, format!("binding `{name}` targets unknown node {nuuid}"));
                    continue;
                };
                let Some(values) = self.binding_values(b, puuid, &name, target.vertex_count)? else { continue };
                if name.starts_with("tint.") || name.starts_with("screenTint.") {
                    if target.drawable.is_some() {
                        colors.entry(nuuid).or_default().push((puuid, name, values, interp));
                    } else {
                        self.warn(
                            "COMPOSITE_TINT_DROPPED",
                            Some(&node_id(nuuid)),
                            format!("binding `{name}` on a composite dropped"),
                        );
                    }
                    continue;
                }
                self.apply_binding(puuid, nuuid, &target, &name, values, interp)?;
            }
        }
        // Parts without color bindings still need their rest tint/screen
        // converted (only matters when screen is non-zero).
        let unbound: Vec<u32> = self
            .mapped
            .iter()
            .filter(|(u, m)| m.drawable.is_some() && !colors.contains_key(u))
            .map(|(u, _)| *u)
            .collect();
        for nuuid in unbound {
            self.apply_colors(nuuid, Vec::new())?;
        }
        for (nuuid, list) in colors {
            self.apply_colors(nuuid, list)?;
        }
        Ok(())
    }

    /// Parses and reinterpolates a binding's grid. `None` = skipped.
    fn binding_values(
        &mut self,
        b: &Value,
        puuid: u32,
        name: &str,
        vertex_count: usize,
    ) -> Result<Option<Values>, InochiError> {
        let object = uuid(b, "node").map(node_id);
        let Some(info) = self.param_info.get(&puuid) else { return Ok(None) };
        let (nx, ny) = (info.axis[0].len(), if info.vec2 { info.axis[1].len() } else { 1 });
        let grid = (nx as u64).saturating_mul(ny as u64);
        if grid > self.limits.max_keyform_grid {
            return Err(InochiError::Limit { what: "binding grid", limit: self.limits.max_keyform_grid });
        }
        let ax: Vec<f32> = info.axis[0].clone();
        let ay: Vec<f32> = if info.vec2 { info.axis[1].clone() } else { vec![0.0] };
        let raw = b.get("values").and_then(Value::as_array);
        let set: Vec<Vec<bool>> = match b.get("isSet").and_then(Value::as_array) {
            Some(cols) => cols
                .iter()
                .map(|c| {
                    c.as_array().map(|r| r.iter().map(|x| x.as_bool().unwrap_or(false)).collect()).unwrap_or_default()
                })
                .collect(),
            None => vec![vec![true; ny]; nx],
        };
        let shape_ok = |n: usize| n == nx;
        let bad = |s: &mut Self| {
            s.warn("BINDING_INVALID", object.as_deref(), format!("binding `{name}` has a malformed grid; skipped"));
            Ok(None)
        };
        let Some(raw) = raw.filter(|r| shape_ok(r.len())) else { return bad(self) };
        if name == "deform" {
            let mut values: Vec<Vec<Vec<Vec2>>> = Vec::with_capacity(nx);
            for col in raw {
                let mut c = Vec::with_capacity(ny);
                for cell in col.as_array().into_iter().flatten() {
                    let pts: Option<Vec<Vec2>> = cell.as_array().map(|vs| {
                        vs.iter()
                            .map(|p| {
                                let f = floats(Some(p));
                                [f.first().copied().unwrap_or(0.0), f.get(1).copied().unwrap_or(0.0)]
                            })
                            .collect()
                    });
                    match pts {
                        Some(p) if p.len() == vertex_count => c.push(p),
                        _ => {
                            self.warn(
                                "BINDING_INVALID",
                                object.as_deref(),
                                "deform binding vertex count doesn't match the mesh; skipped",
                            );
                            return Ok(None);
                        }
                    }
                }
                values.push(c);
            }
            if values.iter().any(|c| c.len() != ny) {
                return bad(self);
            }
            match reinterpolate::reinterpolate(Grid { values: &mut values, set: &set, axis: [&ax, &ay] }) {
                Ok(true) => Ok(Some(Values::Deform(values))),
                Ok(false) => Ok(None),
                Err(_) => bad(self),
            }
        } else {
            let mut values: Vec<Vec<f32>> = raw.iter().map(|c| floats(Some(c))).collect();
            if values.iter().any(|c| c.len() != ny) || values.iter().flatten().any(|f| !f.is_finite()) {
                return bad(self);
            }
            match reinterpolate::reinterpolate(Grid { values: &mut values, set: &set, axis: [&ax, &ay] }) {
                Ok(true) => Ok(Some(Values::Scalar(values))),
                Ok(false) => Ok(None),
                Err(_) => bad(self),
            }
        }
    }

    /// Axes plus a grid in Riglet order (axis 0 fastest), interpolation
    /// expanded (Nearest -> step with midpoints).
    fn grid<T: reinterpolate::Value>(
        &mut self,
        puuid: u32,
        values: &[Vec<T>],
        interp: Interp,
        object: &str,
    ) -> Option<(Vec<KeyAxis>, Vec<T>)> {
        let info = self.param_info.get(&puuid)?;
        let comps = if info.vec2 { 2 } else { 1 };
        let mut axes = Vec::new();
        let mut srcs = Vec::new();
        for c in 0..comps {
            let keys = info.keys.get(c)?;
            let (k, s) = expand_axis(keys, interp);
            axes.push(KeyAxis {
                param: ParamAxis { parameter: info.id.clone(), component: c as u8 },
                keys: k,
                interpolation: match interp {
                    Interp::Nearest => AxisInterpolation::Step,
                    Interp::Linear => AxisInterpolation::Linear,
                    Interp::Cubic => AxisInterpolation::Cubic,
                },
            });
            srcs.push(s);
        }
        let total: u64 = srcs.iter().map(|s| s.len() as u64).product();
        if total > self.limits.max_keyform_grid {
            warn(
                &mut self.warnings,
                "BINDING_INVALID",
                Some(object),
                "expanded binding grid exceeds the limit; skipped",
            );
            return None;
        }
        let ys = srcs.get(1).cloned().unwrap_or_else(|| vec![(0, 0.0)]);
        let xs = srcs.first()?.clone();
        let (nx, ny) = (values.len(), values.first().map_or(0, Vec::len));
        let at = |x: isize, y: isize| -> Option<T> {
            let xi = x.clamp(0, nx as isize - 1) as usize;
            let yi = y.clamp(0, ny as isize - 1) as usize;
            values.get(xi)?.get(yi).cloned()
        };
        let mut out = Vec::with_capacity(total as usize);
        for &(yi, yt) in &ys {
            for &(xi, xt) in &xs {
                let v = if xt == 0.0 && yt == 0.0 {
                    at(xi as isize, yi as isize)?
                } else {
                    // Inochi2D's interpolateCubic: Catmull-Rom with clamped
                    // neighbors, along x then y.
                    let (xi, yi) = (xi as isize, yi as isize);
                    let row = |y: isize| -> Option<T> {
                        Some(reinterpolate::cubic(&at(xi - 1, y)?, &at(xi, y)?, &at(xi + 1, y)?, &at(xi + 2, y)?, xt))
                    };
                    if ny <= 1 {
                        row(0)?
                    } else {
                        reinterpolate::cubic(&row(yi - 1)?, &row(yi)?, &row(yi + 1)?, &row(yi + 2)?, yt)
                    }
                };
                out.push(v);
            }
        }
        Some((axes, out))
    }

    fn push_set(&mut self, node: &str, axes: Vec<KeyAxis>, keyforms: Vec<Keyform>, combine: Combine) {
        if let Some(n) = self.nodes.get_mut(node) {
            let combine = (combine == Combine::Multiply).then_some(Combine::Multiply);
            n.base_mut().keyform_sets.push(KeyformSet { axes, keyforms, combine });
        }
    }

    fn apply_binding(
        &mut self,
        puuid: u32,
        nuuid: u32,
        target: &Mapped,
        name: &str,
        values: Values,
        interp: Interp,
    ) -> Result<(), InochiError> {
        let object = node_id(nuuid);
        match (name, values) {
            ("deform", Values::Deform(v)) => {
                let Some(dst) = target.drawable.clone().or(target.mesh_deformer.clone()) else {
                    self.warn("BINDING_UNSUPPORTED", Some(&object), "deform binding on a node without a mesh");
                    return Ok(());
                };
                let Some((axes, grid)) = self.grid(puuid, &v, interp, &object) else { return Ok(()) };
                let bake = target.bake.unwrap_or(Affine::IDENTITY);
                let kfs = grid
                    .iter()
                    .map(|d| {
                        let d: Vec<Vec2> = d.iter().map(|p| bake.vector(*p)).collect();
                        Keyform { positions: Some(self.buf.push_vec2(&d)), ..Default::default() }
                    })
                    .collect();
                self.push_set(&dst, axes, kfs, Combine::Add);
            }
            (_, Values::Scalar(v)) => {
                let Some((axes, grid)) = self.grid(puuid, &v, interp, &object) else { return Ok(()) };
                let one = |k: Keyform| k;
                let kf = |f: &dyn Fn(f32) -> Keyform| grid.iter().map(|&x| one(f(x))).collect::<Vec<_>>();
                match name {
                    "transform.t.x" | "transform.t.y" | "transform.r.z" | "transform.s.x" | "transform.s.y" => {
                        let Some(rid) = target.transform.clone() else { return Ok(()) };
                        let (kfs, combine) = match name {
                            "transform.t.x" => {
                                (kf(&|x| Keyform { origin: Some([x, 0.0]), ..Default::default() }), Combine::Add)
                            }
                            "transform.t.y" => {
                                (kf(&|y| Keyform { origin: Some([0.0, y]), ..Default::default() }), Combine::Add)
                            }
                            "transform.r.z" => {
                                (kf(&|r| Keyform { angle: Some(r.to_degrees()), ..Default::default() }), Combine::Add)
                            }
                            "transform.s.x" => {
                                (kf(&|s| Keyform { scale_xy: Some([s, 1.0]), ..Default::default() }), Combine::Multiply)
                            }
                            _ => {
                                (kf(&|s| Keyform { scale_xy: Some([1.0, s]), ..Default::default() }), Combine::Multiply)
                            }
                        };
                        self.push_set(&rid, axes, kfs, combine);
                    }
                    "transform.r.x" | "transform.r.y" => {
                        // S6: rotation about y scales x by cos, about x
                        // scales y. Offsets add to the rest angle; the
                        // scale delta is exact at the keys.
                        let Some(rid) = target.transform.clone() else { return Ok(()) };
                        if grid.iter().all(|&o| o == 0.0) {
                            return Ok(());
                        }
                        let (axis, comp) = if name == "transform.r.y" { (1, 0) } else { (0, 1) };
                        let a0 = target.rot3.get(axis).copied().unwrap_or(0.0);
                        let s0 = target.rest_scale.get(comp).copied().unwrap_or(1.0);
                        let delta = |o: f32| s0 * ((a0 + o).cos() - a0.cos());
                        let kfs = kf(&|o| {
                            let mut xy = [0.0; 2];
                            if let Some(c) = xy.get_mut(comp) {
                                *c = delta(o);
                            }
                            Keyform { scale_xy: Some(xy), ..Default::default() }
                        });
                        // Between keys Inochi2D interpolates the angle,
                        // Riglet the scale: compare at the midpoints.
                        let n0 = axes.first().map_or(1, |a| a.keys.len()).max(1);
                        let mut worst = 0.0f32;
                        if interp != Interp::Nearest {
                            for (i, &o) in grid.iter().enumerate() {
                                for j in [i + 1, i + n0] {
                                    if j == i + 1 && j % n0 == 0 {
                                        continue;
                                    }
                                    if let Some(&p) = grid.get(j) {
                                        let exact = (a0 + (o + p) / 2.0).cos();
                                        let mid = ((a0 + o).cos() + (a0 + p).cos()) / 2.0;
                                        worst = worst.max((exact - mid).abs() * s0.abs());
                                    }
                                }
                            }
                        }
                        if worst > 1e-3 {
                            self.warn(
                                "TRANSFORM_3D_ROTATION_INTERPOLATED",
                                Some(&object),
                                format!(
                                    "binding `{name}`: scale differs from Inochi2D between keys by up to {worst:.4}"
                                ),
                            );
                        }
                        let max = grid.iter().map(|o| (a0 + o).abs()).fold(0.0f32, f32::max);
                        if self.note_rot3(nuuid, axis, max) {
                            self.warn(
                                "TRANSFORM_3D_ROTATION_INTERPOLATED",
                                Some(&object),
                                format!(
                                    "several `{name}` bindings: Riglet adds their scale changes, Inochi2D their angles"
                                ),
                            );
                        }
                        self.push_set(&rid, axes, kfs, Combine::Add);
                    }
                    "transform.t.z" => {}
                    "opacity" => {
                        let Some(dst) = target.drawable.clone().or(target.composite.clone()) else { return Ok(()) };
                        let kfs = kf(&|o| Keyform { opacity: Some(o), ..Default::default() });
                        self.push_set(&dst, axes, kfs, Combine::Multiply);
                    }
                    "zSort" => {
                        let members = self.scope_members(nuuid);
                        let varies = grid.iter().zip(grid.iter().skip(1)).any(|(a, b)| a != b);
                        if interp != Interp::Nearest && varies {
                            self.warn(
                                "DRAW_ORDER_INTERPOLATION",
                                Some(&object),
                                "linear or cubic zSort binding: Riglet switches draw order at the nearest key (E3)",
                            );
                        }
                        let kfs = kf(&|z| Keyform { draw_order: Some(-z), ..Default::default() });
                        for m in members {
                            self.push_set(&m, axes.clone(), kfs.clone(), Combine::Add);
                        }
                    }
                    "alphaThreshold" => {} // no effect in Inochi2D 0.8 (kept in extensions)
                    _ => self.warn("BINDING_UNSUPPORTED", Some(&object), format!("binding `{name}` is not supported")),
                }
            }
            _ => self.warn("BINDING_INVALID", Some(&object), format!("binding `{name}` has the wrong value type")),
        }
        Ok(())
    }

    /// Drawables and composites in the subtree of `uuid` that share its
    /// draw scope (not entering composites below it).
    fn scope_members(&self, uuid: u32) -> Vec<Id> {
        let mut out = Vec::new();
        let mut stack = vec![(uuid, true)];
        while let Some((u, is_root)) = stack.pop() {
            let Some(m) = self.mapped.get(&u) else { continue };
            if let Some(c) = &m.composite {
                out.push(c.clone());
                if !is_root {
                    continue;
                }
                // The composite itself moves; its contents keep their order.
                continue;
            }
            if let Some(d) = &m.drawable {
                out.push(d.clone());
            }
            stack.extend(m.children.iter().map(|&c| (c, false)));
        }
        out
    }

    /// Tint (multiplicative) and screenTint (additive) bindings of one part,
    /// converted from Inochi2D's screen-then-multiply to Riglet's
    /// multiply-then-screen (section 0, item 4): exact at every grid point.
    fn apply_colors(&mut self, nuuid: u32, list: Vec<(u32, String, Values, Interp)>) -> Result<(), InochiError> {
        let object = node_id(nuuid);
        let Some(target) = self.mapped.get(&nuuid).cloned() else { return Ok(()) };
        let Some(did) = target.drawable.clone() else { return Ok(()) };
        let (rest_m, rest_s) = match self.nodes.get(&did) {
            Some(Node::Drawable(d)) => (d.rest.multiply_color, d.rest.screen_color),
            _ => return Ok(()),
        };
        if list.is_empty() {
            if rest_s == [0.0; 3] {
                return Ok(());
            }
            let (m2, s2) = tint_screen_to_riglet(rest_m, rest_s);
            if let Some(Node::Drawable(d)) = self.nodes.get_mut(&did) {
                d.rest.multiply_color = m2;
                d.rest.screen_color = s2;
            }
            if target.texture.and_then(|t| self.texture_info.get(t)).is_some_and(|t| t.partial_alpha) {
                self.warn(
                    "COLOR_ORDER_PARTIAL_ALPHA",
                    Some(&object),
                    "screenTint on a texture with partial alpha: Inochi2D's premultiplied screen differs there",
                );
            }
            return Ok(());
        }
        // Union grid over every parameter involved: grid points are each
        // binding's own keys, so values there are exact.
        let mut params: Vec<u32> = list.iter().map(|(p, ..)| *p).collect();
        params.sort_unstable();
        params.dedup();
        let mut axes = Vec::new();
        let mut dims = Vec::new();
        for p in &params {
            let Some(info) = self.param_info.get(p) else { return Ok(()) };
            for c in 0..if info.vec2 { 2 } else { 1 } {
                let keys = info.keys.get(c).cloned().unwrap_or_default();
                dims.push((*p, c, keys.len()));
                axes.push(KeyAxis {
                    param: ParamAxis { parameter: info.id.clone(), component: c as u8 },
                    keys,
                    interpolation: AxisInterpolation::Linear,
                });
            }
        }
        let total: u64 = dims.iter().map(|d| d.2 as u64).product();
        if total > self.limits.max_keyform_grid {
            self.warn("BINDING_INVALID", Some(&object), "color binding grid exceeds the limit; skipped");
            return Ok(());
        }
        let screen_possible = rest_s != [0.0; 3] || list.iter().any(|(_, n, ..)| n.starts_with("screenTint."));
        if list.iter().any(|(.., i)| *i != Interp::Linear) {
            self.warn(
                "COLOR_ORDER_INTERPOLATION",
                Some(&object),
                "non-linear tint/screen bindings resampled at keys only",
            );
        }
        let convert = tint_screen_to_riglet;
        // Inochi2D's color at a grid point (index per dim).
        let eval = |idx: &[usize]| -> ([f32; 3], [f32; 3]) {
            let mut m = rest_m;
            let mut s = rest_s;
            for (p, name, values, _) in &list {
                let Values::Scalar(v) = values else { continue };
                let pos: Vec<usize> = dims.iter().zip(idx).filter(|(d, _)| d.0 == *p).map(|(_, &i)| i).collect();
                let x = pos.first().copied().unwrap_or(0);
                let y = pos.get(1).copied().unwrap_or(0);
                let val = v.get(x).and_then(|c| c.get(y)).copied().unwrap_or(0.0);
                let ch = match name.rsplit('.').next() {
                    Some("r") => 0,
                    Some("g") => 1,
                    _ => 2,
                };
                let slot = if name.starts_with("tint.") { m.get_mut(ch) } else { s.get_mut(ch) };
                if let Some(slot) = slot {
                    if name.starts_with("tint.") {
                        *slot *= val;
                    } else {
                        *slot += val;
                    }
                }
            }
            (m.map(|c| c.clamp(0.0, 1.0)), s.map(|c| c.clamp(0.0, 1.0)))
        };
        let (rest_m2, rest_s2) = convert(rest_m, rest_s);
        let mut kfs = Vec::with_capacity(total as usize);
        let mut idx = vec![0usize; dims.len()];
        for _ in 0..total {
            let (m, s) = eval(&idx);
            let (m2, s2) = convert(m, s);
            kfs.push(Keyform {
                multiply_color: Some(sub3(m2, rest_m2)),
                screen_color: Some(sub3(s2, rest_s2)),
                ..Default::default()
            });
            // Increment, axis 0 fastest.
            for (i, d) in idx.iter_mut().zip(&dims) {
                *i += 1;
                if *i < d.2 {
                    break;
                }
                *i = 0;
            }
        }
        if let Some(Node::Drawable(d)) = self.nodes.get_mut(&did) {
            d.rest.multiply_color = rest_m2;
            d.rest.screen_color = rest_s2;
        }
        if screen_possible {
            // Between keys Riglet interpolates the converted colors; Inochi2D
            // converts interpolated colors. Report the largest difference at
            // the midpoints of 1D segments of each axis.
            let mut worst = 0.0f32;
            let mut idx = vec![0usize; dims.len()];
            for a in 0..dims.len() {
                for i in 0..dims.get(a).map_or(0, |d| d.2.saturating_sub(1)) {
                    idx.iter_mut().for_each(|v| *v = 0);
                    if let Some(v) = idx.get_mut(a) {
                        *v = i;
                    }
                    let (m0, s0) = eval(&idx);
                    if let Some(v) = idx.get_mut(a) {
                        *v = i + 1;
                    }
                    let (m1, s1) = eval(&idx);
                    let (mi, si) = (mid3(m0, m1), mid3(s0, s1));
                    let ((ma, sa), (mb, sb)) = (convert(m0, s0), convert(m1, s1));
                    let (mr, sr) = (mid3(ma, mb), mid3(sa, sb));
                    // Compare the result for a mid-gray opaque texel: Inochi2D
                    // screens the interpolated colors, then multiplies;
                    // Riglet interpolates the converted colors.
                    let c = 0.5;
                    for (((mi, si), mr), sr) in mi.into_iter().zip(si).zip(mr).zip(sr) {
                        let inochi = (c + si - c * si) * mi;
                        let riglet = c * mr + sr - c * mr * sr;
                        worst = worst.max((inochi - riglet).abs());
                    }
                }
            }
            if worst > 0.5 / 255.0 {
                self.warn(
                    "COLOR_ORDER_INTERPOLATION",
                    Some(&object),
                    format!("tint/screen differ from Inochi2D between keys by up to {:.4}", worst),
                );
            }
            if target.texture.and_then(|t| self.texture_info.get(t)).is_some_and(|t| t.partial_alpha) {
                self.warn(
                    "COLOR_ORDER_PARTIAL_ALPHA",
                    Some(&object),
                    "screenTint on a texture with partial alpha: Inochi2D's premultiplied screen differs there",
                );
            }
        }
        self.push_set(&did, axes, kfs, Combine::Add);
        Ok(())
    }
}

fn sub3(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    let mut out = a;
    out.iter_mut().zip(b).for_each(|(o, b)| *o -= b);
    out
}

fn mid3(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    let mut out = a;
    out.iter_mut().zip(b).for_each(|(o, b)| *o = (*o + b) / 2.0);
    out
}

/// Inochi2D screen-then-multiply `(c + S - cS) * M` as Riglet
/// multiply-then-screen `cM' + S' - cM'S'` (section 0, item 4), per channel:
/// `S' = S*M`, `M' = M(1-S)/(1-S*M)`; `S*M = 1` gives white: `M' = S' = 1`.
/// Exact for opaque texels (and wherever S = 0).
pub fn tint_screen_to_riglet(m: [f32; 3], s: [f32; 3]) -> ([f32; 3], [f32; 3]) {
    let mut mo = [0.0; 3];
    let mut so = [0.0; 3];
    for (k, (mk, sk)) in m.into_iter().zip(s).enumerate() {
        let (out_m, out_s) =
            if (1.0 - sk * mk).abs() < 1e-6 { (1.0, 1.0) } else { (mk * (1.0 - sk) / (1.0 - sk * mk), sk * mk) };
        if let (Some(a), Some(b)) = (mo.get_mut(k), so.get_mut(k)) {
            (*a, *b) = (out_m, out_s);
        }
    }
    (mo, so)
}

#[allow(dead_code, reason = "used by the exporter (M3 step 2) and the unit tests")]
/// Inverse of [`tint_screen_to_riglet`] (for export): `M = M' + S' - M'S'`,
/// `S = S'/M` (with M = 0, any S gives black; use S = 0).
pub fn tint_screen_from_riglet(m: [f32; 3], s: [f32; 3]) -> ([f32; 3], [f32; 3]) {
    let mut mo = [0.0; 3];
    let mut so = [0.0; 3];
    for (k, (mk, sk)) in m.into_iter().zip(s).enumerate() {
        let inochi_m = mk + sk - mk * sk;
        let inochi_s = if inochi_m.abs() < 1e-6 { 0.0 } else { sk / inochi_m };
        if let (Some(a), Some(b)) = (mo.get_mut(k), so.get_mut(k)) {
            (*a, *b) = (inochi_m, inochi_s);
        }
    }
    (mo, so)
}

// ---------- images ----------

fn decode_png(bytes: &[u8], max: u32) -> Result<(u32, u32, Vec<u8>), InochiError> {
    let bad = |e: String| InochiError::Texture(format!("PNG: {e}"));
    let mut probe = png::Decoder::new(Cursor::new(bytes));
    probe.set_transformations(png::Transformations::IDENTITY);
    let (w, h) = {
        let r = probe.read_info().map_err(|e| bad(e.to_string()))?;
        (r.info().width, r.info().height)
    };
    if w == 0 || h == 0 || w > max || h > max {
        return Err(InochiError::Limit { what: "PNG dimensions", limit: u64::from(max) });
    }
    let mut d = png::Decoder::new(Cursor::new(bytes));
    d.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    d.set_limits(png::Limits { bytes: (w as usize) * (h as usize) * 8 + (1 << 20) });
    let mut r = d.read_info().map_err(|e| bad(e.to_string()))?;
    let mut buf = vec![0u8; r.output_buffer_size().ok_or_else(|| bad("too large".into()))?];
    let f = r.next_frame(&mut buf).map_err(|e| bad(e.to_string()))?;
    let data = buf.get(..f.buffer_size()).unwrap_or(&[]);
    let rgba: Vec<u8> = match f.color_type {
        png::ColorType::Rgba => data.to_vec(),
        png::ColorType::Rgb => data.as_chunks::<3>().0.iter().flat_map(|&[r, g, b]| [r, g, b, 255]).collect(),
        png::ColorType::GrayscaleAlpha => data.as_chunks::<2>().0.iter().flat_map(|&[v, a]| [v, v, v, a]).collect(),
        png::ColorType::Grayscale => data.iter().flat_map(|&v| [v, v, v, 255]).collect(),
        other => return Err(bad(format!("unsupported color type {other:?}"))),
    };
    Ok((w, h, rgba))
}

fn encode_png(w: u32, h: u32, rgba: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut e = png::Encoder::new(&mut out, w, h);
    e.set_color(png::ColorType::Rgba);
    e.set_depth(png::BitDepth::Eight);
    if let Ok(mut wr) = e.write_header() {
        let _ = wr.write_image_data(rgba);
    }
    out
}

/// ISO 8601 UTC timestamp (no external date crate).
fn now_iso8601() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs()) as i64;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Civil from days (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

// ---------- entry point ----------

pub fn import(bytes: &[u8], options: &Options) -> Result<Import, InochiError> {
    let limits = &options.limits;
    let c = container::parse(bytes, limits)?;
    let json: Value = serde_json::from_slice(&c.json).map_err(|e| {
        if e.to_string().contains("recursion limit exceeded") {
            InochiError::NestingTooDeep
        } else {
            InochiError::Json(e.to_string())
        }
    })?;
    let root = json.get("nodes").ok_or_else(|| InochiError::Puppet("missing `nodes`".into()))?;
    let params: Vec<Value> = json.get("param").and_then(Value::as_array).cloned().unwrap_or_default();

    let mut im = Importer {
        limits,
        nodes: BTreeMap::new(),
        buf: BufferWriter::new(BUFFER_FILE),
        files: BTreeMap::new(),
        textures: Vec::new(),
        texture_info: Vec::new(),
        params: Vec::new(),
        param_info: HashMap::new(),
        mapped: HashMap::new(),
        warnings: Vec::new(),
        pending_masks: Vec::new(),
        mask_thresholds: HashMap::new(),
        animated_transform: HashSet::new(),
        deformed: HashSet::new(),
        placeholder: HashMap::new(),
        twins: Vec::new(),
        mesh_bakes: HashMap::new(),
        static_deform_warned: HashSet::new(),
        rot3_used: HashMap::new(),
    };
    im.textures(c.textures)?;
    im.params(&params)?;
    im.scan_animated_transforms(&params);
    im.walk(root)?;
    im.check_mesh_group_coverage();
    im.resolve_masks();
    im.bindings(&params)?;
    im.copy_twin_sets();
    im.check_rot3(root);

    if json.get("automation").and_then(Value::as_array).is_some_and(|a| !a.is_empty()) {
        im.warn("AUTOMATION_UNSUPPORTED", None, "automation (sine/physics) is kept in extensions");
    }
    let motions = motions(&json, &im.param_info, &mut im.warnings);

    // Extension entries are kept verbatim as package files.
    for (name, payload) in c.extensions {
        let safe: String =
            name.chars().map(|ch| if ch.is_ascii_alphanumeric() || "._-".contains(ch) { ch } else { '_' }).collect();
        im.files.insert(format!("extensions/inochi2d/ext/{safe}"), payload);
    }

    let mut ext = Map::new();
    for k in ["meta", "physics", "automation", "groups", "animations"] {
        if let Some(v) = json.get(k) {
            ext.insert(k.into(), v.clone());
        }
    }
    let gravity = json.get("physics").and_then(|p| num(p, "gravity")).unwrap_or(9.8);
    let source_version = json.get("meta").and_then(|m| string(m, "version")).map(str::to_owned);

    let Importer { nodes, buf, mut files, textures, params: parameters, mut warnings, .. } = im;
    buf.finish(&mut files);
    let mut model_ext = Map::new();
    model_ext.insert(EXT.into(), Value::Object(ext));
    let mut model = Model {
        canvas: Canvas { width: 1.0, height: 1.0 },
        textures,
        export_ids: ExportIdMap {
            live2d: None,
            inochi2d: Some(
                nodes
                    .keys()
                    .chain(parameters.iter().map(|p| &p.id))
                    .filter_map(|id| {
                        let rest = id.strip_prefix("inochi:")?;
                        rest.parse::<u32>().ok().map(|u| (id.clone(), u))
                    })
                    .collect(),
            ),
        },
        parameters,
        root: "root".into(),
        nodes,
        physics: Vec::new(),
        physics_environment: PhysicsEnvironment { gravity: [0.0, gravity], wind: [0.0, 0.0] },
        motions,
        extensions: Some(model_ext),
    };
    let manifest = |warnings: Vec<ConversionWarning>| Manifest {
        format: "riglet".into(),
        schema_version: riglet_format::migrate::CURRENT_SCHEMA_VERSION.into(),
        generator: format!("riglet-inochi2d {}", env!("CARGO_PKG_VERSION")),
        provenance: vec![Provenance {
            source: if options.inx { ProvenanceSource::Inochi2dInx } else { ProvenanceSource::Inochi2dInp },
            source_version: source_version.clone(),
            imported_at: now_iso8601(),
            warnings,
        }],
    };

    // Canvas (section 3): the bounding box of the rest geometry, centered.
    let probe = Document { manifest: manifest(Vec::new()), model: model.clone(), files: files.clone() };
    let rig = Rig::prepare(&probe, limits).map_err(|issues| {
        InochiError::InvalidResult(issues.iter().map(ToString::to_string).collect::<Vec<_>>().join("; "))
    })?;
    let mut out = EvalOutput::new();
    rig.evaluate(&ParamValues::defaults(&rig), &mut out);
    let extent = out
        .drawables
        .iter()
        .flat_map(|d| d.positions.iter())
        .fold([0.0f32; 2], |e, p| [e[0].max(p[0].abs()), e[1].max(p[1].abs())]);
    model.canvas = Canvas { width: (2.0 * extent[0]).ceil().max(1.0), height: (2.0 * extent[1]).ceil().max(1.0) };

    warnings.shrink_to_fit();
    let document = Document { manifest: manifest(warnings.clone()), model, files };
    Ok(Import { document, warnings })
}

/// Animations -> Motions (section 4, package-level).
fn motions(json: &Value, params: &HashMap<u32, ParamInfo>, warnings: &mut Vec<ConversionWarning>) -> Vec<Motion> {
    let Some(anims) = json.get("animations").and_then(Value::as_object) else { return Vec::new() };
    let mut out = Vec::new();
    for (i, (name, a)) in anims.iter().enumerate() {
        let timestep = num(a, "timestep").filter(|t| *t > 0.0).unwrap_or(1.0 / 60.0);
        let length = num(a, "length").unwrap_or(0.0).max(0.0);
        let mut tracks = Vec::new();
        for lane in a.get("lanes").and_then(Value::as_array).into_iter().flatten() {
            let Some(info) = uuid(lane, "uuid").and_then(|u| params.get(&u)) else { continue };
            let component = lane.get("target").and_then(Value::as_u64).unwrap_or(0).min(1) as u8;
            let interp = match string(lane, "interpolation") {
                Some("Nearest") | Some("Stepped") => CurveInterp::Step,
                Some("Cubic") | Some("Bezier") => {
                    warn(
                        warnings,
                        "ANIMATION_CUBIC_APPROXIMATED",
                        None,
                        format!("animation `{name}`: cubic lane approximated as linear"),
                    );
                    CurveInterp::Linear
                }
                _ => CurveInterp::Linear,
            };
            let mut keys: Vec<CurveKey> = Vec::new();
            for k in lane.get("keyframes").and_then(Value::as_array).into_iter().flatten() {
                let (Some(frame), Some(value)) = (num(k, "frame"), num(k, "value")) else { continue };
                let t = frame * timestep;
                if keys.last().is_some_and(|l| l.t >= t) {
                    continue;
                }
                // Lane values are parameter values (to verify on real files).
                keys.push(CurveKey { t, value, interp, handles: None });
            }
            if !keys.is_empty() {
                tracks.push(MotionTrack {
                    target: MotionTarget::Param { param: ParamAxis { parameter: info.id.clone(), component } },
                    keys,
                });
            }
        }
        out.push(Motion {
            id: format!("inochi:anim:{i}"),
            name: name.clone(),
            duration: length * timestep,
            looped: false,
            tracks,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tint_screen_identity_over_a_grid() {
        let steps = [0.0f32, 0.1, 0.25, 0.5, 0.75, 0.9, 1.0];
        for &c in &steps {
            for &m in &steps {
                for &s in &steps {
                    let ([m2, ..], [s2, ..]) = tint_screen_to_riglet([m; 3], [s; 3]);
                    let inochi = (c + s - c * s) * m;
                    let riglet = c * m2 + s2 - c * m2 * s2;
                    assert!((inochi - riglet).abs() < 1e-5, "c={c} m={m} s={s}: {inochi} vs {riglet}");
                    // Inverse (export) restores the Inochi2D values.
                    if s * m < 1.0 - 1e-6 && m > 1e-6 {
                        let ([mb, ..], [sb, ..]) = tint_screen_from_riglet([m2; 3], [s2; 3]);
                        assert!((mb - m).abs() < 1e-4 && (sb - s).abs() < 1e-4, "m={m} s={s}: {mb} {sb}");
                    }
                }
            }
        }
    }

    #[test]
    fn premultiplied_screen_quirk_is_exact_only_when_opaque() {
        // Inochi2D shader: t + S*a - t*S*a (t premultiplied). At a = 1 it is
        // the standard screen; at a = 0.5 it differs from Riglet's R3.
        let (t, s, a) = (0.25f32, 0.5f32, 0.5f32);
        let inochi = t + s * a - t * s * a;
        let standard = t + s * a - t * s;
        assert!((inochi - standard).abs() > 0.01);
        let (t, a) = (0.5f32, 1.0f32);
        assert!(((t + s * a - t * s * a) - (t + s * a - t * s)).abs() < 1e-7);
    }

    #[test]
    fn expand_axis_nearest_ties_go_up() {
        let (keys, src) = expand_axis(&[0.0, 1.0, 3.0], Interp::Nearest);
        assert_eq!(keys, [0.0, 0.5, 1.0, 2.0, 3.0]);
        assert_eq!(src.iter().map(|s| s.0).collect::<Vec<_>>(), [0, 1, 1, 2, 2]);
    }
}
