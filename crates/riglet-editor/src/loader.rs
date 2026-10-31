//! Background loading. Files are untrusted, so loading runs on its own
//! thread: the window stays responsive, a panic in the loader (a bug, but
//! possible with hostile input) becomes a [`LoadError::Internal`] instead of
//! taking down the editor, and only the newest request can deliver a
//! result, so a slow load never replaces a document opened after it.

use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, TryRecvError};

use riglet_format::Limits;

use crate::state::{LoadError, Loaded, load};

/// The function that reads a file (tests substitute slow or failing ones).
pub type LoadFn = Arc<dyn Fn(&Path, &Limits) -> Result<Loaded, LoadError> + Send + Sync>;

struct Job {
    path: PathBuf,
    rx: Receiver<Result<Loaded, LoadError>>,
}

pub struct Loader {
    load: LoadFn,
    limits: Limits,
    job: Option<Job>,
}

impl Loader {
    pub fn new(limits: Limits) -> Self {
        Self::with_load_fn(limits, Arc::new(load))
    }

    pub fn with_load_fn(limits: Limits, load: LoadFn) -> Self {
        Loader { load, limits, job: None }
    }

    pub fn limits(&self) -> &Limits {
        &self.limits
    }

    /// The file being loaded, if any.
    pub fn loading(&self) -> Option<&Path> {
        self.job.as_ref().map(|j| j.path.as_path())
    }

    /// Starts loading `path`, superseding any load in progress: the older
    /// load's thread runs to completion but its result is dropped.
    /// `done` runs on the loader thread when the result is ready (the UI
    /// passes a repaint request).
    pub fn start(&mut self, path: PathBuf, done: impl FnOnce() + Send + 'static) {
        let (tx, rx) = std::sync::mpsc::channel();
        let (load, limits, p) = (Arc::clone(&self.load), self.limits.clone(), path.clone());
        let spawned = std::thread::Builder::new().name("riglet-load".into()).spawn(move || {
            let result = std::panic::catch_unwind(AssertUnwindSafe(|| load(&p, &limits))).unwrap_or_else(|payload| {
                let message = payload
                    .downcast_ref::<&str>()
                    .map(|s| (*s).to_owned())
                    .or_else(|| payload.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "unknown panic".into());
                Err(LoadError::Internal { path: p.clone(), message })
            });
            // The receiver is gone when a newer load superseded this one.
            let _ = tx.send(result);
            done();
        });
        if let Err(e) = spawned {
            let (tx, rx) = std::sync::mpsc::channel();
            let _ = tx
                .send(Err(LoadError::Internal { path: path.clone(), message: format!("cannot start a thread: {e}") }));
            self.job = Some(Job { path, rx });
            return;
        }
        self.job = Some(Job { path, rx });
    }

    /// The result of the current load, once it is ready.
    pub fn poll(&mut self) -> Option<(PathBuf, Result<Loaded, LoadError>)> {
        let job = self.job.as_ref()?;
        let result = match job.rx.try_recv() {
            Err(TryRecvError::Empty) => return None,
            Ok(result) => result,
            Err(TryRecvError::Disconnected) => {
                Err(LoadError::Internal { path: job.path.clone(), message: "the loader stopped unexpectedly".into() })
            }
        };
        self.job.take().map(|j| (j.path, result))
    }
}
