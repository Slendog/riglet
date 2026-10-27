//! Validation (spec V1–V9) and the prepared, evaluable [`Rig`].
//!
//! [`Rig::prepare`] checks a [`Document`] and resolves it: string IDs become
//! indices, buffers are decoded, nodes are ordered depth-first pre-order,
//! and mesh-deformer bindings (E6) are derived.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;

use crate::buffer::{self, BufferError};
use crate::eval::{self, Bindings, DeformerState, Member};
use crate::geometry;
use crate::limits::Limits;
use crate::migrate::{CURRENT_SCHEMA_VERSION, FORMAT_NAME};
use crate::model::{
    AxisInterpolation, BlendMode, BufferRef, CurveInterp, Id, Keyform, KeyformSet, MaskMode, MaskRef, MotionTarget,
    Node, ParamAxis, ParamKind, Rgb, Vec2,
};
use crate::package::{Document, check_package_path};

/// One validation problem. `code` is stable and safe to match on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    pub code: &'static str,
    pub object: Option<Id>,
    pub message: String,
}

impl fmt::Display for Issue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.object {
            Some(id) => write!(f, "{} [{}]: {}", self.code, id, self.message),
            None => write!(f, "{}: {}", self.code, self.message),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RigParam {
    pub id: Id,
    pub kind: ParamKind,
    pub min: [f32; 2],
    pub max: [f32; 2],
    pub default: [f32; 2],
    pub repeat: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RigAxis {
    pub param: usize,
    pub component: usize,
    pub keys: Vec<f32>,
    pub interpolation: AxisInterpolation,
}

/// A keyform with buffers decoded. Missing properties are zero deltas.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RigKeyform {
    pub positions: Option<Vec<Vec2>>,
    pub opacity: Option<f32>,
    pub draw_order: Option<f32>,
    pub angle: Option<f32>,
    pub origin: Option<Vec2>,
    pub scale: Option<f32>,
    pub multiply_color: Option<Rgb>,
    pub screen_color: Option<Rgb>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RigSet {
    pub axes: Vec<RigAxis>,
    pub keyforms: Vec<RigKeyform>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RigKind {
    Group {
        composite: bool,
        /// Composite layer fields (R6); defaults for non-composites.
        blend_mode: BlendMode,
        masks: Vec<(usize, MaskMode)>,
        draw_order: f32,
    },
    Drawable {
        texture: usize,
        positions: Vec<Vec2>,
        uvs: Vec<Vec2>,
        indices: Vec<u32>,
        blend_mode: BlendMode,
        culling: bool,
        masks: Vec<(usize, MaskMode)>,
        draw_order: f32,
        multiply_color: Rgb,
        screen_color: Rgb,
    },
    Warp {
        rows: u32,
        cols: u32,
        points: Vec<Vec2>,
    },
    Rotation {
        origin: Vec2,
        angle: f32,
        scale: f32,
    },
    MeshDeformer {
        positions: Vec<Vec2>,
        indices: Vec<u32>,
    },
}

impl RigKind {
    /// The node's own point set (E6 binds these), in its parent space.
    pub fn rest_points(&self) -> Option<Vec<Vec2>> {
        match self {
            RigKind::Group { .. } => None,
            RigKind::Drawable { positions, .. } => Some(positions.clone()),
            RigKind::Warp { points, .. } => Some(points.clone()),
            RigKind::Rotation { origin, .. } => Some(vec![*origin]),
            RigKind::MeshDeformer { positions, .. } => Some(positions.clone()),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RigNode {
    pub id: Id,
    pub name: String,
    pub parent: Option<usize>,
    /// Nearest ancestor that is not a group: the node whose local space this
    /// node's points live in. `None` means model space.
    pub owner: Option<usize>,
    pub visible: bool,
    pub opacity: f32,
    pub sets: Vec<RigSet>,
    pub kind: RigKind,
}

/// A validated model ready for evaluation. Node indices follow depth-first
/// pre-order, root first.
#[derive(Debug, Clone, PartialEq)]
pub struct Rig {
    pub params: Vec<RigParam>,
    pub nodes: Vec<RigNode>,
    pub textures: Vec<Id>,
    /// Node indices of every drawable, in pre-order: the slots of
    /// [`crate::EvalOutput::drawables`].
    pub drawables: Vec<usize>,
    /// Node indices of every composite group, in pre-order: the slots of
    /// [`crate::EvalOutput::composites`].
    pub composites: Vec<usize>,
    /// Per node: its drawable or composite slot, if it is a scope member.
    pub(crate) slot: Vec<Option<Member>>,
    /// Per node: composite slot of its nearest composite ancestor (its draw
    /// scope, E4), `None` for the top scope.
    pub(crate) scope: Vec<Option<usize>>,
    param_index: HashMap<Id, usize>,
    node_index: HashMap<Id, usize>,
    pub(crate) bindings: Bindings,
    pub(crate) rest_states: Vec<DeformerState>,
    binding_work: u64,
}

impl Rig {
    pub fn param_index(&self, id: &str) -> Option<usize> {
        self.param_index.get(id).copied()
    }

    pub fn node_index(&self, id: &str) -> Option<usize> {
        self.node_index.get(id).copied()
    }

    /// The drawable or composite slot of node `node` (see
    /// [`crate::EvalOutput`]), if it is a scope member.
    pub fn slot_of(&self, node: usize) -> Option<Member> {
        self.slot.get(node).copied().flatten()
    }

    /// Deepest composite nesting: 0 without composites, 1 for composites
    /// that contain no other composite, and so on.
    pub fn composite_depth(&self) -> usize {
        let mut depth = vec![0usize; self.composites.len()];
        let mut max = 0;
        // Composites are in pre-order, so an enclosing composite's depth is
        // known before its children's.
        for (slot, &node) in self.composites.iter().enumerate() {
            let parent_depth = self.scope.get(node).copied().flatten().and_then(|c| depth.get(c).copied()).unwrap_or(0);
            if let Some(d) = depth.get_mut(slot) {
                *d = parent_depth + 1;
                max = max.max(*d);
            }
        }
        max
    }

    /// Work spent on mesh-deformer binding at load, in the units of
    /// [`Limits::binding_work_budget`]. Deterministic for a given file.
    pub fn binding_work(&self) -> u64 {
        self.binding_work
    }

    /// Validates `doc` against V1–V9 and the limits, and builds the rig.
    /// Returns every issue found when the document is invalid.
    pub fn prepare(doc: &Document, limits: &Limits) -> Result<Rig, Vec<Issue>> {
        let mut v = Validator { doc, limits, issues: Vec::new(), vertices_total: 0, keyform_vertices_total: 0 };
        let rig = v.run();
        match rig {
            Some(rig) if v.issues.is_empty() => Ok(rig),
            _ => Err(v.issues),
        }
    }
}

type Scopes = (Vec<usize>, Vec<usize>, Vec<Option<Member>>, Vec<Option<usize>>);

/// Drawable and composite slots, and each node's draw scope (E4). Nodes are
/// in pre-order, so a parent's scope is known before its children.
fn scopes(nodes: &[RigNode]) -> Scopes {
    let (mut drawables, mut composites) = (Vec::new(), Vec::new());
    let mut slot: Vec<Option<Member>> = Vec::with_capacity(nodes.len());
    let mut scope: Vec<Option<usize>> = Vec::with_capacity(nodes.len());
    for (i, node) in nodes.iter().enumerate() {
        // A composite parent is the scope of its children.
        let inherited = node.parent.and_then(|p| match slot.get(p) {
            Some(Some(Member::Composite(c))) => Some(*c),
            _ => scope.get(p).copied().flatten(),
        });
        scope.push(inherited);
        slot.push(match node.kind {
            RigKind::Drawable { .. } => {
                drawables.push(i);
                Some(Member::Drawable(drawables.len() - 1))
            }
            RigKind::Group { composite: true, .. } => {
                composites.push(i);
                Some(Member::Composite(composites.len() - 1))
            }
            _ => None,
        });
    }
    (drawables, composites, slot, scope)
}

/// Runs all checks and returns the issues (empty when valid).
pub fn validate(doc: &Document, limits: &Limits) -> Vec<Issue> {
    match Rig::prepare(doc, limits) {
        Ok(_) => Vec::new(),
        Err(issues) => issues,
    }
}

struct Validator<'a> {
    doc: &'a Document,
    limits: &'a Limits,
    issues: Vec<Issue>,
    vertices_total: u64,
    keyform_vertices_total: u64,
}

fn finite(values: &[f32]) -> bool {
    values.iter().all(|v| v.is_finite())
}

/// Properties a keyform may animate per node type (V5).
struct Allowed {
    positions: bool,
    draw_order: bool,
    rotation: bool,
    colors: bool,
}

fn allowed(node: &Node) -> Allowed {
    let none = Allowed { positions: false, draw_order: false, rotation: false, colors: false };
    match node {
        // Only composites have a draw order (V2).
        Node::Group(g) => Allowed { draw_order: g.composite, ..none },
        Node::Drawable(_) => Allowed { positions: true, draw_order: true, colors: true, ..none },
        Node::Warp(_) | Node::MeshDeformer(_) => Allowed { positions: true, ..none },
        Node::Rotation(_) => Allowed { rotation: true, ..none },
    }
}

const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

fn png_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.get(0..8)? != PNG_SIGNATURE || bytes.get(12..16)? != b"IHDR" {
        return None;
    }
    let w = u32::from_be_bytes(bytes.get(16..20)?.try_into().ok()?);
    let h = u32::from_be_bytes(bytes.get(20..24)?.try_into().ok()?);
    Some((w, h))
}

impl Validator<'_> {
    fn issue(&mut self, code: &'static str, object: Option<&str>, message: impl Into<String>) {
        self.issues.push(Issue { code, object: object.map(str::to_owned), message: message.into() });
    }

    fn run(&mut self) -> Option<Rig> {
        self.check_package();
        let (params, param_index) = self.check_params();
        let textures = self.check_textures();
        let texture_index: HashMap<&str, usize> = textures.iter().enumerate().map(|(i, id)| (id.as_str(), i)).collect();
        let order = self.check_tree()?;
        let node_index: HashMap<Id, usize> = order.iter().enumerate().map(|(i, id)| (id.clone(), i)).collect();

        let mut nodes = Vec::with_capacity(order.len());
        for id in &order {
            let node = self.doc.model.nodes.get(id)?;
            nodes.push(self.build_node(node, &node_index, &nodes, &params, &param_index, &texture_index));
        }
        let nodes: Option<Vec<RigNode>> = nodes.into_iter().collect();
        self.check_masks(&node_index);
        self.check_physics(&params, &param_index);
        self.check_motions(&params, &param_index, &node_index);
        self.check_export_ids(&param_index, &node_index);
        let nodes = nodes?;
        if !self.issues.is_empty() {
            return None;
        }
        let rest_states = eval::rest_states(&nodes);
        let Ok((bindings, binding_work)) =
            eval::compute_bindings(&nodes, &rest_states, self.limits.binding_work_budget)
        else {
            self.issue(
                "LIMIT_MESH_BINDING",
                None,
                format!(
                    "mesh-deformer binding exceeds this loader's work budget of {} \
                     (a resource limit, not a format rule)",
                    self.limits.binding_work_budget
                ),
            );
            return None;
        };
        let (drawables, composites, slot, scope) = scopes(&nodes);
        Some(Rig {
            params,
            nodes,
            textures,
            drawables,
            composites,
            slot,
            scope,
            param_index,
            node_index,
            bindings,
            rest_states,
            binding_work,
        })
    }

    // ----- V1 -----

    fn check_package(&mut self) {
        let m = &self.doc.manifest;
        if m.format != FORMAT_NAME {
            self.issue("V1_FORMAT", None, format!("manifest format is `{}`, expected `{FORMAT_NAME}`", m.format));
        }
        if m.schema_version != CURRENT_SCHEMA_VERSION {
            self.issue(
                "V1_SCHEMA_VERSION",
                None,
                format!("schemaVersion `{}` is not the current `{CURRENT_SCHEMA_VERSION}`", m.schema_version),
            );
        }
        if self.doc.files.len() as u64 + 2 > self.limits.zip_max_entries {
            self.issue("V1_TOO_MANY_ENTRIES", None, "too many package entries");
        }
        let mut folded = HashSet::new();
        let mut total: u64 = 0;
        for (name, data) in &self.doc.files {
            if let Err(reason) = check_package_path(name) {
                self.issue("V1_BAD_PATH", None, format!("`{name}`: {reason}"));
            }
            if !folded.insert(name.trim_end_matches('/').to_lowercase()) {
                self.issue("V1_CASE_COLLISION", None, format!("`{name}` differs from another entry only by case"));
            }
            if data.len() as u64 > self.limits.zip_max_entry_uncompressed {
                self.issue("V1_ENTRY_TOO_LARGE", None, format!("`{name}` is too large"));
            }
            total = total.saturating_add(data.len() as u64);
        }
        if total > self.limits.zip_max_total_uncompressed {
            self.issue("V1_TOTAL_TOO_LARGE", None, "package is too large");
        }
        if !self.doc.model.canvas.width.is_finite() || !self.doc.model.canvas.height.is_finite() {
            self.issue("V8_NON_FINITE", None, "canvas size is not finite");
        }
    }

    // ----- V4 -----

    fn check_params(&mut self) -> (Vec<RigParam>, HashMap<Id, usize>) {
        let mut params = Vec::new();
        let mut index = HashMap::new();
        for p in &self.doc.model.parameters {
            let n = p.kind.components();
            let id = Some(p.id.as_str());
            if index.contains_key(&p.id) {
                self.issue("V3_DUPLICATE_ID", id, "duplicate parameter id");
                continue;
            }
            if p.min.len() != n || p.max.len() != n || p.default.len() != n {
                self.issue("V4_PARAM_LENGTH", id, format!("min/max/default must have {n} values"));
                continue;
            }
            if !finite(&p.min) || !finite(&p.max) || !finite(&p.default) {
                self.issue("V8_NON_FINITE", id, "parameter range is not finite");
                continue;
            }
            let mut rp = RigParam {
                id: p.id.clone(),
                kind: p.kind,
                min: [0.0; 2],
                max: [0.0; 2],
                default: [0.0; 2],
                repeat: p.repeat.unwrap_or(false),
            };
            let mut ok = true;
            for c in 0..n {
                let (lo, hi, d) = (p.min.get(c), p.max.get(c), p.default.get(c));
                let (Some(&lo), Some(&hi), Some(&d)) = (lo, hi, d) else { continue };
                if lo >= hi {
                    self.issue("V4_PARAM_RANGE", id, "min must be less than max");
                    ok = false;
                } else if d < lo || d > hi {
                    self.issue("V4_PARAM_DEFAULT", id, "default is outside [min, max]");
                    ok = false;
                }
                if let (Some(a), Some(b), Some(e)) = (rp.min.get_mut(c), rp.max.get_mut(c), rp.default.get_mut(c)) {
                    (*a, *b, *e) = (lo, hi, d);
                }
            }
            if ok {
                index.insert(p.id.clone(), params.len());
                params.push(rp);
            }
        }
        (params, index)
    }

    fn resolve_axis(
        &mut self,
        axis: &ParamAxis,
        params: &[RigParam],
        index: &HashMap<Id, usize>,
        object: &str,
    ) -> Option<(usize, usize)> {
        let Some(&p) = index.get(&axis.parameter) else {
            self.issue("V3_UNRESOLVED", Some(object), format!("unknown parameter `{}`", axis.parameter));
            return None;
        };
        let components = params.get(p).map_or(1, |rp| rp.kind.components());
        if usize::from(axis.component) >= components {
            self.issue(
                "V4_PARAM_COMPONENT",
                Some(object),
                format!("component {} is not valid for parameter `{}`", axis.component, axis.parameter),
            );
            return None;
        }
        Some((p, usize::from(axis.component)))
    }

    // ----- textures (V3, Limits) -----

    fn check_textures(&mut self) -> Vec<Id> {
        let mut ids = Vec::new();
        let mut seen = HashSet::new();
        for t in &self.doc.model.textures {
            let id = Some(t.id.as_str());
            if !seen.insert(t.id.clone()) {
                self.issue("V3_DUPLICATE_ID", id, "duplicate texture id");
                continue;
            }
            ids.push(t.id.clone());
            let max = self.limits.max_texture_dimension;
            if t.width > max || t.height > max {
                self.issue("LIMIT_TEXTURE_SIZE", id, format!("declared size exceeds {max} pixels"));
            }
            if let Err(reason) = check_package_path(&t.file) {
                self.issue("V1_BAD_PATH", id, format!("`{}`: {reason}", t.file));
                continue;
            }
            match self.doc.files.get(&t.file) {
                None => self.issue("V3_UNRESOLVED", id, format!("texture file `{}` is missing", t.file)),
                Some(bytes) => match png_dimensions(bytes) {
                    None => self.issue("V3_TEXTURE_NOT_PNG", id, format!("`{}` is not a PNG file", t.file)),
                    Some((w, h)) if w > max || h > max => {
                        self.issue("LIMIT_TEXTURE_SIZE", id, format!("image is {w}x{h}, limit is {max}"))
                    }
                    Some(_) => {}
                },
            }
        }
        ids
    }

    // ----- V2 -----

    /// Checks the tree and returns node IDs in depth-first pre-order.
    fn check_tree(&mut self) -> Option<Vec<Id>> {
        let nodes = &self.doc.model.nodes;
        let before = self.issues.len();
        if nodes.len() as u64 > self.limits.max_nodes {
            self.issue("LIMIT_NODES", None, format!("more than {} nodes", self.limits.max_nodes));
            return None;
        }
        for (key, node) in nodes {
            if &node.base().id != key {
                self.issue("V2_ID_MISMATCH", Some(key), "node key differs from its id");
            }
        }
        let root = &self.doc.model.root;
        match nodes.get(root) {
            None => self.issue("V2_ROOT", Some(root), "root node does not exist"),
            Some(Node::Group(_)) => {}
            Some(_) => self.issue("V2_ROOT", Some(root), "root node is not a group"),
        }
        let roots: Vec<&Id> = nodes.iter().filter(|(_, n)| n.base().parent.is_none()).map(|(k, _)| k).collect();
        if roots.len() != 1 || roots.first() != Some(&root) {
            self.issue("V2_ROOT", None, "exactly one node, the root, must have parent null");
        }

        let mut issues = Vec::new();
        for (key, node) in nodes {
            let base = node.base();
            if let Some(parent) = &base.parent {
                match nodes.get(parent) {
                    None => issues.push(("V3_UNRESOLVED", key.clone(), format!("parent `{parent}` does not exist"))),
                    Some(p) if !p.base().children.contains(key) => {
                        issues.push(("V2_PARENT_CHILDREN", key.clone(), format!("not listed in `{parent}` children")))
                    }
                    Some(Node::Drawable(_)) => {
                        issues.push(("V2_PARENT_TYPE", key.clone(), "a drawable cannot have children".to_owned()))
                    }
                    Some(_) => {}
                }
            }
            let mut seen = HashSet::new();
            for child in &base.children {
                if !seen.insert(child) {
                    issues.push(("V2_PARENT_CHILDREN", key.clone(), format!("child `{child}` is listed twice")));
                    continue;
                }
                match nodes.get(child) {
                    None => issues.push(("V3_UNRESOLVED", key.clone(), format!("child `{child}` does not exist"))),
                    Some(c) if c.base().parent.as_ref() != Some(key) => issues.push((
                        "V2_PARENT_CHILDREN",
                        key.clone(),
                        format!("child `{child}` has a different parent"),
                    )),
                    Some(_) => {}
                }
            }
        }
        for (code, key, msg) in issues {
            self.issue(code, Some(&key), msg);
        }
        if self.issues.len() != before {
            return None;
        }

        // Iterative DFS: deep trees must not overflow the stack.
        let mut order = Vec::with_capacity(nodes.len());
        let mut visited = HashSet::new();
        let mut stack = vec![root.clone()];
        while let Some(id) = stack.pop() {
            if !visited.insert(id.clone()) {
                self.issue("V2_CYCLE", Some(&id), "node is reachable twice");
                return None;
            }
            if let Some(node) = nodes.get(&id) {
                stack.extend(node.base().children.iter().rev().cloned());
            }
            order.push(id);
        }
        if order.len() != nodes.len() {
            self.issue("V2_UNREACHABLE", None, "some nodes are not reachable from the root (cycle?)");
            return None;
        }
        Some(order)
    }

    // ----- per node: V5, V6, V7, V8 -----

    fn vec2s(&mut self, r: &BufferRef, object: &str, what: &str) -> Option<Vec<Vec2>> {
        let max = self.limits.max_vertices_per_mesh;
        match buffer::read_vec2(&self.doc.files, r, max) {
            Ok(v) => Some(v),
            Err(e) => {
                self.buffer_issue(e, object, what);
                None
            }
        }
    }

    fn buffer_issue(&mut self, e: BufferError, object: &str, what: &str) {
        let code = match e {
            BufferError::TooLarge { .. } => "LIMIT_VERTICES",
            BufferError::NonFinite => "V8_NON_FINITE",
            BufferError::MissingFile(_) => "V3_UNRESOLVED",
            _ => "V6_BUFFER",
        };
        self.issue(code, Some(object), format!("{what}: {e}"));
    }

    fn count_vertices(&mut self, n: usize, object: &str) -> bool {
        self.vertices_total = self.vertices_total.saturating_add(n as u64);
        if self.vertices_total > self.limits.max_vertices_total {
            self.issue("LIMIT_VERTICES", Some(object), "total vertex count exceeds the limit");
            return false;
        }
        true
    }

    fn indices(&mut self, r: &BufferRef, vertex_count: usize, object: &str) -> Option<Vec<u32>> {
        let max = self.limits.max_vertices_per_mesh.saturating_mul(6);
        let indices = match buffer::read_indices(&self.doc.files, r, max) {
            Ok(i) => i,
            Err(e) => {
                self.buffer_issue(e, object, "indices");
                return None;
            }
        };
        if indices.len() % 3 != 0 {
            self.issue("V6_INDICES", Some(object), "index count is not a multiple of 3");
            return None;
        }
        if indices.iter().any(|&i| i as usize >= vertex_count) {
            self.issue("V6_INDICES", Some(object), "index out of range");
            return None;
        }
        Some(indices)
    }

    #[allow(clippy::too_many_arguments)]
    fn build_node(
        &mut self,
        node: &Node,
        node_index: &HashMap<Id, usize>,
        built: &[Option<RigNode>],
        params: &[RigParam],
        param_index: &HashMap<Id, usize>,
        texture_index: &HashMap<&str, usize>,
    ) -> Option<RigNode> {
        let base = node.base();
        let id = base.id.as_str();
        let parent = base.parent.as_ref().and_then(|p| node_index.get(p).copied());
        // Parents precede children in pre-order, so the parent is built.
        let owner = parent.and_then(|p| match built.get(p) {
            Some(Some(pn)) => match pn.kind {
                RigKind::Group { .. } => pn.owner,
                _ => Some(p),
            },
            _ => None,
        });

        let opacity = node.rest_opacity();
        if !opacity.is_finite() {
            self.issue("V8_NON_FINITE", Some(id), "rest opacity is not finite");
        }

        let kind = match node {
            Node::Group(g) => {
                let draw_order = g.rest.draw_order.unwrap_or(0.0);
                if !draw_order.is_finite() {
                    self.issue("V8_NON_FINITE", Some(id), "rest drawOrder is not finite");
                }
                if !g.composite && (g.blend_mode.is_some() || g.masks.is_some() || g.rest.draw_order.is_some()) {
                    self.issue(
                        "V2_COMPOSITE_FIELD",
                        Some(id),
                        "blendMode, masks and rest.drawOrder are only allowed on composite groups",
                    );
                }
                // Masks are checked in check_masks once every node is known.
                let masks =
                    g.masks.iter().flatten().filter_map(|m| node_index.get(&m.source).map(|&i| (i, m.mode))).collect();
                Some(RigKind::Group {
                    composite: g.composite,
                    blend_mode: g.blend_mode.unwrap_or(BlendMode::Normal),
                    masks,
                    draw_order,
                })
            }
            Node::Drawable(d) => {
                let positions = self.vec2s(&d.mesh.positions, id, "positions");
                let uvs = self.vec2s(&d.mesh.uvs, id, "uvs");
                let mut ok = true;
                if let (Some(p), Some(u)) = (&positions, &uvs)
                    && p.len() != u.len()
                {
                    self.issue("V6_UVS", Some(id), "uvs count differs from positions count");
                    ok = false;
                }
                let texture = texture_index.get(d.texture.as_str()).copied();
                if texture.is_none() {
                    self.issue("V3_UNRESOLVED", Some(id), format!("unknown texture `{}`", d.texture));
                }
                let r = &d.rest;
                if !opacity.is_finite()
                    || !r.draw_order.is_finite()
                    || !finite(&r.multiply_color)
                    || !finite(&r.screen_color)
                {
                    self.issue("V8_NON_FINITE", Some(id), "rest values are not finite");
                    ok = false;
                }
                let positions = positions?;
                ok &= self.count_vertices(positions.len(), id);
                let indices = self.indices(&d.mesh.indices, positions.len(), id);
                // Masks are checked in check_masks once every node is known.
                let masks = d.masks.iter().filter_map(|m| node_index.get(&m.source).map(|&i| (i, m.mode))).collect();
                match (ok, uvs, indices, texture) {
                    (true, Some(uvs), Some(indices), Some(texture)) => Some(RigKind::Drawable {
                        texture,
                        positions,
                        uvs,
                        indices,
                        blend_mode: d.blend_mode,
                        culling: d.culling,
                        masks,
                        draw_order: r.draw_order,
                        multiply_color: r.multiply_color,
                        screen_color: r.screen_color,
                    }),
                    _ => None,
                }
            }
            Node::Warp(w) => {
                if w.rows == 0 || w.cols == 0 {
                    self.issue("V7_WARP_GRID", Some(id), "rows and cols must be at least 1");
                    return None;
                }
                let expected = (u64::from(w.rows) + 1) * (u64::from(w.cols) + 1);
                if expected > self.limits.max_vertices_per_mesh {
                    self.issue("LIMIT_VERTICES", Some(id), "warp grid exceeds the vertex limit");
                    return None;
                }
                let points = self.vec2s(&w.rest.points, id, "warp points")?;
                if points.len() as u64 != expected {
                    self.issue("V7_WARP_GRID", Some(id), format!("expected {expected} control points"));
                    return None;
                }
                if !self.count_vertices(points.len(), id) {
                    return None;
                }
                Some(RigKind::Warp { rows: w.rows, cols: w.cols, points })
            }
            Node::Rotation(r) => {
                let rest = &r.rest;
                if !finite(&rest.origin) || !rest.angle.is_finite() || !rest.scale.is_finite() {
                    self.issue("V8_NON_FINITE", Some(id), "rest values are not finite");
                    return None;
                }
                Some(RigKind::Rotation { origin: rest.origin, angle: rest.angle, scale: rest.scale })
            }
            Node::MeshDeformer(m) => {
                let positions = self.vec2s(&m.mesh.positions, id, "positions")?;
                if !self.count_vertices(positions.len(), id) {
                    return None;
                }
                let indices = self.indices(&m.mesh.indices, positions.len(), id)?;
                for tri in 0..indices.len() / 3 {
                    let area = geometry::doubled_area(geometry::triangle(&positions, &indices, tri)) / 2.0;
                    if area.abs() < 1e-9 {
                        self.issue("V7_ZERO_AREA", Some(id), format!("rest triangle {tri} has zero area"));
                        return None;
                    }
                }
                Some(RigKind::MeshDeformer { positions, indices })
            }
        };

        let rest_count = kind.as_ref().and_then(RigKind::rest_points).map(|p| p.len());
        let allowed = allowed(node);
        let mut sets = Vec::with_capacity(base.keyform_sets.len());
        for set in &base.keyform_sets {
            if let Some(s) = self.build_set(set, id, &allowed, rest_count, params, param_index) {
                sets.push(s);
            }
        }

        Some(RigNode {
            id: base.id.clone(),
            name: base.name.clone(),
            parent,
            owner,
            visible: base.visible,
            opacity,
            sets,
            kind: kind?,
        })
    }

    fn build_set(
        &mut self,
        set: &KeyformSet,
        id: &str,
        allowed: &Allowed,
        rest_count: Option<usize>,
        params: &[RigParam],
        param_index: &HashMap<Id, usize>,
    ) -> Option<RigSet> {
        if set.axes.is_empty() {
            self.issue("V5_NO_AXES", Some(id), "keyform set has no axes");
            return None;
        }
        let mut axes = Vec::with_capacity(set.axes.len());
        let mut seen = HashSet::new();
        let mut grid: u64 = 1;
        for axis in &set.axes {
            let (param, component) = self.resolve_axis(&axis.param, params, param_index, id)?;
            if !seen.insert((param, component)) {
                self.issue("V5_DUPLICATE_AXIS", Some(id), "a parameter axis appears twice in one set");
                return None;
            }
            if axis.keys.is_empty() {
                self.issue("V5_KEYS", Some(id), "axis has no keys");
                return None;
            }
            if !finite(&axis.keys) {
                self.issue("V8_NON_FINITE", Some(id), "axis keys are not finite");
                return None;
            }
            if axis.keys.windows(2).any(|w| matches!(w, [a, b] if a >= b)) {
                self.issue("V5_KEYS", Some(id), "axis keys are not strictly ascending");
                return None;
            }
            grid = grid.saturating_mul(axis.keys.len() as u64);
            if grid > self.limits.max_keyform_grid {
                self.issue("LIMIT_KEYFORM_GRID", Some(id), "keyform grid exceeds the limit");
                return None;
            }
            axes.push(RigAxis { param, component, keys: axis.keys.clone(), interpolation: axis.interpolation });
        }
        if set.keyforms.len() as u64 != grid {
            self.issue("V5_KEYFORM_COUNT", Some(id), format!("{} keyforms, the grid needs {grid}", set.keyforms.len()));
            return None;
        }
        let mut keyforms = Vec::with_capacity(set.keyforms.len());
        for kf in &set.keyforms {
            keyforms.push(self.build_keyform(kf, id, allowed, rest_count)?);
        }
        Some(RigSet { axes, keyforms })
    }

    fn build_keyform(
        &mut self,
        kf: &Keyform,
        id: &str,
        allowed: &Allowed,
        rest_count: Option<usize>,
    ) -> Option<RigKeyform> {
        let bad_property = (kf.positions.is_some() && !allowed.positions)
            || (kf.draw_order.is_some() && !allowed.draw_order)
            || ((kf.angle.is_some() || kf.origin.is_some() || kf.scale.is_some()) && !allowed.rotation)
            || ((kf.multiply_color.is_some() || kf.screen_color.is_some()) && !allowed.colors);
        if bad_property {
            self.issue("V5_PROPERTY", Some(id), "keyform animates a property this node type does not have");
            return None;
        }
        let scalars = [kf.opacity, kf.draw_order, kf.angle, kf.scale];
        let vectors = [kf.origin.as_ref().map(|v| &v[..]), kf.multiply_color.as_ref().map(|v| &v[..])];
        if scalars.iter().flatten().any(|v| !v.is_finite())
            || vectors.iter().flatten().any(|v| !finite(v))
            || kf.screen_color.is_some_and(|c| !finite(&c))
        {
            self.issue("V8_NON_FINITE", Some(id), "keyform value is not finite");
            return None;
        }
        let positions = match &kf.positions {
            None => None,
            Some(r) => {
                let expected = rest_count.unwrap_or(0);
                if r.count != expected as u64 {
                    self.issue(
                        "V5_POSITIONS_COUNT",
                        Some(id),
                        format!("keyform has {} positions, rest has {expected}", r.count),
                    );
                    return None;
                }
                // Checked before decoding: many keyforms may point at the
                // same bytes, so decoded size is not bounded by file size.
                self.keyform_vertices_total = self.keyform_vertices_total.saturating_add(r.count);
                if self.keyform_vertices_total > self.limits.max_keyform_vertices_total {
                    self.issue("LIMIT_KEYFORM_VERTICES", Some(id), "total keyform vertex count exceeds the limit");
                    return None;
                }
                Some(self.vec2s(r, id, "keyform positions")?)
            }
        };
        Some(RigKeyform {
            positions,
            opacity: kf.opacity,
            draw_order: kf.draw_order,
            angle: kf.angle,
            origin: kf.origin,
            scale: kf.scale,
            multiply_color: kf.multiply_color,
            screen_color: kf.screen_color,
        })
    }

    // ----- V3 masks -----

    fn check_masks(&mut self, node_index: &HashMap<Id, usize>) {
        let nodes = &self.doc.model.nodes;
        let mut graph: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        let mut issues = Vec::new();
        for (key, node) in nodes {
            let masks: &[MaskRef] = match node {
                Node::Drawable(d) => &d.masks,
                Node::Group(g) => g.masks.as_deref().unwrap_or(&[]),
                _ => continue,
            };
            for m in masks {
                if !node_index.contains_key(&m.source) {
                    issues.push(("V3_UNRESOLVED", format!("unknown mask source `{}`", m.source), key));
                } else if &m.source == key {
                    issues.push(("V3_MASK_SELF", "a node cannot mask itself".to_owned(), key));
                } else if !matches!(nodes.get(&m.source), Some(Node::Drawable(_))) {
                    issues.push(("V3_MASK_SOURCE", format!("mask source `{}` is not a drawable", m.source), key));
                } else {
                    graph.entry(key.as_str()).or_default().push(m.source.as_str());
                }
            }
        }
        for (code, msg, key) in issues {
            self.issue(code, Some(key), msg);
        }

        // Iterative three-color DFS for cycle detection.
        let mut state: HashMap<&str, u8> = HashMap::new(); // 1 = on stack, 2 = done
        for &start in graph.keys() {
            if state.contains_key(start) {
                continue;
            }
            let mut stack: Vec<(&str, usize)> = vec![(start, 0)];
            state.insert(start, 1);
            while let Some(&mut (node, ref mut next)) = stack.last_mut() {
                let edges = graph.get(node).map_or(&[][..], Vec::as_slice);
                if let Some(&to) = edges.get(*next) {
                    *next += 1;
                    match state.get(to) {
                        Some(1) => {
                            self.issue("V3_MASK_CYCLE", Some(to), "masks form a cycle");
                            return;
                        }
                        Some(_) => {}
                        None => {
                            state.insert(to, 1);
                            stack.push((to, 0));
                        }
                    }
                } else {
                    state.insert(node, 2);
                    stack.pop();
                }
            }
        }
    }

    // ----- V3 physics -----

    fn check_physics(&mut self, params: &[RigParam], param_index: &HashMap<Id, usize>) {
        for rig in &self.doc.model.physics {
            let id = rig.id.as_str();
            for input in &rig.inputs {
                self.resolve_axis(&input.param, params, param_index, id);
                if !input.weight.is_finite() {
                    self.issue("V8_NON_FINITE", Some(id), "input weight is not finite");
                }
            }
            for p in &rig.pendulum {
                if !finite(&p.position) || !finite(&[p.mobility, p.delay, p.acceleration, p.radius]) {
                    self.issue("V8_NON_FINITE", Some(id), "pendulum value is not finite");
                }
            }
            for out in &rig.outputs {
                self.resolve_axis(&out.param, params, param_index, id);
                if out.pendulum_index as usize >= rig.pendulum.len() {
                    self.issue("V3_PENDULUM_INDEX", Some(id), "output pendulumIndex is out of range");
                }
                if !out.scale.is_finite() || !out.weight.is_finite() {
                    self.issue("V8_NON_FINITE", Some(id), "output value is not finite");
                }
            }
            let n = &rig.normalization;
            if !finite(&[n.position.min, n.position.max, n.position.default, n.angle.min, n.angle.max, n.angle.default])
            {
                self.issue("V8_NON_FINITE", Some(id), "normalization is not finite");
            }
        }
        let env = &self.doc.model.physics_environment;
        if !finite(&env.gravity) || !finite(&env.wind) {
            self.issue("V8_NON_FINITE", None, "physics environment is not finite");
        }
    }

    // ----- V9 -----

    fn check_motions(&mut self, params: &[RigParam], param_index: &HashMap<Id, usize>, nodes: &HashMap<Id, usize>) {
        let mut seen = HashSet::new();
        for motion in &self.doc.model.motions {
            let id = motion.id.as_str();
            if !seen.insert(id) {
                self.issue("V3_DUPLICATE_ID", Some(id), "duplicate motion id");
            }
            if !motion.duration.is_finite() || motion.duration < 0.0 {
                self.issue("V8_NON_FINITE", Some(id), "duration must be finite and >= 0");
            }
            for track in &motion.tracks {
                match &track.target {
                    MotionTarget::Param { param } => {
                        self.resolve_axis(param, params, param_index, id);
                    }
                    MotionTarget::Node { node, .. } => {
                        if !nodes.contains_key(node) {
                            self.issue("V3_UNRESOLVED", Some(id), format!("unknown node `{node}`"));
                        }
                    }
                }
                for key in &track.keys {
                    let handles_ok = key.handles.is_some() == (key.interp == CurveInterp::Bezier);
                    if !handles_ok {
                        self.issue("V9_HANDLES", Some(id), "handles must be present exactly for bezier keys");
                    }
                    let handles_finite = key.handles.is_none_or(|[a, b]| finite(&a) && finite(&b));
                    if !key.t.is_finite() || !key.value.is_finite() || !handles_finite {
                        self.issue("V8_NON_FINITE", Some(id), "curve key is not finite");
                    }
                }
                if track.keys.windows(2).any(|w| matches!(w, [a, b] if a.t >= b.t)) {
                    self.issue("V9_KEY_ORDER", Some(id), "curve keys are not strictly ascending in t");
                }
            }
        }
    }

    fn check_export_ids(&mut self, params: &HashMap<Id, usize>, nodes: &HashMap<Id, usize>) {
        let ids = &self.doc.model.export_ids;
        let keys = ids.live2d.iter().flat_map(|m| m.keys()).chain(ids.inochi2d.iter().flat_map(|m| m.keys()));
        let unknown: Vec<Id> = keys.filter(|k| !params.contains_key(*k) && !nodes.contains_key(*k)).cloned().collect();
        for k in unknown {
            self.issue("V3_UNRESOLVED", Some(&k), "export id refers to an unknown parameter or node");
        }
    }
}
