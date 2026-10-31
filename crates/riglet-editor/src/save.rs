//! Atomic file replacement: write a temporary file next to the target,
//! flush it to disk, then rename it over the target. A failure at any step
//! leaves the target as it was and removes the temporary file.
//!
//! The temporary file is always in the target's directory, so the rename
//! never crosses file systems. Temporary files that a crash or power loss
//! left behind are never read; each save removes those of its target that
//! are older than [`STALE_TEMP_AGE`] (a younger one may belong to another
//! editor saving the same file right now).

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Age after which a leftover temporary file is removed.
pub const STALE_TEMP_AGE: std::time::Duration = std::time::Duration::from_secs(10 * 60);

/// Writes `bytes` to `target` atomically (see the module docs). On Unix the
/// directory is synced too, so the rename survives a crash; on Windows
/// `std::fs::rename` replaces an existing target.
pub fn write_atomic(target: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let temp = temp_path(target)?;
    remove_stale_temps(target);
    let result = write_and_rename(&temp, target, bytes);
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

/// A fresh name in the target's directory: `.<name>.<pid>-<n>.tmp`.
fn temp_path(target: &Path) -> std::io::Result<PathBuf> {
    let name = target
        .file_name()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "the save path has no file name"))?;
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut temp = std::ffi::OsString::from(".");
    temp.push(name);
    temp.push(format!(".{}-{n}.tmp", std::process::id()));
    Ok(target.with_file_name(temp))
}

/// Whether `file` is a temporary file of `target_name`: exactly
/// `.<target_name>.<digits>-<digits>.tmp`.
fn is_temp_of(file: &std::ffi::OsStr, target_name: &std::ffi::OsStr) -> bool {
    let (Some(file), Some(target)) = (file.to_str(), target_name.to_str()) else { return false };
    let Some(rest) = file.strip_prefix('.').and_then(|f| f.strip_prefix(target)) else { return false };
    let Some(middle) = rest.strip_prefix('.').and_then(|r| r.strip_suffix(".tmp")) else { return false };
    let Some((pid, n)) = middle.split_once('-') else { return false };
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    digits(pid) && digits(n)
}

/// Best effort: removes `target`'s leftover temporary files older than
/// [`STALE_TEMP_AGE`]. Never touches anything else, and never fails a save.
fn remove_stale_temps(target: &Path) {
    let (Some(dir), Some(name)) = (target.parent(), target.file_name()) else { return };
    let dir = if dir.as_os_str().is_empty() { Path::new(".") } else { dir };
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        if !is_temp_of(&entry.file_name(), name) {
            continue;
        }
        // symlink_metadata: a link of that name is old data, not a file
        // to follow.
        let Ok(meta) = std::fs::symlink_metadata(entry.path()) else { continue };
        let stale = meta.modified().ok().and_then(|m| m.elapsed().ok()).is_some_and(|age| age > STALE_TEMP_AGE);
        if stale && !meta.is_dir() {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

fn write_and_rename(temp: &Path, target: &Path, bytes: &[u8]) -> std::io::Result<()> {
    // create_new: never write through an existing file or link of that name.
    let mut file = OpenOptions::new().write(true).create_new(true).open(temp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(temp, target)?;
    #[cfg(unix)]
    if let Some(dir) = target.parent() {
        let dir = if dir.as_os_str().is_empty() { Path::new(".") } else { dir };
        // Best effort: some file systems refuse to sync a directory.
        if let Ok(d) = std::fs::File::open(dir) {
            let _ = d.sync_all();
        }
    }
    Ok(())
}
