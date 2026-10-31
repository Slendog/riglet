//! Atomic saving: replacing files, failures, leftover temporary files.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use riglet_editor::save::{STALE_TEMP_AGE, write_atomic};

fn temp_dir(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("editor-save").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn names(dir: &std::path::Path) -> Vec<String> {
    let mut n: Vec<_> =
        std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    n.sort();
    n
}

/// Runs on every CI platform, Windows included: an existing target is
/// replaced in place.
#[test]
fn replaces_an_existing_file() {
    let dir = temp_dir("replace");
    let target = dir.join("m.riglet");
    write_atomic(&target, b"first").unwrap();
    write_atomic(&target, b"second, longer").unwrap();
    write_atomic(&target, b"3").unwrap();
    assert_eq!(std::fs::read(&target).unwrap(), b"3");
    assert_eq!(names(&dir), ["m.riglet"]);
}

/// Leftover temporary files of the target older than STALE_TEMP_AGE are
/// removed by the next save; recent ones (maybe another editor's save in
/// progress), other files' temporaries and look-alikes are kept.
#[test]
fn stale_temporary_files_are_removed() {
    let dir = temp_dir("stale");
    let target = dir.join("m.riglet");
    let old = SystemTime::now() - STALE_TEMP_AGE - Duration::from_secs(60);
    let make = |name: &str, modified: SystemTime| {
        let f = std::fs::File::create(dir.join(name)).unwrap();
        f.set_modified(modified).unwrap();
    };
    make(".m.riglet.123-4.tmp", old);
    make(".m.riglet.77-0.tmp", old);
    make(".m.riglet.55-1.tmp", SystemTime::now());
    make(".other.riglet.123-4.tmp", old);
    make(".m.riglet.x-4.tmp", old);
    make("m.riglet.123-4.tmp", old);
    make(".m.riglet.123-4.tmp.keep", old);
    write_atomic(&target, b"data").unwrap();
    assert_eq!(
        names(&dir),
        [
            ".m.riglet.123-4.tmp.keep",
            ".m.riglet.55-1.tmp",
            ".m.riglet.x-4.tmp",
            ".other.riglet.123-4.tmp",
            "m.riglet",
            "m.riglet.123-4.tmp"
        ]
    );
}

/// Windows refuses to replace a read-only file: the save fails, the target
/// keeps its content, and no temporary file is left.
#[cfg(windows)]
#[test]
fn a_refused_replace_on_windows_keeps_the_target() {
    let dir = temp_dir("readonly");
    let target = dir.join("m.riglet");
    std::fs::write(&target, b"original").unwrap();
    let mut perms = std::fs::metadata(&target).unwrap().permissions();
    perms.set_readonly(true);
    std::fs::set_permissions(&target, perms.clone()).unwrap();
    assert!(write_atomic(&target, b"new").is_err());
    assert_eq!(std::fs::read(&target).unwrap(), b"original");
    assert_eq!(names(&dir), ["m.riglet"]);
    #[allow(clippy::permissions_set_readonly_false)]
    perms.set_readonly(false);
    std::fs::set_permissions(&target, perms).unwrap();
    // Writable again: the replace works.
    write_atomic(&target, b"new").unwrap();
    assert_eq!(std::fs::read(&target).unwrap(), b"new");
}
