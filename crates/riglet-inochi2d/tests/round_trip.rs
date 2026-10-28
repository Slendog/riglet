//! Inochi2D -> Riglet -> Inochi2D -> Riglet: both Riglet models must
//! evaluate the same (vertices, opacity, visibility, draw order) at sampled
//! parameter values.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

mod common;

use common::*;
use riglet_format::{EvalOutput, Limits, ParamValues, Rig};
use serde_json::json;

/// Parameter (uuid, min, max) of a Riglet model, by `inochi:<uuid>` id.
fn params(rig_doc: &riglet_format::Document) -> Vec<(String, Vec<f32>, Vec<f32>)> {
    rig_doc.model.parameters.iter().map(|p| (p.id.clone(), p.min.clone(), p.max.clone())).collect()
}

/// Round trip and compare; returns the export warnings.
fn round_trip(puppet: &serde_json::Value) -> Vec<String> {
    let first = import(puppet);
    let exported = riglet_inochi2d::export(&first.document).unwrap_or_else(|e| panic!("export: {} ({e})", e.code()));
    let second = riglet_inochi2d::import(&exported.bytes, &Default::default())
        .unwrap_or_else(|e| panic!("re-import: {} ({e})", e.code()));
    let (a, b) = (rig(&first), rig(&second));
    let ps = params(&first.document);
    // Samples: defaults, each parameter at 0, 0.25, ..., 1 of its range,
    // and a few combinations.
    let mut samples: Vec<Vec<(String, [f32; 2])>> = vec![vec![]];
    for (id, lo, hi) in &ps {
        for t in [0.0, 0.25, 0.4, 0.5, 0.75, 1.0] {
            let v = [0, 1].map(|c| lo.get(c).map_or(0.0, |l| l + t * (hi[c] - l)));
            samples.push(vec![(id.clone(), v)]);
        }
    }
    for k in 0..8 {
        let t = (k as f32 * 0.37).fract();
        samples.push(
            ps.iter()
                .map(|(id, lo, hi)| (id.clone(), [0, 1].map(|c| lo.get(c).map_or(0.0, |l| l + t * (hi[c] - l)))))
                .collect(),
        );
    }
    let (mut oa, mut ob) = (EvalOutput::new(), EvalOutput::new());
    for s in &samples {
        let mut pa = ParamValues::defaults(&a);
        let mut pb = ParamValues::defaults(&b);
        for (id, v) in s {
            assert!(pa.set(&a, id, *v));
            assert!(pb.set(&b, id, *v), "parameter {id} lost in the round trip");
        }
        a.evaluate(&pa, &mut oa);
        b.evaluate(&pb, &mut ob);
        for (i, da) in oa.drawables.iter().enumerate() {
            let id = &a.nodes[da.node].id;
            let uuid = id.trim_start_matches("inochi:").split(':').next().unwrap();
            let j = b
                .node_index(&format!("inochi:{uuid}"))
                .filter(|&j| ob.drawable(j).is_some())
                .or_else(|| b.node_index(&format!("inochi:{uuid}:part")))
                .unwrap_or_else(|| panic!("drawable {id} lost"));
            let db = ob.drawable(j).unwrap();
            assert_eq!(da.positions.len(), db.positions.len(), "{id}");
            for (p, q) in da.positions.iter().zip(&db.positions) {
                assert!((p[0] - q[0]).abs() < 1e-3 && (p[1] - q[1]).abs() < 1e-3, "{id} at {s:?}: {p:?} vs {q:?}");
            }
            assert!((da.opacity - db.opacity).abs() < 1e-4, "{id} opacity at {s:?}: {} vs {}", da.opacity, db.opacity);
            assert_eq!(da.visible, db.visible, "{id} visibility");
            assert!(
                (da.draw_order - db.draw_order).abs() < 1e-4,
                "{id} draw order at {s:?}: {} vs {}",
                da.draw_order,
                db.draw_order
            );
            let _ = i;
        }
    }
    exported.warnings.iter().map(|w| w.code.clone()).collect()
}

fn translated(x: f32, y: f32) -> serde_json::Value {
    json!({"transform": {"trans": [x, y, 0], "rot": [0, 0, 0], "scale": [1, 1]}})
}

#[test]
fn transforms_and_value_bindings() {
    let child = part(5, "child", [0.0, 0.0], [2.0, 1.0], json!({"zsort": 1.0}));
    let p = part(
        4,
        "p",
        [-1.0, -1.0],
        [1.0, 1.0],
        json!({"transform": {"trans": [10, 5, 0], "rot": [0, 0, 0.3], "scale": [2, 0.5]}, "children": [child], "opacity": 0.8}),
    );
    let n = node(3, "n", vec![p], translated(-3.0, 2.0));
    let bindings = vec![
        binding(4, "transform.t.x", json!([[0], [4], [1]]), "Linear"),
        binding(4, "transform.r.z", json!([[0], [0.5], [-0.2]]), "Linear"),
        binding(4, "transform.s.y", json!([[1], [2], [0.5]]), "Linear"),
        binding(4, "opacity", json!([[1], [0.5], [0.25]]), "Linear"),
        binding(
            4,
            "deform",
            json!([
                [[[0, 0], [0, 0], [0, 0], [0, 0]]],
                [[[1, 0], [0, 1], [0, 0], [2, 2]]],
                [[[0, 0], [0, 0], [0, 0], [0, 0]]]
            ]),
            "Linear",
        ),
        binding(5, "zSort", json!([[0], [2], [-1]]), "Linear"),
    ];
    let w = round_trip(&puppet(vec![n], vec![param(10, "p", -1.0, 1.0, &[0.0, 0.5, 1.0], bindings)]));
    assert!(w.is_empty(), "{w:?}");
}

#[test]
fn two_d_cubic_and_nearest_bindings() {
    let p = part(4, "p", [0.0, 0.0], [1.0, 1.0], json!({}));
    let q = part(5, "q", [2.0, 0.0], [3.0, 1.0], json!({}));
    let d2 = binding(4, "transform.t.y", json!([[0, 1], [2, 3]]), "Linear");
    let cubic = binding(5, "transform.t.x", json!([[0], [3], [-1], [2]]), "Cubic");
    let pz = binding(4, "zSort", json!([[0], [1], [0], [2]]), "Nearest");
    let w = round_trip(&puppet(
        vec![p, q],
        vec![
            param2d(10, "xy", &[0.0, 1.0], &[0.0, 1.0], vec![d2]),
            param(11, "c", 0.0, 1.0, &[0.0, 0.3, 0.6, 1.0], vec![cubic, pz]),
        ],
    ));
    // Nearest imports as step axes with midpoint keys; export recognizes the
    // shape and writes Nearest at the original keys (so the cubic binding on
    // the same parameter keeps its axis points and stays Cubic).
    assert!(w.is_empty(), "{w:?}");
}

#[test]
fn mesh_groups_with_static_and_animated_children() {
    // MeshGroup -> static translated part, animated part (pointwise),
    // translate_children node with a leaf; nested static MeshGroup.
    let stat = part(5, "static", [1.0, 1.0], [2.0, 2.0], translated(1.0, 1.0));
    // An animated part inside another animated part: nested in a
    // pointwise frame, exact (no MESHGROUP_RIGID_APPROXIMATED).
    let nested = part(13, "nested", [0.0, 0.0], [1.0, 1.0], translated(0.5, 0.5));
    let anim = part(6, "anim", [3.0, 3.0], [4.0, 4.0], json!({"children": [nested]}));
    let leaf = part(8, "leaf", [0.0, 0.0], [1.0, 1.0], json!({}));
    let n = node(7, "n", vec![leaf], translated(4.0, 5.0));
    let inner_child = part(10, "ic", [1.0, 1.0], [2.0, 2.0], json!({}));
    let mut inner = node(
        9,
        "inner",
        vec![inner_child],
        json!({"type": "MeshGroup", "transform": {"trans": [2, 3, 0], "rot": [0, 0, 0], "scale": [1, 1]}}),
    );
    inner["mesh"] = json!({"verts": [0, 0, 6, 0, 0, 6, 6, 6], "uvs": [0, 0, 1, 0, 0, 1, 1, 1], "indices": [0, 1, 2, 1, 3, 2], "origin": [0, 0]});
    let mut mg = node(3, "mg", vec![stat, anim, n, inner], json!({"type": "MeshGroup", "translate_children": true}));
    mg["mesh"] = json!({"verts": [0, 0, 10, 0, 0, 10, 10, 10], "uvs": [0, 0, 1, 0, 0, 1, 1, 1], "indices": [0, 1, 2, 1, 3, 2], "origin": [0, 0]});
    let stretch = binding(
        3,
        "deform",
        json!([[[[0, 0], [0, 0], [0, 0], [0, 0]]], [[[0, 0], [10, 0], [0, 0], [10, 0]]]]),
        "Linear",
    );
    let tx = binding(6, "transform.t.x", json!([[0], [2]]), "Linear");
    let ty = binding(13, "transform.t.y", json!([[0], [1]]), "Linear");
    let d =
        binding(5, "deform", json!([[[[0, 0], [0, 0], [0, 0], [0, 0]]], [[[1, 0], [1, 0], [1, 0], [1, 0]]]]), "Linear");
    let w = round_trip(&puppet(
        vec![mg],
        vec![
            param(11, "stretch", 0.0, 1.0, &[0.0, 1.0], vec![stretch]),
            param(12, "m", 0.0, 1.0, &[0.0, 1.0], vec![tx, ty, d]),
        ],
    ));
    assert!(w.is_empty(), "{w:?}");
}

#[test]
fn colors_masks_composites_and_blend_modes() {
    let src = part(
        5,
        "src",
        [0.0, 0.0],
        [2.0, 2.0],
        json!({"blend_mode": "Screen", "tint": [0.8, 0.6, 1.0], "screenTint": [0.1, 0.0, 0.2]}),
    );
    let masked = part(6, "masked", [0.0, 0.0], [1.0, 1.0], json!({"masks": [{"source": 5, "mode": "DodgeMask"}]}));
    let comp = node(
        4,
        "comp",
        vec![masked],
        json!({"type": "Composite", "blend_mode": "Multiply", "opacity": 0.7, "tint": [1, 1, 1], "screenTint": [0, 0, 0]}),
    );
    let tint = binding(5, "tint.r", json!([[1], [0.5]]), "Linear");
    let screen = binding(5, "screenTint.b", json!([[0], [0.4]]), "Linear");
    let op = binding(4, "opacity", json!([[1], [0.5]]), "Linear");
    let first = import(&puppet(vec![src, comp], vec![param(10, "c", 0.0, 1.0, &[0.0, 1.0], vec![tint, screen, op])]));
    let w = round_trip(&puppet(
        vec![
            part(
                5,
                "src",
                [0.0, 0.0],
                [2.0, 2.0],
                json!({"blend_mode": "Screen", "tint": [0.8, 0.6, 1.0], "screenTint": [0.1, 0.0, 0.2]}),
            ),
            node(
                4,
                "comp",
                vec![part(6, "masked", [0.0, 0.0], [1.0, 1.0], json!({"masks": [{"source": 5, "mode": "DodgeMask"}]}))],
                json!({"type": "Composite", "blend_mode": "Multiply", "opacity": 0.7, "tint": [1, 1, 1], "screenTint": [0, 0, 0]}),
            ),
        ],
        vec![param(
            10,
            "c",
            0.0,
            1.0,
            &[0.0, 1.0],
            vec![
                binding(5, "tint.r", json!([[1], [0.5]]), "Linear"),
                binding(5, "screenTint.b", json!([[0], [0.4]]), "Linear"),
                binding(4, "opacity", json!([[1], [0.5]]), "Linear"),
            ],
        )],
    ));
    assert!(w.is_empty(), "{w:?}");
    // Colors, masks and blend modes survive too.
    let exported = riglet_inochi2d::export(&first.document).unwrap();
    let second = riglet_inochi2d::import(&exported.bytes, &Default::default()).unwrap();
    let (ra, rb) = (rig(&first), rig(&second));
    for v in [0.0f32, 1.0] {
        let (ea, eb) = (eval(&ra, &[(10, [v, 0.0])]), eval(&rb, &[(10, [v, 0.0])]));
        let (ia, ib) = (ra.node_index("inochi:5").unwrap(), rb.node_index("inochi:5").unwrap());
        let (fa, fb) = (ea.drawable(ia).unwrap(), eb.drawable(ib).unwrap());
        for (x, y) in
            fa.multiply_color.iter().zip(&fb.multiply_color).chain(fa.screen_color.iter().zip(&fb.screen_color))
        {
            assert!((x - y).abs() < 1e-4, "colors at {v}: {fa:?} vs {fb:?}");
        }
        let (ca, cb) = (&ea.composites[0], &eb.composites[0]);
        assert!((ca.opacity - cb.opacity).abs() < 1e-4);
    }
    let riglet_format::model::Node::Drawable(m) = &second.document.model.nodes["inochi:6"] else { panic!() };
    assert_eq!(m.masks.len(), 1);
    let riglet_format::model::Node::Drawable(s) = &second.document.model.nodes["inochi:5"] else { panic!() };
    assert_eq!(s.blend_mode, riglet_format::model::BlendMode::Screen);
}

#[test]
fn z_sort_bindings_on_composites_and_parts_with_children() {
    // A zSort binding on a composite moves the composite only; one on a
    // part with children moves its whole subtree.
    let inner = part(6, "inner", [0.0, 0.0], [1.0, 1.0], json!({"zsort": 0.5}));
    let comp = node(
        4,
        "comp",
        vec![inner],
        json!({"type": "Composite", "blend_mode": "Normal", "opacity": 1.0, "tint": [1, 1, 1], "screenTint": [0, 0, 0], "zsort": 1.0}),
    );
    let kid = part(8, "kid", [0.0, 0.0], [1.0, 1.0], json!({"zsort": -0.25}));
    let holder = part(7, "holder", [0.0, 0.0], [1.0, 1.0], json!({"children": [kid], "zsort": 2.0}));
    let z1 = binding(4, "zSort", json!([[0], [3]]), "Linear");
    let z2 = binding(7, "zSort", json!([[0], [-2]]), "Linear");
    let z3 = binding(6, "zSort", json!([[0], [1]]), "Linear");
    let w = round_trip(&puppet(vec![comp, holder], vec![param(10, "z", 0.0, 1.0, &[0.0, 1.0], vec![z1, z2, z3])]));
    assert!(w.is_empty(), "{w:?}");
}

#[test]
fn export_of_an_invalid_model_is_an_error() {
    let i = import(&puppet(vec![part(3, "p", [0.0, 0.0], [1.0, 1.0], json!({}))], vec![]));
    let mut doc = i.document.clone();
    doc.model.root = "missing".into();
    let e = riglet_inochi2d::export(&doc).unwrap_err();
    assert_eq!(e.code(), "INP_INVALID_MODEL");
    let _ = Limits::default();
    let _ = Rig::prepare(&i.document, &Limits::default()).unwrap();
}

/// The sample models (gitignored): import, export, import again, and
/// compare evaluations; prints the export warnings and the largest
/// differences. `cargo test --release -p riglet-inochi2d --test round_trip -- --ignored --nocapture`
#[test]
#[ignore = "needs models in testing_files_gitignored/inochi2d/"]
fn real_models_round_trip() {
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testing_files_gitignored/inochi2d");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        eprintln!("SKIPPED: {} not found", dir.display());
        return;
    };
    let mut paths: Vec<_> = entries
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "inp" || e == "inx"))
        .collect();
    paths.sort();
    for path in paths {
        let bytes = std::fs::read(&path).unwrap();
        let inx = path.extension().is_some_and(|e| e == "inx");
        let first = riglet_inochi2d::import(&bytes, &riglet_inochi2d::Options { inx, ..Default::default() }).unwrap();
        let exported = riglet_inochi2d::export(&first.document).unwrap();
        let second = riglet_inochi2d::import(&exported.bytes, &Default::default()).unwrap();
        let (a, b) = (rig(&first), rig(&second));
        let mut counts = std::collections::BTreeMap::new();
        for w in &exported.warnings {
            *counts.entry(w.code.clone()).or_insert(0usize) += 1;
        }
        let ps = params(&first.document);
        let (mut oa, mut ob) = (EvalOutput::new(), EvalOutput::new());
        let (mut worst, mut worst_op, mut worst_z, mut lost) = (0.0f32, 0.0f32, 0.0f32, 0usize);
        let mut samples: Vec<Vec<(String, [f32; 2])>> = vec![vec![]];
        for (id, lo, hi) in &ps {
            for t in [0.0, 0.3, 0.5, 0.8, 1.0] {
                samples.push(vec![(id.clone(), [0, 1].map(|c| lo.get(c).map_or(0.0, |l| l + t * (hi[c] - l))))]);
            }
        }
        for s in &samples {
            let mut pa = ParamValues::defaults(&a);
            let mut pb = ParamValues::defaults(&b);
            for (id, v) in s {
                pa.set(&a, id, *v);
                pb.set(&b, id, *v);
            }
            a.evaluate(&pa, &mut oa);
            b.evaluate(&pb, &mut ob);
            for da in &oa.drawables {
                let id = &a.nodes[da.node].id;
                let uuid = id.trim_start_matches("inochi:").split(':').next().unwrap();
                let Some(db) = b
                    .node_index(&format!("inochi:{uuid}"))
                    .filter(|&j| ob.drawable(j).is_some())
                    .or_else(|| b.node_index(&format!("inochi:{uuid}:part")))
                    .and_then(|j| ob.drawable(j))
                else {
                    lost += 1;
                    continue;
                };
                for (p, q) in da.positions.iter().zip(&db.positions) {
                    worst = worst.max((p[0] - q[0]).hypot(p[1] - q[1]));
                }
                worst_op = worst_op.max((da.opacity - db.opacity).abs());
                worst_z = worst_z.max((da.draw_order - db.draw_order).abs());
            }
        }
        println!(
            "== {}: {} -> {} bytes; export warnings {counts:?}; max vertex {worst:.4} px, opacity {worst_op:.4}, draw order {worst_z:.4}, lost drawables {lost}",
            path.file_name().unwrap().to_string_lossy(),
            bytes.len(),
            exported.bytes.len()
        );
    }
}
