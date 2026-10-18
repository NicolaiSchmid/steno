//! Writes a file so a reader never sees a half-written one.

use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};

use steno_core::busy_file;
use thiserror::Error;

/// A write that did not complete. `path` is the caller's target, never the
/// temp file; `underlying` names the step and the OS error.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{underlying}")]
pub struct WriteFailure {
    pub path: String,
    pub underlying: String,
}

/// Writes a file so a reader never sees a half-written one: the bytes go to
/// `.steno-tmp-<8 hex>-<name>` beside the target, are `fsync`ed and renamed
/// over it; then the directory (Unix) or the renamed file (Windows) is
/// synced. Windows retries while another handle briefly holds the file. A
/// failure before the rename leaves the target as it was.
pub struct AtomicFileWriter;

impl AtomicFileWriter {
    /// Temp files of the writer; the only files a destination ever removes.
    pub const TEMPORARY_PREFIX: &'static str = ".steno-tmp-";

    pub fn write(data: &[u8], target: &Path) -> Result<(), WriteFailure> {
        let temporary = Self::temporary_path(target);
        let outcome = Self::write_bytes(data, &temporary, target).and_then(|()| {
            busy_file::rename(&temporary, target)
                .map_err(|error| Self::failure(target, "rename", &error))
        });
        if outcome.is_err() {
            let _ = fs::remove_file(&temporary);
            return outcome;
        }
        Self::make_rename_durable(target)
    }

    /// `fsync` the directory after the rename. Best effort: the data is
    /// already durable, and some network file systems cannot sync it.
    #[cfg(unix)]
    #[allow(clippy::unnecessary_wraps)]
    fn make_rename_durable(target: &Path) -> Result<(), WriteFailure> {
        if let Some(parent) = target.parent()
            && let Ok(directory) = fs::File::open(if parent.as_os_str().is_empty() {
                Path::new(".")
            } else {
                parent
            })
        {
            let _ = directory.sync_all();
        }
        Ok(())
    }

    /// Flushes the renamed file, since Windows' folder flush does not cover
    /// a rename on every drive. A failure fails the write: the vault's
    /// mixdown may be the only audio left after the sweep.
    #[cfg(not(unix))]
    fn make_rename_durable(target: &Path) -> Result<(), WriteFailure> {
        busy_file::retried(|| OpenOptions::new().write(true).open(target))
            .map_err(|error| Self::failure(target, "open", &error))?
            .sync_all()
            .map_err(|error| Self::failure(target, "fsync", &error))
    }

    /// Removes every `.steno-tmp-*` an earlier crash left in `directory`.
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

    /// The longest file name, in bytes, the target file systems accept.
    const MAX_NAME_BYTES: usize = 255;

    /// `<dir>/.steno-tmp-<8 hex>-<name>` beside `target`, the name cut on a
    /// character boundary to fit `MAX_NAME_BYTES`.
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

    #[must_use]
    pub fn random_hex() -> String {
        uuid::Uuid::new_v4().simple().to_string()[..8].to_owned()
    }

    /// Open, write, `fsync`; failures name the caller's target.
    fn write_bytes(data: &[u8], temporary: &Path, target: &Path) -> Result<(), WriteFailure> {
        let failure = |step: &str, error: std::io::Error| Self::failure(target, step, &error);
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

    fn failure(target: &Path, step: &str, error: &std::io::Error) -> WriteFailure {
        WriteFailure {
            path: target.to_string_lossy().into_owned(),
            underlying: format!("{step}: {error}"),
        }
    }
}
