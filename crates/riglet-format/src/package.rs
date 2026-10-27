//! `.riglet` ZIP package read/write (spec "On disk", V1).

use std::collections::{BTreeMap, HashMap};
use std::io::{Cursor, Read, Seek, Write};

use crate::limits::Limits;
use crate::migrate::{self, CURRENT_SCHEMA_VERSION, FORMAT_NAME, MigrateError};
use crate::model::{Manifest, Model};

pub const MANIFEST_FILE: &str = "manifest.json";
pub const MODEL_FILE: &str = "model.json";

/// A package as stored on disk: typed manifest and model, plus every other
/// entry (buffers, textures, source artwork, editor.json, unknown files)
/// kept verbatim so a round-trip loses nothing.
#[derive(Debug, Clone, PartialEq)]
pub struct Document {
    pub manifest: Manifest,
    pub model: Model,
    pub files: BTreeMap<String, Vec<u8>>,
}

#[derive(Debug, thiserror::Error)]
pub enum PackageError {
    #[error("not a readable ZIP archive: {0}")]
    Zip(String),
    #[error("package has more than {limit} entries")]
    TooManyEntries { limit: u64 },
    #[error("entry `{name}` is larger than {limit} bytes uncompressed")]
    EntryTooLarge { name: String, limit: u64 },
    #[error("package is larger than {limit} bytes uncompressed")]
    TotalTooLarge { limit: u64 },
    #[error("entry `{name}` exceeds the {limit}:1 compression ratio limit")]
    CompressionRatio { name: String, limit: u64 },
    #[error("entry `{name}` does not match its declared size")]
    SizeMismatch { name: String },
    #[error("invalid entry path `{name}`: {reason}")]
    BadPath { name: String, reason: &'static str },
    #[error("entries `{first}` and `{second}` differ only by case")]
    CaseCollision { first: String, second: String },
    #[error("required entry `{0}` is missing")]
    Missing(&'static str),
    #[error("`{file}` is not valid: {source}")]
    Json { file: &'static str, source: serde_json::Error },
    #[error(transparent)]
    Migrate(#[from] MigrateError),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

impl From<zip::result::ZipError> for PackageError {
    fn from(e: zip::result::ZipError) -> Self {
        PackageError::Zip(e.to_string())
    }
}

/// Checks a path inside the package: relative, `/`-separated, no `..`, no
/// drive letter, no backslash (spec V1). A trailing `/` (directory entry) is
/// allowed.
pub fn check_package_path(path: &str) -> Result<(), &'static str> {
    let trimmed = path.strip_suffix('/').unwrap_or(path);
    if trimmed.is_empty() {
        return Err("empty path");
    }
    if trimmed.starts_with('/') {
        return Err("absolute path");
    }
    if trimmed.contains('\\') {
        return Err("backslash in path");
    }
    if trimmed.contains(':') {
        return Err("drive letter or `:` in path");
    }
    if trimmed.contains('\0') {
        return Err("NUL in path");
    }
    for part in trimmed.split('/') {
        match part {
            "" => return Err("empty path component"),
            "." | ".." => return Err("`.` or `..` component"),
            _ => {}
        }
    }
    Ok(())
}

pub fn read_package_bytes(bytes: &[u8], limits: &Limits) -> Result<Document, PackageError> {
    read_package(Cursor::new(bytes), limits)
}

pub fn read_package<R: Read + Seek>(reader: R, limits: &Limits) -> Result<Document, PackageError> {
    let mut archive = zip::ZipArchive::new(reader)?;
    let entry_count = archive.len() as u64;
    if entry_count > limits.zip_max_entries {
        return Err(PackageError::TooManyEntries { limit: limits.zip_max_entries });
    }

    let mut files = BTreeMap::new();
    let mut folded: HashMap<String, String> = HashMap::new();
    let mut total: u64 = 0;

    for index in 0..archive.len() {
        let entry = archive.by_index(index)?;
        let name = entry.name()?.into_owned();
        check_package_path(&name).map_err(|reason| PackageError::BadPath { name: name.clone(), reason })?;

        let key = name.strip_suffix('/').unwrap_or(&name).to_lowercase();
        if let Some(first) = folded.insert(key, name.clone()) {
            return Err(PackageError::CaseCollision { first, second: name });
        }
        if entry.is_dir() {
            continue;
        }

        // Check declared sizes first, then enforce them while
        // decompressing, because declared sizes can lie.
        let declared = entry.size();
        if declared > limits.zip_max_entry_uncompressed {
            return Err(PackageError::EntryTooLarge { name, limit: limits.zip_max_entry_uncompressed });
        }
        total = total.saturating_add(declared);
        if total > limits.zip_max_total_uncompressed {
            return Err(PackageError::TotalTooLarge { limit: limits.zip_max_total_uncompressed });
        }
        let compressed = entry.compressed_size();
        if declared > limits.zip_ratio_check_min_size
            && (compressed == 0 || declared / compressed >= limits.zip_max_compression_ratio)
        {
            return Err(PackageError::CompressionRatio { name, limit: limits.zip_max_compression_ratio });
        }

        let capacity = usize::try_from(declared).unwrap_or(0);
        let mut data = Vec::with_capacity(capacity);
        entry.take(declared.saturating_add(1)).read_to_end(&mut data)?;
        if data.len() as u64 != declared {
            return Err(PackageError::SizeMismatch { name });
        }
        files.insert(name, data);
    }

    let manifest_bytes = files.remove(MANIFEST_FILE).ok_or(PackageError::Missing(MANIFEST_FILE))?;
    let model_bytes = files.remove(MODEL_FILE).ok_or(PackageError::Missing(MODEL_FILE))?;
    let mut manifest_value: serde_json::Value =
        serde_json::from_slice(&manifest_bytes).map_err(|source| PackageError::Json { file: MANIFEST_FILE, source })?;
    let mut model_value: serde_json::Value =
        serde_json::from_slice(&model_bytes).map_err(|source| PackageError::Json { file: MODEL_FILE, source })?;

    migrate::migrate(&mut manifest_value, &mut model_value)?;

    let manifest =
        serde_json::from_value(manifest_value).map_err(|source| PackageError::Json { file: MANIFEST_FILE, source })?;
    let model =
        serde_json::from_value(model_value).map_err(|source| PackageError::Json { file: MODEL_FILE, source })?;
    Ok(Document { manifest, model, files })
}

/// Writes `doc` as a ZIP package. The manifest is always written with the
/// current format name and schema version. Output is deterministic: entries
/// are sorted, timestamps and permissions are fixed.
pub fn write_package<W: Write + Seek>(doc: &Document, writer: W) -> Result<W, PackageError> {
    for name in doc.files.keys() {
        check_package_path(name).map_err(|reason| PackageError::BadPath { name: name.clone(), reason })?;
        if name == MANIFEST_FILE || name == MODEL_FILE {
            return Err(PackageError::BadPath { name: name.clone(), reason: "reserved entry name" });
        }
    }

    let mut manifest = doc.manifest.clone();
    manifest.format = FORMAT_NAME.to_owned();
    manifest.schema_version = CURRENT_SCHEMA_VERSION.to_owned();

    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .last_modified_time(zip::DateTime::DEFAULT)
        .unix_permissions(0o644);

    let mut zip = zip::ZipWriter::new(writer);
    zip.start_file(MANIFEST_FILE, options)?;
    zip.write_all(&to_json(&manifest, MANIFEST_FILE)?)?;
    zip.start_file(MODEL_FILE, options)?;
    zip.write_all(&to_json(&doc.model, MODEL_FILE)?)?;
    for (name, data) in &doc.files {
        if name.ends_with('/') {
            zip.add_directory(name.as_str(), options)?;
        } else {
            zip.start_file(name.as_str(), options)?;
            zip.write_all(data)?;
        }
    }
    Ok(zip.finish()?)
}

pub fn write_package_bytes(doc: &Document) -> Result<Vec<u8>, PackageError> {
    Ok(write_package(doc, Cursor::new(Vec::new()))?.into_inner())
}

fn to_json<T: serde::Serialize>(value: &T, file: &'static str) -> Result<Vec<u8>, PackageError> {
    let mut out = serde_json::to_vec_pretty(value).map_err(|source| PackageError::Json { file, source })?;
    out.push(b'\n');
    Ok(out)
}
