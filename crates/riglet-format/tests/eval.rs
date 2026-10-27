//! Evaluator tests (spec E2–E7).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

mod common;

use common::*;
use riglet_format::model::{AxisInterpolation, Keyform, Node};

/// A single-vertex drawable at the origin, driven by keyforms that move it.
fn one_point(b: &mut Builder) {
    b.drawable("d", "root", &[[0.0, 0.0]]);
}

fn x_of(b: &Builder, values: &[(&str, [f32; 2])]) -> f32 {
    let rig = b.rig();
    positions(&rig, &eval(&rig, values), "d")[0][0]
}

// ---------- E3: interpolation inside one set ----------

#[test]
fn linear_1d_interpolates_and_clamps() {
    let mut b = Builder::new();
    b.param("p", -10.0, 10.0, 0.0);
    one_point(&mut b);
    let kfs = vec![b.positions(&[[0.0, 0.0]]), b.positions(&[[10.0, 0.0]])];
    b.set("d", &[linear("p", &[0.0, 1.0])], kfs);

    assert_close_f(x_of(&b, &[("p", [0.25, 0.0])]), 2.5);
    assert_close_f(x_of(&b, &[("p", [1.0, 0.0])]), 10.0);
    // Outside the key range: clamp to the end key, no extrapolation.
    assert_close_f(x_of(&b, &[("p", [-5.0, 0.0])]), 0.0);
    assert_close_f(x_of(&b, &[("p", [5.0, 0.0])]), 10.0);
}

#[test]
fn non_uniform_keys() {
    let mut b = Builder::new();
    b.param("p", -10.0, 10.0, 0.0);
    one_point(&mut b);
    let kfs = vec![b.positions(&[[-4.0, 0.0]]), b.positions(&[[0.0, 0.0]]), b.positions(&[[8.0, 0.0]])];
    b.set("d", &[linear("p", &[-1.0, 0.0, 3.0])], kfs);
    assert_close_f(x_of(&b, &[("p", [-0.5, 0.0])]), -2.0);
    assert_close_f(x_of(&b, &[("p", [1.5, 0.0])]), 4.0);
}

#[test]
fn bilinear_2d_row_major_axis0_fastest() {
    let mut b = Builder::new();
    b.param("a", 0.0, 1.0, 0.0).param("c", 0.0, 1.0, 0.0);
    one_point(&mut b);
    // Grid order: (a0,c0), (a1,c0), (a0,c1), (a1,c1).
    let kfs = [0.0, 10.0, 20.0, 30.0].iter().map(|&x| b.positions(&[[x, 0.0]])).collect();
    b.set("d", &[linear("a", &[0.0, 1.0]), linear("c", &[0.0, 1.0])], kfs);

    assert_close_f(x_of(&b, &[("a", [1.0, 0.0]), ("c", [0.0, 0.0])]), 10.0);
    assert_close_f(x_of(&b, &[("a", [0.0, 0.0]), ("c", [1.0, 0.0])]), 20.0);
    assert_close_f(x_of(&b, &[("a", [0.5, 0.0]), ("c", [0.5, 0.0])]), 15.0);
    // 0.75*0.25*0 + 0.25*0.25*10 + 0.75*0.75*20 + 0.25*0.75*30
    assert_close_f(x_of(&b, &[("a", [0.25, 0.0]), ("c", [0.75, 0.0])]), 17.5);
}

#[test]
fn trilinear_reproduces_a_linear_function() {
    let mut b = Builder::new();
    b.param("x", 0.0, 1.0, 0.0).param("y", 0.0, 1.0, 0.0).param("z", 0.0, 1.0, 0.0);
    one_point(&mut b);
    let mut kfs = Vec::new();
    for z in 0..2 {
        for y in 0..2 {
            for x in 0..2 {
                let v = x as f32 + 2.0 * y as f32 + 4.0 * z as f32;
                kfs.push(b.positions(&[[v, 0.0]]));
            }
        }
    }
    let keys = [0.0, 1.0];
    b.set("d", &[linear("x", &keys), linear("y", &keys), linear("z", &keys)], kfs);
    let got = x_of(&b, &[("x", [0.3, 0.0]), ("y", [0.6, 0.0]), ("z", [0.9, 0.0])]);
    assert_close_f(got, 0.3 + 1.2 + 3.6);
}

#[test]
fn step_axis_uses_lower_key() {
    let mut b = Builder::new();
    b.param("p", 0.0, 2.0, 0.0);
    one_point(&mut b);
    let kfs = vec![b.positions(&[[0.0, 0.0]]), b.positions(&[[5.0, 0.0]]), b.positions(&[[9.0, 0.0]])];
    b.set("d", &[("p", 0, &[0.0, 1.0, 2.0], AxisInterpolation::Step)], kfs);
    assert_close_f(x_of(&b, &[("p", [0.99, 0.0])]), 0.0);
    assert_close_f(x_of(&b, &[("p", [1.7, 0.0])]), 5.0);
    assert_close_f(x_of(&b, &[("p", [2.0, 0.0])]), 9.0);
}

#[test]
fn single_key_axis_is_constant() {
    let mut b = Builder::new();
    b.param("p", 0.0, 1.0, 0.0);
    one_point(&mut b);
    let kfs = vec![b.positions(&[[3.0, 0.0]])];
    b.set("d", &[linear("p", &[0.5])], kfs);
    assert_close_f(x_of(&b, &[("p", [0.0, 0.0])]), 3.0);
    assert_close_f(x_of(&b, &[("p", [1.0, 0.0])]), 3.0);
}

#[test]
fn two_d_parameter_component_axes() {
    let mut b = Builder::new();
    b.param2d("xy", [-1.0, -1.0], [1.0, 1.0]);
    one_point(&mut b);
    let kfs = vec![b.positions(&[[0.0, -10.0]]), b.positions(&[[0.0, 10.0]])];
    b.set("d", &[("xy", 1, &[-1.0, 1.0], AxisInterpolation::Linear)], kfs);
    let rig = b.rig();
    let p = positions(&rig, &eval(&rig, &[("xy", [0.9, 0.5])]), "d")[0];
    assert_close(p, [0.0, 5.0]);
}

// ---------- E4: additive sets ----------

#[test]
fn sets_on_one_node_add() {
    let mut b = Builder::new();
    b.param("a", 0.0, 1.0, 0.0).param("c", 0.0, 1.0, 0.0);
    one_point(&mut b);
    let kfs = vec![b.positions(&[[0.0, 0.0]]), b.positions(&[[10.0, 0.0]])];
    b.set("d", &[linear("a", &[0.0, 1.0])], kfs);
    let kfs = vec![b.positions(&[[0.0, 0.0]]), b.positions(&[[1.0, 5.0]])];
    b.set("d", &[linear("c", &[0.0, 1.0])], kfs);
    // A second set on the same parameter also adds.
    let kfs = vec![b.positions(&[[0.0, 0.0]]), b.positions(&[[0.0, 1.0]])];
    b.set("d", &[linear("c", &[0.0, 1.0])], kfs);

    let rig = b.rig();
    assert_close(positions(&rig, &eval(&rig, &[("a", [1.0, 0.0])]), "d")[0], [10.0, 0.0]);
    assert_close(positions(&rig, &eval(&rig, &[("a", [1.0, 0.0]), ("c", [1.0, 0.0])]), "d")[0], [11.0, 6.0]);
    assert_close(positions(&rig, &eval(&rig, &[("a", [0.5, 0.0]), ("c", [0.5, 0.0])]), "d")[0], [5.5, 3.0]);
}

#[test]
fn missing_properties_are_zero_deltas() {
    let mut b = Builder::new();
    b.param("p", 0.0, 1.0, 0.0);
    one_point(&mut b);
    let kfs = vec![Keyform::default(), b.positions(&[[4.0, 0.0]])];
    b.set("d", &[linear("p", &[0.0, 1.0])], kfs);
    assert_close_f(x_of(&b, &[("p", [0.5, 0.0])]), 2.0);
}

// ---------- E2: parameter values ----------

#[test]
fn repeat_wraps_value_but_not_interpolation() {
    let mut b = Builder::new();
    b.param("r", 0.0, 360.0, 0.0);
    b.model.parameters[0].repeat = Some(true);
    one_point(&mut b);
    let kfs = vec![b.positions(&[[0.0, 0.0]]), b.positions(&[[36.0, 0.0]])];
    b.set("d", &[linear("r", &[0.0, 360.0])], kfs);
    assert_close_f(x_of(&b, &[("r", [400.0, 0.0])]), 4.0);
    assert_close_f(x_of(&b, &[("r", [-10.0, 0.0])]), 35.0);
    // 360 wraps to 0: the value wraps, interpolation does not.
    assert_close_f(x_of(&b, &[("r", [360.0, 0.0])]), 0.0);
}

#[test]
fn non_finite_values_use_default() {
    let mut b = Builder::new();
    b.param("p", 0.0, 1.0, 0.5);
    one_point(&mut b);
    let kfs = vec![b.positions(&[[0.0, 0.0]]), b.positions(&[[10.0, 0.0]])];
    b.set("d", &[linear("p", &[0.0, 1.0])], kfs);
    assert_close_f(x_of(&b, &[("p", [f32::NAN, 0.0])]), 5.0);
}

// ---------- E4: opacity, colors, draw order, visibility ----------

#[test]
fn opacity_multiplies_down_the_tree_and_clamps() {
    let mut b = Builder::new();
    b.param("p", 0.0, 1.0, 0.0);
    b.group("g", "root", 0.5).drawable("d", "g", &[[0.0, 0.0]]);
    if let Node::Drawable(d) = b.node_mut("d") {
        d.rest.opacity = 0.8;
    }
    let kfs = vec![Keyform::default(), Keyform { opacity: Some(5.0), ..Default::default() }];
    b.set("d", &[linear("p", &[0.0, 1.0])], kfs);
    let rig = b.rig();
    assert_close_f(eval(&rig, &[]).drawables[0].opacity, 0.4);
    assert_close_f(eval(&rig, &[("p", [1.0, 0.0])]).drawables[0].opacity, 0.5);
}

#[test]
fn colors_clamp_to_unit_range() {
    let mut b = Builder::new();
    b.param("p", 0.0, 1.0, 0.0);
    one_point(&mut b);
    let kfs = vec![
        Keyform::default(),
        Keyform { multiply_color: Some([-2.0, 0.0, 0.5]), screen_color: Some([0.25, 3.0, 0.0]), ..Default::default() },
    ];
    b.set("d", &[linear("p", &[0.0, 1.0])], kfs);
    let rig = b.rig();
    let d = &eval(&rig, &[("p", [1.0, 0.0])]).drawables[0];
    for (got, want) in d.multiply_color.iter().zip([0.0, 1.0, 1.0]) {
        assert_close_f(*got, want);
    }
    for (got, want) in d.screen_color.iter().zip([0.25, 1.0, 0.0]) {
        assert_close_f(*got, want);
    }
}

fn crossing_draw_order() -> Builder {
    let mut b = Builder::new();
    b.param("p", 0.0, 1.0, 0.0);
    b.drawable("a", "root", &[[0.0, 0.0]]).drawable("b", "root", &[[0.0, 0.0]]);
    if let Node::Drawable(d) = b.node_mut("b") {
        d.rest.draw_order = 5.0;
    }
    let kfs = vec![Keyform::default(), Keyform { draw_order: Some(10.0), ..Default::default() }];
    b.set("a", &[linear("p", &[0.0, 1.0])], kfs);
    b
}

fn order(b: &Builder, p: f32) -> Vec<String> {
    let rig = b.rig();
    eval(&rig, &[("p", [p, 0.0])]).in_draw_order().map(|d| rig.nodes[d.node].id.clone()).collect()
}

#[test]
fn draw_order_uses_nearest_key_not_interpolation() {
    let b = crossing_draw_order();
    // Interpolated, `a` would reach 5 at p = 0.5 and flicker; nearest key
    // keeps it at 0 until p passes 0.5 (ties go to the lower key).
    assert_eq!(order(&b, 0.4), ["a", "b"]);
    assert_eq!(order(&b, 0.5), ["a", "b"]);
    assert_eq!(order(&b, 0.6), ["b", "a"]);
    let rig = b.rig();
    let a = rig.node_index("a").unwrap();
    let frame = eval(&rig, &[("p", [0.6, 0.0])]);
    assert_close_f(frame.drawable(a).unwrap().draw_order, 10.0);
}

#[test]
fn draw_order_ties_break_by_tree_pre_order() {
    let mut b = Builder::new();
    b.group("g", "root", 1.0);
    b.drawable("late", "root", &[[0.0, 0.0]]);
    b.drawable("early", "g", &[[0.0, 0.0]]);
    // Tree order: root, g, early, late (g is listed before late).
    let rig = b.rig();
    let out = eval(&rig, &[]);
    let ids: Vec<&str> = out.in_draw_order().map(|d| rig.nodes[d.node].id.as_str()).collect();
    assert_eq!(ids, ["early", "late"]);
}

#[test]
fn draw_order_step_axis_uses_lower_key() {
    let mut b = crossing_draw_order();
    if let Node::Drawable(d) = b.node_mut("a") {
        d.base.keyform_sets[0].axes[0].interpolation = AxisInterpolation::Step;
    }
    assert_eq!(order(&b, 0.9), ["a", "b"]);
}

#[test]
fn hidden_ancestor_hides_drawable() {
    let mut b = Builder::new();
    b.group("g", "root", 1.0).drawable("d", "g", &[[0.0, 0.0]]);
    b.node_mut("g").base_mut().visible = false;
    let rig = b.rig();
    assert!(!eval(&rig, &[]).drawables[0].visible);
}

// ---------- E7: warp ----------

const SQUARE: [[f32; 2]; 4] = [[0.0, 0.0], [100.0, 0.0], [0.0, 100.0], [100.0, 100.0]];

#[test]
fn child_of_warp_uses_normalized_coordinates() {
    let mut b = Builder::new();
    b.warp("w", "root", 1, 1, &SQUARE);
    b.drawable("d", "w", &[[0.25, 0.5], [1.5, -0.5], [-1.0, -1.0]]);
    let rig = b.rig();
    let p = positions(&rig, &eval(&rig, &[]), "d");
    assert_close(p[0], [25.0, 50.0]);
    // Outside [0,1]^2: linear extrapolation, corners along both axes.
    assert_close(p[1], [150.0, -50.0]);
    assert_close(p[2], [-100.0, -100.0]);
}

#[test]
fn warp_extrapolation_extends_the_edge_cell_bilinear_map() {
    let mut b = Builder::new();
    b.warp("w", "root", 1, 1, &[[0.0, 0.0], [100.0, 0.0], [0.0, 100.0], [200.0, 200.0]]);
    b.drawable("d", "w", &[[2.0, 0.5]]);
    let rig = b.rig();
    // s = 2, t = 0.5 on the only cell, not clamped.
    assert_close(positions(&rig, &eval(&rig, &[]), "d")[0], [300.0, 150.0]);
}

#[test]
fn multi_cell_warp_picks_the_right_cell() {
    let mut b = Builder::new();
    // 2 cols x 1 row; the middle column is pushed right by 20.
    let pts = [[0.0, 0.0], [70.0, 0.0], [100.0, 0.0], [0.0, 100.0], [70.0, 100.0], [100.0, 100.0]];
    b.warp("w", "root", 1, 2, &pts);
    b.drawable("d", "w", &[[0.25, 0.0], [0.75, 0.0], [0.5, 1.0]]);
    let rig = b.rig();
    let p = positions(&rig, &eval(&rig, &[]), "d");
    assert_close(p[0], [35.0, 0.0]);
    assert_close(p[1], [85.0, 0.0]);
    assert_close(p[2], [70.0, 100.0]);
}

#[test]
fn warp_keyforms_move_children() {
    let mut b = Builder::new();
    b.param("p", 0.0, 1.0, 0.0);
    b.warp("w", "root", 1, 1, &SQUARE);
    b.drawable("d", "w", &[[0.5, 0.5]]);
    let kfs = vec![b.positions(&[[0.0, 0.0]; 4]), b.positions(&[[10.0, 0.0], [10.0, 0.0], [10.0, 0.0], [30.0, 0.0]])];
    b.set("w", &[linear("p", &[0.0, 1.0])], kfs);
    let rig = b.rig();
    assert_close(positions(&rig, &eval(&rig, &[("p", [1.0, 0.0])]), "d")[0], [65.0, 50.0]);
}

#[test]
fn nested_warps_compose() {
    let mut b = Builder::new();
    b.warp("outer", "root", 1, 1, &SQUARE);
    // Inner warp covers the outer's right half, in outer-normalized space.
    b.warp("inner", "outer", 1, 1, &[[0.5, 0.0], [1.0, 0.0], [0.5, 1.0], [1.0, 1.0]]);
    b.drawable("d", "inner", &[[0.5, 0.5]]);
    let rig = b.rig();
    assert_close(positions(&rig, &eval(&rig, &[]), "d")[0], [75.0, 50.0]);
}

#[test]
fn groups_pass_through_parent_space() {
    let mut b = Builder::new();
    b.warp("w", "root", 1, 1, &SQUARE);
    b.group("g", "w", 1.0).drawable("d", "g", &[[0.5, 0.5]]);
    let rig = b.rig();
    assert_close(positions(&rig, &eval(&rig, &[]), "d")[0], [50.0, 50.0]);
}

// ---------- E5: rotation ----------

#[test]
fn rotation_under_root() {
    let mut b = Builder::new();
    b.param("p", 0.0, 1.0, 0.0);
    b.rotation("r", "root", [100.0, 0.0], 90.0, 2.0);
    b.drawable("d", "r", &[[10.0, 0.0]]);
    let kfs = vec![Keyform::default(), Keyform { angle: Some(-90.0), origin: Some([0.0, 5.0]), ..Default::default() }];
    b.set("r", &[linear("p", &[0.0, 1.0])], kfs);
    let rig = b.rig();
    // +90 degrees is clockwise on screen: +X maps to +Y.
    assert_close(positions(&rig, &eval(&rig, &[]), "d")[0], [100.0, 20.0]);
    assert_close(positions(&rig, &eval(&rig, &[("p", [1.0, 0.0])]), "d")[0], [120.0, 5.0]);
}

#[test]
fn rotation_scale_clamps_to_minimum() {
    let mut b = Builder::new();
    b.param("p", 0.0, 1.0, 0.0);
    b.rotation("r", "root", [0.0, 0.0], 0.0, 1.0);
    b.drawable("d", "r", &[[10.0, 0.0]]);
    let kfs = vec![Keyform::default(), Keyform { scale: Some(-3.0), ..Default::default() }];
    b.set("r", &[linear("p", &[0.0, 1.0])], kfs);
    let rig = b.rig();
    let x = positions(&rig, &eval(&rig, &[("p", [1.0, 0.0])]), "d")[0][0];
    assert!(x > 0.0 && x < 1e-3, "scale must clamp to a small positive value, got x = {x}");
}

#[test]
fn rotation_under_rotation_composes() {
    let mut b = Builder::new();
    b.rotation("outer", "root", [100.0, 100.0], 90.0, 1.0);
    b.rotation("inner", "outer", [10.0, 0.0], 90.0, 1.0);
    b.drawable("d", "inner", &[[1.0, 0.0]]);
    let rig = b.rig();
    // inner: (1,0) -> (10,0) + (0,1) = (10,1); outer: (100,100) + R90(10,1) = (99,110).
    assert_close(positions(&rig, &eval(&rig, &[]), "d")[0], [99.0, 110.0]);
}

/// Warp covering [0,200]^2 at rest; `transform` maps rest points to current.
fn rotation_under_warp(transform: impl Fn([f32; 2]) -> [f32; 2]) -> Vec<[f32; 2]> {
    let mut b = Builder::new();
    b.param("p", 0.0, 1.0, 0.0);
    let rest = [[0.0, 0.0], [200.0, 0.0], [0.0, 200.0], [200.0, 200.0]];
    b.warp("w", "root", 1, 1, &rest);
    let deltas: Vec<[f32; 2]> = rest
        .iter()
        .map(|&p| {
            let q = transform(p);
            [q[0] - p[0], q[1] - p[1]]
        })
        .collect();
    let kfs = vec![b.positions(&[[0.0, 0.0]; 4]), b.positions(&deltas)];
    b.set("w", &[linear("p", &[0.0, 1.0])], kfs);
    b.rotation("r", "w", [0.5, 0.5], 0.0, 1.0);
    b.drawable("d", "r", &[[0.0, 0.0], [10.0, 0.0], [0.0, 10.0]]);
    let rig = b.rig();
    positions(&rig, &eval(&rig, &[("p", [1.0, 0.0])]), "d")
}

#[test]
fn rotation_frame_is_pixels_at_rest_under_warp() {
    let p = rotation_under_warp(|p| p);
    assert_close(p[0], [100.0, 100.0]);
    assert_close(p[1], [110.0, 100.0]);
}

#[test]
fn rotation_frame_follows_warp_rotation_and_scale() {
    // Current warp = rest rotated +90 degrees about the origin, scaled by 2.
    let p = rotation_under_warp(|[x, y]| [-2.0 * y, 2.0 * x]);
    assert_close(p[0], [-200.0, 200.0]);
    assert_close(p[1], [-200.0, 220.0]);
    assert_close(p[2], [-220.0, 200.0]);
}

#[test]
fn rotation_frame_under_sheared_warp_stays_rigid() {
    // x' = x + 0.5 y: pure shear, det = 1.
    let p = rotation_under_warp(|[x, y]| [x + 0.5 * y, y]);
    let len = |a: [f32; 2], b: [f32; 2]| ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt();
    let dot = (p[1][0] - p[0][0]) * (p[2][0] - p[0][0]) + (p[1][1] - p[0][1]) * (p[2][1] - p[0][1]);
    assert_close_f(len(p[0], p[1]), 10.0);
    assert_close_f(len(p[0], p[2]), 10.0);
    assert_close_f(dot, 0.0);
    // Rotation part of [[1, 0.5], [0, 1]]: atan2(0 - 0.5, 2).
    let expected = (-0.5f32).atan2(2.0);
    assert_close_f((p[1][1] - p[0][1]).atan2(p[1][0] - p[0][0]), expected);
}

// ---------- E6: mesh deformer ----------

fn mesh_rig(child: &[[f32; 2]]) -> Builder {
    let mut b = Builder::new();
    b.param("p", 0.0, 1.0, 0.0);
    let pts = [[0.0, 0.0], [100.0, 0.0], [0.0, 100.0], [100.0, 100.0]];
    b.mesh_deformer("m", "root", &pts, &[0, 1, 2, 1, 3, 2]);
    // Move vertex 1 right by 100.
    let kfs = vec![b.positions(&[[0.0, 0.0]; 4]), b.positions(&[[0.0, 0.0], [100.0, 0.0], [0.0, 0.0], [0.0, 0.0]])];
    b.set("m", &[linear("p", &[0.0, 1.0])], kfs);
    b.drawable("d", "m", child);
    b
}

#[test]
fn mesh_deformer_moves_bound_points() {
    let b = mesh_rig(&[[25.0, 25.0], [75.0, 75.0]]);
    let rig = b.rig();
    let rest = positions(&rig, &eval(&rig, &[]), "d");
    assert_close(rest[0], [25.0, 25.0]);
    let p = positions(&rig, &eval(&rig, &[("p", [1.0, 0.0])]), "d");
    // Barycentric (0.5, 0.25, 0.25) on triangle 0.
    assert_close(p[0], [50.0, 25.0]);
    // Triangle 1 = (1, 3, 2): (75,75) has weights (0.25, 0.5, 0.25).
    assert_close(p[1], [100.0, 75.0]);
}

#[test]
fn mesh_deformer_extrapolates_outside_points() {
    let b = mesh_rig(&[[-50.0, 0.0]]);
    let rig = b.rig();
    // Nearest triangle is 0; barycentric (1.5, -0.5, 0) applied unclamped.
    assert_close(positions(&rig, &eval(&rig, &[("p", [1.0, 0.0])]), "d")[0], [-100.0, 0.0]);
}

#[test]
fn mesh_deformer_keeps_child_keyform_deltas() {
    let mut b = mesh_rig(&[[25.0, 25.0]]);
    let kfs = vec![b.positions(&[[0.0, 0.0]]), b.positions(&[[0.0, 10.0]])];
    b.set("d", &[linear("p", &[0.0, 1.0])], kfs);
    let rig = b.rig();
    // Child moves to (25,35) first, then maps through deformed triangle 0
    // with barycentric (0.4, 0.25, 0.35).
    assert_close(positions(&rig, &eval(&rig, &[("p", [1.0, 0.0])]), "d")[0], [50.0, 35.0]);
}

#[test]
fn rotation_under_mesh_deformer_follows_triangle() {
    let mut b = Builder::new();
    b.param("p", 0.0, 1.0, 0.0);
    let pts = [[0.0, 0.0], [100.0, 0.0], [0.0, 100.0]];
    b.mesh_deformer("m", "root", &pts, &[0, 1, 2]);
    // Rotate the triangle +90 degrees about the origin.
    let deltas = pts.map(|[x, y]| [-y - x, x - y]);
    let kfs = vec![b.positions(&[[0.0, 0.0]; 3]), b.positions(&deltas)];
    b.set("m", &[linear("p", &[0.0, 1.0])], kfs);
    b.rotation("r", "m", [20.0, 10.0], 0.0, 1.0);
    b.drawable("d", "r", &[[10.0, 0.0]]);
    let rig = b.rig();
    // Origin (20,10) -> (-10,20); child offset (10,0) rotated -> (0,10).
    assert_close(positions(&rig, &eval(&rig, &[("p", [1.0, 0.0])]), "d")[0], [-10.0, 30.0]);
}

// ---------- EvalOutput reuse ----------

#[test]
fn eval_output_is_reused_across_frames() {
    let mut b = Builder::new();
    b.param("p", 0.0, 1.0, 0.0);
    b.warp("w", "root", 1, 1, &SQUARE);
    b.drawable("d", "w", &[[0.25, 0.5], [0.5, 0.5], [0.75, 0.5]]);
    b.drawable("e", "root", &[[1.0, 1.0]]);
    let kfs = vec![b.positions(&[[0.0, 0.0]; 3]), b.positions(&[[0.1, 0.0]; 3])];
    b.set("d", &[linear("p", &[0.0, 1.0])], kfs);
    let rig = b.rig();

    let mut params = riglet_format::ParamValues::defaults(&rig);
    let mut out = riglet_format::EvalOutput::new();
    rig.evaluate(&params, &mut out);
    let first = out.drawables.clone();
    let buffer = out.drawables[0].positions.as_ptr();

    params.set(&rig, "p", [1.0, 0.0]);
    rig.evaluate(&params, &mut out);
    assert_eq!(out.drawables[0].positions.as_ptr(), buffer, "positions buffer was reallocated");
    assert_close(out.drawables[0].positions[0], [35.0, 50.0]);

    // Same input again gives the same output as a fresh EvalOutput.
    params.set(&rig, "p", [0.0, 0.0]);
    rig.evaluate(&params, &mut out);
    assert_eq!(out.drawables, first);
    // Slots are stable and in pre-order.
    let ids: Vec<&str> = out.drawables.iter().map(|d| rig.nodes[d.node].id.as_str()).collect();
    assert_eq!(ids, ["d", "e"]);
}
