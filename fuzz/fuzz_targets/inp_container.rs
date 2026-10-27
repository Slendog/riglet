//! Fuzzes Inochi2D import end to end from raw bytes: container, JSON,
//! textures, mapping, and validation of the result.
#![no_main]

use libfuzzer_sys::fuzz_target;
use riglet_format::{EvalOutput, Limits, ParamValues, Rig};

fuzz_target!(|data: &[u8]| {
    if let Ok(import) = riglet_inochi2d::import(data, &Default::default()) {
        // An accepted file must convert to a valid model.
        let rig = Rig::prepare(&import.document, &Limits::default()).expect("importer produced an invalid model");
        rig.evaluate(&ParamValues::defaults(&rig), &mut EvalOutput::new());
    }
});
