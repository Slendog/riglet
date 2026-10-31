//! Fuzzes the puppet JSON (wrapped in a valid container with one PNG
//! texture), which reaches the mapping code far more often than raw bytes.
#![no_main]

use libfuzzer_sys::fuzz_target;
use riglet_format::{EvalOutput, Limits, ParamValues, Rig};
use riglet_inochi2d::container::{self, Container, Encoding, RawTexture};

const PNG_2X2: &[u8] = include_bytes!("../seeds/inp_texture.png");

/// E10: evaluates at the defaults and at every parameter's minimum and
/// maximum (where huge keyforms overflow); the output must be finite or
/// flagged.
fn check_eval(rig: &Rig) {
    let mut out = EvalOutput::new();
    let mut values = ParamValues::defaults(rig);
    rig.evaluate(&values, &mut out);
    assert!(out.finite_or_flagged(), "non-finite output not flagged: {:?}", out.non_finite);
    for end in [0, 1] {
        for (v, p) in values.values.iter_mut().zip(&rig.params) {
            *v = if end == 0 { p.min } else { p.max };
        }
        rig.evaluate(&values, &mut out);
        assert!(out.finite_or_flagged(), "non-finite output not flagged: {:?}", out.non_finite);
    }
}

fuzz_target!(|data: &[u8]| {
    let c = Container {
        json: data.to_vec(),
        textures: vec![RawTexture { encoding: Encoding::Png, data: PNG_2X2.to_vec() }],
        extensions: vec![],
    };
    let Ok(bytes) = container::write(&c) else { return };
    if let Ok(import) = riglet_inochi2d::import(&bytes, &Default::default()) {
        let rig = Rig::prepare(&import.document, &Limits::default()).expect("importer produced an invalid model");
        check_eval(&rig);
    }
});
