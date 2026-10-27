//! Serde types for `manifest.json` and `model.json`, mirroring
//! `docs/format/riglet-format-v0.4.ts` one to one.
//!
//! These types are the on-disk shape only. They are not validated; use
//! [`crate::Rig::prepare`] to validate a document and get an evaluable rig.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub type Id = String;
pub type Vec2 = [f32; 2];
pub type Rgb = [f32; 3];

/// Namespaced bag for target-specific data ("live2d", "inochi2d"),
/// preserved untouched on round-trip.
pub type Extensions = serde_json::Map<String, serde_json::Value>;

// ---------- Package-level ----------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    pub format: String,
    pub schema_version: String,
    pub generator: String,
    pub provenance: Vec<Provenance>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProvenanceSource {
    #[serde(rename = "native")]
    Native,
    #[serde(rename = "live2d-moc3")]
    Live2dMoc3,
    #[serde(rename = "inochi2d-inp")]
    Inochi2dInp,
    #[serde(rename = "inochi2d-inx")]
    Inochi2dInx,
    #[serde(rename = "psd")]
    Psd,
    #[serde(rename = "kra")]
    Kra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Provenance {
    pub source: ProvenanceSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_version: Option<String>,
    pub imported_at: String,
    pub warnings: Vec<ConversionWarning>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversionWarning {
    pub code: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub object_id: Option<Id>,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ElementType {
    F32,
    U16,
    U32,
}

impl ElementType {
    pub fn size(self) -> u64 {
        match self {
            ElementType::F32 | ElementType::U32 => 4,
            ElementType::U16 => 2,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BufferRef {
    pub file: String,
    pub byte_offset: u64,
    pub count: u64,
    #[serde(rename = "type")]
    pub element_type: ElementType,
    pub components: u8,
}

// ---------- Model ----------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Model {
    pub canvas: Canvas,
    pub textures: Vec<Texture>,
    pub parameters: Vec<Parameter>,
    pub root: Id,
    pub nodes: BTreeMap<Id, Node>,
    pub physics: Vec<PhysicsRig>,
    pub physics_environment: PhysicsEnvironment,
    pub motions: Vec<Motion>,
    pub export_ids: ExportIdMap,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extensions: Option<Extensions>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Canvas {
    pub width: f32,
    pub height: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Texture {
    pub id: Id,
    pub file: String,
    pub width: u32,
    pub height: u32,
    pub premultiplied_alpha: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ExportIdMap {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub live2d: Option<BTreeMap<Id, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inochi2d: Option<BTreeMap<Id, u32>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PhysicsEnvironment {
    pub gravity: Vec2,
    pub wind: Vec2,
}

// ---------- Parameters ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ParamKind {
    #[serde(rename = "1d")]
    OneD,
    #[serde(rename = "2d")]
    TwoD,
}

impl ParamKind {
    pub fn components(self) -> usize {
        match self {
            ParamKind::OneD => 1,
            ParamKind::TwoD => 2,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Parameter {
    pub id: Id,
    pub name: String,
    pub kind: ParamKind,
    pub min: Vec<f32>,
    pub max: Vec<f32>,
    pub default: Vec<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repeat: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extensions: Option<Extensions>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ParamAxis {
    pub parameter: Id,
    pub component: u8,
}

// ---------- Node tree ----------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Node {
    #[serde(rename = "group")]
    Group(GroupNode),
    #[serde(rename = "drawable")]
    Drawable(DrawableNode),
    #[serde(rename = "warp")]
    Warp(WarpDeformerNode),
    #[serde(rename = "rotation")]
    Rotation(RotationDeformerNode),
    #[serde(rename = "meshDeformer")]
    MeshDeformer(MeshDeformerNode),
}

impl Node {
    pub fn base(&self) -> &NodeBase {
        match self {
            Node::Group(n) => &n.base,
            Node::Drawable(n) => &n.base,
            Node::Warp(n) => &n.base,
            Node::Rotation(n) => &n.base,
            Node::MeshDeformer(n) => &n.base,
        }
    }

    pub fn base_mut(&mut self) -> &mut NodeBase {
        match self {
            Node::Group(n) => &mut n.base,
            Node::Drawable(n) => &mut n.base,
            Node::Warp(n) => &mut n.base,
            Node::Rotation(n) => &mut n.base,
            Node::MeshDeformer(n) => &mut n.base,
        }
    }

    pub fn type_name(&self) -> &'static str {
        match self {
            Node::Group(_) => "group",
            Node::Drawable(_) => "drawable",
            Node::Warp(_) => "warp",
            Node::Rotation(_) => "rotation",
            Node::MeshDeformer(_) => "meshDeformer",
        }
    }

    pub fn rest_opacity(&self) -> f32 {
        match self {
            Node::Group(n) => n.rest.opacity,
            Node::Drawable(n) => n.rest.opacity,
            Node::Warp(n) => n.rest.opacity,
            Node::Rotation(n) => n.rest.opacity,
            Node::MeshDeformer(n) => n.rest.opacity,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeBase {
    pub id: Id,
    pub name: String,
    pub parent: Option<Id>,
    pub children: Vec<Id>,
    pub visible: bool,
    pub keyform_sets: Vec<KeyformSet>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extensions: Option<Extensions>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupNode {
    #[serde(flatten)]
    pub base: NodeBase,
    pub composite: bool,
    /// Composites only (V2); `None` means normal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blend_mode: Option<BlendMode>,
    /// Composites only (V2); masks the finished layer (R5).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub masks: Option<Vec<MaskRef>>,
    pub rest: GroupRest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupRest {
    pub opacity: f32,
    /// Composites only (V2); `None` means 0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draw_order: Option<f32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpacityRest {
    pub opacity: f32,
}

/// R4. Group (a) modes (screen .. sliceFromLower) are Inochi2D's
/// fixed-function ones (its legacy GL path).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BlendMode {
    Normal,
    Additive,
    Multiply,
    Screen,
    Lighten,
    ColorDodge,
    AddGlow,
    Subtract,
    Exclusion,
    Inverse,
    DestinationIn,
    ClipToLower,
    SliceFromLower,
}

impl BlendMode {
    pub const ALL: [BlendMode; 13] = [
        BlendMode::Normal,
        BlendMode::Additive,
        BlendMode::Multiply,
        BlendMode::Screen,
        BlendMode::Lighten,
        BlendMode::ColorDodge,
        BlendMode::AddGlow,
        BlendMode::Subtract,
        BlendMode::Exclusion,
        BlendMode::Inverse,
        BlendMode::DestinationIn,
        BlendMode::ClipToLower,
        BlendMode::SliceFromLower,
    ];
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DrawableNode {
    #[serde(flatten)]
    pub base: NodeBase,
    pub texture: Id,
    pub mesh: DrawableMesh,
    pub blend_mode: BlendMode,
    pub culling: bool,
    pub masks: Vec<MaskRef>,
    /// S4: binary coverage threshold when used as a mask source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mask_threshold: Option<f32>,
    pub rest: DrawableRest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DrawableMesh {
    pub positions: BufferRef,
    pub uvs: BufferRef,
    pub indices: BufferRef,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DrawableRest {
    pub opacity: f32,
    pub draw_order: f32,
    pub multiply_color: Rgb,
    pub screen_color: Rgb,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MaskMode {
    Clip,
    Invert,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MaskRef {
    pub source: Id,
    pub mode: MaskMode,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WarpDeformerNode {
    #[serde(flatten)]
    pub base: NodeBase,
    pub rows: u32,
    pub cols: u32,
    pub rest: WarpRest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WarpRest {
    pub points: BufferRef,
    pub opacity: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RotationDeformerNode {
    #[serde(flatten)]
    pub base: NodeBase,
    pub rest: RotationRest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RotationRest {
    pub origin: Vec2,
    pub angle: f32,
    pub scale: Scale,
    pub opacity: f32,
}

/// Rotation-deformer scale: uniform, or per axis (S1).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Scale {
    Uniform(f32),
    PerAxis(Vec2),
}

impl Scale {
    pub fn xy(self) -> Vec2 {
        match self {
            Scale::Uniform(s) => [s, s],
            Scale::PerAxis(v) => v,
        }
    }
}

impl From<f32> for Scale {
    fn from(s: f32) -> Self {
        Scale::Uniform(s)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MeshDeformerNode {
    #[serde(flatten)]
    pub base: NodeBase,
    pub mesh: DeformerMesh,
    pub rest: OpacityRest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeformerMesh {
    pub positions: BufferRef,
    pub indices: BufferRef,
}

// ---------- Keyforms ----------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KeyformSet {
    pub axes: Vec<KeyAxis>,
    pub keyforms: Vec<Keyform>,
    /// S2; `None` means add.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub combine: Option<Combine>,
}

/// How a keyform set combines with rest and other sets (S2, E4).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Combine {
    /// Keyforms hold deltas from rest.
    #[default]
    Add,
    /// Keyforms hold factors (missing = 1).
    Multiply,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AxisInterpolation {
    Linear,
    Step,
    /// Uniform Catmull-Rom over the neighboring keys (E3).
    Cubic,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KeyAxis {
    pub param: ParamAxis,
    pub keys: Vec<f32>,
    pub interpolation: AxisInterpolation,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Keyform {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub positions: Option<BufferRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opacity: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draw_order: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub angle: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<Vec2>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub multiply_color: Option<Rgb>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub screen_color: Option<Rgb>,
    /// Rotation deformers, per axis (S1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale_xy: Option<Vec2>,
}

// ---------- Physics ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PhysicsDrive {
    X,
    Y,
    Angle,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PhysicsRig {
    pub id: Id,
    pub name: String,
    pub inputs: Vec<PhysicsInput>,
    pub pendulum: Vec<PendulumPoint>,
    pub outputs: Vec<PhysicsOutput>,
    pub normalization: PhysicsNormalization,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PhysicsInput {
    pub param: ParamAxis,
    pub weight: f32,
    pub drive: PhysicsDrive,
    pub reflect: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PendulumPoint {
    pub position: Vec2,
    pub mobility: f32,
    pub delay: f32,
    pub acceleration: f32,
    pub radius: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PhysicsOutput {
    pub param: ParamAxis,
    pub pendulum_index: u32,
    pub drive: PhysicsDrive,
    pub scale: f32,
    pub weight: f32,
    pub reflect: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PhysicsNormalization {
    pub position: NormalizationRange,
    pub angle: NormalizationRange,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NormalizationRange {
    pub min: f32,
    pub max: f32,
    pub default: f32,
}

// ---------- Motions ----------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Motion {
    pub id: Id,
    pub name: String,
    pub duration: f32,
    #[serde(rename = "loop")]
    pub looped: bool,
    pub tracks: Vec<MotionTrack>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MotionTrack {
    pub target: MotionTarget,
    pub keys: Vec<CurveKey>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MotionTarget {
    Param { param: ParamAxis },
    Node { node: Id, property: NodeProperty },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NodeProperty {
    Opacity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CurveInterp {
    Linear,
    Step,
    InverseStep,
    Bezier,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CurveKey {
    pub t: f32,
    pub value: f32,
    pub interp: CurveInterp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handles: Option<[Vec2; 2]>,
}
