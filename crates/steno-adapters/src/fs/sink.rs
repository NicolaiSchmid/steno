//! The local file system under one folder.
//! Swift: `Sources/StenoAdapters/FileSystem/LocalFolderSink.swift`.

use std::fs;
use std::path::{Path, PathBuf};

use super::{AtomicFileWriter, WriteFailure};

/// The local file system under one folder, addressed by paths relative to
/// its root and written with [`AtomicFileWriter`]. The destination's one
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

    /// Whether anything is at `relative`, a final symlink not followed: a
    /// dangling link is there, where [`Self::exists`] says it is not.
    #[must_use]
    pub fn entry_exists(&self, relative: &str) -> bool {
        fs::symlink_metadata(self.path(relative)).is_ok()
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
    /// Swift: none; the Swift destination's first write creates the folder.
    pub fn create_new_directory(&self, relative: &str) -> std::io::Result<()> {
        fs::create_dir(self.path(relative))
    }

    /// Whether `left` and `right` are one file under two spellings, as
    /// `anna.md` and `Anna.md` are on a case-insensitive folder (APFS and
    /// NTFS by default); false when either is missing. A symlink counts as
    /// its target. On Unix by device and inode; on Windows by the final
    /// path name of each (`fs::canonicalize`, which gives the name as
    /// stored, also in a case-sensitive directory). Only when Windows
    /// cannot resolve a path for another reason are the two compared
    /// NFC-normalised and lowercased, close to NTFS's default but not
    /// exact (NTFS upcases `ı` to `I` and `ſ` to `S`; lowercasing keeps
    /// them apart).
    /// Swift: none; `removeMeetingLine` compares the paths as written
    /// (parity note in the plan).
    #[must_use]
    pub fn same_file(&self, left: &str, right: &str) -> bool {
        same_entry(&self.path(left), &self.path(right))
    }

    /// Whether `other` names this sink's root under another spelling: a
    /// symlink to it, or another case on a case-insensitive file system
    /// ([`Self::same_file`]'s rule). False when either is missing, so a
    /// receipt whose root is gone falls back to the lexical
    /// [`DeliveryLedger::same_root`](crate::runtime::DeliveryLedger::same_root).
    /// Swift: none; Swift compares the standardized paths only.
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
            // Either path missing gives false, whatever the other gave.
            (Err(error), _) | (_, Err(error)) if error.kind() == ErrorKind::NotFound => false,
            _ => folded(left) == folded(right),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A vault folder `Vault` in a fresh temporary directory, with a
    /// sibling `Other`, and its sink.
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

    /// Runs where the disk is case-insensitive (macOS and Windows CI); a
    /// case-sensitive disk has no `VAULT` and returns early.
    #[test]
    fn the_root_in_another_case_on_a_case_insensitive_disk_is_the_root() {
        let (directory, _root, sink) = vault();
        let shouted = directory.path().join("VAULT");
        if !shouted.exists() {
            return;
        }
        assert!(sink.is_root(&shouted.to_string_lossy()));
    }

    /// procfs and sysfs both have inode 1 at their roots, on two devices:
    /// the device is compared too.
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
