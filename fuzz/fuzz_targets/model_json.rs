//! Fuzzes model.json parsing and validation directly (ZIP compression
//! hides JSON structure from the package target's mutations).
#![no_main]

use std::collections::BTreeMap;

use libfuzzer_sys::fuzz_target;
use riglet_format::model::{Manifest, Model};
use riglet_format::{Document, Limits, ParamValues, Rig, evaluate};

fuzz_target!(|data: &[u8]| {
    let Ok(model) = serde_json::from_slice::<Model>(data) else { return };
    let doc = Document {
        manifest: Manifest {
            format: "riglet".into(),
            schema_version: "0.2.0".into(),
            generator: "fuzz".into(),
            provenance: Vec::new(),
        },
        model,
        files: BTreeMap::new(),
    };
    if let Ok(rig) = Rig::prepare(&doc, &Limits::default()) {
        let _ = evaluate(&rig, &ParamValues::defaults(&rig));
    }
});
