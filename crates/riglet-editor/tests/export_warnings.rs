//! Export from the editor and the warnings panel's entries.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

use std::path::{Path, PathBuf};

use riglet_editor::warnings::{Origin, Target};
use riglet_editor::{Command, EditorState, ExportError, Merge, SourceKind, load};
use riglet_fixtures::{Builder, linear};
use riglet_format::Limits;
use riglet_format::model::{AxisInterpolation, ConversionWarning, Provenance, ProvenanceSource};

fn temp_dir(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("editor-export").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A quad moved by `p` (linear) and `s` (step: exports with a
/// STEP_AS_NEAREST warning on the node).
fn model() -> Builder {
    let mut b = Builder::new();
    b.param("p", 0.0, 1.0, 0.0).param("s", 0.0, 1.0, 0.0);
    b.quad("q", "root", "tex", [-10.0, -10.0], [10.0, 10.0]);
    let kfs = vec![b.positions(&[[0.0; 2]; 4]), b.positions(&[[5.0, 0.0]; 4])];
    b.set("q", &[linear("p", &[0.0, 1.0])], kfs);
    let kfs = vec![b.positions(&[[0.0; 2]; 4]), b.positions(&[[0.0, 5.0]; 4])];
    b.set("q", &[("s", 0, &[0.0, 1.0], AxisInterpolation::Step)], kfs);
    b
}

fn open(path: &Path) -> EditorState {
    EditorState::open(load(path, &Limits::default()).unwrap(), Limits::default())
}

fn names(dir: &Path) -> Vec<String> {
    let mut n: Vec<_> =
        std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    n.sort();
    n
}

#[test]
fn export_writes_atomically_and_keeps_its_warnings() {
    let dir = temp_dir("export");
    let path = dir.join("m.riglet");
    std::fs::write(&path, riglet_format::write_package_bytes(&model().document()).unwrap()).unwrap();
    let mut s = open(&path);
    assert!(matches!(s.export_inochi2d(&dir.join("m.riglet"), None), Err(ExportError::NotInochi2d)));
    assert!(matches!(s.export_inochi2d(&dir.join("m.png"), None), Err(ExportError::NotInochi2d)));

    let out = dir.join("m.inp");
    let record = s.export_inochi2d(&out, None).unwrap().clone();
    assert!(record.warnings.iter().any(|w| w.code == "STEP_AS_NEAREST"), "{:?}", record.warnings);
    assert!(!s.is_dirty(), "exporting is not an edit");
    assert_eq!(names(&dir), ["m.inp", "m.riglet"], "no temporary files left");
    // The file is a valid Inochi2D export that imports again.
    let back = load(&out, &Limits::default()).unwrap();
    assert_eq!(back.source.kind, SourceKind::Inp);

    // The export's warnings show in the panel, labeled and linked.
    let entries = s.warnings();
    let e = entries.iter().find(|e| e.code == "STEP_AS_NEAREST").unwrap();
    assert_eq!(e.origin, Origin::Export { file: "m.inp".into() });
    assert_eq!(e.target, Target::Node("q".into()));
    // A second export replaces the record and the file.
    s.export_inochi2d(&out, None).unwrap();
    assert_eq!(s.warnings().iter().filter(|e| e.code == "STEP_AS_NEAREST").count(), 1);
}

/// Exporting over the imported file needs a confirmation naming exactly
/// that file; anything else writes nothing.
#[test]
fn exporting_over_the_imported_file_needs_its_confirmation() {
    let dir = temp_dir("source");
    let src = dir.join("m.inp");
    let original = riglet_inochi2d::export(&model().document()).unwrap().bytes;
    std::fs::write(&src, &original).unwrap();
    let mut s = open(&src);
    s.apply(Command::RenameNode { node: "q".into(), name: "Changed name".into() }, Merge::Never).unwrap();

    match s.export_inochi2d(&src, None) {
        Err(ExportError::NeedsSourceConfirmation(p)) => assert_eq!(p, src),
        other => panic!("expected a confirmation request, got {:?}", other.map(|_| ())),
    }
    // Confirming another file doesn't count; another spelling of the same
    // path still needs it.
    let other = dir.join("other.inp");
    assert!(matches!(s.export_inochi2d(&src, Some(&other)), Err(ExportError::NeedsSourceConfirmation(_))));
    let respelled = dir.join(".").join("m.inp");
    assert!(matches!(s.export_inochi2d(&respelled, None), Err(ExportError::NeedsSourceConfirmation(_))));
    assert_eq!(std::fs::read(&src).unwrap(), original, "nothing written without confirmation");

    s.export_inochi2d(&respelled, Some(&src)).unwrap();
    assert_ne!(std::fs::read(&src).unwrap(), original);
    assert_eq!(names(&dir), ["m.inp"]);
    // Exporting elsewhere needs no confirmation.
    s.export_inochi2d(&other, None).unwrap();
}

/// Import warnings come from the provenance and are linked against the
/// current document: a node or parameter that exists is linked, an id
/// that no longer exists is shown as removed, no crash.
#[test]
fn import_warnings_are_labeled_and_survive_stale_links() {
    let dir = temp_dir("import");
    let mut doc = model().document();
    let w = |code: &str, id: Option<&str>| ConversionWarning {
        code: code.into(),
        object_id: id.map(Into::into),
        message: format!("{code} message"),
    };
    doc.manifest.provenance.push(Provenance {
        source: ProvenanceSource::Inochi2dInp,
        source_version: Some("0.8.6".into()),
        imported_at: "2026-10-31T12:00:00Z".into(),
        warnings: vec![
            w("ON_NODE", Some("q")),
            w("ON_PARAM", Some("p")),
            w("GONE", Some("deleted-node")),
            w("GLOBAL", None),
        ],
    });
    let path = dir.join("m.riglet");
    std::fs::write(&path, riglet_format::write_package_bytes(&doc).unwrap()).unwrap();
    let mut s = open(&path);
    let entries = s.warnings();
    let get = |code: &str| entries.iter().find(|e| e.code == code).unwrap().clone();
    let origin = Origin::Import {
        source: ProvenanceSource::Inochi2dInp,
        source_version: Some("0.8.6".into()),
        imported_at: "2026-10-31T12:00:00Z".into(),
    };
    for code in ["ON_NODE", "ON_PARAM", "GONE", "GLOBAL"] {
        assert_eq!(get(code).origin, origin);
    }
    assert_eq!(get("ON_NODE").target, Target::Node("q".into()));
    assert_eq!(get("ON_PARAM").target, Target::Parameter("p".into()));
    assert_eq!(get("GONE").target, Target::Removed("deleted-node".into()));
    assert_eq!(get("GLOBAL").target, Target::None);
    // No export yet, finite frame: import entries only.
    assert_eq!(entries.len(), 4);
}

/// Live E10 entries follow the current frame: present while values are
/// non-finite, gone when they are finite again, identical between calls
/// for the same frame (no flicker).
#[test]
fn evaluation_entries_follow_the_current_frame() {
    let dir = temp_dir("live");
    let mut b = model();
    b.param("boom", 0.0, 1.0, 0.0);
    // Two sets of 3e38 on vertex 1: their sum overflows to infinity.
    for _ in 0..2 {
        let kfs = vec![b.positions(&[[0.0; 2]; 4]), b.positions(&[[0.0; 2], [3.0e38, 0.0], [0.0; 2], [0.0; 2]])];
        b.set("q", &[linear("boom", &[0.0, 1.0])], kfs);
    }
    let path = dir.join("m.riglet");
    std::fs::write(&path, riglet_format::write_package_bytes(&b.document()).unwrap()).unwrap();
    let mut s = open(&path);
    let live =
        |s: &mut EditorState| s.warnings().into_iter().filter(|e| e.origin == Origin::Evaluation).collect::<Vec<_>>();
    assert!(live(&mut s).is_empty());
    assert!(s.set_param("boom", [1.0, 0.0]));
    let first = live(&mut s);
    assert_eq!(first.len(), 1, "{first:?}");
    assert_eq!(first[0].code, "E10_NON_FINITE_VERTICES");
    assert_eq!(first[0].target, Target::Node("q".into()));
    assert!(first[0].message.starts_with("1 vertex"), "{}", first[0].message);
    for _ in 0..3 {
        assert_eq!(live(&mut s), first, "same frame, same entries");
    }
    assert!(s.set_param("boom", [0.0, 0.0]));
    assert!(live(&mut s).is_empty(), "cleared once the values are finite again");
}
