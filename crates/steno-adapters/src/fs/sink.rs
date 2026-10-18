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

#[cfg(test)]
mod tests {
    use super::*;

    /// A vault `Vault` with a sibling `Other` in a temporary directory.
    fn vault() -> (tempfile::TempDir, PathBuf, LocalFolderSink) {
        let directory = tempfile::Builder::new()
            .prefix("steno-adapters-sink-")
            .tempdir()
            .unwrap();
        let root = directory.path().join("Vault");
        fs::create_dir(&root).unwrap();
        fs::create_dir(directory.path().join("Other")).unwrap();
        let sink = LocalFolderSink::new(root.clone());
        (directory, root, sink)
    }

    #[test]
    fn the_root_is_the_root_and_a_sibling_or_a_gone_path_is_not() {
        let (directory, root, sink) = vault();
        assert!(sink.is_root(&root.to_string_lossy()));
        assert!(!sink.is_root(&directory.path().join("Other").to_string_lossy()));
        assert!(
            !sink.is_root(&directory.path().join("Gone").to_string_lossy()),
            "a gone path is not the root"
        );
    }

    /// Returns early on a case-sensitive disk.
    #[test]
    fn the_root_in_another_case_on_a_case_insensitive_disk_is_the_root() {
        let (directory, _root, sink) = vault();
        let shouted = directory.path().join("VAULT");
        if !shouted.exists() {
            return;
        }
        assert!(sink.is_root(&shouted.to_string_lossy()));
    }

    /// procfs and sysfs both have inode 1 at their roots, so the device is
    /// compared too.
    #[cfg(target_os = "linux")]
    #[test]
    fn roots_of_two_file_systems_with_one_inode_are_not_one_root() {
        use std::os::unix::fs::MetadataExt as _;
        let (proc, sys) = (
            fs::metadata("/proc").unwrap(),
            fs::metadata("/sys").unwrap(),
        );
        assert_eq!(proc.ino(), sys.ino(), "the premise: one inode");
        assert_ne!(proc.dev(), sys.dev(), "the premise: two devices");
        let sink = LocalFolderSink::new(PathBuf::from("/proc"));
        assert!(sink.is_root("/proc"));
        assert!(!sink.is_root("/sys"), "another device, same inode");
    }
}
