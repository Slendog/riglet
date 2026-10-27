//! Mutation smoke tests: malformed input must give errors, never panics.
//! These run on stable in CI; deeper fuzzing lives in `fuzz/` (nightly).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

mod common;

use common::*;
use riglet_format::{Document, EvalOutput, Limits, ParamValues, Rig, read_package_bytes, write_package_bytes};

/// Deterministic xorshift64 so failures reproduce.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

fn sample() -> Document {
    let mut b = Builder::new();
    b.param("p", 0.0, 1.0, 0.0).param("q", -1.0, 1.0, 0.0);
    b.warp("w", "root", 1, 1, &[[0.0, 0.0], [100.0, 0.0], [0.0, 100.0], [100.0, 100.0]]);
    b.rotation("r", "w", [0.5, 0.5], 0.0, 1.0);
    b.drawable("d", "r", &[[0.0, 0.0], [10.0, 0.0], [0.0, 10.0]]);
    b.mesh_deformer("m", "root", &[[0.0, 0.0], [10.0, 0.0], [0.0, 10.0]], &[0, 1, 2]);
    b.drawable("e", "m", &[[1.0, 1.0], [2.0, 1.0], [1.0, 2.0]]);
    let kfs = vec![b.positions(&[[0.0, 0.0]; 3]), b.positions(&[[1.0, 0.0]; 3])];
    b.set("d", &[linear("p", &[0.0, 1.0])], kfs);
    let kfs = (0..4).map(|i| b.positions(&[[i as f32, 0.0]; 4])).collect();
    b.set("w", &[linear("p", &[0.0, 1.0]), linear("q", &[-1.0, 1.0])], kfs);
    b.document()
}

/// Full pipeline on untrusted bytes; only checks that nothing panics.
fn exercise(bytes: &[u8]) {
    if let Ok(doc) = read_package_bytes(bytes, &Limits::default()) {
        exercise_doc(&doc);
    }
}

fn exercise_doc(doc: &Document) {
    if let Ok(rig) = Rig::prepare(doc, &Limits::default()) {
        let mut params = ParamValues::defaults(&rig);
        let mut out = EvalOutput::new();
        for v in [f32::NAN, f32::INFINITY, -1e30, 0.5, 1e30] {
            for p in params.values.iter_mut() {
                *p = [v, -v];
            }
            rig.evaluate(&params, &mut out);
        }
    }
}

fn mutate(rng: &mut Rng, data: &mut Vec<u8>) {
    match rng.below(4) {
        0 => {
            let i = rng.below(data.len());
            if let Some(b) = data.get_mut(i) {
                *b ^= 1 << rng.below(8);
            }
        }
        1 => {
            let i = rng.below(data.len());
            if let Some(b) = data.get_mut(i) {
                *b = rng.next() as u8;
            }
        }
        2 => {
            let len = rng.below(data.len() + 1);
            data.truncate(len);
        }
        _ => {
            let i = rng.below(data.len() + 1);
            data.insert(i.min(data.len()), rng.next() as u8);
        }
    }
}

#[test]
fn mutated_packages_never_panic() {
    let original = write_package_bytes(&sample()).unwrap();
    exercise(&original);
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    for _ in 0..3000 {
        let mut bytes = original.clone();
        for _ in 0..=rng.below(4) {
            mutate(&mut rng, &mut bytes);
        }
        exercise(&bytes);
    }
}

#[test]
fn mutated_model_json_never_panics() {
    // Deflate hides JSON from byte flips, so mutate model.json directly.
    let doc = sample();
    let json = serde_json::to_vec(&doc.model).unwrap();
    let mut rng = Rng(0xD1B5_4A32_D192_ED03);
    for _ in 0..3000 {
        let mut bytes = json.clone();
        for _ in 0..=rng.below(3) {
            mutate(&mut rng, &mut bytes);
        }
        if let Ok(model) = serde_json::from_slice(&bytes) {
            exercise_doc(&Document { model, ..doc.clone() });
        }
    }
}

#[test]
fn mutated_buffers_never_panic() {
    let doc = sample();
    let mut rng = Rng(0x2545_F491_4F6C_DD1D);
    for _ in 0..2000 {
        let mut d = doc.clone();
        if let Some(buf) = d.files.get_mut("buffers/data.bin") {
            for _ in 0..=rng.below(8) {
                mutate(&mut rng, buf);
            }
        }
        exercise_doc(&d);
    }
}

#[test]
fn deep_tree_does_not_overflow_the_stack() {
    let mut b = Builder::new();
    let mut parent = "root".to_owned();
    for i in 0..20_000 {
        let id = format!("g{i}");
        b.group(&id, &parent, 1.0);
        parent = id;
    }
    b.drawable("d", &parent, &[[0.0, 0.0]]);
    let rig = b.rig();
    let mut out = EvalOutput::new();
    rig.evaluate(&ParamValues::defaults(&rig), &mut out);
    assert_eq!(out.drawables.len(), 1);
}

/// Writes fuzz corpus seeds: `cargo test --test malformed -- --ignored`.
#[test]
#[ignore = "writes fuzz/seeds; run manually when the format changes"]
fn write_fuzz_seeds() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fuzz/seeds");
    let doc = sample();
    std::fs::create_dir_all(root.join("package")).unwrap();
    std::fs::create_dir_all(root.join("model_json")).unwrap();
    std::fs::write(root.join("package/sample.riglet"), write_package_bytes(&doc).unwrap()).unwrap();
    std::fs::write(root.join("model_json/sample.json"), serde_json::to_vec_pretty(&doc.model).unwrap()).unwrap();
    // The model_json target pairs every input with these package files.
    std::fs::write(root.join("data.bin"), &doc.files["buffers/data.bin"]).unwrap();
}
