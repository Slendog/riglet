//! Riglet project format library: model types, `.riglet` package I/O,
//! validation, schema migration and the rig evaluator.
//!
//! No GPU, UI or filesystem dependency: everything works from bytes, so
//! converters, the CLI and the editor share this crate.
//!
//! Spec: `docs/format/riglet-format-v0.2.ts`. Rule IDs (E*, V*) in comments
//! refer to its Evaluation and Validation sections.

pub mod bind;
pub mod buffer;
pub mod eval;
pub mod geometry;
pub mod limits;
pub mod migrate;
pub mod model;
pub mod package;
pub mod rig;

pub use eval::{DrawableFrame, EvalOutput, ParamValues};
pub use limits::Limits;
pub use package::{Document, PackageError, read_package, read_package_bytes, write_package, write_package_bytes};
pub use rig::{Issue, Rig, validate};
