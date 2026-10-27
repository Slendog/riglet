//! Fuzzes texture decoding and premultiplication (spec R2): PNG bytes come
//! straight from untrusted packages.
#![no_main]

use libfuzzer_sys::fuzz_target;
use riglet_render::image::decode_texture;

fuzz_target!(|data: &[u8]| {
    for premultiplied in [false, true] {
        if let Ok(t) = decode_texture("fuzz", data, premultiplied, 4096) {
            assert_eq!(t.rgba.len(), (t.width as usize) * (t.height as usize) * 4);
        }
    }
});
