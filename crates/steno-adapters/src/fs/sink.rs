//! The local file system under one folder.

use std::fs;
use std::path::{Path, PathBuf};

use super::{AtomicFileWriter, WriteFailure};

/// The local file system under one folder, addressed by relative paths and
/// written with [`AtomicFileWriter`]: the destination's one seam to disk.
#[derive(Debug, Clone)]
pub struct LocalFolderSink {
    root: PathBuf,
}

impl LocalFolderSink {
    #[must_use]
    pub fn new(root: PathBuf) -> Self {
        LocalFolderSink { root }
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The absolute path of `relative`; the root itself for `""`.
    #[must_use]
    pub fn path(&self, relative: &str) -> PathBuf {
        if relative.is_empty() {
            self.root.clone()
        } else {
            self.root.join(relative)
        }
    }

    #[must_use]
    pub fn exists(&self, relative: &str) -> bool {
        self.path(relative).exists()
    }

    /// Whether anything is at `relative`, a dangling link included.
    #[must_use]
    pub fn entry_exists(&self, relative: &str) -> bool {
        fs::symlink_metadata(self.path(relative)).is_ok()
    }

    #[must_use]
    pub fn is_directory(&self, relative: &str) -> bool {
        self.path(relative).is_dir()
    }

    /// The names in a directory; empty when it cannot be listed.
    #[must_use]
    pub fn file_names(&self, relative: &str) -> Vec<String> {
        fs::read_dir(self.path(relative))
            .map(|entries| {
                entries
                    .flatten()
                    .map(|entry| entry.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// `None` when there is no such file; one read, so no probe races.
    pub fn read(&self, relative: &str) -> std::io::Result<Option<Vec<u8>>> {
        match fs::read(self.path(relative)) {
            Ok(data) => Ok(Some(data)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    pub fn write(&self, data: &[u8], relative: &str) -> Result<(), WriteFailure> {
        AtomicFileWriter::write(data, &self.path(relative))
    }

    /// Creates the folder and its missing parents.
    pub fn create_directory(&self, relative: &str) -> std::io::Result<()> {
        fs::create_dir_all(self.path(relative))
    }

    /// Creates the folder, whose parent must exist, failing with
    /// `AlreadyExists` when anything is there: of two writers, in any
    /// process, exactly one succeeds.
    pub fn create_new_directory(&self, relative: &str) -> std::io::Result<()> {
        fs::create_dir(self.path(relative))
    }

    /// Whether `left` and `right` are one file under two spellings, as on a
    /// case-insensitive disk; false when either is missing. Symlinks count
    /// as their target. Unix compares device and inode; Windows the stored
    /// final path names, falling back to NFC-lowercased paths.
    #[must_use]
    pub fn same_file(&self, left: &str, right: &str) -> bool {
        same_entry(&self.path(left), &self.path(right))
    }

    /// Whether `other` names this sink's root under another spelling
    /// ([`Self::same_file`]'s rule); false when either is missing.
    #[must_use]
    pub fn is_root(&self, other: &str) -> bool {
        same_entry(&self.root, Path::new(other))
    }
}

/// [`LocalFolderSink::same_file`] on two absolute paths.
fn same_entry(left: &Path, right: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        match (fs::metadata(left), fs::metadata(right)) {
            (Ok(left), Ok(right)) => left.dev() == right.dev() && left.ino() == right.ino(),
            _ => false,
        }
    }
    #[cfg(not(unix))]
    {
        use std::io::ErrorKind;
        use unicode_normalization::UnicodeNormalization as _;
        let folded = |path: &Path| {
            let text: &str = &path.to_string_lossy();
            text.nfc().collect::<String>().to_lowercase()
        };
        match (fs::canonicalize(left), fs::canonicalize(right)) {
            (Ok(left), Ok(right)) => left == right,
            (Err(error), _) | (_, Err(error)) if error.kind() == ErrorKind::NotFound => false,
            _ => folded(left) == folded(right),
        }
    }
}
