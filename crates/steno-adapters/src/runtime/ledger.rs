//! The delivery policy with no I/O in it.
//! Swift: `Sources/StenoAdapters/Runtime/DeliveryLedger.swift`.

use std::collections::{BTreeMap, HashSet};
use std::path::{Component, Path, PathBuf};

use steno_core::content_hash::sha256;
use steno_core::{DeliveredFile, DeliveryReceipt, FileOwnership};
use uuid::Uuid;

/// Which receipt applies to this root, whether a path may be opened for
/// writing, what has been written, and the receipt that results. A
/// destination renders, asks the ledger and calls its sink; the rules live
/// here once, so a second destination reuses them untouched and the "files
/// the app never wrote are never opened for writing" rule cannot drift
/// between destinations.
#[derive(Debug, Clone)]
pub struct DeliveryLedger {
    /// The destination root this delivery writes to (the vault path).
    root: String,
    /// The receipt that applies to `root`: `None` on a first delivery, which
    /// is also what a receipt from another root becomes, or one whose folder
    /// or file paths would leave the root. A moved vault, a scratch run or a
    /// tampered receipt does not pin a folder or protect a file here.
    previous: Option<DeliveryReceipt>,
    files: BTreeMap<String, DeliveredFile>,
}

impl DeliveryLedger {
    #[must_use]
    pub fn new(previous: Option<&DeliveryReceipt>, root: &str) -> Self {
        let previous = previous
            .filter(|receipt| Self::same_root(&receipt.root, root))
            .filter(|receipt| Self::stays_inside_root(receipt))
            .cloned();
        // Files from the previous receipt stay listed unless rewritten, so
        // an opted-out audio copy or a disabled people folder keeps its
        // entry.
        let files: BTreeMap<String, DeliveredFile> = previous
            .iter()
            .flat_map(|receipt| receipt.files.iter())
            .map(|file| (file.relative_path.clone(), file.clone()))
            .collect();
        DeliveryLedger {
            root: root.to_owned(),
            previous,
            files,
        }
    }

    #[must_use]
    pub fn root(&self) -> &str {
        &self.root
    }

    #[must_use]
    pub fn is_first_delivery(&self) -> bool {
        self.previous.is_none()
    }

    /// The folder the previous receipt pinned, when one applies.
    #[must_use]
    pub fn pinned_folder(&self) -> Option<&str> {
        self.previous
            .as_ref()
            .map(|receipt| receipt.folder.as_str())
    }

    /// The files written so far plus those carried over, by path.
    #[must_use]
    pub fn files(&self) -> &BTreeMap<String, DeliveredFile> {
        &self.files
    }

    /// The files of the previous receipt, when one applies.
    fn previous_files(&self) -> impl Iterator<Item = &DeliveredFile> {
        self.previous.iter().flat_map(|receipt| &receipt.files)
    }

    /// Whether an owned path may be opened for writing: on first delivery
    /// always; on re-export when the receipt lists it as owned or nothing is
    /// there yet. A file the app never wrote is never opened for writing.
    #[must_use]
    pub fn may_write(&self, path: &str, exists: bool) -> bool {
        self.is_first_delivery()
            || !exists
            || self
                .previous_files()
                .any(|file| file.ownership == FileOwnership::Owned && file.relative_path == path)
    }

    pub fn record(&mut self, path: &str, ownership: FileOwnership, data: &[u8]) {
        self.files.insert(
            path.to_owned(),
            DeliveredFile {
                relative_path: path.to_owned(),
                ownership,
                sha256: sha256(data),
            },
        );
    }

    /// The managed-block pages of the previous receipt that this delivery
    /// did not render (`rendered` holds the paths it did): the pages a
    /// person who left the meeting is still listed on.
    #[must_use]
    pub fn stale_managed_pages(&self, rendered: &HashSet<String>) -> Vec<String> {
        self.previous_files()
            .filter(|file| file.ownership == FileOwnership::ManagedBlock)
            .filter(|file| !rendered.contains(&file.relative_path))
            .map(|file| file.relative_path.clone())
            .collect()
    }

    /// Whether a listed file sits directly in `folder` with a name that
    /// satisfies `name`.
    #[must_use]
    pub fn lists(&self, folder: &str, name: impl Fn(&str) -> bool) -> bool {
        let prefix = format!("{folder}/");
        self.files.keys().any(|path| {
            path.strip_prefix(&prefix)
                .is_some_and(|child| !child.contains('/') && name(child))
        })
    }

    /// The receipt of this delivery: every file by path, with the renderer
    /// version the destination rendered with.
    #[must_use]
    pub fn receipt(&self, folder: &str, renderer_version: i64) -> DeliveryReceipt {
        DeliveryReceipt {
            root: self.root.clone(),
            folder: folder.to_owned(),
            files: self.files.values().cloned().collect(),
            renderer_version,
        }
    }

    /// Two spellings of one root: trailing slashes, `.` and `..` components
    /// and repeated separators are not a different root. Lexical only, as
    /// Foundation's `standardizedFileURL` is; symlinks are not resolved.
    #[must_use]
    pub fn same_root(left: &str, right: &str) -> bool {
        Self::standardized(left) == Self::standardized(right)
    }

    /// `..` pops a plain component, vanishes against the root (`/..` is
    /// `/`) and otherwise stays, so `../vault` is not `vault`.
    fn standardized(path: &str) -> PathBuf {
        let mut result = PathBuf::new();
        for component in Path::new(path).components() {
            match component {
                Component::CurDir => {}
                Component::ParentDir => match result.components().next_back() {
                    Some(Component::Normal(_)) => {
                        result.pop();
                    }
                    Some(Component::RootDir | Component::Prefix(_)) => {}
                    _ => result.push(component),
                },
                other => result.push(other),
            }
        }
        result
    }

    /// The receipt's folder and every file path are plain relative paths
    /// (non-empty, only named components: no `..`, no `.`, no root, no
    /// drive), the rule the destination applies to its people folder. A
    /// stored path that fails it would be joined with the root blindly, so
    /// such a receipt applies to nothing.
    fn stays_inside_root(receipt: &DeliveryReceipt) -> bool {
        Self::is_plain_relative(&receipt.folder)
            && receipt
                .files
                .iter()
                .all(|file| Self::is_plain_relative(&file.relative_path))
    }

    fn is_plain_relative(path: &str) -> bool {
        let mut components = Path::new(path).components().peekable();
        components.peek().is_some()
            && components.all(|component| matches!(component, Component::Normal(_)))
    }

    /// The folder of a first delivery: `base`, with `-2`, `-3`, … appended
    /// while the candidate exists and holds another meeting's `meeting.json`
    /// (or none); a candidate holding `meeting_id` is a crashed attempt and
    /// is reused. `exists` and `meeting_of` are the destination's two
    /// lookups.
    #[must_use]
    pub fn resolve_folder(
        base: &str,
        meeting_id: Uuid,
        exists: impl Fn(&str) -> bool,
        meeting_of: impl Fn(&str) -> Option<Uuid>,
    ) -> String {
        let mut candidate = base.to_owned();
        let mut suffix = 2;
        while exists(&candidate) {
            if meeting_of(&candidate) == Some(meeting_id) {
                return candidate;
            }
            candidate = format!("{base}-{suffix}");
            suffix += 1;
        }
        candidate
    }
}

/// `root` joined with `folder`: the meeting folder to reveal in the file
/// manager. Swift: `DeliveryReceipt.folderURL`.
#[must_use]
pub fn receipt_folder_path(receipt: &DeliveryReceipt) -> PathBuf {
    Path::new(&receipt.root).join(&receipt.folder)
}
