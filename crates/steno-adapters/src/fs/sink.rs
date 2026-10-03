//! The local file system under one folder.
//! Swift: `Sources/StenoAdapters/FileSystem/LocalFolderSink.swift`.

use std::fs;
use std::path::{Path, PathBuf};

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

    pub fn create_directory(&self, relative: &str) -> std::io::Result<()> {
        fs::create_dir_all(self.path(relative))
    }
}
