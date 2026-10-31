//! Editor core without a UI: loading, commands, undo/redo, dirty tracking,
//! view state, and saving (design `docs/design/m4-editor.md`, sections 4-6).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

use std::path::{Path, PathBuf};

use riglet_editor::{
    Command, CommandError, EditorState, HistoryLimits, LoadError, MAX_NAME_BYTES, SaveError, SourceKind, load,
};
use riglet_fixtures::{Builder, linear};
use riglet_format::{Document, Limits};

/// A fresh, empty directory for one test.
fn temp_dir(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("editor-state").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A quad with one parameter that moves it, and a 2D parameter.
fn model() -> Builder {
    let mut b = Builder::new();
    b.param("p", -1.0, 1.0, 0.0).param2d("xy", [-1.0, -1.0], [1.0, 1.0]);
    b.quad("q", "root", "tex", [-10.0, -10.0], [10.0, 10.0]);
    let kfs = vec![b.positions(&[[0.0; 2]; 4]), b.positions(&[[5.0, 0.0]; 4])];
    b.set("q", &[linear("p", &[0.0, 1.0])], kfs);
    b
}

fn write(dir: &Path, name: &str, doc: &Document) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, riglet_format::write_package_bytes(doc).unwrap()).unwrap();
    path
}

fn open(path: &Path) -> EditorState {
    EditorState::open(load(path, &Limits::default()).unwrap(), Limits::default())
}

fn rename(node: &str, name: &str) -> Command {
    Command::RenameNode { node: node.into(), name: name.into() }
}

fn reload(path: &Path) -> Document {
    load(path, &Limits::default()).unwrap().document
}

#[test]
fn a_riglet_opens_clean_and_saves_what_changed() {
    let dir = temp_dir("open_save");
    let path = write(&dir, "m.riglet", &model().document());
    let mut s = open(&path);
    assert_eq!(s.source().kind, SourceKind::Riglet);
    assert_eq!(s.save_path(), Some(path.as_path()));
    assert!(!s.is_dirty());

    s.apply(rename("q", "Face"), false).unwrap();
    assert!(s.is_dirty());
    s.save().unwrap();
    assert!(!s.is_dirty());
    let saved = reload(&path);
    assert_eq!(&saved, s.document());
    assert_eq!(saved.model.nodes["q"].base().name, "Face");
    // No temporary files left next to it.
    let names: Vec<_> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name()).collect();
    assert_eq!(names, ["m.riglet"]);
}

/// Unknown entries, including another tool's editor.json, survive a save
/// byte for byte, and the editor writes no editor.json of its own.
#[test]
fn unknown_entries_and_editor_json_are_preserved() {
    let dir = temp_dir("preserve");
    let mut doc = model().document();
    doc.files.insert("editor.json".into(), b"{\"otherTool\": {\"zoom\": 3}}".to_vec());
    doc.files.insert("thirdparty/notes.bin".into(), vec![0, 1, 2, 255]);
    let path = write(&dir, "m.riglet", &doc);
    let mut s = open(&path);
    s.view.zoom = 4.0;
    s.view.hidden.insert("q".into());
    s.apply(Command::SetNodeVisible { node: "q".into(), visible: false }, false).unwrap();
    let out = dir.join("copy.riglet");
    s.save_as(&out).unwrap();
    let saved = reload(&out);
    assert_eq!(saved.files, doc.files);
    assert!(!saved.model.nodes["q"].base().visible);

    // Without edits, a save writes back an equal document.
    let mut s = open(&path);
    s.save_as(&dir.join("same.riglet")).unwrap();
    assert_eq!(reload(&dir.join("same.riglet")), reload(&path));
}

#[test]
fn undo_and_redo_restore_each_state() {
    let dir = temp_dir("undo");
    let path = write(&dir, "m.riglet", &model().document());
    let mut s = open(&path);
    let original = s.document().clone();
    s.apply(rename("q", "A"), false).unwrap();
    let after_rename = s.document().clone();
    s.apply(Command::SetNodeVisible { node: "q".into(), visible: false }, false).unwrap();
    s.apply(Command::RenameParameter { parameter: "p".into(), name: "Smile".into() }, false).unwrap();
    s.apply(Command::SetParameterGroup { parameter: "p".into(), group: Some("Mouth".into()) }, false).unwrap();
    s.apply(Command::SetParameterDefault { parameter: "xy".into(), default: vec![0.5, -0.25] }, false).unwrap();
    let last = s.document().clone();
    assert_eq!(s.document().model.parameters[1].default, [0.5, -0.25]);
    assert_eq!(s.rig().params[1].default, [0.5, -0.25], "the rig is rebuilt after a command");

    for _ in 0..4 {
        assert!(s.undo().unwrap());
    }
    assert_eq!(s.document(), &after_rename);
    assert!(s.undo().unwrap());
    assert_eq!(s.document(), &original);
    assert!(!s.is_dirty(), "undoing back to the saved state is clean");
    assert!(!s.undo().unwrap());

    for _ in 0..5 {
        assert!(s.redo().unwrap());
    }
    assert_eq!(s.document(), &last);
    assert!(!s.redo().unwrap());

    // A new command after an undo drops the redo stack.
    assert!(s.undo().unwrap());
    s.apply(rename("q", "B"), false).unwrap();
    assert!(!s.history().can_redo());
}

#[test]
fn edits_of_one_field_coalesce_until_sealed() {
    let dir = temp_dir("coalesce");
    let path = write(&dir, "m.riglet", &model().document());
    let mut s = open(&path);
    for name in ["F", "Fa", "Fac", "Face"] {
        s.apply(rename("q", name), true).unwrap();
    }
    assert_eq!(s.history().undo_len(), 1);
    // A different field starts a new step; so does the same field after a
    // seal.
    s.apply(Command::RenameParameter { parameter: "p".into(), name: "P".into() }, true).unwrap();
    s.seal();
    s.apply(Command::RenameParameter { parameter: "p".into(), name: "P2".into() }, true).unwrap();
    assert_eq!(s.history().undo_len(), 3);

    s.undo().unwrap();
    s.undo().unwrap();
    assert_eq!(s.document().model.nodes["q"].base().name, "Face");
    s.undo().unwrap();
    assert_eq!(s.document().model.nodes["q"].base().name, "q");
    assert!(!s.is_dirty());
}

/// Coalescing into the step that was saved makes the document dirty again.
#[test]
fn coalescing_after_a_save_is_dirty() {
    let dir = temp_dir("coalesce_save");
    let path = write(&dir, "m.riglet", &model().document());
    let mut s = open(&path);
    s.apply(rename("q", "F"), true).unwrap();
    s.save().unwrap();
    assert!(!s.is_dirty());
    // Saving seals: this is a new step.
    s.apply(rename("q", "Fa"), true).unwrap();
    assert!(s.is_dirty());
    assert_eq!(s.history().undo_len(), 2);
    s.undo().unwrap();
    assert!(!s.is_dirty());
}

#[test]
fn the_history_cap_drops_the_oldest_steps() {
    let dir = temp_dir("cap");
    let path = write(&dir, "m.riglet", &model().document());
    let limits = HistoryLimits { max_steps: 3, max_bytes: usize::MAX };
    let mut s = EditorState::open_with(load(&path, &Limits::default()).unwrap(), Limits::default(), limits);
    for i in 0..5 {
        s.apply(rename("q", &format!("n{i}")), false).unwrap();
    }
    assert_eq!(s.history().undo_len(), 3);
    while s.undo().unwrap() {}
    assert_eq!(s.document().model.nodes["q"].base().name, "n1");
    assert!(s.is_dirty(), "the saved state is no longer reachable");

    // The byte cap too: each step holds a long name.
    let limits = HistoryLimits { max_steps: 1000, max_bytes: 4 * MAX_NAME_BYTES };
    let mut s = EditorState::open_with(load(&path, &Limits::default()).unwrap(), Limits::default(), limits);
    for i in 0..10 {
        s.apply(rename("q", &format!("{i}").repeat(MAX_NAME_BYTES)), false).unwrap();
    }
    assert!(s.history().undo_len() < 4, "{}", s.history().undo_len());
}

#[test]
fn bad_commands_change_nothing() {
    let dir = temp_dir("bad");
    let path = write(&dir, "m.riglet", &model().document());
    let mut s = open(&path);
    let before = s.document().clone();
    let cases = [
        (rename("nope", "x"), CommandError::NoNode("nope".into())),
        (rename("q", &"x".repeat(MAX_NAME_BYTES + 1)), CommandError::NameTooLong),
        (
            Command::RenameParameter { parameter: "nope".into(), name: "x".into() },
            CommandError::NoParameter("nope".into()),
        ),
        (
            Command::SetParameterDefault { parameter: "p".into(), default: vec![2.0] },
            CommandError::BadDefault("p".into()),
        ),
        (
            Command::SetParameterDefault { parameter: "p".into(), default: vec![f32::NAN] },
            CommandError::BadDefault("p".into()),
        ),
        (
            Command::SetParameterDefault { parameter: "xy".into(), default: vec![0.0] },
            CommandError::BadDefault("xy".into()),
        ),
        (
            Command::SetParameterGroup { parameter: "p".into(), group: Some("g".repeat(MAX_NAME_BYTES + 1)) },
            CommandError::NameTooLong,
        ),
    ];
    for (command, want) in cases {
        assert_eq!(s.apply(command.clone(), false), Err(want), "{command:?}");
    }
    assert_eq!(s.document(), &before);
    assert_eq!(s.history().undo_len(), 0);
    assert!(!s.is_dirty());
}

/// Parameter values and view state are not model edits.
#[test]
fn parameters_and_view_state_never_dirty_the_document() {
    let dir = temp_dir("params");
    let path = write(&dir, "m.riglet", &model().document());
    let mut s = open(&path);
    let x0 = s.evaluate().drawables[0].positions[0][0];
    assert!(s.set_param("p", [1.0, 0.0]));
    assert_eq!(s.evaluate().drawables[0].positions[0][0], x0 + 5.0);
    assert!(s.set_param("p", [7.0, 0.0]), "clamped to the range");
    assert_eq!(s.param_values().values[0], [1.0, 0.0]);
    assert!(!s.set_param("p", [f32::NAN, 0.0]));
    assert!(!s.set_param("nope", [0.0, 0.0]));
    assert!(s.set_param("xy", [0.25, -0.5]));
    s.view.zoom = 3.0;
    s.view.pan = [10.0, -4.0];
    s.view.selection = Some("q".into());
    s.view.hidden.insert("q".into());
    s.view.background = Some([0, 0, 0]);
    assert!(!s.is_dirty());
    assert_eq!(s.history().undo_len(), 0);

    // Values survive a command (the rig is rebuilt, ids unchanged).
    s.apply(Command::RenameParameter { parameter: "p".into(), name: "Smile".into() }, false).unwrap();
    assert_eq!(s.param_values().values, [[1.0, 0.0], [0.25, -0.5]]);
    assert!(s.reset_param("p"));
    assert_eq!(s.param_values().values[0], [0.0, 0.0]);
    s.reset_params();
    assert_eq!(s.param_values().values[1], [-1.0, -1.0]);
    assert_eq!(s.evaluate().drawables[0].positions[0][0], x0);

    // The model's visibility flag is an edit and reaches evaluation.
    s.apply(Command::SetNodeVisible { node: "q".into(), visible: false }, false).unwrap();
    assert!(!s.evaluate().drawables[0].visible);
}

/// An import has no save path and its source is never a save target.
#[test]
fn imported_files_are_never_overwritten() {
    let dir = temp_dir("import");
    let exported = riglet_inochi2d::export(&model().document()).unwrap();
    let inp = dir.join("m.inp");
    std::fs::write(&inp, &exported.bytes).unwrap();
    // An Inochi2D file under a .riglet name is still recognized by content.
    let disguised = dir.join("disguised.riglet");
    std::fs::write(&disguised, &exported.bytes).unwrap();

    let mut s = open(&inp);
    assert_eq!(s.source().kind, SourceKind::Inp);
    assert!(s.is_dirty(), "an import is not saved yet");
    assert!(s.save_path().is_none());
    assert!(matches!(s.save(), Err(SaveError::NoPath)));
    assert!(matches!(s.save_as(&inp), Err(SaveError::NotRiglet)));

    let mut s = open(&disguised);
    assert_eq!(s.source().kind, SourceKind::Inp);
    assert!(matches!(s.save_as(&disguised), Err(SaveError::WouldOverwriteImport(_))));
    // The same file through another spelling of the path.
    let other = dir.join(".").join("disguised.riglet");
    assert!(matches!(s.save_as(&other), Err(SaveError::WouldOverwriteImport(_))));
    assert_eq!(std::fs::read(&disguised).unwrap(), exported.bytes);
    assert_eq!(std::fs::read(&inp).unwrap(), exported.bytes);

    let out = dir.join("m.riglet");
    s.save_as(&out).unwrap();
    assert!(!s.is_dirty());
    assert_eq!(s.save_path(), Some(out.as_path()));
    assert_eq!(&reload(&out), s.document());
}

#[test]
fn hostile_and_missing_files_are_errors() {
    let dir = temp_dir("hostile");
    let limits = Limits::default();
    let garbage = dir.join("g.riglet");
    std::fs::write(&garbage, b"not a model at all").unwrap();
    assert!(matches!(load(&garbage, &limits), Err(LoadError::UnknownFormat { .. })));

    let bytes = riglet_format::write_package_bytes(&model().document()).unwrap();
    let truncated = dir.join("t.riglet");
    std::fs::write(&truncated, &bytes[..bytes.len() / 2]).unwrap();
    assert!(matches!(load(&truncated, &limits), Err(LoadError::Package { .. })));

    let inochi = dir.join("t.inp");
    std::fs::write(&inochi, b"TRNSRTS\0\0\0\xff\xff\xff\xff").unwrap();
    assert!(matches!(load(&inochi, &limits), Err(LoadError::Import { .. })));

    assert!(matches!(load(&dir.join("missing.riglet"), &limits), Err(LoadError::Io { .. })));

    // Larger than the cap: rejected, not read whole.
    let small = Limits { zip_max_total_uncompressed: 64, ..Limits::default() };
    let full = write(&dir, "m.riglet", &model().document());
    assert!(load(&full, &small).is_err());

    // A model that fails validation.
    let mut doc = model().document();
    doc.model.parameters[0].default = vec![5.0];
    let invalid = write(&dir, "invalid.riglet", &doc);
    assert!(matches!(load(&invalid, &limits), Err(LoadError::Invalid { .. })));
}

/// A failed save leaves the target as it was and no temporary file.
#[test]
fn failed_saves_leave_the_target_alone() {
    let dir = temp_dir("failed_save");
    let path = write(&dir, "m.riglet", &model().document());
    let mut s = open(&path);
    s.apply(rename("q", "changed"), false).unwrap();

    // The target is a directory: the rename fails.
    let blocked = dir.join("blocked.riglet");
    std::fs::create_dir(&blocked).unwrap();
    std::fs::write(blocked.join("keep"), b"x").unwrap();
    assert!(matches!(s.save_as(&blocked), Err(SaveError::Io { .. })));
    assert_eq!(std::fs::read(blocked.join("keep")).unwrap(), b"x");
    // Missing directory: nothing is created.
    assert!(matches!(s.save_as(&dir.join("no/such/dir/m.riglet")), Err(SaveError::Io { .. })));
    assert!(s.is_dirty());
    assert_eq!(s.save_path(), Some(path.as_path()));

    let mut names: Vec<_> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name()).collect();
    names.sort();
    assert_eq!(names, ["blocked.riglet", "m.riglet"]);
    assert_eq!(reload(&path).model.nodes["q"].base().name, "q", "the original is unchanged");
}
