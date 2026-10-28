//! Export must convert, not only restore: import, EDIT the Riglet model
//! (a transform, keyform values, colors, a mask, new native nodes, a
//! motion), export, import again, and check that the edited model and the
//! re-import evaluate the same. Synthetic puppets run in CI; the sample
//! models (gitignored) in an ignored test.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

mod common;

use common::*;
use riglet_format::buffer::BufferWriter;
use riglet_format::model::*;
use riglet_format::{Document, EvalOutput, ParamValues, Rig};
use serde_json::json;

/// What `edit` changed, for the checks.
#[derive(Debug, Default)]
struct Edits {
    /// Drawables whose result Inochi2D can only approximate (skipped in the
    /// per-vertex comparison, but their warning is checked).
    approximate: Vec<String>,
    masked: Option<String>,
    colored: Option<String>,
    motion: bool,
    transform: bool,
    origin_key: bool,
    deform_key: bool,
}

fn quad(w: &mut BufferWriter, min: Vec2, max: Vec2) -> DrawableMesh {
    DrawableMesh {
        positions: w.push_vec2(&[[min[0], min[1]], [max[0], min[1]], [max[0], max[1]], [min[0], max[1]]]),
        uvs: w.push_vec2(&[[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]]),
        indices: w.push_u16(&[0, 1, 2, 0, 2, 3]),
    }
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

fn insert(doc: &mut Document, node: Node) {
    let id = node.base().id.clone();
    if let Some(p) = node.base().parent.clone()
        && let Some(parent) = doc.model.nodes.get_mut(&p)
    {
        parent.base_mut().children.push(id.clone());
    }
    doc.model.nodes.insert(id, node);
}

fn drawable(id: &str, parent: &str, texture: &str, mesh: DrawableMesh) -> Node {
    Node::Drawable(DrawableNode {
        base: base(id, parent),
        texture: texture.into(),
        mesh,
        blend_mode: BlendMode::Normal,
        culling: false,
        masks: Vec::new(),
        mask_threshold: None,
        rest: DrawableRest { opacity: 1.0, draw_order: 0.5, multiply_color: [1.0; 3], screen_color: [0.0; 3] },
    })
}

fn rotation(id: &str, parent: &str, origin: Vec2, angle: f32) -> Node {
    Node::Rotation(RotationDeformerNode {
        base: base(id, parent),
        rest: RotationRest { origin, angle, scale: Scale::PerAxis([1.2, 0.8]), opacity: 1.0 },
        mesh_parent: None,
    })
}

/// Edits any imported model the same way.
fn edit(doc: &mut Document) -> Edits {
    let mut out = Edits::default();
    let mut w = BufferWriter::new("buffers/edit.bin");
    let texture = doc.model.textures.first().map(|t| t.id.clone()).unwrap();
    let ids: Vec<Id> = doc.model.nodes.keys().cloned().collect();

    // 1. A transform: move and turn the first imported rotation deformer.
    if let Some(id) = ids.iter().find(|i| i.ends_with(":transform"))
        && let Some(Node::Rotation(r)) = doc.model.nodes.get_mut(id)
    {
        r.rest.origin = [r.rest.origin[0] + 3.0, r.rest.origin[1] - 2.0];
        r.rest.angle += 10.0;
        out.transform = true;
    }
    // 2. Keyform values: an origin key, and a deform key (new buffer).
    'origin: for id in &ids {
        let n = doc.model.nodes.get_mut(id).unwrap();
        for set in &mut n.base_mut().keyform_sets {
            if let Some(kf) = set.keyforms.iter_mut().rev().find(|k| k.origin.is_some()) {
                let o = kf.origin.unwrap();
                kf.origin = Some([o[0] + 1.0, o[1] + 0.5]);
                out.origin_key = true;
                break 'origin;
            }
        }
    }
    let files = doc.files.clone();
    'deform: for id in &ids {
        let Some(Node::Drawable(_)) = doc.model.nodes.get(id) else { continue };
        let n = doc.model.nodes.get_mut(id).unwrap();
        for set in &mut n.base_mut().keyform_sets {
            if let Some(kf) = set.keyforms.iter_mut().rev().find(|k| k.positions.is_some()) {
                let old = riglet_format::buffer::read_vec2(&files, kf.positions.as_ref().unwrap(), u64::MAX).unwrap();
                let new: Vec<Vec2> =
                    old.iter().enumerate().map(|(i, p)| [p[0] * 1.5 + 0.25, p[1] * 1.5 - (i % 3) as f32]).collect();
                kf.positions = Some(w.push_vec2(&new));
                out.deform_key = true;
                break 'deform;
            }
        }
    }
    // 3. A color.
    if let Some(id) = ids.iter().find(|i| matches!(doc.model.nodes.get(*i), Some(Node::Drawable(_)))) {
        if let Some(Node::Drawable(d)) = doc.model.nodes.get_mut(id) {
            d.rest.multiply_color = [0.9, 0.7, 0.8];
            d.rest.screen_color = [0.05, 0.1, 0.0];
        }
        out.colored = Some(id.clone());
    }
    // 4. A mask: the last drawable without masks gets a clip mask.
    let drawables: Vec<Id> =
        ids.iter().filter(|i| matches!(doc.model.nodes.get(*i), Some(Node::Drawable(_)))).cloned().collect();
    if drawables.len() >= 2 {
        let target = drawables
            .iter()
            .rev()
            .find(|i| matches!(doc.model.nodes.get(*i), Some(Node::Drawable(d)) if d.masks.is_empty()))
            .cloned();
        if let Some(t) = target {
            let source = drawables.iter().find(|i| **i != t).cloned().unwrap();
            if let Some(Node::Drawable(d)) = doc.model.nodes.get_mut(&t) {
                d.masks.push(MaskRef { source, mode: MaskMode::Clip });
            }
            out.masked = Some(t);
        }
    }
    // 5. New native nodes: a drawable, a rotation deformer with a child,
    // and a rigid rotation deformer under a mesh deformer (approximated).
    let root = doc.model.root.clone();
    let mesh = quad(&mut w, [-4.0, -3.0], [5.0, 6.0]);
    insert(doc, drawable("edit:new", &root, &texture, mesh));
    insert(doc, rotation("edit:rot", &root, [7.0, -5.0], 30.0));
    let mesh = quad(&mut w, [0.0, 0.0], [2.0, 3.0]);
    insert(doc, drawable("edit:child", "edit:rot", &texture, mesh));
    if let Some(m) = ids.iter().find(|i| matches!(doc.model.nodes.get(*i), Some(Node::MeshDeformer(_)))).cloned() {
        insert(doc, rotation("edit:rigid", &m, [1.0, 1.0], 0.0));
        let mesh = quad(&mut w, [0.0, 0.0], [1.0, 1.0]);
        insert(doc, drawable("edit:rigid_child", "edit:rigid", &texture, mesh));
        out.approximate.push("edit:rigid_child".into());
    }
    // 6. A motion on the first parameter.
    if let Some(p) = doc.model.parameters.first() {
        let (lo, hi) = (p.min[0], p.max[0]);
        doc.model.motions.push(Motion {
            id: "edit:motion".into(),
            name: "edit".into(),
            duration: 1.0,
            looped: false,
            tracks: vec![MotionTrack {
                target: MotionTarget::Param { param: ParamAxis { parameter: p.id.clone(), component: 0 } },
                keys: [(0.0, lo), (0.5, hi), (1.0, lo)]
                    .map(|(t, value)| CurveKey { t, value, interp: CurveInterp::Linear, handles: None })
                    .to_vec(),
            }],
        });
        out.motion = true;
    }
    w.finish(&mut doc.files);
    Rig::prepare(doc, &riglet_format::Limits::default()).unwrap_or_else(|e| panic!("edited model invalid: {e:#?}"));
    out
}

/// The re-imported index of a drawable of the edited model.
fn find(b: &Rig, out: &EvalOutput, id: &str) -> Option<usize> {
    if b.node_index(id).is_some_and(|j| out.drawable(j).is_some()) {
        return b.node_index(id);
    }
    let uuid = id.strip_prefix("inochi:")?.split(':').next()?;
    b.node_index(&format!("inochi:{uuid}"))
        .filter(|&j| out.drawable(j).is_some())
        .or_else(|| b.node_index(&format!("inochi:{uuid}:part")))
}

/// Exports the edited model, imports it again and compares. Returns the
/// largest vertex difference.
fn check(edited: &Document, edits: &Edits) -> f32 {
    let exported = riglet_inochi2d::export(edited).unwrap_or_else(|e| panic!("export: {} ({e})", e.code()));
    let codes: Vec<&str> = exported.warnings.iter().map(|w| w.code.as_str()).collect();
    if !edits.approximate.is_empty() {
        assert!(codes.contains(&"MESHGROUP_RIGID_APPROXIMATED"), "{codes:?}");
    }
    let back = riglet_inochi2d::import(&exported.bytes, &Default::default())
        .unwrap_or_else(|e| panic!("re-import: {} ({e})", e.code()));
    let (a, b) = (rig_of(edited), rig(&back));
    // Native ids and the canvas come back through org.riglet.model.
    for id in ["edit:new", "edit:rot", "edit:child"] {
        assert!(back.document.model.nodes.contains_key(id), "{id} lost its id");
    }
    assert_eq!(back.document.model.canvas, edited.model.canvas);

    let ps: Vec<(Id, Vec<f32>, Vec<f32>)> =
        edited.model.parameters.iter().map(|p| (p.id.clone(), p.min.clone(), p.max.clone())).collect();
    let mut samples: Vec<Vec<(Id, [f32; 2])>> = vec![vec![]];
    for (id, lo, hi) in &ps {
        for t in [0.0, 0.3, 0.5, 0.8, 1.0] {
            samples.push(vec![(id.clone(), [0, 1].map(|c| lo.get(c).map_or(0.0, |l| l + t * (hi[c] - l))))]);
        }
    }
    let (mut oa, mut ob) = (EvalOutput::new(), EvalOutput::new());
    let mut worst = 0.0f32;
    for s in &samples {
        let mut pa = ParamValues::defaults(&a);
        let mut pb = ParamValues::defaults(&b);
        for (id, v) in s {
            pa.set(&a, id, *v);
            assert!(pb.set(&b, id, *v), "parameter {id} lost");
        }
        a.evaluate(&pa, &mut oa);
        b.evaluate(&pb, &mut ob);
        for da in &oa.drawables {
            let id = a.nodes[da.node].id.clone();
            let j = find(&b, &ob, &id).unwrap_or_else(|| panic!("drawable {id} lost"));
            let db = ob.drawable(j).unwrap();
            if edits.approximate.contains(&id) {
                continue;
            }
            for (p, q) in da.positions.iter().zip(&db.positions) {
                let e = (p[0] - q[0]).hypot(p[1] - q[1]);
                worst = worst.max(e);
                assert!(e < 1e-2, "{id} at {s:?}: {p:?} vs {q:?}");
            }
            assert!((da.opacity - db.opacity).abs() < 1e-4, "{id} opacity");
            assert!(
                (da.draw_order - db.draw_order).abs() < 1e-3,
                "{id} draw order at {s:?}: {} vs {}",
                da.draw_order,
                db.draw_order
            );
            for (x, y) in
                da.multiply_color.iter().zip(&db.multiply_color).chain(da.screen_color.iter().zip(&db.screen_color))
            {
                assert!(
                    (x - y).abs() < 1e-3,
                    "{id} colors at {s:?}: {:?}/{:?} vs {:?}/{:?}",
                    da.multiply_color,
                    da.screen_color,
                    db.multiply_color,
                    db.screen_color
                );
            }
        }
    }
    if let Some(m) = &edits.masked {
        let j = find(&b, &ob, m).unwrap();
        let Some(Node::Drawable(d)) = back.document.model.nodes.get(&b.nodes[j].id) else { panic!() };
        let Some(Node::Drawable(orig)) = edited.model.nodes.get(m) else { panic!() };
        assert_eq!(d.masks.len(), orig.masks.len(), "masks of {m}");
    }
    if edits.motion {
        let m = back.document.model.motions.iter().find(|m| m.name == "edit").expect("motion lost");
        let keys: Vec<(f32, f32)> = m.tracks[0].keys.iter().map(|k| (k.t, k.value)).collect();
        let orig = &edited.model.motions.iter().find(|m| m.name == "edit").unwrap().tracks[0].keys;
        assert_eq!(keys.len(), orig.len());
        for ((t, v), k) in keys.iter().zip(orig) {
            assert!((t - k.t).abs() < 1e-4 && (v - k.value).abs() < 1e-4, "motion key {t}/{v} vs {}/{}", k.t, k.value);
        }
    }
    worst
}

fn rig_of(doc: &Document) -> Rig {
    Rig::prepare(doc, &riglet_format::Limits::default()).unwrap()
}

fn translated(x: f32, y: f32) -> serde_json::Value {
    json!({"transform": {"trans": [x, y, 0], "rot": [0, 0, 0], "scale": [1, 1]}})
}

#[test]
fn edited_synthetic_puppet_round_trips() {
    // Transforms with bindings, a part with a child, a MeshGroup with an
    // animated part, colors, masks.
    let child = part(5, "child", [0.0, 0.0], [2.0, 1.0], json!({"zsort": 1.0}));
    let p = part(
        4,
        "p",
        [-1.0, -1.0],
        [1.0, 1.0],
        json!({"transform": {"trans": [10, 5, 0], "rot": [0, 0, 0.3], "scale": [2, 0.5]}, "children": [child]}),
    );
    let anim = part(7, "anim", [3.0, 3.0], [4.0, 4.0], json!({"masks": [{"source": 4, "mode": "Mask"}]}));
    let mut mg = node(6, "mg", vec![anim], json!({"type": "MeshGroup"}));
    mg["mesh"] = json!({"verts": [0, 0, 10, 0, 0, 10, 10, 10], "uvs": [0, 0, 1, 0, 0, 1, 1, 1], "indices": [0, 1, 2, 1, 3, 2], "origin": [0, 0]});
    let n = node(3, "n", vec![p, mg], translated(-3.0, 2.0));
    let bindings = vec![
        binding(4, "transform.t.x", json!([[0], [4], [1]]), "Linear"),
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
        binding(
            6,
            "deform",
            json!([
                [[[0, 0], [0, 0], [0, 0], [0, 0]]],
                [[[0, 0], [10, 0], [0, 0], [10, 0]]],
                [[[0, 0], [0, 0], [0, 0], [0, 0]]]
            ]),
            "Linear",
        ),
        binding(7, "transform.t.y", json!([[0], [2], [-1]]), "Linear"),
        binding(5, "tint.g", json!([[1], [0.5], [1]]), "Linear"),
    ];
    let i = import(&puppet(vec![n], vec![param(10, "p", -1.0, 1.0, &[0.0, 0.5, 1.0], bindings)]));
    let mut doc = i.document.clone();
    let edits = edit(&mut doc);
    assert!(
        !edits.approximate.is_empty() && edits.masked.is_some() && edits.colored.is_some() && edits.motion,
        "{edits:?}"
    );
    assert!(edits.transform && edits.origin_key && edits.deform_key, "{edits:?}");
    check(&doc, &edits);
}

#[test]
#[ignore = "needs models in testing_files_gitignored/inochi2d/"]
fn edited_real_models_round_trip() {
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
        let i = riglet_inochi2d::import(&bytes, &riglet_inochi2d::Options { inx, ..Default::default() }).unwrap();
        let mut doc = i.document.clone();
        let edits = edit(&mut doc);
        let worst = check(&doc, &edits);
        println!("== {}: edited round trip, max vertex {worst:.4} px", path.file_name().unwrap().to_string_lossy());
    }
}

#[test]
fn hostile_riglet_entry_cannot_merge_or_break_nodes() {
    use riglet_inochi2d::container::{self, Container, Encoding, RawTexture};
    // Two uuids mapped to one id, one to an existing id, a bad canvas.
    let pup = puppet(
        vec![part(3, "a", [0.0, 0.0], [1.0, 1.0], json!({})), part(4, "b", [0.0, 0.0], [1.0, 1.0], json!({}))],
        vec![],
    );
    let entry =
        json!({"canvas": [f32::MAX, -1.0], "nodeIds": {"3": "x", "4": "x", "1": "inochi:3"}, "parameterIds": {}});
    let bytes = container::write(&Container {
        json: serde_json::to_vec(&pup).unwrap(),
        textures: vec![RawTexture { encoding: Encoding::Png, data: white_png() }],
        extensions: vec![("org.riglet.model".into(), serde_json::to_vec(&entry).unwrap())],
    })
    .unwrap();
    let i = riglet_inochi2d::import(&bytes, &Default::default()).unwrap();
    let nodes = &i.document.model.nodes;
    let drawables = nodes.values().filter(|n| matches!(n, Node::Drawable(_))).count();
    assert_eq!(drawables, 2, "no node lost: {:?}", nodes.keys().collect::<Vec<_>>());
    assert!(nodes.contains_key("x"));
    assert!(i.document.model.canvas.width.is_finite() && i.document.model.canvas.height > 0.0);
    rig(&i);
}
