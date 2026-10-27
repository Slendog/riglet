//! Minimal TGA decoder for Inochi2D textures (Inochi2D 0.8.7 writes TGA).
//!
//! Supported, exactly: image types 2 and 3 (uncompressed truecolor and
//! grayscale) and 10 and 11 (RLE), 24/32-bit truecolor or 8-bit grayscale,
//! no color map, either vertical origin. Everything else is an error.
//! Input is untrusted: dimensions are checked before allocating, and RLE
//! packets can't write past the image.

use crate::InochiError;

/// Decoded straight RGBA8, top row first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

fn bad(reason: &str) -> InochiError {
    InochiError::Texture(format!("TGA: {reason}"))
}

pub fn decode(data: &[u8], max_dimension: u32) -> Result<Image, InochiError> {
    let header: &[u8; 18] = data.get(..18).and_then(|h| h.try_into().ok()).ok_or_else(|| bad("truncated header"))?;
    let [id_len, color_map_type, image_type, _, _, _, _, _, _, _, _, _, w0, w1, h0, h1, bpp, descriptor] = *header;
    if color_map_type != 0 {
        return Err(bad("color-mapped images are not supported"));
    }
    let (rle, gray) = match image_type {
        2 => (false, false),
        3 => (false, true),
        10 => (true, false),
        11 => (true, true),
        other => return Err(bad(&format!("image type {other} is not supported"))),
    };
    let bytes_per_pixel = match (gray, bpp) {
        (false, 24) => 3,
        (false, 32) => 4,
        (true, 8) => 1,
        _ => return Err(bad(&format!("{bpp} bits per pixel is not supported for this image type"))),
    };
    if descriptor & 0x10 != 0 {
        return Err(bad("right-to-left images are not supported"));
    }
    let top_down = descriptor & 0x20 != 0;
    let width = u32::from(u16::from_le_bytes([w0, w1]));
    let height = u32::from(u16::from_le_bytes([h0, h1]));
    if width == 0 || height == 0 {
        return Err(bad("zero size"));
    }
    if width > max_dimension || height > max_dimension {
        return Err(InochiError::Limit { what: "TGA dimensions", limit: u64::from(max_dimension) });
    }
    let pixels = (width as usize) * (height as usize);
    let mut src = data.get(18 + usize::from(id_len)..).ok_or_else(|| bad("truncated image ID"))?;

    let to_rgba = |p: &[u8]| -> [u8; 4] {
        match *p {
            [v] => [v, v, v, 255],
            [b, g, r] => [r, g, b, 255],
            [b, g, r, a] => [r, g, b, a],
            _ => [0, 0, 0, 0],
        }
    };
    let mut rows = Vec::with_capacity(pixels * 4);
    if rle {
        let mut written = 0usize;
        while written < pixels {
            let (&packet, rest) = src.split_first().ok_or_else(|| bad("truncated RLE data"))?;
            src = rest;
            let count = usize::from(packet & 0x7f) + 1;
            if written + count > pixels {
                return Err(bad("RLE packet runs past the image"));
            }
            if packet & 0x80 != 0 {
                let p = src.get(..bytes_per_pixel).ok_or_else(|| bad("truncated RLE data"))?;
                let px = to_rgba(p);
                src = src.get(bytes_per_pixel..).unwrap_or(&[]);
                for _ in 0..count {
                    rows.extend_from_slice(&px);
                }
            } else {
                let n = count * bytes_per_pixel;
                let raw = src.get(..n).ok_or_else(|| bad("truncated RLE data"))?;
                src = src.get(n..).unwrap_or(&[]);
                for p in raw.chunks_exact(bytes_per_pixel) {
                    rows.extend_from_slice(&to_rgba(p));
                }
            }
            written += count;
        }
    } else {
        let raw = src.get(..pixels * bytes_per_pixel).ok_or_else(|| bad("truncated pixel data"))?;
        for p in raw.chunks_exact(bytes_per_pixel) {
            rows.extend_from_slice(&to_rgba(p));
        }
    }

    // TGA's default origin is bottom-left; flip to top row first.
    if !top_down {
        let stride = width as usize * 4;
        let flipped: Vec<u8> = rows.chunks_exact(stride).rev().flatten().copied().collect();
        rows = flipped;
    }
    Ok(Image { width, height, rgba: rows })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(image_type: u8, w: u16, h: u16, bpp: u8, descriptor: u8) -> Vec<u8> {
        let mut out = vec![0u8, 0, image_type, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        out.extend_from_slice(&w.to_le_bytes());
        out.extend_from_slice(&h.to_le_bytes());
        out.extend([bpp, descriptor]);
        out
    }

    #[test]
    fn uncompressed_32bit_bottom_up() {
        let mut t = header(2, 2, 2, 32, 8);
        // Bottom row first in the file: blue, green; then top: red, white.
        t.extend([255, 0, 0, 255, 0, 255, 0, 128, 0, 0, 255, 255, 255, 255, 255, 64]);
        let img = decode(&t, 16).unwrap();
        assert_eq!(img.rgba, [255, 0, 0, 255, 255, 255, 255, 64, 0, 0, 255, 255, 0, 255, 0, 128]);
    }

    #[test]
    fn rle_24bit_top_down_and_grayscale() {
        let mut t = header(10, 3, 1, 24, 0x20);
        t.extend([0x81, 10, 20, 30, 0x00, 1, 2, 3]); // run of 2, then 1 raw
        assert_eq!(decode(&t, 16).unwrap().rgba, [30, 20, 10, 255, 30, 20, 10, 255, 3, 2, 1, 255]);
        let mut g = header(3, 2, 1, 8, 0x20);
        g.extend([7, 200]);
        assert_eq!(decode(&g, 16).unwrap().rgba, [7, 7, 7, 255, 200, 200, 200, 255]);
    }

    #[test]
    fn rejects_unsupported_and_malformed() {
        let mut cm = header(1, 1, 1, 8, 0);
        cm[1] = 1;
        assert!(decode(&cm, 16).is_err());
        assert!(decode(&header(9, 1, 1, 8, 0), 16).is_err());
        assert!(decode(&header(2, 1, 1, 16, 0), 16).is_err());
        assert!(decode(&header(2, 1, 1, 32, 0x10), 16).is_err());
        assert!(decode(&header(2, 0, 1, 32, 0), 16).is_err());
        assert!(matches!(decode(&header(2, 100, 1, 32, 0), 16), Err(InochiError::Limit { .. })));
        // Truncated pixels, and an RLE run past the end of the image.
        assert!(decode(&header(2, 2, 2, 32, 0), 16).is_err());
        let mut run = header(10, 2, 1, 24, 0);
        run.extend([0x85, 1, 2, 3]);
        assert!(decode(&run, 16).is_err());
        assert!(decode(&[0; 5], 16).is_err());
    }
}
