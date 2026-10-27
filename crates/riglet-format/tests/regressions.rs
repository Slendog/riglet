//! Replays every fuzz crash input in `tests/regressions/<target>/` through
//! the same path as its fuzz target. A crash input is added here together
//! with its fix and stays forever (CLAUDE.md, fuzzing policy).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::collections::BTreeMap;
use std::path::PathBuf;

use riglet_format::model::{Manifest, Model};
use riglet_format::{Document, EvalOutput, Limits, ParamValues, Rig, read_package_bytes};

/// The buffer file the model_json fuzz target pairs every input with.
const DATA_BIN: &[u8] = include_bytes!("../../../fuzz/seeds/data.bin");

fn inputs(target: &str) -> Vec<(String, Vec<u8>)> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/regressions").join(target);
    let mut out: Vec<(String, Vec<u8>)> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.file_name().is_some_and(|n| n != "README.md"))
        .map(|p| (p.display().to_string(), std::fs::read(&p).unwrap()))
        .collect();
    out.sort();
    out
}

fn exercise(doc: &Document) {
    if let Ok(rig) = Rig::prepare(doc, &Limits::default()) {
        rig.evaluate(&ParamValues::defaults(&rig), &mut EvalOutput::new());
    }
}

#[test]
fn package_regressions() {
    for (name, bytes) in inputs("package") {
        eprintln!("replaying {name}");
        if let Ok(doc) = read_package_bytes(&bytes, &Limits::default()) {
            exercise(&doc);
        }
    }
}

#[test]
fn model_json_regressions() {
    for (name, bytes) in inputs("model_json") {
        eprintln!("replaying {name}");
        let Ok(model) = serde_json::from_slice::<Model>(&bytes) else { continue };
        let mut files = BTreeMap::new();
        files.insert("buffers/data.bin".to_owned(), DATA_BIN.to_vec());
        files.insert("textures/t.png".to_owned(), riglet_fixtures::PNG_1X1.to_vec());
        exercise(&Document {
            manifest: Manifest {
                format: "riglet".into(),
                schema_version: riglet_format::migrate::CURRENT_SCHEMA_VERSION.into(),
                generator: "regression".into(),
                provenance: Vec::new(),
            },
            model,
            files,
        });
    }
}
