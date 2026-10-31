//! Atomic file replacement: write a temporary file next to the target,
//! flush it to disk, then rename it over the target. A failure at any step
//! leaves the target as it was and removes the temporary file.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Writes `bytes` to `target` atomically (see the module docs). On Unix the
/// directory is synced too, so the rename survives a crash; on Windows
/// `std::fs::rename` replaces an existing target.
pub fn write_atomic(target: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let temp = temp_path(target)?;
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
