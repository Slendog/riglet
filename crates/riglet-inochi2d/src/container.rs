//! The Inochi2D 0.7/0.8 container ("INP1"), shared by `.inp` and `.inx`:
//!
//! ```text
//! "TRNSRTS\0" | u32 jsonLength | JSON
//! "TEX_SECT" | u32 count | count x { u32 length | u8 encoding | bytes }
//! ["EXT_SECT" | u32 count | count x { u32 nameLength | name | u32 length | bytes }]
//! ```
//!
//! All integers are big-endian. Every length and count is checked against
//! the remaining input and the reader limits before anything is allocated.

use riglet_format::Limits;

use crate::InochiError;

pub const MAGIC: &[u8; 8] = b"TRNSRTS\0";
/// Inochi2D 0.9's in-development format; not supported.
pub const MAGIC_INP2: &[u8; 8] = b"TRNSRTS2";
const TEX_SECT: &[u8; 8] = b"TEX_SECT";
const EXT_SECT: &[u8; 8] = b"EXT_SECT";

/// Texture encodings in the container.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    Png,
    Tga,
    Bc7,
    Unknown(u8),
}

impl Encoding {
    pub fn from_byte(b: u8) -> Self {
        match b {
            0 => Encoding::Png,
            1 => Encoding::Tga,
            2 => Encoding::Bc7,
            other => Encoding::Unknown(other),
        }
    }

    pub fn byte(self) -> u8 {
        match self {
            Encoding::Png => 0,
            Encoding::Tga => 1,
            Encoding::Bc7 => 2,
            Encoding::Unknown(b) => b,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawTexture {
    pub encoding: Encoding,
    pub data: Vec<u8>,
}

/// The parsed container: JSON payload, textures (by slot index), and the
/// opaque extension entries, in file order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Container {
    pub json: Vec<u8>,
    pub textures: Vec<RawTexture>,
    pub extensions: Vec<(String, Vec<u8>)>,
}

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn remaining(&self) -> usize {
        self.data.len().saturating_sub(self.pos)
    }

    fn take(&mut self, n: usize, what: &'static str) -> Result<&'a [u8], InochiError> {
        let end = self.pos.checked_add(n).ok_or(InochiError::Truncated(what))?;
        let bytes = self.data.get(self.pos..end).ok_or(InochiError::Truncated(what))?;
        self.pos = end;
        Ok(bytes)
    }

    fn u32(&mut self, what: &'static str) -> Result<u32, InochiError> {
        let b = self.take(4, what)?;
        b.try_into().map(u32::from_be_bytes).map_err(|_| InochiError::Truncated(what))
    }

    /// A length that must fit in the rest of the input and under `cap`.
    fn len(&mut self, what: &'static str, cap: u64) -> Result<usize, InochiError> {
        let n = self.u32(what)?;
        if u64::from(n) > cap {
            return Err(InochiError::Limit { what, limit: cap });
        }
        if n as usize > self.remaining() {
            return Err(InochiError::Truncated(what));
        }
        Ok(n as usize)
    }
}

pub fn parse(data: &[u8], limits: &Limits) -> Result<Container, InochiError> {
    if data.get(..8) == Some(&MAGIC_INP2[..]) {
        return Err(InochiError::Inp2Unsupported);
    }
    if data.get(..8) != Some(&MAGIC[..]) {
        return Err(InochiError::NotInochi);
    }
    let total_cap = limits.zip_max_total_uncompressed;
    if data.len() as u64 > total_cap {
        return Err(InochiError::Limit { what: "file size", limit: total_cap });
    }
    let entry_cap = limits.zip_max_entry_uncompressed;
    let mut r = Reader { data, pos: 8 };
    let json_len = r.len("JSON payload", entry_cap)?;
    let json = r.take(json_len, "JSON payload")?.to_vec();

    if r.take(8, "texture section")? != &TEX_SECT[..] {
        return Err(InochiError::BadSection("TEX_SECT"));
    }
    let count = r.u32("texture count")?;
    if u64::from(count) > limits.zip_max_entries {
        return Err(InochiError::Limit { what: "texture count", limit: limits.zip_max_entries });
    }
    let mut textures = Vec::with_capacity((count as usize).min(r.remaining() / 5));
    for _ in 0..count {
        let len = r.len("texture", entry_cap)?;
        let encoding = Encoding::from_byte(r.take(1, "texture encoding")?.first().copied().unwrap_or(0));
        textures.push(RawTexture { encoding, data: r.take(len, "texture")?.to_vec() });
    }

    let mut extensions = Vec::new();
    if r.remaining() > 0 {
        if r.take(8, "extension section")? != &EXT_SECT[..] {
            return Err(InochiError::BadSection("EXT_SECT"));
        }
        let count = r.u32("extension count")?;
        if u64::from(count) > limits.zip_max_entries {
            return Err(InochiError::Limit { what: "extension count", limit: limits.zip_max_entries });
        }
        for _ in 0..count {
            let name_len = r.len("extension name", 4096)?;
            let name = String::from_utf8(r.take(name_len, "extension name")?.to_vec())
                .map_err(|_| InochiError::BadSection("EXT_SECT name is not UTF-8"))?;
            let len = r.len("extension payload", entry_cap)?;
            extensions.push((name, r.take(len, "extension payload")?.to_vec()));
        }
    }
    Ok(Container { json, textures, extensions })
}

/// Writes a container (export, and test fixtures).
pub fn write(c: &Container) -> Result<Vec<u8>, InochiError> {
    let len32 = |n: usize, what: &'static str| {
        u32::try_from(n).map_err(|_| InochiError::Limit { what, limit: u64::from(u32::MAX) })
    };
    let mut out = Vec::with_capacity(c.json.len() + 64);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&len32(c.json.len(), "JSON payload")?.to_be_bytes());
    out.extend_from_slice(&c.json);
    out.extend_from_slice(TEX_SECT);
    out.extend_from_slice(&len32(c.textures.len(), "texture count")?.to_be_bytes());
    for t in &c.textures {
        out.extend_from_slice(&len32(t.data.len(), "texture")?.to_be_bytes());
        out.push(t.encoding.byte());
        out.extend_from_slice(&t.data);
    }
    if !c.extensions.is_empty() {
        out.extend_from_slice(EXT_SECT);
        out.extend_from_slice(&len32(c.extensions.len(), "extension count")?.to_be_bytes());
        for (name, data) in &c.extensions {
            out.extend_from_slice(&len32(name.len(), "extension name")?.to_be_bytes());
            out.extend_from_slice(name.as_bytes());
            out.extend_from_slice(&len32(data.len(), "extension payload")?.to_be_bytes());
            out.extend_from_slice(data);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Container {
        Container {
            json: br#"{"nodes":{}}"#.to_vec(),
            textures: vec![
                RawTexture { encoding: Encoding::Png, data: vec![1, 2, 3] },
                RawTexture { encoding: Encoding::Tga, data: vec![] },
            ],
            extensions: vec![("com.example.thing".into(), b"{}".to_vec())],
        }
    }

    #[test]
    fn round_trips() {
        let c = sample();
        assert_eq!(parse(&write(&c).unwrap(), &Limits::default()).unwrap(), c);
        let mut no_ext = sample();
        no_ext.extensions.clear();
        assert_eq!(parse(&write(&no_ext).unwrap(), &Limits::default()).unwrap(), no_ext);
    }

    #[test]
    fn rejects_bad_input() {
        let bytes = write(&sample()).unwrap();
        // Cutting exactly where the optional EXT_SECT starts leaves a valid
        // file without extensions; every other cut is an error.
        let ext_start = bytes.windows(8).position(|w| w == EXT_SECT).unwrap();
        for cut in (0..bytes.len()).filter(|&c| c != ext_start) {
            assert!(parse(&bytes[..cut], &Limits::default()).is_err(), "cut at {cut}");
        }
        assert!(parse(&bytes[..ext_start], &Limits::default()).unwrap().extensions.is_empty());
        let mut inp2 = bytes.clone();
        inp2[7] = b'2';
        assert!(matches!(parse(&inp2, &Limits::default()), Err(InochiError::Inp2Unsupported)));
        assert!(matches!(parse(b"not inochi", &Limits::default()), Err(InochiError::NotInochi)));
        // A huge declared JSON length: rejected before allocating.
        let mut huge = MAGIC.to_vec();
        huge.extend_from_slice(&u32::MAX.to_be_bytes());
        assert!(matches!(parse(&huge, &Limits::default()), Err(InochiError::Limit { .. } | InochiError::Truncated(_))));
        let limits = Limits { zip_max_entries: 1, ..Limits::default() };
        assert!(matches!(parse(&bytes, &limits), Err(InochiError::Limit { what: "texture count", .. })));
    }
}
