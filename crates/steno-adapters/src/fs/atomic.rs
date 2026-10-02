//! Writes a file so a reader never sees a half-written one.
//! Swift: `Sources/StenoAdapters/FileSystem/AtomicFileWriter.swift`.

use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};

use thiserror::Error;

/// A write that did not complete. `path` is the target the caller asked
/// for, never the temporary file; `underlying` names the step (`open`,
/// `write`, `fsync`, `rename`) and the OS error.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{underlying}")]
pub struct WriteFailure {
    pub path: String,
    pub underlying: String,
}

/// Writes a file so a reader never sees a half-written one: the bytes go to
/// `.steno-tmp-<8 hex>-<name>` in the target directory, are `fsync`ed, and
/// a rename replaces the target in one step; on Unix the directory is
/// `fsync`ed after the rename so the new name survives a crash too. A
/// failure removes the temp file and leaves the target as it was.
pub struct AtomicFileWriter;

impl AtomicFileWriter {
    /// Temp files of the writer; the only files a destination ever removes.
    pub const TEMPORARY_PREFIX: &'static str = ".steno-tmp-";

    pub fn write(data: &[u8], target: &Path) -> Result<(), WriteFailure> {
        let temporary = Self::temporary_path(target);
        let outcome = Self::write_bytes(data, &temporary, target).and_then(|()| {
            fs::rename(&temporary, target).map_err(|error| WriteFailure {
                path: target.to_string_lossy().into_owned(),
                underlying: format!("rename: {error}"),
            })
        });
        if outcome.is_err() {
            let _ = fs::remove_file(&temporary);
            return outcome;
        }
        Self::sync_directory(target);
        outcome
    }

    /// `fsync` the directory after the rename so the directory entry is on
    /// disk, not only the bytes. Best effort: the data is already durable
    /// and the file is in place, so a directory that cannot be synced
    /// (some network file systems) is not a failed write. Windows has no
    /// directory handle to sync; the rename is left as it is there.
    #[cfg(unix)]
    fn sync_directory(target: &Path) {
        if let Some(parent) = target.parent()
            && let Ok(directory) = fs::File::open(if parent.as_os_str().is_empty() {
                Path::new(".")
            } else {
                parent
            })
        {
            let _ = directory.sync_all();
        }
    }

    #[cfg(not(unix))]
    fn sync_directory(_target: &Path) {}

    /// Removes every `.steno-tmp-*` left in `directory` by an earlier crash.
    /// Nothing else is ever removed.
    pub fn remove_stale_temporaries(directory: &Path) {
        let Ok(entries) = fs::read_dir(directory) else {
            return;
        };
        for entry in entries.flatten() {
            if entry
                .file_name()
                .to_string_lossy()
                .starts_with(Self::TEMPORARY_PREFIX)
            {
                let _ = fs::remove_file(entry.path());
            }
        }
    }

    /// The longest file name the file systems we write to accept, in bytes.
    const MAX_NAME_BYTES: usize = 255;

    /// `<dir>/.steno-tmp-<8 hex>-<name>` beside `target`, the name cut on a
    /// character boundary so the whole temp name fits in
    /// 255 bytes (`MAX_NAME_BYTES`); a target name near the limit
    /// otherwise failed with "file name too long" before the first byte.
    #[must_use]
    pub fn temporary_path(target: &Path) -> PathBuf {
        let name = target
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let prefix = format!("{}{}-", Self::TEMPORARY_PREFIX, Self::random_hex());
        let budget = Self::MAX_NAME_BYTES.saturating_sub(prefix.len());
        let mut cut = name.len().min(budget);
        while !name.is_char_boundary(cut) {
            cut -= 1;
        }
        let parent = target.parent().unwrap_or_else(|| Path::new(""));
        parent.join(format!("{prefix}{}", &name[..cut]))
    }

    /// Eight lowercase hex digits.
    #[must_use]
    pub fn random_hex() -> String {
        uuid::Uuid::new_v4().simple().to_string()[..8].to_owned()
    }

    /// Open, write every byte, `fsync`; failures name the target the caller
    /// asked for, not the temp file.
    fn write_bytes(data: &[u8], temporary: &Path, target: &Path) -> Result<(), WriteFailure> {
        let failure = |step: &str, error: std::io::Error| WriteFailure {
            path: target.to_string_lossy().into_owned(),
            underlying: format!("{step}: {error}"),
        };
        let mut options = OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o644);
        }
        let mut file = options
            .open(temporary)
            .map_err(|error| failure("open", error))?;
        file.write_all(data)
            .map_err(|error| failure("write", error))?;
        file.sync_all().map_err(|error| failure("fsync", error))
    }
}
