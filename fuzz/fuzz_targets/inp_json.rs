//! Fuzzes the puppet JSON (wrapped in a valid container with one PNG
//! texture), which reaches the mapping code far more often than raw bytes.
#![no_main]

use libfuzzer_sys::fuzz_target;
use riglet_format::{EvalOutput, Limits, ParamValues, Rig};
use riglet_inochi2d::container::{self, Container, Encoding, RawTexture};

const PNG_2X2: &[u8] = include_bytes!("../seeds/inp_texture.png");

fuzz_target!(|data: &[u8]| {
    let c = Container {
        json: data.to_vec(),
        textures: vec![RawTexture { encoding: Encoding::Png, data: PNG_2X2.to_vec() }],
        extensions: vec![],
    };
    let Ok(bytes) = container::write(&c) else { return };
    if let Ok(import) = riglet_inochi2d::import(&bytes, &Default::default()) {
        let rig = Rig::prepare(&import.document, &Limits::default()).expect("importer produced an invalid model");
        rig.evaluate(&ParamValues::defaults(&rig), &mut EvalOutput::new());
    }
});
