//! Autosave and crash recovery (design `docs/design/m4-editor.md`, section
//! 6): copies only while dirty, never next to the user's file, live
//! sessions never offered, restore keeps the original paths and the
//! import protection, hostile recovery folders are harmless.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use riglet_editor::recovery::{MAX_RECORD_BYTES, orphans};
use riglet_editor::{
    Autosave, AutosaveStatus, Command, EditorState, ExportError, Merge, SaveError, Session, SourceKind, load,
};
use riglet_fixtures::{Builder, linear};
use riglet_format::Limits;

fn temp_dir(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("editor-recovery").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn model() -> Builder {
    let mut b = Builder::new();
    b.param("p", -1.0, 1.0, 0.0);
    b.quad("q", "root", "tex", [-10.0, -10.0], [10.0, 10.0]);
    let kfs = vec![b.positions(&[[0.0; 2]; 4]), b.positions(&[[5.0, 0.0]; 4])];
    b.set("q", &[linear("p", &[0.0, 1.0])], kfs);
    b
}

fn write_model(dir: &Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, riglet_format::write_package_bytes(&model().document()).unwrap()).unwrap();
    path
}

fn open(path: &Path) -> EditorState {
    EditorState::open(load(path, &Limits::default()).unwrap(), Limits::default())
}

fn rename(s: &mut EditorState, name: &str) {
    s.apply(Command::RenameNode { node: "q".into(), name: name.into() }, Merge::Never).unwrap();
}

fn node_name(s: &EditorState) -> String {
    s.document().model.nodes["q"].base().name.clone()
}

fn names(dir: &Path) -> Vec<String> {
    let mut n: Vec<_> =
        std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    n.sort();
    n
}

/// Ticks until the background write has finished.
fn settle(a: &mut Autosave, s: Option<&EditorState>, now: Instant) {
    for _ in 0..1000 {
        a.tick(s, now);
        if !matches!(a.status(), AutosaveStatus::Writing) {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("the autosave did not finish");
}

#[test]
fn copies_are_written_only_while_dirty_and_removed_when_clean() {
    let dir = temp_dir("dirty");
    let root = dir.join("recovery");
    let file = write_model(&dir, "m.riglet");
    let original = std::fs::read(&file).unwrap();
    let mut s = open(&file);
    let mut a = Autosave::with_interval(Session::start(&root).unwrap(), Duration::from_secs(60));
    let session = a.session_dir().to_owned();
    let t0 = Instant::now();

    a.tick(Some(&s), t0);
    assert_eq!(names(&session), ["lock"], "a clean document has no copy");
    rename(&mut s, "first");
    a.tick(Some(&s), t0);
    a.tick(Some(&s), t0 + Duration::from_secs(59));
    assert_eq!(names(&session), ["lock"], "no copy before the interval");
    settle(&mut a, Some(&s), t0 + Duration::from_secs(60));
    assert!(matches!(a.status(), AutosaveStatus::Saved(_)), "{:?}", a.status());
    assert_eq!(names(&session), ["lock", "model.riglet", "record.json"]);
    let copy = load(&session.join("model.riglet"), &Limits::default()).unwrap();
    assert_eq!(copy.document, *s.document());
    assert_eq!(std::fs::read(&file).unwrap(), original, "autosave never touches the user's file");
    assert_eq!(names(&dir), ["m.riglet", "recovery"]);

    // Unchanged: no new write. Changed: the next copy waits an interval.
    a.tick(Some(&s), t0 + Duration::from_secs(600));
    assert!(matches!(a.status(), AutosaveStatus::Saved(_)));
    rename(&mut s, "second");
    let t1 = t0 + Duration::from_secs(700);
    a.tick(Some(&s), t1);
    assert_eq!(
        load(&session.join("model.riglet"), &Limits::default()).unwrap().document.model.nodes,
        copy.document.model.nodes
    );
    settle(&mut a, Some(&s), t1 + Duration::from_secs(60));
    assert_eq!(load(&session.join("model.riglet"), &Limits::default()).unwrap().document, *s.document());

    // Undoing back to the saved state, or saving, removes the copy.
    s.undo().unwrap();
    s.undo().unwrap();
    assert!(!s.is_dirty());
    a.tick(Some(&s), t1 + Duration::from_secs(61));
    assert_eq!(names(&session), ["lock"]);
    rename(&mut s, "third");
    settle(&mut a, Some(&s), t1 + Duration::from_secs(200));
    settle(&mut a, Some(&s), t1 + Duration::from_secs(300));
    assert_eq!(names(&session), ["lock", "model.riglet", "record.json"]);
    s.save().unwrap();
    a.tick(Some(&s), t1 + Duration::from_secs(301));
    assert_eq!(names(&session), ["lock"]);
    assert_eq!(*a.status(), AutosaveStatus::Idle);

    // Closing the document (or opening another one) removes it too.
    rename(&mut s, "fourth");
    a.write_now(&s).unwrap();
    assert_eq!(names(&session), ["lock", "model.riglet", "record.json"]);
    let other = open(&write_model(&dir, "other.riglet"));
    a.tick(Some(&other), t1 + Duration::from_secs(302));
    assert_eq!(names(&session), ["lock"], "a copy of dropped changes is removed");
    a.write_now(&s).unwrap();
    a.tick(None, t1 + Duration::from_secs(303));
    assert_eq!(names(&session), ["lock"]);

    // A clean exit removes the session.
    a.close().unwrap();
    assert!(!session.exists());
    assert!(names(&root).is_empty());
}

/// An import has no .riglet yet: it is dirty from the start, so it gets a
/// copy too, and restoring keeps its source, so Save still never writes
/// over the imported file and Export over it still asks.
#[test]
fn restoring_an_import_keeps_its_protection() {
    let dir = temp_dir("import");
    let root = dir.join("recovery");
    let src = dir.join("m.inp");
    std::fs::write(&src, riglet_inochi2d::export(&model().document()).unwrap().bytes).unwrap();
    let original = std::fs::read(&src).unwrap();
    let mut s = open(&src);
    rename(&mut s, "edited");
    let mut a = Autosave::new(Session::start(&root).unwrap());
    a.write_now(&s).unwrap();
    // A crash: the editor never closes its session.
    drop(a);

    let own = Session::start(&root).unwrap();
    let found = orphans(&root, Some(&own));
    assert_eq!(found.len(), 1);
    let record = found[0].record.clone().unwrap();
    assert!(Path::new(record.source_path.as_deref().unwrap()).is_absolute());
    assert_eq!(record.display_name(), "m.inp");
    let loaded = load(&found[0].model_path(), &Limits::default()).unwrap();
    let mut r = EditorState::open_recovered(loaded, record.source(), record.save_path(), Limits::default());
    assert!(r.is_dirty());
    assert_eq!(r.source().path, src);
    assert_eq!(r.source().kind, SourceKind::Inp);
    assert_eq!(r.save_path(), None);
    assert_eq!(node_name(&r), "edited");
    assert!(matches!(r.save(), Err(SaveError::NoPath)));
    assert!(matches!(r.export_inochi2d(&src, None), Err(ExportError::NeedsSourceConfirmation(_))));
    assert_eq!(std::fs::read(&src).unwrap(), original);
    r.save_as(&dir.join("m.riglet")).unwrap();
    assert!(!r.is_dirty());
}

#[test]
fn live_sessions_are_never_offered_and_crashed_ones_restore() {
    let dir = temp_dir("live");
    let root = dir.join("recovery");
    let file = write_model(&dir, "m.riglet");
    let mut s = open(&file);
    rename(&mut s, "unsaved work");
    let mut a = Autosave::new(Session::start(&root).unwrap());
    a.write_now(&s).unwrap();
    let session = a.session_dir().to_owned();

    // Another editor starting now sees a running session: not offered.
    let other = Session::start(&root).unwrap();
    assert!(orphans(&root, Some(&other)).is_empty());
    assert!(orphans(&root, None).is_empty(), "own sessions are locked too");

    drop(a);
    let found = orphans(&root, Some(&other));
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].dir(), session);
    // Claimed: a third editor doesn't offer it at the same time.
    assert!(orphans(&root, None).is_empty());

    let record = found[0].record.clone().unwrap();
    let loaded = load(&found[0].model_path(), &Limits::default()).unwrap();
    let mut r = EditorState::open_recovered(loaded, record.source(), record.save_path(), Limits::default());
    assert!(r.is_dirty());
    assert_eq!(r.save_path(), Some(file.as_path()));
    assert_eq!(node_name(&r), "unsaved work");
    // Protect the restored changes in this editor's session, then drop the
    // old one.
    let mut mine = Autosave::new(other);
    mine.write_now(&r).unwrap();
    found.into_iter().next().unwrap().discard().unwrap();
    assert!(!session.exists());
    r.save().unwrap();
    assert_eq!(node_name(&open(&file)), "unsaved work");
    // Dropped without restoring or discarding: offered again next time.
    drop(mine);
    let again = orphans(&root, None);
    assert_eq!(again.len(), 1);
    drop(again);
    assert_eq!(orphans(&root, None).len(), 1);
}

/// The recovery folder is the user's, but its contents are still read as
/// untrusted: bad records, odd entries and links never crash, never
/// follow a link out, and never delete anything but session files.
#[test]
fn hostile_recovery_folders_are_harmless() {
    let dir = temp_dir("hostile");
    let root = dir.join("recovery");
    std::fs::create_dir_all(&root).unwrap();
    let mk = |name: &str, files: &[(&str, &[u8])]| {
        let d = root.join(name);
        std::fs::create_dir_all(&d).unwrap();
        for (f, bytes) in files {
            std::fs::write(d.join(f), bytes).unwrap();
        }
        d
    };
    let package = riglet_format::write_package_bytes(&model().document()).unwrap();
    mk("1-1-0", &[("model.riglet", &package), ("record.json", b"{ not json")]);
    let big = vec![b' '; MAX_RECORD_BYTES as usize + 10];
    mk("1-2-0", &[("model.riglet", &package), ("record.json", &big)]);
    mk("1-3-0", &[("model.riglet", b"PK\x03\x04garbage"), ("record.json", br#"{"format":"other","version":1}"#)]);
    let empty = mk("1-4-0", &[("lock", b"")]);
    let foreign = mk("not-a-session", &[("model.riglet", &package)]);
    let nested = mk("1-5-0", &[("model.riglet", &package)]);
    std::fs::create_dir_all(nested.join("lock")).unwrap();
    std::fs::write(root.join("1-6-0"), b"a file, not a session").unwrap();
    #[cfg(unix)]
    {
        let outside = temp_dir("hostile-outside");
        std::fs::write(outside.join("model.riglet"), &package).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("1-7-0")).unwrap();
    }

    let found = orphans(&root, None);
    let dirs: Vec<_> = found.iter().map(|o| o.dir().file_name().unwrap().to_string_lossy().into_owned()).collect();
    let mut sorted = dirs.clone();
    sorted.sort();
    assert_eq!(sorted, ["1-1-0", "1-2-0", "1-3-0"]);
    assert!(found.iter().all(|o| o.record.is_none()), "bad, oversized and foreign records are ignored");
    assert!(!empty.exists(), "a session without a copy is removed");
    assert!(foreign.exists() && nested.exists() && root.join("1-6-0").exists());
    // A broken copy is an ordinary load error.
    let broken = found.iter().find(|o| o.dir().ends_with("1-3-0")).unwrap();
    assert!(load(&broken.model_path(), &Limits::default()).is_err());
    // A good copy without a record restores with no original path.
    let good = found.iter().find(|o| o.dir().ends_with("1-1-0")).unwrap();
    let loaded = load(&good.model_path(), &Limits::default()).unwrap();
    let r = EditorState::open_recovered(loaded, None, None, Limits::default());
    assert!(r.is_dirty() && r.save_path().is_none());
    for o in found {
        o.discard().unwrap();
    }
    #[cfg(unix)]
    {
        let outside = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("editor-recovery").join("hostile-outside");
        assert!(outside.join("model.riglet").exists(), "a linked folder is never touched");
        assert!(root.join("1-7-0").is_symlink());
    }
}

/// A recorded save path that isn't a .riglet is ignored (Save then asks).
#[test]
fn a_recorded_save_path_must_be_a_riglet() {
    let dir = temp_dir("savepath");
    let file = write_model(&dir, "m.riglet");
    let loaded = load(&file, &Limits::default()).unwrap();
    let r = EditorState::open_recovered(loaded, None, Some(dir.join("m.inp")), Limits::default());
    assert_eq!(r.save_path(), None);
}
