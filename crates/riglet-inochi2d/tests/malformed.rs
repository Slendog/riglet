//! Malformed Inochi2D input must give errors, never panics, hangs or
//! invalid models. Stable mutation smoke tests; deeper fuzzing is in fuzz/
//! (inp_container, inp_json, tga). Also replays regression inputs and writes
//! the fuzz seeds.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

mod common;

use std::path::PathBuf;

use common::*;
use riglet_format::{EvalOutput, Limits, ParamValues, Rig};
use riglet_inochi2d::container::Encoding;
use serde_json::json;

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
            let i = rng.below(data.len() + 1).min(data.len());
            data.insert(i, rng.next() as u8);
        }
    }
}

/// A puppet exercising most mapping paths.
fn rich_puppet() -> serde_json::Value {
    let child = part(
        6,
        "child",
        [1.0, 1.0],
        [2.0, 2.0],
        json!({"transform": {"trans": [1, 1, 0], "rot": [0, 0, 0], "scale": [1, 1]}}),
    );
    let mg = node(
        5,
        "mg",
        vec![child],
        json!({"type": "MeshGroup", "mesh": {
            "verts": [0, 0, 10, 0, 0, 10, 10, 10], "uvs": [0, 0, 1, 0, 0, 1, 1, 1], "indices": [0, 1, 2, 1, 3, 2], "origin": [0, 0]
        }}),
    );
    let masked = part(
        4,
        "masked",
        [0.0, 0.0],
        [4.0, 4.0],
        json!({"masks": [{"source": 3, "mode": "Mask"}], "tint": [0.5, 1, 1], "screenTint": [0.2, 0, 0]}),
    );
    let src = part(3, "src", [0.0, 0.0], [2.0, 2.0], json!({"zsort": 1.0}));
    let comp = node(7, "comp", vec![masked], json!({"type": "Composite", "opacity": 0.5, "blend_mode": "Multiply"}));
    let n = node(
        2,
        "n",
        vec![src, comp, mg],
        json!({"transform": {"trans": [3, 4, 0], "rot": [0, 0, 0.5], "scale": [1, 2]}}),
    );
    let deform =
        binding(3, "deform", json!([[[[0, 0], [0, 0], [0, 0], [0, 0]]], [[[1, 0], [1, 0], [1, 0], [1, 0]]]]), "Cubic");
    let opacity = binding(4, "opacity", json!([[1], [0.5]]), "Nearest");
    let tint = binding(4, "tint.r", json!([[1], [0.5]]), "Linear");
    let tx = binding(2, "transform.t.x", json!([[0], [5]]), "Linear");
    let z = binding(2, "zSort", json!([[0], [-2]]), "Linear");
    puppet(vec![n], vec![param(10, "a", 0.0, 1.0, &[0.0, 1.0], vec![deform, opacity, tint, tx, z])])
}

fn exercise(bytes: &[u8]) {
    if let Ok(i) = riglet_inochi2d::import(bytes, &Default::default()) {
        let rig = Rig::prepare(&i.document, &Limits::default()).expect("accepted input must give a valid model");
        rig.evaluate(&ParamValues::defaults(&rig), &mut EvalOutput::new());
    }
}

#[test]
fn rich_puppet_imports() {
    let i = import(&rich_puppet());
    rig(&i);
}

#[test]
fn mutated_containers_never_panic() {
    let original = package(&rich_puppet(), vec![(Encoding::Png, white_png())]);
    let mut rng = Rng(0x0123_4567_89AB_CDEF);
    for _ in 0..2000 {
        let mut bytes = original.clone();
        for _ in 0..=rng.below(4) {
            mutate(&mut rng, &mut bytes);
        }
        exercise(&bytes);
    }
}

#[test]
fn mutated_json_never_panics() {
    let json = serde_json::to_vec(&rich_puppet()).unwrap();
    let mut rng = Rng(0xFEED_FACE_CAFE_BEEF);
    for _ in 0..3000 {
        let mut j = json.clone();
        for _ in 0..=rng.below(3) {
            mutate(&mut rng, &mut j);
        }
        let Ok(v) = serde_json::from_slice::<serde_json::Value>(&j) else { continue };
        exercise(&package(&v, vec![(Encoding::Png, white_png())]));
    }
}

#[test]
fn mutated_tga_never_panics() {
    let mut tga = vec![0u8, 0, 10, 0, 0, 0, 0, 0, 0, 0, 0, 0, 4, 0, 2, 0, 32, 0];
    tga.extend([0x83, 1, 2, 3, 4, 0x03, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20]);
    let mut rng = Rng(0xABCD_EF01_2345_6789);
    for _ in 0..3000 {
        let mut t = tga.clone();
        for _ in 0..=rng.below(4) {
            mutate(&mut rng, &mut t);
        }
        if let Ok(img) = riglet_inochi2d::tga::decode(&t, 64) {
            assert_eq!(img.rgba.len(), (img.width * img.height * 4) as usize);
        }
    }
}

/// Fuzz crash inputs, replayed through the same paths as their targets.
#[test]
fn regressions() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/regressions");
    for target in ["inp_container", "inp_json", "tga"] {
        for entry in std::fs::read_dir(root.join(target)).unwrap() {
            let path = entry.unwrap().path();
            if path.file_name().is_some_and(|n| n == "README.md") {
                continue;
            }
            let bytes = std::fs::read(&path).unwrap();
            match target {
                "inp_container" => exercise(&bytes),
                "inp_json" => {
                    if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                        exercise(&package(&v, vec![(Encoding::Png, white_png())]));
                    }
                }
                _ => {
                    let _ = riglet_inochi2d::tga::decode(&bytes, 4096);
                }
            }
        }
    }
}

/// Writes fuzz seeds: `cargo test -p riglet-inochi2d --test malformed -- --ignored`.
#[test]
#[ignore = "writes fuzz/seeds; run manually when the importer changes"]
fn write_fuzz_seeds() {
    let seeds = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fuzz/seeds");
    for d in ["inp_container", "inp_json", "tga"] {
        std::fs::create_dir_all(seeds.join(d)).unwrap();
    }
    let pup = rich_puppet();
    std::fs::write(seeds.join("inp_container/rich.inp"), package(&pup, vec![(Encoding::Png, white_png())])).unwrap();
    std::fs::write(seeds.join("inp_json/rich.json"), serde_json::to_vec(&pup).unwrap()).unwrap();
    std::fs::write(seeds.join("inp_texture.png"), white_png()).unwrap();
    let mut tga = vec![0u8, 0, 10, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2, 0, 2, 0, 32, 0x20];
    tga.extend([0x81, 1, 2, 3, 4, 0x01, 5, 6, 7, 8, 9, 10, 11, 12]);
    std::fs::write(seeds.join("tga/rle32.tga"), tga).unwrap();
}
