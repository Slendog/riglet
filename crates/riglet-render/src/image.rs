//! CPU-side pixel work with exact integer rules: texture decoding and
//! premultiplication (spec R2), readback row unpadding, and the output
//! image's un-premultiplication (R8). No GPU needed, so all of it is tested
//! on every platform.

use std::io::Cursor;

use crate::RenderError;

/// A decoded texture: premultiplied RGBA8, row-major, top row first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextureData {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// R2 premultiply on 8-bit values: `c' = (c * a + 127) / 255`.
pub fn premultiply_channel(c: u8, a: u8) -> u8 {
    ((u32::from(c) * u32::from(a) + 127) / 255) as u8
}

/// Decodes a PNG and returns premultiplied RGBA8 (R2). `premultiplied`
/// is the texture's `premultipliedAlpha` flag: when false the pixels are
/// premultiplied here; when true, channels above alpha are clamped to it.
/// `max_dimension` is checked from the header before any pixel buffer is
/// allocated.
pub fn decode_texture(
    id: &str,
    bytes: &[u8],
    premultiplied: bool,
    max_dimension: u32,
) -> Result<TextureData, RenderError> {
    let bad = |reason: String| RenderError::Texture { id: id.to_owned(), reason };
    let mut decoder = png::Decoder::new(Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    // Header first: dimensions bound every allocation below.
    let header = {
        let mut probe = png::Decoder::new(Cursor::new(bytes));
        probe.set_transformations(png::Transformations::IDENTITY);
        let reader = probe.read_info().map_err(|e| bad(e.to_string()))?;
        let info = reader.info();
        (info.width, info.height)
    };
    let (width, height) = header;
    if width == 0 || height == 0 {
        return Err(bad("image has zero size".into()));
    }
    if width > max_dimension || height > max_dimension {
        return Err(RenderError::TextureTooLarge { id: id.to_owned(), width, height, max: max_dimension });
    }
    // RGBA8 output plus decoder working space.
    let budget = (width as usize).saturating_mul(height as usize).saturating_mul(8).saturating_add(1 << 20);
    decoder.set_limits(png::Limits { bytes: budget });
    let mut reader = decoder.read_info().map_err(|e| bad(e.to_string()))?;
    let size = reader.output_buffer_size().ok_or_else(|| bad("image is too large".into()))?;
    let mut buf = vec![0u8; size];
    let frame = reader.next_frame(&mut buf).map_err(|e| bad(e.to_string()))?;
    let data = buf.get(..frame.buffer_size()).ok_or_else(|| bad("short frame".into()))?;
    let pixels = (width as usize) * (height as usize);

    let mut rgba = Vec::with_capacity(pixels * 4);
    match frame.color_type {
        png::ColorType::Rgba => rgba.extend_from_slice(data),
        png::ColorType::Rgb => data.as_chunks::<3>().0.iter().for_each(|&[r, g, b]| rgba.extend([r, g, b, 255])),
        png::ColorType::GrayscaleAlpha => data.as_chunks::<2>().0.iter().for_each(|&[v, a]| rgba.extend([v, v, v, a])),
        png::ColorType::Grayscale => data.iter().for_each(|&v| rgba.extend([v, v, v, 255])),
        other => return Err(bad(format!("unsupported color type {other:?} after expansion"))),
    }
    if rgba.len() != pixels * 4 {
        return Err(bad("pixel data has the wrong size".into()));
    }
    for px in rgba.as_chunks_mut::<4>().0 {
        let a = px[3];
        for c in &mut px[..3] {
            *c = if premultiplied { (*c).min(a) } else { premultiply_channel(*c, a) };
        }
    }
    Ok(TextureData { width, height, rgba })
}

/// Removes the per-row padding of a GPU readback (rows are padded to
/// `padded_row` bytes, a multiple of 256 in wgpu).
pub fn unpad_rows(padded: &[u8], width: u32, height: u32, padded_row: usize) -> Option<Vec<u8>> {
    let row = width as usize * 4;
    let mut out = Vec::with_capacity(row * height as usize);
    for y in 0..height as usize {
        out.extend_from_slice(padded.get(y * padded_row..y * padded_row + row)?);
    }
    Some(out)
}

/// Final output image: straight-alpha RGBA8 (R8), top row first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl Image {
    /// R8: from premultiplied RGBA8, optionally flattened over an opaque
    /// `background` (straight RGB) first. Additive blending can leave
    /// `p > a`; it is clamped to `a` before un-premultiplying (on a
    /// transparent output) and shows as light over a background.
    pub fn from_premultiplied(width: u32, height: u32, mut rgba: Vec<u8>, background: Option<[u8; 3]>) -> Self {
        for px in rgba.as_chunks_mut::<4>().0 {
            let a = u32::from(px[3]);
            match background {
                Some(bg) => {
                    // Premultiplied over: C = p + bg * (1 - a).
                    for (c, b) in px[..3].iter_mut().zip(bg) {
                        let under = (u32::from(b) * (255 - a) + 127) / 255;
                        *c = (u32::from(*c) + under).min(255) as u8;
                    }
                    px[3] = 255;
                }
                None => {
                    for c in &mut px[..3] {
                        let p = u32::from(*c).min(a);
                        *c = (p * 255 + a / 2).checked_div(a).map_or(0, |v| v.min(255) as u8);
                    }
                }
            }
        }
        Image { width, height, rgba }
    }

    pub fn to_png(&self) -> Result<Vec<u8>, RenderError> {
        let mut out = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut out, self.width, self.height);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            // R1: the values are sRGB-encoded.
            encoder.set_source_srgb(png::SrgbRenderingIntent::Perceptual);
            let mut writer = encoder.write_header().map_err(|e| RenderError::Encode(e.to_string()))?;
            writer.write_image_data(&self.rgba).map_err(|e| RenderError::Encode(e.to_string()))?;
        }
        Ok(out)
    }

    /// Decodes a straight-alpha RGBA8 PNG (goldens written by
    /// [`Image::to_png`]). Values are kept exactly; nothing is premultiplied.
    pub fn from_png(bytes: &[u8]) -> Result<Self, RenderError> {
        let bad = |e: String| RenderError::Encode(e);
        let decoder = png::Decoder::new(Cursor::new(bytes));
        let mut reader = decoder.read_info().map_err(|e| bad(e.to_string()))?;
        let (width, height) = (reader.info().width, reader.info().height);
        if width > 16_384 || height > 16_384 {
            return Err(bad("image is too large".into()));
        }
        let mut buf = vec![0u8; reader.output_buffer_size().ok_or_else(|| bad("image is too large".into()))?];
        let frame = reader.next_frame(&mut buf).map_err(|e| bad(e.to_string()))?;
        if frame.color_type != png::ColorType::Rgba || frame.bit_depth != png::BitDepth::Eight {
            return Err(bad("expected an 8-bit RGBA PNG".into()));
        }
        buf.truncate(frame.buffer_size());
        Ok(Image { width, height, rgba: buf })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Independent oracle: round-to-nearest of the exact rational value.
    /// For c * a / 255 an exact .5 can't occur (255 is odd), so there are
    /// no ties to decide.
    fn premultiply_oracle(c: u8, a: u8) -> u8 {
        (f64::from(c) * f64::from(a) / 255.0 + 0.5).floor() as u8
    }

    /// Oracle for R8: round p * 255 / a, halves up, after p = min(p, a).
    fn unpremultiply_oracle(p: u8, a: u8) -> u8 {
        if a == 0 {
            return 0;
        }
        let p = p.min(a);
        (f64::from(p) * 255.0 / f64::from(a) + 0.5).floor().min(255.0) as u8
    }

    #[test]
    fn premultiply_matches_oracle_for_every_pair() {
        for c in 0..=255u8 {
            for a in 0..=255u8 {
                assert_eq!(premultiply_channel(c, a), premultiply_oracle(c, a), "c={c} a={a}");
            }
        }
    }

    #[test]
    fn unpremultiply_matches_oracle_for_every_pair() {
        // All 256 x 256 (premultiplied value, alpha) pairs, including the
        // invalid p > a that additive blending produces (R4, R8).
        for p in 0..=255u8 {
            for a in 0..=255u8 {
                let got = Image::from_premultiplied(1, 1, vec![p, p, p, a], None).rgba;
                let want = unpremultiply_oracle(p, a);
                assert_eq!(got, [want, want, want, a], "p={p} a={a}");
            }
        }
    }

    #[test]
    fn premultiply_rounds_exactly() {
        assert_eq!(premultiply_channel(255, 255), 255);
        assert_eq!(premultiply_channel(255, 0), 0);
        assert_eq!(premultiply_channel(200, 128), 100); // 25600 + 127 = 25727 / 255 = 100
        assert_eq!(premultiply_channel(1, 128), 1); // 128 + 127 = 255 / 255 = 1
        assert_eq!(premultiply_channel(1, 127), 0); // 127 + 127 = 254 / 255 = 0
        for c in 0..=255u8 {
            for a in 0..=255u8 {
                let p = premultiply_channel(c, a);
                assert!(p <= a, "premultiplied {p} > alpha {a}");
            }
        }
    }

    #[test]
    fn unpremultiply_round_trips_premultiplied_values() {
        // For every alpha, premultiply then un-premultiply lands within one
        // step of the original (8-bit premultiplied data loses precision).
        for a in 1..=255u8 {
            for c in 0..=255u8 {
                let p = premultiply_channel(c, a);
                let img = Image::from_premultiplied(1, 1, vec![p, p, p, a], None);
                let back = img.rgba[0];
                let step = 255 / u32::from(a) + 1;
                assert!(u32::from(back).abs_diff(u32::from(c)) <= step, "c={c} a={a} p={p} back={back}");
            }
        }
    }

    #[test]
    fn unpremultiply_clamps_additive_overflow() {
        // Additive over transparency: p > a.
        let img = Image::from_premultiplied(1, 1, vec![200, 10, 0, 100], None);
        assert_eq!(img.rgba, [255, 26, 0, 100]);
        let img = Image::from_premultiplied(1, 1, vec![200, 0, 0, 0], None);
        assert_eq!(img.rgba, [0, 0, 0, 0]);
    }

    #[test]
    fn background_shows_additive_light() {
        let img = Image::from_premultiplied(1, 1, vec![200, 0, 0, 0], Some([0, 0, 0]));
        assert_eq!(img.rgba, [200, 0, 0, 255]);
        let img = Image::from_premultiplied(1, 1, vec![0, 0, 0, 0], Some([10, 20, 30]));
        assert_eq!(img.rgba, [10, 20, 30, 255]);
    }

    #[test]
    fn unpad_handles_odd_widths() {
        for width in [1u32, 3, 63, 64, 65, 127] {
            let height = 3u32;
            let row = width as usize * 4;
            let padded_row = row.div_ceil(256) * 256;
            let mut padded = vec![0xEE; padded_row * height as usize];
            for y in 0..height as usize {
                for x in 0..row {
                    padded[y * padded_row + x] = (y * 7 + x) as u8;
                }
            }
            let out = unpad_rows(&padded, width, height, padded_row).unwrap();
            assert_eq!(out.len(), row * height as usize);
            for y in 0..height as usize {
                for x in 0..row {
                    assert_eq!(out[y * row + x], (y * 7 + x) as u8, "width {width}");
                }
            }
        }
        assert_eq!(unpad_rows(&[0; 10], 4, 2, 256), None);
    }

    fn png(width: u32, height: u32, color: png::ColorType, data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut e = png::Encoder::new(&mut out, width, height);
        e.set_color(color);
        e.set_depth(png::BitDepth::Eight);
        e.write_header().unwrap().write_image_data(data).unwrap();
        out
    }

    #[test]
    fn decodes_and_premultiplies() {
        let bytes = png(2, 1, png::ColorType::Rgba, &[200, 100, 50, 128, 10, 20, 30, 255]);
        let t = decode_texture("t", &bytes, false, 64).unwrap();
        assert_eq!(t.rgba, [100, 50, 25, 128, 10, 20, 30, 255]);
        // Flagged premultiplied: kept, but channels above alpha clamp.
        let t = decode_texture("t", &bytes, true, 64).unwrap();
        assert_eq!(t.rgba, [128, 100, 50, 128, 10, 20, 30, 255]);
        let gray = png(1, 1, png::ColorType::Grayscale, &[77]);
        assert_eq!(decode_texture("g", &gray, false, 64).unwrap().rgba, [77, 77, 77, 255]);
    }

    #[test]
    fn rejects_oversized_and_garbage_textures() {
        let bytes = png(4, 2, png::ColorType::Rgba, &[0; 32]);
        assert!(matches!(decode_texture("t", &bytes, false, 3), Err(RenderError::TextureTooLarge { .. })));
        assert!(decode_texture("t", b"not a png", false, 64).is_err());
        assert!(decode_texture("t", &bytes[..bytes.len() - 20], false, 64).is_err());
    }

    #[test]
    fn png_round_trip() {
        let img = Image { width: 3, height: 1, rgba: vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12] };
        assert_eq!(Image::from_png(&img.to_png().unwrap()).unwrap(), img);
    }
}
