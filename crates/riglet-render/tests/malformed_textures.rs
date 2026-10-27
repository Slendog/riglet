//! Mutated PNG textures must give errors, never panics or unbounded
//! allocation (PNG data comes from untrusted packages). Deeper fuzzing:
//! `cargo +nightly fuzz run texture_png`.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use riglet_render::image::decode_texture;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

fn sample_pngs() -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    for (w, h, color, bytes_per_px) in [
        (5u32, 3u32, png::ColorType::Rgba, 4usize),
        (4, 4, png::ColorType::Rgb, 3),
        (7, 2, png::ColorType::Grayscale, 1),
    ] {
        let data: Vec<u8> = (0..(w * h) as usize * bytes_per_px).map(|i| (i * 37) as u8).collect();
        let mut png_bytes = Vec::new();
        let mut e = png::Encoder::new(&mut png_bytes, w, h);
        e.set_color(color);
        e.set_depth(png::BitDepth::Eight);
        e.write_header().unwrap().write_image_data(&data).unwrap();
        out.push(png_bytes);
    }
    out
}

#[test]
fn mutated_pngs_never_panic() {
    let mut rng = Rng(0xA5A5_5A5A_1234_8765);
    for original in sample_pngs() {
        for _ in 0..3000 {
            let mut bytes = original.clone();
            for _ in 0..=(rng.next() % 4) {
                let len = bytes.len().max(1) as u64;
                match rng.next() % 3 {
                    0 => {
                        let i = (rng.next() % len) as usize;
                        if let Some(b) = bytes.get_mut(i) {
                            *b ^= 1 << (rng.next() % 8);
                        }
                    }
                    1 => {
                        let i = (rng.next() % len) as usize;
                        if let Some(b) = bytes.get_mut(i) {
                            *b = rng.next() as u8;
                        }
                    }
                    _ => bytes.truncate((rng.next() % (len + 1)) as usize),
                }
            }
            for premultiplied in [false, true] {
                if let Ok(t) = decode_texture("t", &bytes, premultiplied, 64) {
                    assert_eq!(t.rgba.len(), (t.width * t.height * 4) as usize);
                    assert!(t.rgba.as_chunks::<4>().0.iter().all(|p| p[0] <= p[3] && p[1] <= p[3] && p[2] <= p[3]));
                }
            }
        }
    }
}

#[test]
fn huge_declared_dimensions_are_rejected_before_decoding() {
    // Patch the IHDR width/height of a valid PNG to 60000 x 60000: the
    // header check must reject it without allocating 14 GB.
    let mut bytes = sample_pngs().remove(0);
    bytes[16..20].copy_from_slice(&60_000u32.to_be_bytes());
    bytes[20..24].copy_from_slice(&60_000u32.to_be_bytes());
    assert!(decode_texture("t", &bytes, false, 16_384).is_err());
}
