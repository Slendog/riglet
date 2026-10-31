//! Background loading: panics become errors, and a newer request always
//! wins over a slower older one.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::channel;
use std::time::{Duration, Instant};

use riglet_editor::{LoadError, Loaded, Loader};
use riglet_format::{Limits, Rig};

/// A tiny valid model, named after the path it was "loaded" from.
fn fake(path: &Path) -> Loaded {
    let mut doc = riglet_fixtures::Builder::new().document();
    doc.manifest.generator = path.display().to_string();
    let rig = Rig::prepare(&doc, &Limits::default()).unwrap();
    Loaded {
        document: doc,
        rig,
        source: riglet_editor::Source { path: path.to_owned(), kind: riglet_editor::SourceKind::Riglet },
        import_warnings: Vec::new(),
    }
}

fn wait(loader: &mut Loader) -> (PathBuf, Result<Loaded, LoadError>) {
    let start = Instant::now();
    loop {
        if let Some(r) = loader.poll() {
            return r;
        }
        assert!(start.elapsed() < Duration::from_secs(20), "load never finished");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn a_panicking_loader_is_an_error() {
    let mut loader = Loader::with_load_fn(Limits::default(), Arc::new(|_: &Path, _: &Limits| panic!("hostile input")));
    loader.start("evil.riglet".into(), || {});
    let (path, result) = wait(&mut loader);
    assert_eq!(path, Path::new("evil.riglet"));
    match result {
        Err(LoadError::Internal { message, .. }) => assert!(message.contains("hostile input"), "{message}"),
        other => panic!("expected an internal error, got {:?}", other.map(|_| ())),
    }
    assert!(loader.loading().is_none());
    // The loader keeps working afterwards.
    let mut loader = Loader::with_load_fn(Limits::default(), Arc::new(|p: &Path, _: &Limits| Ok(fake(p))));
    loader.start("fine.riglet".into(), || {});
    assert!(wait(&mut loader).1.is_ok());
}

/// A slow load superseded by a fast one never delivers its result.
#[test]
fn a_newer_load_wins_over_a_slower_older_one() {
    let (release_tx, release_rx) = channel::<()>();
    let release_rx = std::sync::Mutex::new(release_rx);
    let load = Arc::new(move |p: &Path, _: &Limits| {
        if p == Path::new("slow.riglet") {
            // Blocks until the test lets it finish (after the fast one).
            let _ = release_rx.lock().unwrap().recv_timeout(Duration::from_secs(10));
        }
        Ok(fake(p))
    });
    let mut loader = Loader::with_load_fn(Limits::default(), load);
    let (done_tx, done_rx) = channel::<&'static str>();
    let tx = done_tx.clone();
    loader.start("slow.riglet".into(), move || {
        let _ = tx.send("slow");
    });
    loader.start("fast.riglet".into(), move || {
        let _ = done_tx.send("fast");
    });
    assert_eq!(loader.loading(), Some(Path::new("fast.riglet")));
    let (path, result) = wait(&mut loader);
    assert_eq!(path, Path::new("fast.riglet"));
    assert_eq!(result.unwrap().document.manifest.generator, "fast.riglet");

    // Now let the slow one finish: its result is dropped.
    release_tx.send(()).unwrap();
    let mut finished = Vec::new();
    while finished.len() < 2 {
        finished.push(done_rx.recv_timeout(Duration::from_secs(10)).unwrap());
    }
    assert!(finished.contains(&"slow"));
    std::thread::sleep(Duration::from_millis(20));
    assert!(loader.poll().is_none(), "the stale result must not arrive");
    assert!(loader.loading().is_none());
}

/// The real loader reads real files through the same path.
#[test]
fn the_default_loader_reads_files() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("loader");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("m.riglet");
    std::fs::write(&path, riglet_format::write_package_bytes(&riglet_fixtures::Builder::new().document()).unwrap())
        .unwrap();
    let mut loader = Loader::new(Limits::default());
    loader.start(path.clone(), || {});
    assert!(wait(&mut loader).1.is_ok());
    loader.start(dir.join("missing.riglet"), || {});
    assert!(matches!(wait(&mut loader).1, Err(LoadError::Io { .. })));
}
