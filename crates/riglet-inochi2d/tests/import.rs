//! Import tests: one synthetic puppet per mapping row (design doc section
//! 4), checked by evaluating the Riglet rig against Inochi2D's own formulas.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

mod common;

use common::*;
use riglet_inochi2d::container::Encoding;
use riglet_inochi2d::reinterpolate::cubic;
use serde_json::json;

// ---------- nodes, transforms, draw order ----------

#[test]
fn transform_is_t_r_s_and_vertices_subtract_origin() {
    let p = part(
        3,
        "p",
        [1.0, 0.0],
        [2.0, 1.0],
        json!({"mesh": {
            "verts": [1, 0, 2, 0, 2, 1], "uvs": [0, 0, 1, 0, 1, 1], "indices": [0, 1, 2], "origin": [1, 0]
        }}),
    );
    let n = node(
        2,
        "n",
        vec![p],
        json!({"transform": {"trans": [10, 20, 5], "rot": [0, 0, std::f32::consts::FRAC_PI_2], "scale": [2, 1]}}),
    );
    let i = import(&puppet(vec![n], vec![]));
    let rig = rig(&i);
    let pos = positions(&rig, &eval(&rig, &[]), 3);
    // vert (2,0) - origin (1,0) = (1,0); S -> (2,0); R(90) -> (0,2); T -> (10,22).
    assert_close(pos[1], [10.0, 22.0]);
    assert_close(pos[0], [10.0, 20.0]);
    // trans.z has no 2D effect: no warning, kept in extensions.
    assert!(!codes(&i).contains(&"TRANSFORM_3D_ROTATION_DROPPED"));
}

#[test]
fn zsort_accumulates_and_higher_draws_further_back() {
    let back = part(3, "back", [0.0, 0.0], [1.0, 1.0], json!({"zsort": 0.5}));
    let front = part(4, "front", [0.0, 0.0], [1.0, 1.0], json!({"zsort": -0.5}));
    let n = node(2, "n", vec![front, back], json!({"zsort": 1.0}));
    let i = import(&puppet(vec![n], vec![]));
    let rig = rig(&i);
    let out = eval(&rig, &[]);
    let order: Vec<&str> = out.in_draw_order().iter().map(|d| rig.nodes[d.node].name.as_str()).collect();
    assert_eq!(order, ["back", "front"]);
    assert_eq!(out.in_draw_order()[0].draw_order, -1.5);
}

#[test]
fn part_with_children_keeps_the_drawable_first() {
    let child = part(4, "child", [0.0, 0.0], [1.0, 1.0], json!({}));
    let parent = part(3, "parent", [0.0, 0.0], [1.0, 1.0], json!({"children": [child]}));
    let i = import(&puppet(vec![parent], vec![]));
    let rig = rig(&i);
    let holder = &i.document.model.nodes["inochi:3"];
    assert_eq!(holder.base().children, ["inochi:3:part", "inochi:4"]);
    assert!(matches!(holder, riglet_format::model::Node::Group(_)));
    assert_eq!(rig.drawables.len(), 2);
}

#[test]
fn disabled_nodes_are_hidden_and_mask_nodes_still_mask() {
    let mask = node(
        5,
        "mask",
        vec![],
        json!({"type": "Mask", "mesh": {
            "verts": [0, 0, 1, 0, 1, 1], "uvs": [0, 0, 1, 0, 1, 1], "indices": [0, 1, 2], "origin": [0, 0]
        }}),
    );
    let hidden = part(3, "hidden", [0.0, 0.0], [1.0, 1.0], json!({"enabled": false}));
    let masked = part(4, "masked", [0.0, 0.0], [1.0, 1.0], json!({"masks": [{"source": 5, "mode": "Mask"}]}));
    let i = import(&puppet(vec![mask, hidden, masked], vec![]));
    let rig = rig(&i);
    let out = eval(&rig, &[]);
    let vis = |name: &str| out.drawables.iter().find(|d| rig.nodes[d.node].name == name).unwrap().visible;
    assert!(!vis("hidden"));
    assert!(!vis("mask"), "a Mask node is a hidden mask source");
    let m = &i.document.model.nodes["inochi:4"];
    let riglet_format::model::Node::Drawable(d) = m else { panic!() };
    assert_eq!(d.masks[0].source, "inochi:5");
}

#[test]
fn masks_map_modes_thresholds_and_warn_on_order() {
    let a = part(3, "a", [0.0, 0.0], [1.0, 1.0], json!({"mask_threshold": 0.3}));
    let b = part(4, "b", [0.0, 0.0], [1.0, 1.0], json!({}));
    let ok = part(
        5,
        "ok",
        [0.0, 0.0],
        [1.0, 1.0],
        json!({"masks": [{"source": 3, "mode": "Mask"}, {"source": 4, "mode": "DodgeMask"}]}),
    );
    let bad = part(
        6,
        "bad",
        [0.0, 0.0],
        [1.0, 1.0],
        json!({"masks": [{"source": 4, "mode": "DodgeMask"}, {"source": 3, "mode": "Mask"}]}),
    );
    let i = import(&puppet(vec![a, b, ok, bad], vec![]));
    rig(&i);
    let nodes = &i.document.model.nodes;
    let riglet_format::model::Node::Drawable(a) = &nodes["inochi:3"] else { panic!() };
    assert_eq!(a.mask_threshold, Some(0.3), "the source's own threshold (S4)");
    let riglet_format::model::Node::Drawable(ok) = &nodes["inochi:5"] else { panic!() };
    assert_eq!(ok.masks[1].mode, riglet_format::model::MaskMode::Invert);
    let order: Vec<_> = i.warnings.iter().filter(|w| w.code == "MASK_ORDER").map(|w| w.object_id.clone()).collect();
    assert_eq!(order, [Some("inochi:6".to_owned())]);
}

#[test]
fn composite_wraps_its_transform_and_carries_masks() {
    let shape = part(5, "shape", [0.0, 0.0], [1.0, 1.0], json!({}));
    let inside = part(4, "inside", [0.0, 0.0], [1.0, 1.0], json!({}));
    let comp = node(
        3,
        "comp",
        vec![inside],
        json!({
            "type": "Composite", "blend_mode": "Normal", "opacity": 0.5, "tint": [1, 1, 1], "screenTint": [0, 0, 0],
            "masks": [{"source": 5, "mode": "Mask"}], "transform": {"trans": [4, 0, 0], "rot": [0, 0, 0], "scale": [1, 1]}
        }),
    );
    let i = import(&puppet(vec![shape, comp], vec![]));
    let rig = rig(&i);
    let nodes = &i.document.model.nodes;
    let riglet_format::model::Node::Group(g) = &nodes["inochi:3"] else { panic!() };
    assert!(g.composite);
    assert_eq!(g.masks.as_ref().unwrap()[0].source, "inochi:5");
    assert_eq!(g.base.children, ["inochi:3:transform"]);
    let out = eval(&rig, &[]);
    assert_close(positions(&rig, &out, 4)[0], [4.0, 0.0]);
    assert!((out.composites[0].opacity - 0.5).abs() < 1e-6);
}

#[test]
fn unsupported_things_warn_and_are_kept() {
    let shapes = node(3, "shapes", vec![], json!({"type": "Shapes"}));
    let camera = node(4, "cam", vec![], json!({"type": "Camera", "viewport": [1920, 1080]}));
    let screen = part(5, "screen", [0.0, 0.0], [1.0, 1.0], json!({"blend_mode": "Overlay"}));
    let i = import(&puppet(vec![shapes, camera, screen], vec![]));
    rig(&i);
    let c = codes(&i);
    assert!(c.contains(&"NODE_TYPE_UNSUPPORTED"));
    assert!(c.contains(&"BLEND_MODE_UNSUPPORTED"));
    assert_eq!(c.iter().filter(|x| **x == "NODE_TYPE_UNSUPPORTED").count(), 1, "Camera is kept without a warning");
    let root_ext = i.document.model.nodes["inochi:1"].base().extensions.as_ref().unwrap();
    assert_eq!(root_ext["inochi2d"]["unsupportedChildren"].as_array().unwrap().len(), 2);
    // EXT_SECT entries are kept verbatim.
    assert!(i.document.files.contains_key("extensions/inochi2d/ext/com.inochi2d.creator.FlipConfig"));
}

// ---------- bindings ----------

/// A one-vertex-translating deform binding on part 3 over `axis` with x
/// offsets `xs` (all 4 quad vertices move by the same delta).
fn deform_puppet(axis: &[f32], xs: &[f32], interp: &str) -> serde_json::Value {
    let values: Vec<serde_json::Value> = xs.iter().map(|&x| json!([[[x, 0], [x, 0], [x, 0], [x, 0]]])).collect();
    let b = binding(3, "deform", json!(values), interp);
    puppet(vec![part(3, "p", [0.0, 0.0], [1.0, 1.0], json!({}))], vec![param(10, "move", 0.0, 1.0, axis, vec![b])])
}

fn x_at(rig: &riglet_format::Rig, v: f32) -> f32 {
    positions(rig, &eval(rig, &[(10, [v, 0.0])]), 3)[0][0]
}

#[test]
fn linear_binding_matches_lerp() {
    let i = import(&deform_puppet(&[0.0, 0.25, 1.0], &[0.0, 10.0, 40.0], "Linear"));
    let rig = rig(&i);
    for (v, want) in [(0.0, 0.0), (0.125, 5.0), (0.25, 10.0), (0.625, 25.0), (1.0, 40.0), (2.0, 40.0)] {
        assert!((x_at(&rig, v) - want).abs() < TOL, "v={v}");
    }
}

#[test]
fn nearest_binding_matches_inochi_including_ties() {
    let axis = [0.0f32, 0.2, 1.0];
    let xs = [0.0f32, 10.0, 20.0];
    let i = import(&deform_puppet(&axis, &xs, "Nearest"));
    let rig = rig(&i);
    // Inochi2D: segment i, offset t; t >= 0.5 takes the upper key.
    let inochi = |v: f32| {
        let i = if v < 0.2 { 0 } else { 1 };
        let t = (v - axis[i]) / (axis[i + 1] - axis[i]);
        if t >= 0.5 { xs[i + 1] } else { xs[i] }
    };
    for step in 0..=200 {
        let v = step as f32 / 200.0;
        assert!((x_at(&rig, v) - inochi(v)).abs() < TOL, "v={v}: {} vs {}", x_at(&rig, v), inochi(v));
    }
    assert_eq!(x_at(&rig, 0.1), 10.0, "exact tie goes up");
}

#[test]
fn cubic_binding_is_resampled_from_inochis_formula() {
    let axis = [0.0f32, 0.5, 1.0];
    let xs = [0.0f32, 10.0, 0.0];
    let i = import(&deform_puppet(&axis, &xs, "Cubic"));
    assert!(codes(&i).contains(&"CUBIC_RESAMPLED"));
    let rig = rig(&i);
    // Inochi2D: Catmull-Rom with clamped neighbors on segment 0.
    for q in [0.25f32, 0.5, 0.75] {
        let want = cubic(&xs[0], &xs[0], &xs[1], &xs[2], q);
        assert!((x_at(&rig, 0.5 * q) - want).abs() < TOL, "q={q}");
    }
    // Between resampled keys the error stays small.
    let mid = cubic(&xs[0], &xs[0], &xs[1], &xs[2], 0.125 / 0.5 * 0.5);
    assert!((x_at(&rig, 0.0625) - mid).abs() < 0.5);
}

#[test]
fn unset_points_are_reinterpolated() {
    let values = json!([
        [[[0, 0], [0, 0], [0, 0], [0, 0]]],
        [[[99, 0], [99, 0], [99, 0], [99, 0]]],
        [[[20, 0], [20, 0], [20, 0], [20, 0]]]
    ]);
    let mut b = binding(3, "deform", values, "Linear");
    b["isSet"] = json!([[true], [false], [true]]);
    let pup = puppet(
        vec![part(3, "p", [0.0, 0.0], [1.0, 1.0], json!({}))],
        vec![param(10, "m", 0.0, 1.0, &[0.0, 0.25, 1.0], vec![b])],
    );
    let rig = rig(&import(&pup));
    // The unset middle (99) is ignored and filled by axis position: 5.
    assert!((x_at(&rig, 0.25) - 5.0).abs() < TOL);
}

#[test]
fn two_d_parameter_bindings_are_bilinear() {
    // values[x][y], x and y keys at 0 and 1.
    let v = |x: f32| json!([[x, 0], [x, 0], [x, 0], [x, 0]]);
    let values = json!([[v(0.0), v(20.0)], [v(10.0), v(30.0)]]);
    let b = binding(3, "deform", values, "Linear");
    let pup = puppet(
        vec![part(3, "p", [0.0, 0.0], [1.0, 1.0], json!({}))],
        vec![param2d(10, "xy", &[0.0, 1.0], &[0.0, 1.0], vec![b])],
    );
    let rig = rig(&import(&pup));
    let at = |x: f32, y: f32| positions(&rig, &eval(&rig, &[(10, [x, y])]), 3)[0][0];
    assert!((at(0.25, 0.75) - (0.25 * 10.0 + 0.75 * 20.0)).abs() < TOL);
    assert!((at(1.0, 1.0) - 30.0).abs() < TOL);
}

#[test]
fn transform_bindings_add_and_scale_multiplies() {
    let p = part(3, "p", [1.0, 0.0], [2.0, 1.0], json!({}));
    let n = node(2, "n", vec![p], json!({}));
    let params = vec![
        param(10, "tx", 0.0, 1.0, &[0.0, 1.0], vec![binding(2, "transform.t.x", json!([[0], [5]]), "Linear")]),
        param(
            11,
            "rz",
            0.0,
            1.0,
            &[0.0, 1.0],
            vec![binding(2, "transform.r.z", json!([[0], [std::f32::consts::PI]]), "Linear")],
        ),
        param(12, "sx", 0.0, 1.0, &[0.0, 1.0], vec![binding(2, "transform.s.x", json!([[1], [3]]), "Linear")]),
        param(13, "sx2", 0.0, 1.0, &[0.0, 1.0], vec![binding(2, "transform.s.x", json!([[1], [2]]), "Linear")]),
    ];
    let i = import(&puppet(vec![n], params));
    let rig = rig(&i);
    let p0 = |vals: &[(u32, [f32; 2])]| positions(&rig, &eval(&rig, vals), 3)[0];
    assert_close(p0(&[(10, [1.0, 0.0])]), [6.0, 0.0]);
    assert_close(p0(&[(11, [1.0, 0.0])]), [-1.0, 0.0]);
    // Two scale bindings multiply (S2): 3 * 2.
    assert_close(p0(&[(12, [1.0, 0.0]), (13, [1.0, 0.0])]), [6.0, 0.0]);
    // Animated transform: a rotation deformer exists for node 2.
    assert!(i.document.model.nodes.contains_key("inochi:2:transform"));
}

#[test]
fn opacity_bindings_multiply() {
    let p = part(3, "p", [0.0, 0.0], [1.0, 1.0], json!({"opacity": 0.8}));
    let params = vec![
        param(10, "a", 0.0, 1.0, &[0.0, 1.0], vec![binding(3, "opacity", json!([[1], [0.5]]), "Linear")]),
        param(11, "b", 0.0, 1.0, &[0.0, 1.0], vec![binding(3, "opacity", json!([[1], [0.5]]), "Linear")]),
    ];
    let rig = rig(&import(&puppet(vec![p], params)));
    let out = eval(&rig, &[(10, [1.0, 0.0]), (11, [1.0, 0.0])]);
    assert!((out.drawables[0].opacity - 0.2).abs() < 1e-5);
}

#[test]
fn zsort_bindings_move_every_member_of_the_scope() {
    let a = part(3, "a", [0.0, 0.0], [1.0, 1.0], json!({"zsort": 0.0}));
    let b1 = part(4, "b1", [0.0, 0.0], [1.0, 1.0], json!({"zsort": 0.0}));
    let b2 = part(5, "b2", [0.0, 0.0], [1.0, 1.0], json!({"zsort": 0.1}));
    let group = node(2, "g", vec![b1, b2], json!({"zsort": 1.0}));
    let z = binding(2, "zSort", json!([[0], [-2]]), "Nearest");
    let rig = rig(&import(&puppet(vec![a, group], vec![param(10, "z", 0.0, 1.0, &[0.0, 1.0], vec![z])])));
    let names = |v: f32| -> Vec<String> {
        eval(&rig, &[(10, [v, 0.0])]).in_draw_order().iter().map(|d| rig.nodes[d.node].name.clone()).collect()
    };
    assert_eq!(names(0.0), ["b2", "b1", "a"]);
    // The group's zSort drops below a's: both children move, keeping order.
    assert_eq!(names(1.0), ["a", "b2", "b1"]);
}

// ---------- tint and screen ----------

/// Inochi2D's color for an opaque texel c: screen, then multiply.
fn inochi_color(c: f32, m: f32, s: f32) -> f32 {
    (c + s - c * s) * m
}

fn riglet_color(c: f32, m: f32, s: f32) -> f32 {
    c * m + s - c * m * s
}

#[test]
fn tint_and_screen_rest_values_convert_exactly() {
    let p = part(3, "p", [0.0, 0.0], [1.0, 1.0], json!({"tint": [0.5, 1.0, 0.2], "screenTint": [0.4, 0.0, 1.0]}));
    let i = import(&puppet(vec![p], vec![]));
    let rig = rig(&i);
    let d = &eval(&rig, &[]).drawables[0];
    for (k, (m, s)) in [(0.5, 0.4), (1.0, 0.0), (0.2, 1.0)].into_iter().enumerate() {
        for c in [0.0, 0.3, 1.0] {
            let got = riglet_color(c, d.multiply_color[k], d.screen_color[k]);
            assert!((got - inochi_color(c, m, s)).abs() < 1e-5, "channel {k} c={c}");
        }
    }
    // White texture is opaque: no partial-alpha warning.
    assert!(!codes(&i).contains(&"COLOR_ORDER_PARTIAL_ALPHA"));
}

#[test]
fn tint_and_screen_bindings_are_exact_at_keys() {
    let p = part(3, "p", [0.0, 0.0], [1.0, 1.0], json!({"tint": [0.8, 0.8, 0.8], "screenTint": [0.2, 0.2, 0.2]}));
    let params = vec![
        param(10, "t", 0.0, 1.0, &[0.0, 1.0], vec![binding(3, "tint.r", json!([[1], [0.5]]), "Linear")]),
        param(11, "s", 0.0, 1.0, &[0.0, 1.0], vec![binding(3, "screenTint.r", json!([[0], [0.6]]), "Linear")]),
    ];
    let i = import(&puppet(vec![p], params));
    let rig = rig(&i);
    for (t, s) in [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (1.0, 1.0)] {
        let d = &eval(&rig, &[(10, [t, 0.0]), (11, [s, 0.0])]).drawables[0];
        let m = 0.8 * if t == 1.0 { 0.5 } else { 1.0 };
        let sc = 0.2 + if s == 1.0 { 0.6 } else { 0.0 };
        for c in [0.0, 0.5, 1.0] {
            let got = riglet_color(c, d.multiply_color[0], d.screen_color[0]);
            assert!((got - inochi_color(c, m, sc)).abs() < 1e-4, "t={t} s={s} c={c}");
        }
    }
}

// ---------- mesh groups ----------

fn mesh_group(uuid: u32, children: Vec<serde_json::Value>, extra: serde_json::Value) -> serde_json::Value {
    let mut n = node(
        uuid,
        "mg",
        children,
        json!({"type": "MeshGroup", "mesh": {
            "verts": [0, 0, 10, 0, 0, 10, 10, 10], "uvs": [0, 0, 1, 0, 0, 1, 1, 1],
            "indices": [0, 1, 2, 1, 3, 2], "origin": [0, 0]
        }}),
    );
    merge(&mut n, extra);
    n
}

#[test]
fn mesh_group_deforms_children_and_bakes_static_transforms() {
    // Child translated by (1,1) statically: baked into its vertices.
    let child = part(
        4,
        "c",
        [1.0, 1.0],
        [2.0, 2.0],
        json!({"transform": {"trans": [1, 1, 0], "rot": [0, 0, 0], "scale": [1, 1]}}),
    );
    let mg = mesh_group(3, vec![child], json!({}));
    let deform = binding(
        3,
        "deform",
        json!([[[[0, 0], [0, 0], [0, 0], [0, 0]]], [[[0, 0], [10, 0], [0, 0], [10, 0]]]]),
        "Linear",
    );
    let i = import(&puppet(vec![mg], vec![param(10, "stretch", 0.0, 1.0, &[0.0, 1.0], vec![deform])]));
    let rig = rig(&i);
    assert!(!i.document.model.nodes.contains_key("inochi:4:transform"), "static transform baked");
    assert_close(positions(&rig, &eval(&rig, &[]), 4)[0], [2.0, 2.0]);
    // The group's right column moves +10: x doubles (affine on each triangle).
    assert_close(positions(&rig, &eval(&rig, &[(10, [1.0, 0.0])]), 4)[0], [4.0, 2.0]);
    assert!(!codes(&i).contains(&"MESHGROUP_OUTSIDE_POINTS"));
}

#[test]
fn mesh_group_warnings() {
    let outside = part(4, "out", [8.0, 8.0], [20.0, 20.0], json!({}));
    let animated = part(5, "anim", [1.0, 1.0], [2.0, 2.0], json!({}));
    let mut mg = mesh_group(3, vec![outside, animated], json!({"dynamic_deformation": true}));
    mg["mesh"]["verts"] = json!([0, 0, 10, 0, 0, 10, 10, 10, 5, 5]);
    mg["mesh"]["uvs"] = json!([0, 0, 1, 0, 0, 1, 1, 1, 0.5, 0.5]);
    mg["mesh"]["indices"] = json!([0, 1, 2, 1, 3, 2, 0, 4, 4]);
    let tx = binding(5, "transform.t.x", json!([[0], [1]]), "Linear");
    let i = import(&puppet(vec![mg], vec![param(10, "p", 0.0, 1.0, &[0.0, 1.0], vec![tx])]));
    rig(&i);
    let c = codes(&i);
    for code in [
        "MESHGROUP_OUTSIDE_POINTS",
        "MESHGROUP_DYNAMIC_APPROXIMATED",
        "MESHGROUP_TRANSFORM_APPROXIMATED",
        "MESHGROUP_DEGENERATE_TRIANGLES",
    ] {
        assert!(c.contains(&code), "missing {code}: {c:?}");
    }
}

// ---------- textures, parameters, errors ----------

#[test]
fn textures_tga_bc7_and_empty() {
    // 2x1 uncompressed 32-bit TGA, top-down.
    let mut tga = vec![0u8, 0, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2, 0, 1, 0, 32, 0x20];
    tga.extend([0, 0, 255, 255, 255, 0, 0, 128]);
    let p0 = part(3, "tga", [0.0, 0.0], [1.0, 1.0], json!({"textures": [0, 4294967295u64, 4294967295u64]}));
    let p1 = part(4, "bc7", [0.0, 0.0], [1.0, 1.0], json!({"textures": [1, 4294967295u64, 4294967295u64]}));
    let p2 = part(5, "empty", [0.0, 0.0], [1.0, 1.0], json!({"textures": [2, 4294967295u64, 4294967295u64]}));
    let bytes = package(
        &puppet(vec![p0, p1, p2], vec![]),
        vec![(Encoding::Tga, tga), (Encoding::Bc7, vec![7; 32]), (Encoding::Png, vec![])],
    );
    let i = riglet_inochi2d::import(&bytes, &Default::default()).unwrap();
    rig(&i);
    let c = codes(&i);
    assert!(c.contains(&"TEXTURE_UNDECODABLE") && c.contains(&"TEXTURE_EMPTY"), "{c:?}");
    let files = &i.document.files;
    assert!(files["textures/0.png"].starts_with(&[0x89, b'P', b'N', b'G']), "TGA re-encoded as PNG");
    assert_eq!(files["extensions/inochi2d/textures/1.raw2"], vec![7; 32], "BC7 bytes kept for export");
}

#[test]
fn parameters_map_axis_points_and_export_ids() {
    let b =
        binding(3, "deform", json!([[[[0, 0], [0, 0], [0, 0], [0, 0]]], [[[1, 0], [1, 0], [1, 0], [1, 0]]]]), "Linear");
    let pup = puppet(
        vec![part(3, "p", [0.0, 0.0], [1.0, 1.0], json!({}))],
        vec![param(10, "angle", -30.0, 30.0, &[0.0, 1.0], vec![b])],
    );
    let i = import(&pup);
    let m = &i.document.model;
    assert_eq!(m.parameters[0].name, "angle");
    assert_eq!(m.export_ids.inochi2d.as_ref().unwrap()["inochi:10"], 10);
    let set = &m.nodes["inochi:3"].base().keyform_sets[0];
    assert_eq!(set.axes[0].keys, [-30.0, 30.0]);
}

#[test]
fn errors_are_clean_and_coded() {
    let mut inp2 = package(&puppet(vec![], vec![]), vec![]);
    inp2[7] = b'2';
    assert_eq!(riglet_inochi2d::import(&inp2, &Default::default()).unwrap_err().code(), "INP2_UNSUPPORTED");

    // 80 nested nodes exceed serde's recursion limit.
    let mut deep = node(1000, "leaf", vec![], json!({}));
    for k in 0..80 {
        deep = node(2000 + k, "n", vec![deep], json!({}));
    }
    let bytes = package(&puppet(vec![deep], vec![]), vec![]);
    assert_eq!(riglet_inochi2d::import(&bytes, &Default::default()).unwrap_err().code(), "INP_NESTING_TOO_DEEP");

    let bad_json = riglet_inochi2d::container::write(&riglet_inochi2d::container::Container {
        json: b"{not json".to_vec(),
        textures: vec![],
        extensions: vec![],
    })
    .unwrap();
    assert_eq!(riglet_inochi2d::import(&bad_json, &Default::default()).unwrap_err().code(), "INP_JSON");
}
