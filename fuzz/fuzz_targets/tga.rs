//! Fuzzes the TGA decoder (Inochi2D 0.8.7 writes TGA textures).
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(img) = riglet_inochi2d::tga::decode(data, 4096) {
        assert_eq!(img.rgba.len(), img.width as usize * img.height as usize * 4);
    }
});
