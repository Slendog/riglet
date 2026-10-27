//! Fuzzes Inochi2D export from untrusted packages: any package that loads
//! and validates must export without panicking, and the result must import
//! again into a valid model.
#![no_main]

use libfuzzer_sys::fuzz_target;
use riglet_format::{Limits, Rig};

fuzz_target!(|data: &[u8]| {
    let Ok(doc) = riglet_format::read_package_bytes(data, &Limits::default()) else { return };
    if Rig::prepare(&doc, &Limits::default()).is_err() {
        return;
    }
    let exported = riglet_inochi2d::export(&doc).expect("a valid model must export");
    let back = riglet_inochi2d::import(&exported.bytes, &Default::default()).expect("an export must import again");
    Rig::prepare(&back.document, &Limits::default()).expect("re-import produced an invalid model");
});
