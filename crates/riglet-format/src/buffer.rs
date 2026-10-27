//! Binary buffers referenced by [`BufferRef`] (spec V6). All data is
//! little-endian and decoded with explicit `from_le_bytes`.

use std::collections::BTreeMap;

use crate::model::{BufferRef, ElementType, Vec2};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BufferError {
    #[error("buffer file `{0}` is not under buffers/")]
    NotUnderBuffers(String),
    #[error("buffer file `{0}` does not exist in the package")]
    MissingFile(String),
    #[error("buffer type/components do not match the expected {expected}")]
    WrongType { expected: &'static str },
    #[error("byteOffset {offset} is not aligned to {align} bytes")]
    Misaligned { offset: u64, align: u64 },
    #[error("buffer range ends past the end of `{file}`")]
    OutOfBounds { file: String },
    #[error("buffer has {count} elements, more than the limit of {limit}")]
    TooLarge { count: u64, limit: u64 },
    #[error("buffer contains a non-finite number")]
    NonFinite,
}

fn slice<'a>(files: &'a BTreeMap<String, Vec<u8>>, r: &BufferRef, max_count: u64) -> Result<&'a [u8], BufferError> {
    if !r.file.starts_with("buffers/") {
        return Err(BufferError::NotUnderBuffers(r.file.clone()));
    }
    let data = files.get(&r.file).ok_or_else(|| BufferError::MissingFile(r.file.clone()))?;
    if r.count > max_count {
        return Err(BufferError::TooLarge { count: r.count, limit: max_count });
    }
    let size = r.element_type.size();
    if !r.byte_offset.is_multiple_of(size) {
        return Err(BufferError::Misaligned { offset: r.byte_offset, align: size });
    }
    let oob = || BufferError::OutOfBounds { file: r.file.clone() };
    let len = r.count.checked_mul(u64::from(r.components)).and_then(|n| n.checked_mul(size)).ok_or_else(oob)?;
    let end = r.byte_offset.checked_add(len).ok_or_else(oob)?;
    if end > data.len() as u64 {
        return Err(oob());
    }
    let start = usize::try_from(r.byte_offset).map_err(|_| oob())?;
    let end = usize::try_from(end).map_err(|_| oob())?;
    data.get(start..end).ok_or_else(oob)
}

/// Decodes an `f32` x 2 buffer. `max_count` is checked before allocating.
pub fn read_vec2(files: &BTreeMap<String, Vec<u8>>, r: &BufferRef, max_count: u64) -> Result<Vec<Vec2>, BufferError> {
    if r.element_type != ElementType::F32 || r.components != 2 {
        return Err(BufferError::WrongType { expected: "f32 x 2" });
    }
    let bytes = slice(files, r, max_count)?;
    let mut out = Vec::with_capacity(bytes.len() / 8);
    for &[x0, x1, x2, x3, y0, y1, y2, y3] in bytes.as_chunks::<8>().0 {
        let p = [f32::from_le_bytes([x0, x1, x2, x3]), f32::from_le_bytes([y0, y1, y2, y3])];
        if !p[0].is_finite() || !p[1].is_finite() {
            return Err(BufferError::NonFinite);
        }
        out.push(p);
    }
    Ok(out)
}

/// Decodes a `u16` or `u32` x 1 index buffer. `max_count` is checked
/// before allocating.
pub fn read_indices(files: &BTreeMap<String, Vec<u8>>, r: &BufferRef, max_count: u64) -> Result<Vec<u32>, BufferError> {
    if r.components != 1 || r.element_type == ElementType::F32 {
        return Err(BufferError::WrongType { expected: "u16 or u32 x 1" });
    }
    let bytes = slice(files, r, max_count)?;
    Ok(match r.element_type {
        ElementType::U16 => bytes.as_chunks::<2>().0.iter().map(|&c| u16::from_le_bytes(c).into()).collect(),
        _ => bytes.as_chunks::<4>().0.iter().map(|&c| u32::from_le_bytes(c)).collect(),
    })
}

/// Builds buffer files for writers (converters, tests): appends arrays to
/// one `.bin` file and returns the matching [`BufferRef`]s.
#[derive(Debug, Clone)]
pub struct BufferWriter {
    file: String,
    data: Vec<u8>,
}

impl BufferWriter {
    /// `file` is the package path, e.g. `buffers/main.bin`.
    pub fn new(file: impl Into<String>) -> Self {
        BufferWriter { file: file.into(), data: Vec::new() }
    }

    fn align4(&mut self) {
        while !self.data.len().is_multiple_of(4) {
            self.data.push(0);
        }
    }

    fn make_ref(&self, offset: usize, count: usize, element_type: ElementType, components: u8) -> BufferRef {
        BufferRef { file: self.file.clone(), byte_offset: offset as u64, count: count as u64, element_type, components }
    }

    pub fn push_vec2(&mut self, points: &[Vec2]) -> BufferRef {
        self.align4();
        let offset = self.data.len();
        for p in points {
            self.data.extend_from_slice(&p[0].to_le_bytes());
            self.data.extend_from_slice(&p[1].to_le_bytes());
        }
        self.make_ref(offset, points.len(), ElementType::F32, 2)
    }

    pub fn push_u16(&mut self, values: &[u16]) -> BufferRef {
        self.align4();
        let offset = self.data.len();
        for v in values {
            self.data.extend_from_slice(&v.to_le_bytes());
        }
        self.make_ref(offset, values.len(), ElementType::U16, 1)
    }

    pub fn push_u32(&mut self, values: &[u32]) -> BufferRef {
        self.align4();
        let offset = self.data.len();
        for v in values {
            self.data.extend_from_slice(&v.to_le_bytes());
        }
        self.make_ref(offset, values.len(), ElementType::U32, 1)
    }

    /// Adds the buffer file to `files`.
    pub fn finish(self, files: &mut BTreeMap<String, Vec<u8>>) {
        files.insert(self.file, self.data);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files_with(w: BufferWriter) -> BTreeMap<String, Vec<u8>> {
        let mut files = BTreeMap::new();
        w.finish(&mut files);
        files
    }

    #[test]
    fn round_trips_vec2_and_indices() {
        let mut w = BufferWriter::new("buffers/a.bin");
        let idx = w.push_u16(&[0, 1, 2]);
        let pts = w.push_vec2(&[[1.0, -2.5], [3.0, 4.0]]);
        let idx32 = w.push_u32(&[7, 8, 9]);
        let files = files_with(w);
        assert_eq!(pts.byte_offset % 4, 0);
        assert_eq!(read_vec2(&files, &pts, 100), Ok(vec![[1.0, -2.5], [3.0, 4.0]]));
        assert_eq!(read_indices(&files, &idx, 100), Ok(vec![0, 1, 2]));
        assert_eq!(read_indices(&files, &idx32, 100), Ok(vec![7, 8, 9]));
    }

    #[test]
    fn rejects_bad_refs() {
        let mut w = BufferWriter::new("buffers/a.bin");
        let pts = w.push_vec2(&[[0.0, 0.0]]);
        let files = files_with(w);

        let mut r = pts.clone();
        r.count = 2;
        assert!(matches!(read_vec2(&files, &r, 100), Err(BufferError::OutOfBounds { .. })));

        let mut r = pts.clone();
        r.byte_offset = 2;
        assert!(matches!(read_vec2(&files, &r, 100), Err(BufferError::Misaligned { .. })));

        let mut r = pts.clone();
        r.count = u64::MAX;
        assert!(read_vec2(&files, &r, u64::MAX).is_err());
        assert!(matches!(read_vec2(&files, &r, 100), Err(BufferError::TooLarge { .. })));

        let mut r = pts.clone();
        r.byte_offset = u64::MAX - 3;
        assert!(read_vec2(&files, &r, 100).is_err());

        let mut r = pts.clone();
        r.file = "textures/a.bin".into();
        assert!(matches!(read_vec2(&files, &r, 100), Err(BufferError::NotUnderBuffers(_))));

        let mut r = pts.clone();
        r.components = 1;
        assert!(matches!(read_vec2(&files, &r, 100), Err(BufferError::WrongType { .. })));

        assert!(matches!(read_indices(&files, &pts, 100), Err(BufferError::WrongType { .. })));
    }

    #[test]
    fn rejects_non_finite() {
        let mut w = BufferWriter::new("buffers/a.bin");
        let pts = w.push_vec2(&[[f32::NAN, 0.0]]);
        let files = files_with(w);
        assert_eq!(read_vec2(&files, &pts, 100), Err(BufferError::NonFinite));
    }
}
