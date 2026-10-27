//! Inochi2D (`.inp`, `.inx`) import for Riglet. Design:
//! `docs/design/m3-inochi2d.md`. Reads the Inochi2D 0.7/0.8 container
//! ("INP1"); 0.9's "INP2" is detected and rejected.
//!
//! Every lossy step emits a [`ConversionWarning`] with a stable code (also
//! recorded in the document's provenance); data Riglet doesn't model is kept
//! in `extensions` under `"inochi2d"` so export can restore it. Input is
//! untrusted: sizes are checked before allocating, and the result is
//! validated with `Rig::prepare` like any file.

pub mod container;
mod export;
mod import;
pub mod reinterpolate;
pub mod tga;

pub use export::{Export, export};
pub use import::{Import, Options, import};
pub use riglet_format::model::ConversionWarning;

#[derive(Debug, thiserror::Error)]
pub enum InochiError {
    #[error("not an Inochi2D file (missing TRNSRTS magic)")]
    NotInochi,
    #[error("Inochi2D 0.9 files (INP2, magic TRNSRTS2) are not supported yet")]
    Inp2Unsupported,
    #[error("truncated file: {0}")]
    Truncated(&'static str),
    #[error("malformed file: expected section {0}")]
    BadSection(&'static str),
    #[error("{what} exceeds the limit of {limit}")]
    Limit { what: &'static str, limit: u64 },
    #[error("the node tree nests deeper than the JSON reader allows (limit code INP_NESTING_TOO_DEEP)")]
    NestingTooDeep,
    #[error("invalid JSON payload: {0}")]
    Json(String),
    #[error("invalid puppet: {0}")]
    Puppet(String),
    #[error("texture: {0}")]
    Texture(String),
    #[error("the converted model failed validation (an importer bug): {0}")]
    InvalidResult(String),
    #[error("the model to export is invalid: {0}")]
    InvalidModel(String),
}

impl InochiError {
    /// Stable code for reports and the CLI.
    pub fn code(&self) -> &'static str {
        match self {
            InochiError::NotInochi => "INP_NOT_INOCHI",
            InochiError::Inp2Unsupported => "INP2_UNSUPPORTED",
            InochiError::Truncated(_) => "INP_TRUNCATED",
            InochiError::BadSection(_) => "INP_BAD_SECTION",
            InochiError::Limit { .. } => "INP_LIMIT",
            InochiError::NestingTooDeep => "INP_NESTING_TOO_DEEP",
            InochiError::Json(_) => "INP_JSON",
            InochiError::Puppet(_) => "INP_PUPPET",
            InochiError::Texture(_) => "INP_TEXTURE",
            InochiError::InvalidResult(_) => "INP_INVALID_RESULT",
            InochiError::InvalidModel(_) => "INP_INVALID_MODEL",
        }
    }
}
