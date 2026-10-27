//! Synthetic Inochi2D puppets built from code (JSON + container), so every
//! test fixture is reviewable in a diff.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

use riglet_format::model::Vec2;
use riglet_format::{EvalOutput, Limits, ParamValues, Rig};
use riglet_inochi2d::container::{self, Container, Encoding, RawTexture};
use serde_json::{Value, json};

pub const TOL: f32 = 1e-3;

pub fn close(a: Vec2, b: Vec2) -> bool {
    (a[0] - b[0]).abs() <= TOL && (a[1] - b[1]).abs() <= TOL
}

#[track_caller]
pub fn assert_close(a: Vec2, b: Vec2) {
    assert!(close(a, b), "expected {b:?}, got {a:?}");
}

/// A Part with a quad mesh `min..max` (origin at 0) and texture slot 0.
pub fn part(uuid: u32, name: &str, min: Vec2, max: Vec2, extra: Value) -> Value {
    let mut p = json!({
        "uuid": uuid, "name": name, "type": "Part", "enabled": true, "zsort": 0.0,
        "transform": {"trans": [0, 0, 0], "rot": [0, 0, 0], "scale": [1, 1]},
        "lockToRoot": false,
        "mesh": {
            "verts": [min[0], min[1], max[0], min[1], max[0], max[1], min[0], max[1]],
            "uvs": [0, 0, 1, 0, 1, 1, 0, 1],
            "indices": [0, 1, 2, 0, 2, 3],
            "origin": [0, 0]
        },
        "textures": [0, 4294967295u64, 4294967295u64],
        "blend_mode": "Normal", "tint": [1, 1, 1], "screenTint": [0, 0, 0],
        "masks": [], "mask_threshold": 0.5, "opacity": 1.0,
        "children": []
    });
    merge(&mut p, extra);
    p
}

/// A plain Node (or any type via `extra`).
pub fn node(uuid: u32, name: &str, children: Vec<Value>, extra: Value) -> Value {
    let mut n = json!({
        "uuid": uuid, "name": name, "type": "Node", "enabled": true, "zsort": 0.0,
        "transform": {"trans": [0, 0, 0], "rot": [0, 0, 0], "scale": [1, 1]},
        "lockToRoot": false, "children": children
    });
    merge(&mut n, extra);
    n
}

pub fn merge(target: &mut Value, extra: Value) {
    if let (Some(t), Value::Object(e)) = (target.as_object_mut(), extra) {
        for (k, v) in e {
            t.insert(k, v);
        }
    }
}

/// A 1D parameter with axis points (normalized) and bindings.
pub fn param(uuid: u32, name: &str, min: f32, max: f32, axis: &[f32], bindings: Vec<Value>) -> Value {
    json!({
        "uuid": uuid, "name": name, "is_vec2": false,
        "min": [min, 0], "max": [max, 0], "defaults": [min, 0],
        "axis_points": [axis, [0.0]], "merge_mode": "Additive", "bindings": bindings
    })
}

pub fn param2d(uuid: u32, name: &str, axis_x: &[f32], axis_y: &[f32], bindings: Vec<Value>) -> Value {
    json!({
        "uuid": uuid, "name": name, "is_vec2": true,
        "min": [0, 0], "max": [1, 1], "defaults": [0, 0],
        "axis_points": [axis_x, axis_y], "merge_mode": "Additive", "bindings": bindings
    })
}

/// A value binding; `values[x][y]`.
pub fn binding(node: u32, name: &str, values: Value, interp: &str) -> Value {
    json!({"node": node, "param_name": name, "values": values, "interpolate_mode": interp})
}

/// A 2x2 opaque white PNG texture.
pub fn white_png() -> Vec<u8> {
    riglet_fixtures::encode_png(2, 2, &[255; 16])
}

pub fn puppet(root_children: Vec<Value>, params: Vec<Value>) -> Value {
    json!({
        "meta": {"name": "test", "version": "v0.8.7"},
        "physics": {"pixelsPerMeter": 1000, "gravity": 9.8},
        "nodes": node(1, "Root", root_children, json!({})),
        "param": params,
        "automation": [],
        "animations": {}
    })
}

pub fn package(json: &Value, textures: Vec<(Encoding, Vec<u8>)>) -> Vec<u8> {
    container::write(&Container {
        json: serde_json::to_vec(json).unwrap(),
        textures: textures.into_iter().map(|(encoding, data)| RawTexture { encoding, data }).collect(),
        extensions: vec![("com.inochi2d.creator.FlipConfig".into(), b"{}".to_vec())],
    })
    .unwrap()
}

pub fn import(json: &Value) -> riglet_inochi2d::Import {
    let bytes = package(json, vec![(Encoding::Png, white_png())]);
    match riglet_inochi2d::import(&bytes, &Default::default()) {
        Ok(i) => i,
        Err(e) => panic!("import failed: {} ({e})", e.code()),
    }
}

pub fn codes(i: &riglet_inochi2d::Import) -> Vec<&str> {
    i.warnings.iter().map(|w| w.code.as_str()).collect()
}

pub fn rig(i: &riglet_inochi2d::Import) -> Rig {
    Rig::prepare(&i.document, &Limits::default()).unwrap_or_else(|e| panic!("{e:#?}"))
}

/// Evaluates with parameter values by Inochi2D uuid.
pub fn eval(rig: &Rig, values: &[(u32, [f32; 2])]) -> EvalOutput {
    let mut p = ParamValues::defaults(rig);
    for (uuid, v) in values {
        assert!(p.set(rig, &format!("inochi:{uuid}"), *v), "unknown parameter {uuid}");
    }
    let mut out = EvalOutput::new();
    rig.evaluate(&p, &mut out);
    out
}

/// Positions of the drawable for Inochi2D part `uuid`.
pub fn positions(rig: &Rig, out: &EvalOutput, uuid: u32) -> Vec<Vec2> {
    let index = rig
        .node_index(&format!("inochi:{uuid}"))
        .filter(|&i| out.drawable(i).is_some())
        .or_else(|| rig.node_index(&format!("inochi:{uuid}:part")))
        .unwrap_or_else(|| panic!("no drawable for {uuid}"));
    out.drawable(index).unwrap().positions.clone()
}
