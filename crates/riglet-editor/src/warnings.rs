//! The warnings panel's entries, without UI types: import warnings (from
//! the document's provenance), the last export's warnings, and the live
//! E10 report of the current frame, each labeled by source and linked to
//! the object it names.

use riglet_format::model::{ConversionWarning, ProvenanceSource};
use riglet_format::{Document, EvalOutput, Rig};

/// Where an entry comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    /// A conversion warning recorded in `manifest.provenance` when the
    /// model was imported. It describes the model as imported, so it can
    /// be out of date after edits.
    Import { source: ProvenanceSource, source_version: Option<String>, imported_at: String },
    /// The last export from this session (file name).
    Export { file: String },
    /// The current frame (E10), recomputed whenever the frame changes.
    Evaluation,
}

/// What an entry's object id refers to now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    None,
    Node(String),
    Parameter(String),
    /// A motion or texture: named, not selectable.
    Other {
        kind: &'static str,
        id: String,
    },
    /// The id matches nothing in the model any more (removed by an edit,
    /// or stale data in the file): shown, but not linked.
    Removed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub origin: Origin,
    pub code: String,
    pub message: String,
    pub target: Target,
}

/// Resolves an object id against the current document.
pub fn target(doc: &Document, id: Option<&str>) -> Target {
    match id {
        None => Target::None,
        Some(id) if doc.model.nodes.contains_key(id) => Target::Node(id.to_owned()),
        Some(id) if doc.model.parameters.iter().any(|p| p.id == id) => Target::Parameter(id.to_owned()),
        Some(id) if doc.model.motions.iter().any(|m| m.id == id) => Target::Other { kind: "motion", id: id.to_owned() },
        Some(id) if doc.model.textures.iter().any(|t| t.id == id) => {
            Target::Other { kind: "texture", id: id.to_owned() }
        }
        Some(id) => Target::Removed(id.to_owned()),
    }
}

/// Import warnings from every provenance record, oldest import first.
pub fn import_entries(doc: &Document) -> Vec<Entry> {
    doc.manifest
        .provenance
        .iter()
        .flat_map(|p| {
            let origin = Origin::Import {
                source: p.source,
                source_version: p.source_version.clone(),
                imported_at: p.imported_at.clone(),
            };
            p.warnings.iter().map(move |w| (origin.clone(), w))
        })
        .map(|(origin, w)| entry(doc, origin, w))
        .collect()
}

/// Warnings of an export, linked against the document that was exported.
pub fn export_entries(doc: &Document, file: &str, warnings: &[ConversionWarning]) -> Vec<Entry> {
    warnings.iter().map(|w| entry(doc, Origin::Export { file: file.to_owned() }, w)).collect()
}

fn entry(doc: &Document, origin: Origin, w: &ConversionWarning) -> Entry {
    Entry { origin, code: w.code.clone(), message: w.message.clone(), target: target(doc, w.object_id.as_deref()) }
}

/// The E10 report of `frame` as entries: none when everything is finite.
/// A pure function of the frame, so the panel shows exactly the current
/// state: an entry appears when values overflow and disappears when they
/// are finite again, with no flicker in between.
pub fn evaluation_entries(rig: &Rig, frame: &EvalOutput) -> Vec<Entry> {
    let nf = &frame.non_finite;
    let node = |i: Option<usize>| match i.and_then(|i| rig.nodes.get(i)) {
        Some(n) => (Target::Node(n.id.clone()), format!(" (first in `{}`)", n.name)),
        None => (Target::None, String::new()),
    };
    let mut out = Vec::new();
    if nf.vertices > 0 {
        let (target, at) = node(nf.first_vertex_node);
        out.push(Entry {
            origin: Origin::Evaluation,
            code: "E10_NON_FINITE_VERTICES".into(),
            message: format!(
                "{} vertex position(s) evaluated to NaN or infinity{at}; their triangles are not drawn",
                nf.vertices
            ),
            target,
        });
    }
    if nf.values > 0 {
        let (target, at) = node(nf.first_value_node);
        out.push(Entry {
            origin: Origin::Evaluation,
            code: "E10_NON_FINITE_VALUES".into(),
            message: format!(
                "{} opacity, color or draw order value(s) evaluated to NaN or infinity{at}; drawn as invisible",
                nf.values
            ),
            target,
        });
    }
    out
}
