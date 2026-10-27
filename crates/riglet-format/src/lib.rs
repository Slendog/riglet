//! Riglet project format library: model types, `.riglet` package I/O,
//! validation, schema migration and the rig evaluator.
//!
//! No GPU, UI or filesystem dependency: everything works from bytes, so
//! converters, the CLI and the editor share this crate.
//!
//! Spec: `docs/format/riglet-format-v0.2.ts`. Rule IDs (E*, V*) in comments
//! refer to its Evaluation and Validation sections.

pub mod migrate;
pub mod model;
