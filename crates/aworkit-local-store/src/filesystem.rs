//! Small crash-consistent filesystem primitives used by canonical repositories.

use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

static TEMPORARY_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Atomically publishes bytes written and synced in the destination directory.
///
/// Content bodies are immutable content-addressed files, while the manifest is
/// the single replaceable pointer. A crash therefore exposes either the prior
/// complete manifest or the new complete manifest, never a mixed JSON body.
pub(crate) fn write_and_sync_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "canonical document path must have a parent directory",
        )
    })?;
    fs::create_dir_all(parent)?;

    let temporary_path = temporary_path(parent, path.file_name().unwrap_or_default());
    let write_result = write_temporary_file(&temporary_path, bytes)
        .and_then(|()| fs::rename(&temporary_path, path))
        .and_then(|()| sync_directory(parent));
    if write_result.is_err() {
        let _ = fs::remove_file(&temporary_path);
    }
    write_result
}

fn temporary_path(parent: &Path, file_name: &std::ffi::OsStr) -> PathBuf {
    let sequence = TEMPORARY_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    parent.join(format!(
        ".{}.{}.{}.tmp",
        file_name.to_string_lossy(),
        std::process::id(),
        sequence
    ))
}

fn write_temporary_file(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut temporary = OpenOptions::new().create_new(true).write(true).open(path)?;
    temporary.write_all(bytes)?;
    temporary.sync_all()
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> io::Result<()> {
    File::open(path)?.sync_all()
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> io::Result<()> {
    // Windows guarantees file-content durability through the synced temporary
    // file and atomic replacement. Directories cannot be opened for fsync.
    Ok(())
}
