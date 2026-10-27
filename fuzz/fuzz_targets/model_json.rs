//! Fuzzes model.json parsing, validation and evaluation directly (ZIP
//! compression hides JSON structure from the package target's mutations).
//! Inputs are paired with the seed model's buffer file and a 1x1 texture,
//! so mutated models still reach buffer decoding and the evaluator.
#![no_main]

use std::collections::BTreeMap;

use libfuzzer_sys::fuzz_target;
use riglet_format::model::{Manifest, Model};
use riglet_format::{Document, EvalOutput, Limits, ParamValues, Rig};

const DATA_BIN: &[u8] = include_bytes!("../seeds/data.bin");
const PNG_1X1: [u8; 70] = [
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52, 0x00, 0x00, 0x00,
    0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0d, 0x49,
    0x44, 0x41, 0x54, 0x78, 0xda, 0x63, 0x64, 0x60, 0xf8, 0x5f, 0x0f, 0x00, 0x02, 0x87, 0x01, 0x80, 0xeb, 0x47, 0xba,
    0x92, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
];

fuzz_target!(|data: &[u8]| {
    let Ok(model) = serde_json::from_slice::<Model>(data) else { return };
    let mut files = BTreeMap::new();
    files.insert("buffers/data.bin".to_owned(), DATA_BIN.to_vec());
    files.insert("textures/t.png".to_owned(), PNG_1X1.to_vec());
    let doc = Document {
        manifest: Manifest {
            format: "riglet".into(),
            schema_version: riglet_format::migrate::CURRENT_SCHEMA_VERSION.into(),
            generator: "fuzz".into(),
            provenance: Vec::new(),
        },
        model,
        files,
    };
    if let Ok(rig) = Rig::prepare(&doc, &Limits::default()) {
        rig.evaluate(&ParamValues::defaults(&rig), &mut EvalOutput::new());
    }
});
