//! Autosave and crash recovery.
//!
//! Every running editor owns one *session* directory under the recovery
//! root (`<data dir>/recovery/<session id>/`) and holds an exclusive lock
//! on its `lock` file for as long as it runs. While the open document is
//! dirty, [`Autosave`] writes a copy of it there (`model.riglet`, through
//! the same atomic path as Save) plus a small JSON [`Record`] naming the
//! original file. A clean save, a clean close or discarding the changes
//! removes the copy. Autosave never writes anywhere else, so it never
//! touches the user's file.
//!
//! After a crash the lock is gone (the OS releases it with the process),
//! so the next editor finds the session with [`orphans`], which locks each
//! orphan for itself (another editor starting at the same time then skips
//! it). The UI offers restore or discard. A restored copy is read through
//! [`crate::load`], with the same parsers and limits as any other file,
//! and the record's paths are only used as the restored document's source
//! and save path (Save still refuses `.inp`/`.inx` and the imported file).

use std::fs::{File, OpenOptions};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime};

use riglet_format::Document;

use crate::save::write_atomic;
use crate::state::{EditorState, Source, SourceKind};

/// Time between recovery copies of a dirty document (at most one per
/// interval, and only when it changed).
pub const AUTOSAVE_INTERVAL: Duration = Duration::from_secs(60);

/// Largest record file read (a record is a few hundred bytes).
pub const MAX_RECORD_BYTES: u64 = 64 * 1024;

/// Most sessions looked at in one [`orphans`] call.
pub const MAX_ORPHANS: usize = 64;

const LOCK_FILE: &str = "lock";
const MODEL_FILE: &str = "model.riglet";
const RECORD_FILE: &str = "record.json";
const RECORD_FORMAT: &str = "riglet-recovery";
const RECORD_VERSION: u32 = 1;

/// The default recovery root: `RIGLET_DATA_DIR` if set, else the
/// platform's per-user data directory, plus `recovery`. `None` when no
/// home directory is known (autosave is then off, and the UI says so).
pub fn default_root() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("RIGLET_DATA_DIR").filter(|d| !d.is_empty()) {
        return Some(PathBuf::from(dir).join("recovery"));
    }
    platform_data_dir().map(|d| d.join("recovery"))
}

#[cfg(target_os = "macos")]
fn platform_data_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").filter(|h| !h.is_empty())?;
    Some(PathBuf::from(home).join("Library").join("Application Support").join("Riglet"))
}

#[cfg(windows)]
fn platform_data_dir() -> Option<PathBuf> {
    let local = std::env::var_os("LOCALAPPDATA").filter(|d| !d.is_empty())?;
    Some(PathBuf::from(local).join("Riglet"))
}

#[cfg(all(unix, not(target_os = "macos")))]
fn platform_data_dir() -> Option<PathBuf> {
    // XDG: an unset or relative XDG_DATA_HOME means ~/.local/share.
    if let Some(xdg) = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).filter(|p| p.is_absolute()) {
        return Some(xdg.join("riglet"));
    }
    let home = std::env::var_os("HOME").filter(|h| !h.is_empty())?;
    Some(PathBuf::from(home).join(".local").join("share").join("riglet"))
}

#[cfg(not(any(unix, windows)))]
fn platform_data_dir() -> Option<PathBuf> {
    None
}

/// What a recovery copy is a copy of. Written next to the copy as JSON.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Record {
    pub format: String,
    pub version: u32,
    /// The file the document was opened from. `None` when that path is not
    /// valid Unicode (it can't be stored in JSON unchanged).
    pub source_path: Option<String>,
    /// `riglet`, `inp` or `inx`.
    pub source_kind: String,
    /// Where Save would write (`None`: no `.riglet` yet, or a path that is
    /// not valid Unicode).
    pub save_path: Option<String>,
    /// When the copy was written, seconds since the Unix epoch.
    pub saved_at: u64,
}

impl Record {
    pub fn of(state: &EditorState, saved_at: SystemTime) -> Record {
        let source = state.source();
        Record {
            format: RECORD_FORMAT.into(),
            version: RECORD_VERSION,
            source_path: utf8_absolute(&source.path),
            source_kind: kind_name(source.kind).into(),
            save_path: state.save_path().and_then(utf8_absolute),
            saved_at: saved_at.duration_since(SystemTime::UNIX_EPOCH).map_or(0, |d| d.as_secs()),
        }
    }

    /// The original source, if the record names one.
    pub fn source(&self) -> Option<Source> {
        let path = PathBuf::from(self.source_path.as_ref()?);
        let kind = match self.source_kind.as_str() {
            "riglet" => SourceKind::Riglet,
            "inp" => SourceKind::Inp,
            "inx" => SourceKind::Inx,
            _ => return None,
        };
        Some(Source { path, kind })
    }

    pub fn save_path(&self) -> Option<PathBuf> {
        self.save_path.as_ref().map(PathBuf::from)
    }

    /// The file name to show for the original (save path first).
    pub fn display_name(&self) -> String {
        let path = self.save_path.as_deref().or(self.source_path.as_deref());
        path.and_then(|p| Path::new(p).file_name())
            .map_or_else(|| "an unnamed model".into(), |n| n.to_string_lossy().into())
    }
}

/// The path made absolute (a model opened by a relative path, from the
/// command line, must restore to the same file whatever the next start's
/// working directory), as UTF-8; `None` if it isn't valid Unicode.
fn utf8_absolute(path: &Path) -> Option<String> {
    std::path::absolute(path).ok()?.into_os_string().into_string().ok()
}

fn kind_name(kind: SourceKind) -> &'static str {
    match kind {
        SourceKind::Riglet => "riglet",
        SourceKind::Inp => "inp",
        SourceKind::Inx => "inx",
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RecoveryError {
    #[error("cannot create the recovery folder {path}: {source}")]
    Create { path: PathBuf, source: std::io::Error },
    #[error("cannot lock {path}: {source}")]
    Lock { path: PathBuf, source: std::io::Error },
    #[error("cannot encode the recovery copy: {0}")]
    Encode(riglet_format::PackageError),
    #[error("cannot write the recovery copy {path}: {source}")]
    Io { path: PathBuf, source: std::io::Error },
}

/// This editor's session directory, locked while the value lives.
#[derive(Debug)]
pub struct Session {
    dir: PathBuf,
    lock: Option<File>,
}

impl Session {
    /// Creates and locks a new session directory under `root` (created if
    /// missing; owner-only on Unix).
    pub fn start(root: &Path) -> Result<Session, RecoveryError> {
        create_private_dir_all(root).map_err(|source| RecoveryError::Create { path: root.to_owned(), source })?;
        let now = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
        let pid = std::process::id();
        let mut last_error = None;
        for n in 0..100u32 {
            let dir = root.join(format!("{pid}-{now}-{n}"));
            match create_private_dir(&dir) {
                Ok(()) => {
                    let lock = lock_new(&dir)?;
                    return Ok(Session { dir, lock: Some(lock) });
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => last_error = Some(e),
                Err(source) => return Err(RecoveryError::Create { path: dir, source }),
            }
        }
        let source = last_error.unwrap_or_else(|| std::io::Error::other("no free session name"));
        Err(RecoveryError::Create { path: root.to_owned(), source })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Writes the copy, then its record (both atomically).
    pub fn write(&self, document: &Document, record: &Record) -> Result<(), RecoveryError> {
        let bytes = riglet_format::write_package_bytes(document).map_err(RecoveryError::Encode)?;
        let model = self.dir.join(MODEL_FILE);
        write_atomic(&model, &bytes).map_err(|source| RecoveryError::Io { path: model, source })?;
        let json = serde_json::to_vec_pretty(record)
            .map_err(|e| RecoveryError::Io { path: self.dir.join(RECORD_FILE), source: std::io::Error::other(e) })?;
        let path = self.dir.join(RECORD_FILE);
        write_atomic(&path, &json).map_err(|source| RecoveryError::Io { path, source })
    }

    /// Removes the copy and its record (the session stays).
    pub fn clear(&self) -> std::io::Result<()> {
        for name in [RECORD_FILE, MODEL_FILE] {
            match std::fs::remove_file(self.dir.join(name)) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
                _ => {}
            }
        }
        Ok(())
    }

    /// A clean close: removes the session directory.
    pub fn close(mut self) -> std::io::Result<()> {
        drop(self.lock.take());
        remove_session_dir(&self.dir)
    }
}

/// A session left by an editor that is no longer running, locked by us
/// until it is restored, discarded or dropped (then the next start offers
/// it again).
#[derive(Debug)]
pub struct Orphan {
    dir: PathBuf,
    /// `None` when the record is missing or unreadable (the copy can still
    /// be restored, without its original path).
    pub record: Option<Record>,
    /// When the copy was last written (file time).
    pub modified: Option<SystemTime>,
    lock: Option<File>,
}

impl Orphan {
    /// The recovery copy, to open with [`crate::load`] (or the `Loader`).
    pub fn model_path(&self) -> PathBuf {
        self.dir.join(MODEL_FILE)
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Deletes the copy and its session.
    pub fn discard(mut self) -> std::io::Result<()> {
        drop(self.lock.take());
        remove_session_dir(&self.dir)
    }
}

/// The sessions under `root` whose editor is gone, newest first, each
/// locked for the caller. `own` (this editor's session) is skipped.
/// Sessions with no copy (a crash before the first autosave, or nothing
/// unsaved) are removed. Never fails: unreadable entries are skipped.
pub fn orphans(root: &Path, own: Option<&Session>) -> Vec<Orphan> {
    let Ok(entries) = std::fs::read_dir(root) else { return Vec::new() };
    let mut out = Vec::new();
    for entry in entries.flatten().take(MAX_ORPHANS.saturating_mul(4)) {
        if out.len() >= MAX_ORPHANS {
            break;
        }
        let dir = entry.path();
        if own.is_some_and(|s| s.dir == dir) || !is_session_name(&entry.file_name()) {
            continue;
        }
        // A real directory only: never follow a link out of the root.
        if !std::fs::symlink_metadata(&dir).is_ok_and(|m| m.is_dir()) {
            continue;
        }
        let Some(lock) = try_lock_existing(&dir) else { continue };
        let model = dir.join(MODEL_FILE);
        let Ok(meta) = std::fs::symlink_metadata(&model) else {
            // Nothing to recover.
            drop(lock);
            let _ = remove_session_dir(&dir);
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        let record = read_record(&dir.join(RECORD_FILE));
        out.push(Orphan { dir, record, modified: meta.modified().ok(), lock: Some(lock) });
    }
    out.sort_by_key(|o| std::cmp::Reverse(o.modified));
    out
}

/// `<digits>-<digits>-<digits>`, as [`Session::start`] names them.
fn is_session_name(name: &std::ffi::OsStr) -> bool {
    let Some(name) = name.to_str() else { return false };
    let parts: Vec<&str> = name.split('-').collect();
    name.len() <= 64 && parts.len() == 3 && parts.iter().all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

fn read_record(path: &Path) -> Option<Record> {
    if !std::fs::symlink_metadata(path).ok()?.is_file() {
        return None;
    }
    let mut bytes = Vec::new();
    File::open(path).ok()?.take(MAX_RECORD_BYTES).read_to_end(&mut bytes).ok()?;
    let record: Record = serde_json::from_slice(&bytes).ok()?;
    (record.format == RECORD_FORMAT && record.version == RECORD_VERSION).then_some(record)
}

fn lock_new(dir: &Path) -> Result<File, RecoveryError> {
    let path = dir.join(LOCK_FILE);
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|source| RecoveryError::Lock { path: path.clone(), source })?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(std::fs::TryLockError::WouldBlock) => {
            Err(RecoveryError::Lock { path, source: std::io::ErrorKind::WouldBlock.into() })
        }
        Err(std::fs::TryLockError::Error(source)) => Err(RecoveryError::Lock { path, source }),
    }
}

/// Locks an existing session's lock file; `None` while its editor runs (or
/// the lock can't be taken at all).
fn try_lock_existing(dir: &Path) -> Option<File> {
    let path = dir.join(LOCK_FILE);
    // A lock file that is a link or a directory is not ours to open.
    match std::fs::symlink_metadata(&path) {
        Ok(m) if !m.is_file() => return None,
        _ => {}
    }
    let file = OpenOptions::new().write(true).create(true).truncate(false).open(&path).ok()?;
    file.try_lock().ok()?;
    Some(file)
}

/// Removes a session directory: its plain files and links (never followed),
/// then the directory itself. Subdirectories are left alone (the session
/// never makes any), so a strange entry stops the removal instead of
/// deleting more than the session.
fn remove_session_dir(dir: &Path) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)?.flatten() {
        if entry.file_type().is_ok_and(|t| !t.is_dir()) {
            std::fs::remove_file(entry.path())?;
        }
    }
    std::fs::remove_dir(dir)
}

fn create_private_dir_all(path: &Path) -> std::io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(path)
}

fn create_private_dir(path: &Path) -> std::io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(path)
}

/// The last autosave's outcome, for the status bar.
#[derive(Debug, Clone, PartialEq)]
pub enum AutosaveStatus {
    /// No copy (the document is clean, or not dirty for long).
    Idle,
    /// A copy is being written.
    Writing,
    Saved(SystemTime),
    Failed(String),
}

/// Writes recovery copies on a background thread. Call [`Autosave::tick`]
/// every frame (it does nothing most of the time) and [`Autosave::close`]
/// on a clean exit.
pub struct Autosave {
    session: Arc<Session>,
    interval: Duration,
    /// (document instance, history state) of the copy on disk or being
    /// written.
    written: Option<(u64, u64)>,
    /// When the document last got a change not yet copied.
    due_from: Option<Instant>,
    job: Option<JoinHandle<Result<SystemTime, RecoveryError>>>,
    status: AutosaveStatus,
}

impl Autosave {
    pub fn new(session: Session) -> Self {
        Self::with_interval(session, AUTOSAVE_INTERVAL)
    }

    pub fn with_interval(session: Session, interval: Duration) -> Self {
        Autosave {
            session: Arc::new(session),
            interval,
            written: None,
            due_from: None,
            job: None,
            status: AutosaveStatus::Idle,
        }
    }

    pub fn session_dir(&self) -> &Path {
        self.session.dir()
    }

    pub fn status(&self) -> &AutosaveStatus {
        &self.status
    }

    /// Writes a copy when the document is dirty, has changed since the last
    /// copy, and has been waiting for one for an interval; removes the copy
    /// when the document is clean or another document was opened.
    pub fn tick(&mut self, state: Option<&EditorState>, now: Instant) {
        self.collect(false);
        let key = state.filter(|s| s.is_dirty()).map(|s| (s.instance(), s.history().state_id()));
        let Some(key) = key else {
            self.due_from = None;
            if self.written.is_some() {
                self.remove();
            }
            return;
        };
        if self.written.is_some_and(|(doc, _)| doc != key.0) {
            // Another document: the old copy is of changes the user dropped.
            self.remove();
        }
        if self.written == Some(key) {
            return;
        }
        let due = *self.due_from.get_or_insert(now);
        if now.saturating_duration_since(due) >= self.interval
            && self.job.is_none()
            && let Some(state) = state
        {
            self.spawn(state, key);
            self.due_from = None;
        }
    }

    /// Writes a copy now (for example right after restoring one, so the
    /// restored changes are protected before the old session is discarded)
    /// and waits for it.
    pub fn write_now(&mut self, state: &EditorState) -> Result<(), RecoveryError> {
        self.collect(true);
        let key = (state.instance(), state.history().state_id());
        let result = self.session.write(state.document(), &Record::of(state, SystemTime::now()));
        match &result {
            Ok(()) => {
                self.written = Some(key);
                self.due_from = None;
                self.status = AutosaveStatus::Saved(SystemTime::now());
            }
            Err(e) => self.status = AutosaveStatus::Failed(e.to_string()),
        }
        result
    }

    /// A clean exit: waits for a running write, then removes the session.
    pub fn close(mut self) -> std::io::Result<()> {
        self.collect(true);
        match Arc::try_unwrap(self.session) {
            Ok(session) => session.close(),
            Err(_) => Ok(()),
        }
    }

    fn spawn(&mut self, state: &EditorState, key: (u64, u64)) {
        let document = state.document().clone();
        let record = Record::of(state, SystemTime::now());
        let session = Arc::clone(&self.session);
        let spawned = std::thread::Builder::new().name("riglet-autosave".into()).spawn(move || {
            session.write(&document, &record)?;
            Ok(SystemTime::now())
        });
        match spawned {
            Ok(job) => {
                self.job = Some(job);
                self.written = Some(key);
                self.status = AutosaveStatus::Writing;
            }
            Err(e) => self.status = AutosaveStatus::Failed(format!("cannot start the autosave thread: {e}")),
        }
    }

    /// Takes the running write's result if it has finished (or waits for
    /// it with `wait`).
    fn collect(&mut self, wait: bool) {
        if !self.job.as_ref().is_some_and(|j| wait || j.is_finished()) {
            return;
        }
        let Some(job) = self.job.take() else { return };
        match job.join() {
            Ok(Ok(at)) => self.status = AutosaveStatus::Saved(at),
            Ok(Err(e)) => {
                self.status = AutosaveStatus::Failed(e.to_string());
                // Try again after the next interval.
                self.written = None;
            }
            Err(_) => {
                self.status = AutosaveStatus::Failed("the autosave thread stopped unexpectedly".into());
                self.written = None;
            }
        }
    }

    fn remove(&mut self) {
        self.collect(true);
        match self.session.clear() {
            Ok(()) => {
                self.written = None;
                self.status = AutosaveStatus::Idle;
            }
            Err(e) => self.status = AutosaveStatus::Failed(format!("cannot remove the recovery copy: {e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    /// A model opened by a relative path records an absolute one.
    #[test]
    fn recorded_paths_are_absolute() {
        let p = utf8_absolute(Path::new("models").join("m.inp").as_path()).unwrap();
        let p = Path::new(&p);
        assert!(p.is_absolute());
        assert!(p.ends_with(Path::new("models").join("m.inp")));
    }

    #[test]
    fn session_names() {
        for good in ["1-2-3", "13199-1793483838868793000-0"] {
            assert!(is_session_name(std::ffi::OsStr::new(good)), "{good}");
        }
        for bad in ["", "1-2", "1-2-3-4", "a-2-3", "1--3", "../1-2-3", "1-2-3.tmp"] {
            assert!(!is_session_name(std::ffi::OsStr::new(bad)), "{bad}");
        }
    }
}
