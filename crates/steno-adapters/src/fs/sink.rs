//! The local file system under one folder.
//! Swift: `Sources/StenoAdapters/FileSystem/LocalFolderSink.swift`.

use std::fs;
use std::path::{Path, PathBuf};

#[cfg(not(unix))]
use unicode_normalization::UnicodeNormalization as _;

use super::{AtomicFileWriter, WriteFailure};

/// The local file system under one folder, addressed by paths relative to
/// its root and written through [`AtomicFileWriter`]. The destination's one
/// seam to disk; a `WebDAV` sink would be the second implementation, at which
/// point a trait is extracted per the two-implementations rule.
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

    #[must_use]
    pub fn is_directory(&self, relative: &str) -> bool {
        self.path(relative).is_dir()
    }

    /// The names in a directory; empty when it does not exist or cannot be
    /// listed.
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

    /// `None` when there is no such file; one read, so a file that appears
    /// or vanishes between a probe and the read cannot be misreported.
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

    /// Creates the folder and every missing parent; an existing folder is
    /// no error.
    pub fn create_directory(&self, relative: &str) -> std::io::Result<()> {
        fs::create_dir_all(self.path(relative))
    }

    /// Creates the folder, whose parent must exist, and fails with
    /// `AlreadyExists` when anything is already at that path. The one step
    /// that claims a folder: of two writers creating the same path, in this
    /// process or another, exactly one succeeds.
    pub fn create_new_directory(&self, relative: &str) -> std::io::Result<()> {
        fs::create_dir(self.path(relative))
    }

    /// Whether `left` and `right` are one file under two spellings, as on a
    /// case-insensitive folder (APFS and NTFS by default) where `anna.md`
    /// and `Anna.md` are the same page. On Unix by device and inode, so a
    /// case-sensitive folder keeps two such names two files and a symlink
    /// counts as its target; false when either is missing. Windows has no
    /// stable file identity in `std`, so there the two paths are compared
    /// NFC-normalised and lowercased, which is NTFS's default and folds a
    /// rare case-sensitive directory's two pages into one: the stale line
    /// then stays, nothing is lost.
    #[must_use]
    pub fn same_file(&self, left: &str, right: &str) -> bool {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            match (
                fs::metadata(self.path(left)),
                fs::metadata(self.path(right)),
            ) {
                (Ok(left), Ok(right)) => left.dev() == right.dev() && left.ino() == right.ino(),
                _ => false,
            }
        }
        #[cfg(not(unix))]
        {
            let folded = |path: &str| path.nfc().collect::<String>().to_lowercase();
            folded(left) == folded(right)
        }
    }
}
