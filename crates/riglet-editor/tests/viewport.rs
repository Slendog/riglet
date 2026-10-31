//! Viewport helpers without a GPU: physical-pixel snapping at fractional
//! scale factors, and hide-in-view.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

use std::collections::BTreeSet;

use riglet_editor::viewport::{hide_in_view, pixel_rect};
use riglet_format::{EvalOutput, Limits, ParamValues, Rig};

/// At 1, 1.25, 1.5, 1.75, 2, 2.25 and 3 pixels per point, for many panel
/// positions and sizes: the snapped rectangle maps back to whole physical
/// pixels, and sampling the target at each covered pixel's center (as the
/// GPU does with nearest filtering over min_points..max_points) picks
/// exactly texel i for pixel i: no skipped or doubled rows or columns.
#[test]
fn pixel_snapping_is_exact_at_fractional_scale_factors() {
    for ppp in [1.0f32, 1.25, 1.5, 1.75, 2.0, 2.25, 3.0] {
        for i in 0..200 {
            let x0 = 0.37 * i as f32 + 13.1;
            let y0 = 0.61 * i as f32 + 47.3;
            let (w, h) = (100.0 + 3.7 * i as f32, 80.0 + 2.9 * i as f32);
            let r = pixel_rect([x0, y0], [x0 + w, y0 + h], ppp);
            for (points, pixel) in [(r.min_points[0], r.origin[0]), (r.min_points[1], r.origin[1])] {
                assert_eq!(pixel.fract(), 0.0);
                assert!((points * ppp - pixel).abs() < 1e-3, "ppp {ppp}: {points} pt -> {} px", points * ppp);
            }
            let span = [r.max_points[0] * ppp - r.min_points[0] * ppp, r.max_points[1] * ppp - r.min_points[1] * ppp];
            assert!((span[0] - r.width as f32).abs() < 1e-3 && (span[1] - r.height as f32).abs() < 1e-3);
            // Nearest sampling along x and y.
            for (n, lo, hi) in [
                (r.width, r.min_points[0] * ppp, r.max_points[0] * ppp),
                (r.height, r.min_points[1] * ppp, r.max_points[1] * ppp),
            ] {
                for k in 0..n {
                    // Pixel centers of the rasterized quad: whole pixel + 0.5.
                    let center = lo.round() + k as f32 + 0.5;
                    let uv = (center - lo) / (hi - lo);
                    assert_eq!((uv * n as f32).floor() as u32, k, "ppp {ppp}, size {n}, pixel {k}");
                }
            }
        }
    }
    // Bad scale factors fall back to 1.
    assert_eq!(pixel_rect([0.0, 0.0], [10.0, 10.0], f32::NAN).width, 10);
    assert_eq!(pixel_rect([0.0, 0.0], [10.0, 10.0], 0.0).width, 10);
    assert_eq!(pixel_rect([10.0, 0.0], [0.0, 10.0], 1.0).width, 0);
}

/// Hiding a group hides every drawable and composite under it, only in the
/// shown frame; the evaluation output itself is untouched.
#[test]
fn hide_in_view_hides_descendants_only_in_the_shown_frame() {
    let mut b = riglet_fixtures::Builder::new();
    b.group("g", "root", 1.0);
    b.composite("c", "g", 1.0, 0.0);
    b.quad("a", "c", "tex", [0.0, 0.0], [1.0, 1.0]);
    b.quad("b", "root", "tex", [0.0, 0.0], [1.0, 1.0]);
    let doc = b.document();
    let rig = Rig::prepare(&doc, &Limits::default()).unwrap();
    let mut frame = EvalOutput::new();
    rig.evaluate(&ParamValues::defaults(&rig), &mut frame);
    let id_of = |node: usize| rig.nodes[node].id.as_str();
    let mut out = EvalOutput::new();

    hide_in_view(&rig, &frame, &BTreeSet::new(), &mut out);
    assert_eq!(out.drawables, frame.drawables);
    assert_eq!(out.composites, frame.composites);

    hide_in_view(&rig, &frame, &BTreeSet::from(["g".to_owned()]), &mut out);
    for d in &out.drawables {
        assert_eq!(d.visible, id_of(d.node) == "b", "{}", id_of(d.node));
    }
    assert!(out.composites.iter().all(|c| !c.visible));
    assert!(frame.drawables.iter().all(|d| d.visible), "the evaluation is unchanged");

    hide_in_view(&rig, &frame, &BTreeSet::from(["b".to_owned()]), &mut out);
    for d in &out.drawables {
        assert_eq!(d.visible, id_of(d.node) == "a");
    }
}

/// The outline of a node is the evaluated edges of the drawables at or
/// under it, each shared edge once, skipping non-finite ends.
#[test]
fn outline_edges_cover_the_selection_once() {
    use riglet_editor::viewport::outline_edges;
    let mut b = riglet_fixtures::Builder::new();
    b.group("g", "root", 1.0);
    b.quad("a", "g", "tex", [0.0, 0.0], [1.0, 1.0]);
    b.quad("b", "root", "tex", [2.0, 0.0], [3.0, 1.0]);
    let doc = b.document();
    let rig = Rig::prepare(&doc, &Limits::default()).unwrap();
    let mut frame = EvalOutput::new();
    rig.evaluate(&ParamValues::defaults(&rig), &mut frame);
    // A quad: 2 triangles, 5 distinct edges (the diagonal once).
    assert_eq!(outline_edges(&rig, &frame, "a").len(), 5);
    assert_eq!(outline_edges(&rig, &frame, "g").len(), 5);
    assert_eq!(outline_edges(&rig, &frame, "root").len(), 10);
    assert!(outline_edges(&rig, &frame, "nope").is_empty());
    assert!(outline_edges(&rig, &frame, "b").iter().all(|[p, _]| p[0] >= 2.0));
    // A non-finite vertex drops the edges that use it (vertex 1 is on
    // edges 0-1 and 1-2 only).
    let slot = frame.drawables.iter().position(|d| rig.nodes[d.node].id == "a").unwrap();
    frame.drawables[slot].positions[1] = [f32::INFINITY, 0.0];
    let edges = outline_edges(&rig, &frame, "a");
    assert_eq!(edges.len(), 3);
    assert!(edges.iter().flatten().flatten().all(|v| v.is_finite()));
}
