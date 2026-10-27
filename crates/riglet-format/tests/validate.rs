//! Validation tests (spec V1–V9, Limits). Each test breaks one rule of a
//! valid model and checks the stable issue code.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

mod common;

use common::*;
use riglet_format::model::*;
use riglet_format::{Document, Limits, validate};

fn valid() -> Builder {
    let mut b = Builder::new();
    b.param("p", 0.0, 1.0, 0.0);
    b.warp("w", "root", 1, 1, &[[0.0, 0.0], [100.0, 0.0], [0.0, 100.0], [100.0, 100.0]]);
    b.drawable("d", "w", &[[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]]);
    b.drawable("e", "root", &[[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]]);
    b.mesh_deformer("m", "root", &[[0.0, 0.0], [10.0, 0.0], [0.0, 10.0]], &[0, 1, 2]);
    let kfs = vec![b.positions(&[[0.0, 0.0]; 3]), b.positions(&[[1.0, 0.0]; 3])];
    b.set("d", &[linear("p", &[0.0, 1.0])], kfs);
    b
}

fn codes(doc: &Document) -> Vec<&'static str> {
    codes_with(doc, &Limits::default())
}

fn codes_with(doc: &Document, limits: &Limits) -> Vec<&'static str> {
    validate(doc, limits).into_iter().map(|i| i.code).collect()
}

#[track_caller]
fn expect(b: &Builder, code: &str) {
    let got = codes(&b.document());
    assert!(got.contains(&code), "expected {code}, got {got:?}");
}

fn drawable<'a>(b: &'a mut Builder, id: &str) -> &'a mut DrawableNode {
    match b.node_mut(id) {
        Node::Drawable(d) => d,
        _ => panic!("not a drawable"),
    }
}

fn first_set(b: &mut Builder) -> &mut KeyformSet {
    &mut b.node_mut("d").base_mut().keyform_sets[0]
}

#[test]
fn valid_model_has_no_issues() {
    assert_eq!(codes(&valid().document()), Vec::<&str>::new());
}

// ----- V1 -----

#[test]
fn v1_package_paths_and_case() {
    let mut b = valid();
    b.files.insert("Textures/T.png".into(), Vec::new());
    expect(&b, "V1_CASE_COLLISION");
    let mut b = valid();
    b.files.insert("../x".into(), Vec::new());
    expect(&b, "V1_BAD_PATH");
}

#[test]
fn v1_format_and_version() {
    let mut doc = valid().document();
    doc.manifest.format = "other".into();
    doc.manifest.schema_version = "0.1.0".into();
    let got = codes(&doc);
    assert!(got.contains(&"V1_FORMAT") && got.contains(&"V1_SCHEMA_VERSION"), "{got:?}");
}

// ----- V2 -----

#[test]
fn v2_root_must_exist_and_be_unique() {
    let mut b = valid();
    b.model.root = "nope".into();
    expect(&b, "V2_ROOT");
    let mut b = valid();
    b.node_mut("e").base_mut().parent = None;
    expect(&b, "V2_ROOT");
}

#[test]
fn v2_parent_children_must_agree() {
    let mut b = valid();
    b.node_mut("root").base_mut().children.retain(|c| c != "e");
    expect(&b, "V2_PARENT_CHILDREN");
    let mut b = valid();
    b.node_mut("root").base_mut().children.push("e".into());
    expect(&b, "V2_PARENT_CHILDREN");
}

#[test]
fn v2_cycles_are_rejected() {
    let mut b = valid();
    b.group("g1", "root", 1.0).group("g2", "g1", 1.0);
    // Detach g1 from root and close the loop g1 <-> g2.
    b.node_mut("root").base_mut().children.retain(|c| c != "g1");
    b.node_mut("g1").base_mut().parent = Some("g2".into());
    b.node_mut("g2").base_mut().children.push("g1".into());
    expect(&b, "V2_UNREACHABLE");
}

#[test]
fn v2_drawables_cannot_have_children() {
    let mut b = valid();
    b.group("g", "e", 1.0);
    expect(&b, "V2_PARENT_TYPE");
}

#[test]
fn v2_node_key_must_match_id() {
    let mut b = valid();
    b.node_mut("e").base_mut().id = "other".into();
    expect(&b, "V2_ID_MISMATCH");
}

// ----- V3 -----

#[test]
fn v3_references_resolve() {
    let mut b = valid();
    drawable(&mut b, "e").texture = "missing".into();
    expect(&b, "V3_UNRESOLVED");
    let mut b = valid();
    first_set(&mut b).axes[0].param.parameter = "missing".into();
    expect(&b, "V3_UNRESOLVED");
    let mut b = valid();
    b.model.export_ids.live2d = Some([("ghost".to_owned(), "ParamX".to_owned())].into());
    expect(&b, "V3_UNRESOLVED");
}

#[test]
fn v3_masks() {
    let mut b = valid();
    drawable(&mut b, "e").masks.push(MaskRef { source: "e".into(), mode: MaskMode::Clip });
    expect(&b, "V3_MASK_SELF");
    let mut b = valid();
    drawable(&mut b, "e").masks.push(MaskRef { source: "w".into(), mode: MaskMode::Clip });
    expect(&b, "V3_MASK_SOURCE");
    let mut b = valid();
    drawable(&mut b, "e").masks.push(MaskRef { source: "d".into(), mode: MaskMode::Clip });
    drawable(&mut b, "d").masks.push(MaskRef { source: "e".into(), mode: MaskMode::Invert });
    expect(&b, "V3_MASK_CYCLE");
    // A chain without a cycle is fine.
    let mut b = valid();
    drawable(&mut b, "e").masks.push(MaskRef { source: "d".into(), mode: MaskMode::Clip });
    assert_eq!(codes(&b.document()), Vec::<&str>::new());
}

#[test]
fn v3_texture_must_be_a_png() {
    let mut b = valid();
    b.files.insert("textures/t.png".into(), b"GIF89a".to_vec());
    expect(&b, "V3_TEXTURE_NOT_PNG");
}

#[test]
fn v3_physics_pendulum_index_in_range() {
    let mut b = valid();
    let axis = ParamAxis { parameter: "p".into(), component: 0 };
    b.model.physics.push(PhysicsRig {
        id: "phys".into(),
        name: "phys".into(),
        inputs: Vec::new(),
        pendulum: Vec::new(),
        outputs: vec![PhysicsOutput {
            param: axis,
            pendulum_index: 0,
            drive: PhysicsDrive::Angle,
            scale: 1.0,
            weight: 1.0,
            reflect: false,
        }],
        normalization: PhysicsNormalization {
            position: NormalizationRange { min: -1.0, max: 1.0, default: 0.0 },
            angle: NormalizationRange { min: -1.0, max: 1.0, default: 0.0 },
        },
    });
    expect(&b, "V3_PENDULUM_INDEX");
}

// ----- V4 -----

#[test]
fn v4_parameter_ranges() {
    let mut b = valid();
    b.model.parameters[0].max = vec![0.0];
    expect(&b, "V4_PARAM_RANGE");
    let mut b = valid();
    b.model.parameters[0].default = vec![2.0];
    expect(&b, "V4_PARAM_DEFAULT");
    let mut b = valid();
    b.model.parameters[0].min = vec![0.0, 0.0];
    expect(&b, "V4_PARAM_LENGTH");
}

#[test]
fn v4_1d_parameter_accepts_component_0_only() {
    let mut b = valid();
    first_set(&mut b).axes[0].param.component = 1;
    expect(&b, "V4_PARAM_COMPONENT");
}

// ----- V5 -----

#[test]
fn v5_keys_strictly_ascending_and_finite() {
    let mut b = valid();
    first_set(&mut b).axes[0].keys = vec![1.0, 1.0];
    expect(&b, "V5_KEYS");
    let mut b = valid();
    first_set(&mut b).axes[0].keys = vec![0.0, f32::INFINITY];
    expect(&b, "V8_NON_FINITE");
}

#[test]
fn v5_keyform_count_matches_grid() {
    let mut b = valid();
    first_set(&mut b).keyforms.pop();
    expect(&b, "V5_KEYFORM_COUNT");
}

#[test]
fn v5_duplicate_axis() {
    let mut b = valid();
    let axis = first_set(&mut b).axes[0].clone();
    first_set(&mut b).axes.push(axis);
    expect(&b, "V5_DUPLICATE_AXIS");
}

#[test]
fn v5_positions_count_matches_rest() {
    let mut b = valid();
    let short = b.positions(&[[0.0, 0.0]; 2]);
    first_set(&mut b).keyforms[1] = short;
    expect(&b, "V5_POSITIONS_COUNT");
}

#[test]
fn v5_property_must_fit_node_type() {
    let mut b = valid();
    first_set(&mut b).keyforms[1].angle = Some(10.0);
    expect(&b, "V5_PROPERTY");
}

#[test]
fn v5_set_needs_axes() {
    let mut b = valid();
    b.node_mut("e").base_mut().keyform_sets.push(KeyformSet { axes: Vec::new(), keyforms: vec![Keyform::default()] });
    expect(&b, "V5_NO_AXES");
}

// ----- V6 -----

#[test]
fn v6_buffer_bounds_and_alignment() {
    let mut b = valid();
    drawable(&mut b, "e").mesh.positions.byte_offset += 2;
    expect(&b, "V6_BUFFER");
    let mut b = valid();
    drawable(&mut b, "e").mesh.positions.byte_offset = 1 << 40;
    expect(&b, "V6_BUFFER");
    let mut b = valid();
    drawable(&mut b, "e").mesh.positions.file = "buffers/none.bin".into();
    expect(&b, "V3_UNRESOLVED");
}

#[test]
fn v6_indices() {
    let mut b = valid();
    let bad = b.buf.push_u16(&[0, 1, 9]);
    drawable(&mut b, "e").mesh.indices = bad;
    expect(&b, "V6_INDICES");
    let mut b = valid();
    let bad = b.buf.push_u16(&[0, 1]);
    drawable(&mut b, "e").mesh.indices = bad;
    expect(&b, "V6_INDICES");
}

#[test]
fn v6_uvs_match_positions() {
    let mut b = valid();
    let uvs = b.buf.push_vec2(&[[0.0, 0.0]]);
    drawable(&mut b, "e").mesh.uvs = uvs;
    expect(&b, "V6_UVS");
}

// ----- V7 -----

#[test]
fn v7_warp_grid() {
    let mut b = valid();
    if let Node::Warp(w) = b.node_mut("w") {
        w.rows = 2;
    }
    expect(&b, "V7_WARP_GRID");
}

#[test]
fn v7_zero_area_mesh_deformer_triangle() {
    let mut b = valid();
    let pos = b.buf.push_vec2(&[[0.0, 0.0], [10.0, 0.0], [20.0, 0.0]]);
    if let Node::MeshDeformer(m) = b.node_mut("m") {
        m.mesh.positions = pos;
    }
    expect(&b, "V7_ZERO_AREA");
}

// ----- V8 -----

#[test]
fn v8_non_finite_values() {
    let mut b = valid();
    drawable(&mut b, "e").rest.opacity = f32::NAN;
    expect(&b, "V8_NON_FINITE");
    let mut b = valid();
    let bad = b.positions(&[[f32::INFINITY, 0.0]; 3]);
    first_set(&mut b).keyforms[1] = bad;
    expect(&b, "V8_NON_FINITE");
}

// ----- V9 -----

fn motion(keys: Vec<CurveKey>) -> Motion {
    Motion {
        id: "mo".into(),
        name: "mo".into(),
        duration: 1.0,
        looped: false,
        tracks: vec![MotionTrack {
            target: MotionTarget::Param { param: ParamAxis { parameter: "p".into(), component: 0 } },
            keys,
        }],
    }
}

fn key(t: f32, interp: CurveInterp, handles: Option<[Vec2; 2]>) -> CurveKey {
    CurveKey { t, value: 0.0, interp, handles }
}

#[test]
fn v9_motion_keys() {
    let mut b = valid();
    b.model.motions.push(motion(vec![key(0.5, CurveInterp::Linear, None), key(0.5, CurveInterp::Linear, None)]));
    expect(&b, "V9_KEY_ORDER");
    let mut b = valid();
    b.model.motions.push(motion(vec![key(0.0, CurveInterp::Bezier, None)]));
    expect(&b, "V9_HANDLES");
    let mut b = valid();
    b.model.motions.push(motion(vec![key(0.0, CurveInterp::InverseStep, Some([[0.0, 0.0]; 2]))]));
    expect(&b, "V9_HANDLES");
}

// ----- Limits -----

#[test]
fn limits_are_enforced() {
    let doc = valid().document();
    let got = codes_with(&doc, &Limits { max_nodes: 3, ..Limits::default() });
    assert!(got.contains(&"LIMIT_NODES"), "{got:?}");
    let got = codes_with(&doc, &Limits { max_vertices_per_mesh: 2, ..Limits::default() });
    assert!(got.contains(&"LIMIT_VERTICES"), "{got:?}");
    let got = codes_with(&doc, &Limits { max_vertices_total: 10, ..Limits::default() });
    assert!(got.contains(&"LIMIT_VERTICES"), "{got:?}");
    let got = codes_with(&doc, &Limits { max_keyform_grid: 1, ..Limits::default() });
    assert!(got.contains(&"LIMIT_KEYFORM_GRID"), "{got:?}");
    let got = codes_with(&doc, &Limits { max_keyform_vertices_total: 5, ..Limits::default() });
    assert!(got.contains(&"LIMIT_KEYFORM_VERTICES"), "{got:?}");
}

#[test]
fn texture_dimension_limit_reads_png_header() {
    let doc = valid().document();
    // The PNG is 1x1; declared and actual sizes both pass at limit 1.
    assert!(codes_with(&doc, &Limits { max_texture_dimension: 1, ..Limits::default() }).is_empty());
    let mut b = valid();
    b.model.textures[0].width = 20_000;
    expect(&b, "LIMIT_TEXTURE_SIZE");
}

#[test]
fn huge_declared_buffer_count_is_rejected_before_allocating() {
    let mut b = valid();
    drawable(&mut b, "e").mesh.positions.count = u64::MAX / 2;
    expect(&b, "LIMIT_VERTICES");
}

// ----- v0.3 composite fields -----

#[test]
fn v2_composite_fields_only_on_composites() {
    let mut b = valid();
    b.group("g", "root", 1.0);
    if let Node::Group(g) = b.node_mut("g") {
        g.blend_mode = Some(BlendMode::Additive);
    }
    expect(&b, "V2_COMPOSITE_FIELD");
    let mut b = valid();
    b.group("g", "root", 1.0).set_draw_order("g", 3.0);
    expect(&b, "V2_COMPOSITE_FIELD");
}

#[test]
fn draw_order_keyforms_only_on_composite_groups() {
    let mut b = valid();
    b.group("g", "root", 1.0);
    b.set(
        "g",
        &[linear("p", &[0.0, 1.0])],
        vec![Keyform::default(), Keyform { draw_order: Some(1.0), ..Default::default() }],
    );
    expect(&b, "V5_PROPERTY");
    let mut b = valid();
    b.composite("c", "root", 1.0, 0.0);
    b.set(
        "c",
        &[linear("p", &[0.0, 1.0])],
        vec![Keyform::default(), Keyform { draw_order: Some(1.0), ..Default::default() }],
    );
    assert_eq!(codes(&b.document()), Vec::<&str>::new());
}

#[test]
fn composite_masks_are_checked() {
    let mut b = valid();
    b.composite("c", "root", 1.0, 0.0);
    if let Node::Group(g) = b.node_mut("c") {
        g.masks = Some(vec![MaskRef { source: "w".into(), mode: MaskMode::Clip }]);
    }
    expect(&b, "V3_MASK_SOURCE");
    let mut b = valid();
    b.composite("c", "root", 1.0, 0.0);
    if let Node::Group(g) = b.node_mut("c") {
        g.masks = Some(vec![MaskRef { source: "e".into(), mode: MaskMode::Clip }]);
    }
    assert_eq!(codes(&b.document()), Vec::<&str>::new());
}
