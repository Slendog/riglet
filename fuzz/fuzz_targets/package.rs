//! Fuzzes the full .riglet pipeline: ZIP read, migration, validation,
//! evaluation. Must never panic, hang, or allocate without bound.
#![no_main]

use libfuzzer_sys::fuzz_target;
use riglet_format::{Limits, ParamValues, Rig, evaluate, read_package_bytes};

fuzz_target!(|data: &[u8]| {
    let limits = Limits::default();
    if let Ok(doc) = read_package_bytes(data, &limits)
        && let Ok(rig) = Rig::prepare(&doc, &limits)
    {
        let _ = evaluate(&rig, &ParamValues::defaults(&rig));
    }
});
