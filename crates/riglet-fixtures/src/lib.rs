//! Synthetic test models for Riglet tests: a model [`Builder`] and one
//! fixture per rendering feature ([`fixtures`]). Every test model is built
//! from code, so it is reviewable in a diff; `.riglet` files are generated
//! during tests, never committed.
//!
//! Dev-only crate: setup helpers panic on misuse, which is fine in tests.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

pub mod fixtures;

use std::collections::BTreeMap;

use riglet_format::buffer::BufferWriter;
use riglet_format::model::*;
use riglet_format::{Document, EvalOutput, Limits, ParamValues, Rig};

/// Valid 1x1 RGBA PNG.
pub const PNG_1X1: [u8; 70] = [
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52, 0x00, 0x00, 0x00,
    0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0d, 0x49,
    0x44, 0x41, 0x54, 0x78, 0xda, 0x63, 0x64, 0x60, 0xf8, 0x5f, 0x0f, 0x00, 0x02, 0x87, 0x01, 0x80, 0xeb, 0x47, 0xba,
    0x92, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
];

pub const TOL: f32 = 1e-3;

pub fn assert_close(actual: Vec2, expected: Vec2) {
    assert!(
        (actual[0] - expected[0]).abs() <= TOL && (actual[1] - expected[1]).abs() <= TOL,
        "expected {expected:?}, got {actual:?}"
    );
}

pub fn assert_close_f(actual: f32, expected: f32) {
    assert!((actual - expected).abs() <= TOL, "expected {expected}, got {actual}");
}

pub struct Builder {
    pub model: Model,
    pub buf: BufferWriter,
    pub files: BTreeMap<String, Vec<u8>>,
}

fn base(id: &str, parent: &str) -> NodeBase {
    NodeBase {
        id: id.into(),
        name: id.into(),
        parent: Some(parent.into()),
        children: Vec::new(),
        visible: true,
        keyform_sets: Vec::new(),
        extensions: None,
    }
}

/// One axis of a keyform set: (parameter id, component, keys, interpolation).
pub type AxisSpec<'a> = (&'a str, u8, &'a [f32], AxisInterpolation);

pub fn linear<'a>(param: &'a str, keys: &'a [f32]) -> AxisSpec<'a> {
    (param, 0, keys, AxisInterpolation::Linear)
}

impl Builder {
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        let mut nodes = BTreeMap::new();
        let mut root = base("root", "");
        root.parent = None;
        nodes.insert(
            "root".to_owned(),
            Node::Group(GroupNode {
                base: root,
                composite: false,
                blend_mode: None,
                masks: None,
                rest: GroupRest { opacity: 1.0, draw_order: None },
            }),
        );
        let mut files = BTreeMap::new();
        files.insert("textures/t.png".to_owned(), PNG_1X1.to_vec());
        Builder {
            model: Model {
                canvas: Canvas { width: 512.0, height: 512.0 },
                textures: vec![Texture {
                    id: "tex".into(),
                    file: "textures/t.png".into(),
                    width: 1,
                    height: 1,
                    premultiplied_alpha: true,
                }],
                parameters: Vec::new(),
                root: "root".into(),
                nodes,
                physics: Vec::new(),
                physics_environment: PhysicsEnvironment { gravity: [0.0, 1.0], wind: [0.0, 0.0] },
                motions: Vec::new(),
                export_ids: ExportIdMap::default(),
                extensions: None,
            },
            buf: BufferWriter::new("buffers/data.bin"),
            files,
        }
    }

    pub fn param(&mut self, id: &str, min: f32, max: f32, default: f32) -> &mut Self {
        self.model.parameters.push(Parameter {
            id: id.into(),
            name: id.into(),
            kind: ParamKind::OneD,
            min: vec![min],
            max: vec![max],
            default: vec![default],
            repeat: None,
            group: None,
            extensions: None,
        });
        self
    }

    pub fn param2d(&mut self, id: &str, min: [f32; 2], max: [f32; 2]) -> &mut Self {
        self.model.parameters.push(Parameter {
            id: id.into(),
            name: id.into(),
            kind: ParamKind::TwoD,
            min: min.to_vec(),
            max: max.to_vec(),
            default: min.to_vec(),
            repeat: None,
            group: None,
            extensions: None,
        });
        self
    }

    pub fn node_mut(&mut self, id: &str) -> &mut Node {
        self.model.nodes.get_mut(id).expect("node exists")
    }

    fn insert(&mut self, node: Node) {
        let id = node.base().id.clone();
        let parent = node.base().parent.clone().expect("non-root");
        self.node_mut(&parent).base_mut().children.push(id.clone());
        self.model.nodes.insert(id, node);
    }

    pub fn group(&mut self, id: &str, parent: &str, opacity: f32) -> &mut Self {
        self.insert(Node::Group(GroupNode {
            base: base(id, parent),
            composite: false,
            blend_mode: None,
            masks: None,
            rest: GroupRest { opacity, draw_order: None },
        }));
        self
    }

    /// A composite group (R6) with its own draw order.
    pub fn composite(&mut self, id: &str, parent: &str, opacity: f32, draw_order: f32) -> &mut Self {
        self.insert(Node::Group(GroupNode {
            base: base(id, parent),
            composite: true,
            blend_mode: None,
            masks: None,
            rest: GroupRest { opacity, draw_order: Some(draw_order) },
        }));
        self
    }

    pub fn set_draw_order(&mut self, id: &str, draw_order: f32) -> &mut Self {
        match self.node_mut(id) {
            Node::Drawable(d) => d.rest.draw_order = draw_order,
            Node::Group(g) => g.rest.draw_order = Some(draw_order),
            _ => panic!("{id} has no draw order"),
        }
        self
    }

    /// Adds a texture from straight-alpha RGBA8 pixels (PNG-encoded, as in a
    /// real package). `premultiplied` sets the texture's flag only; pass
    /// premultiplied pixels with it.
    pub fn texture(&mut self, id: &str, width: u32, height: u32, rgba: &[u8], premultiplied: bool) -> &mut Self {
        let file = format!("textures/{id}.png");
        self.files.insert(file.clone(), encode_png(width, height, rgba));
        self.model.textures.push(Texture { id: id.into(), file, width, height, premultiplied_alpha: premultiplied });
        self
    }

    /// A drawable with full mesh data.
    pub fn mesh(
        &mut self,
        id: &str,
        parent: &str,
        texture: &str,
        positions: &[Vec2],
        uvs: &[Vec2],
        indices: &[u16],
    ) -> &mut Self {
        let pos = self.buf.push_vec2(positions);
        let uvs = self.buf.push_vec2(uvs);
        let indices = self.buf.push_u16(indices);
        self.insert(Node::Drawable(DrawableNode {
            base: base(id, parent),
            texture: texture.into(),
            mesh: DrawableMesh { positions: pos, uvs, indices },
            blend_mode: BlendMode::Normal,
            culling: false,
            masks: Vec::new(),
            mask_threshold: None,
            rest: DrawableRest { opacity: 1.0, draw_order: 0.0, multiply_color: [1.0; 3], screen_color: [0.0; 3] },
        }));
        self
    }

    /// An axis-aligned textured quad from `(x0, y0)` to `(x1, y1)` (model
    /// space, +Y down). Both triangles are clockwise on screen, i.e. front
    /// facing (R7). UVs run (0,0) top-left to (1,1) bottom-right.
    /// A textured rectangle. UVs span the whole texture, except for a 1 x 1
    /// texture (a flat color): there every corner samples the texel center,
    /// so R2's transparent border doesn't fade the edges.
    pub fn quad(&mut self, id: &str, parent: &str, texture: &str, min: Vec2, max: Vec2) -> &mut Self {
        let positions = [[min[0], min[1]], [max[0], min[1]], [max[0], max[1]], [min[0], max[1]]];
        let flat = self.model.textures.iter().any(|t| t.id == texture && t.width == 1 && t.height == 1);
        let uvs = if flat { [[0.5; 2]; 4] } else { [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]] };
        self.mesh(id, parent, texture, &positions, &uvs, &[0, 1, 2, 0, 2, 3])
    }

    pub fn drawable_mut(&mut self, id: &str) -> &mut DrawableNode {
        match self.node_mut(id) {
            Node::Drawable(d) => d,
            _ => panic!("{id} is not a drawable"),
        }
    }

    pub fn group_mut(&mut self, id: &str) -> &mut GroupNode {
        match self.node_mut(id) {
            Node::Group(g) => g,
            _ => panic!("{id} is not a group"),
        }
    }

    pub fn drawable(&mut self, id: &str, parent: &str, positions: &[Vec2]) -> &mut Self {
        let pos = self.buf.push_vec2(positions);
        let uvs = self.buf.push_vec2(&vec![[0.0, 0.0]; positions.len()]);
        let tris: Vec<u16> = if positions.len() >= 3 { vec![0, 1, 2] } else { Vec::new() };
        let indices = self.buf.push_u16(&tris);
        self.insert(Node::Drawable(DrawableNode {
            base: base(id, parent),
            texture: "tex".into(),
            mesh: DrawableMesh { positions: pos, uvs, indices },
            blend_mode: BlendMode::Normal,
            culling: false,
            masks: Vec::new(),
            mask_threshold: None,
            rest: DrawableRest { opacity: 1.0, draw_order: 0.0, multiply_color: [1.0; 3], screen_color: [0.0; 3] },
        }));
        self
    }

    pub fn warp(&mut self, id: &str, parent: &str, rows: u32, cols: u32, points: &[Vec2]) -> &mut Self {
        let pts = self.buf.push_vec2(points);
        self.insert(Node::Warp(WarpDeformerNode {
            base: base(id, parent),
            rows,
            cols,
            rest: WarpRest { points: pts, opacity: 1.0 },
        }));
        self
    }

    pub fn rotation(&mut self, id: &str, parent: &str, origin: Vec2, angle: f32, scale: f32) -> &mut Self {
        self.insert(Node::Rotation(RotationDeformerNode {
            base: base(id, parent),
            rest: RotationRest { origin, angle, scale: scale.into(), opacity: 1.0 },
        }));
        self
    }

    pub fn mesh_deformer(&mut self, id: &str, parent: &str, positions: &[Vec2], indices: &[u16]) -> &mut Self {
        let pos = self.buf.push_vec2(positions);
        let idx = self.buf.push_u16(indices);
        self.insert(Node::MeshDeformer(MeshDeformerNode {
            base: base(id, parent),
            mesh: DeformerMesh { positions: pos, indices: idx },
            rest: OpacityRest { opacity: 1.0 },
        }));
        self
    }

    /// Adds a multiply set (S2): keyforms hold factors.
    pub fn set_multiply(&mut self, node: &str, axes: &[AxisSpec], keyforms: Vec<Keyform>) -> &mut Self {
        self.set(node, axes, keyforms);
        if let Some(set) = self.node_mut(node).base_mut().keyform_sets.last_mut() {
            set.combine = Some(Combine::Multiply);
        }
        self
    }

    pub fn positions(&mut self, deltas: &[Vec2]) -> Keyform {
        Keyform { positions: Some(self.buf.push_vec2(deltas)), ..Default::default() }
    }

    pub fn set(&mut self, node: &str, axes: &[AxisSpec], keyforms: Vec<Keyform>) -> &mut Self {
        let axes = axes
            .iter()
            .map(|&(p, component, keys, interpolation)| KeyAxis {
                param: ParamAxis { parameter: p.into(), component },
                keys: keys.to_vec(),
                interpolation,
            })
            .collect();
        self.node_mut(node).base_mut().keyform_sets.push(KeyformSet { axes, keyforms, combine: None });
        self
    }

    pub fn document(&self) -> Document {
        let mut files = self.files.clone();
        self.buf.clone().finish(&mut files);
        Document {
            manifest: Manifest {
                format: "riglet".into(),
                schema_version: riglet_format::migrate::CURRENT_SCHEMA_VERSION.into(),
                generator: "riglet-fixtures".into(),
                provenance: Vec::new(),
            },
            model: self.model.clone(),
            files,
        }
    }

    pub fn rig(&self) -> Rig {
        match Rig::prepare(&self.document(), &Limits::default()) {
            Ok(rig) => rig,
            Err(issues) => panic!("invalid test model: {issues:#?}"),
        }
    }
}

/// Encodes straight-alpha RGBA8 pixels as a PNG.
pub fn encode_png(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().expect("PNG header");
        writer.write_image_data(rgba).expect("PNG data");
    }
    out
}

/// `width x height` pixels of one straight-alpha RGBA color.
pub fn solid(width: u32, height: u32, rgba: [u8; 4]) -> Vec<u8> {
    rgba.repeat((width * height) as usize)
}

/// Evaluates with the given parameter values (all others at default).
pub fn eval(rig: &Rig, values: &[(&str, [f32; 2])]) -> EvalOutput {
    let mut params = ParamValues::defaults(rig);
    for (id, v) in values {
        assert!(params.set(rig, id, *v), "unknown parameter {id}");
    }
    let mut out = EvalOutput::new();
    rig.evaluate(&params, &mut out);
    out
}

/// Model-space positions of drawable `id` in `frame`.
pub fn positions(rig: &Rig, out: &EvalOutput, id: &str) -> Vec<Vec2> {
    let index = rig.node_index(id).expect("node exists");
    out.drawable(index).expect("drawable in output").positions.clone()
}
