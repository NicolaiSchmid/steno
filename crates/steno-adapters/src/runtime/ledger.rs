//! The delivery policy with no I/O in it.
//! Swift: `Sources/StenoAdapters/Runtime/DeliveryLedger.swift`.

use std::collections::{BTreeMap, HashSet};
use std::convert::Infallible;
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
    /// The folder a redelivery claimed in place of its pinned one
    /// ([`DeliveryLedger::move_folder`]), written as on a first delivery.
    claimed: Option<String>,
    files: BTreeMap<String, DeliveredFile>,
}

impl DeliveryLedger {
    /// The ledger for `root`; `previous` applies only when it is from this
    /// root and stays inside it.
    #[must_use]
    pub fn new(previous: Option<&DeliveryReceipt>, root: &str) -> Self {
        let previous = previous
            .filter(|receipt| Self::same_root(&receipt.root, root))
            .filter(|receipt| Self::stays_inside_root(receipt))
            .cloned();
        // Files from the previous receipt stay listed unless rewritten or
        // forgotten ([`DeliveryLedger::forget`]), so an opted-out audio copy
        // or a disabled people folder keeps its entry.
        let files: BTreeMap<String, DeliveredFile> = previous
            .iter()
            .flat_map(|receipt| receipt.files.iter())
            .map(|file| (file.relative_path.clone(), file.clone()))
            .collect();
        DeliveryLedger {
            root: root.to_owned(),
            previous,
            claimed: None,
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

    /// Whether an owned path may be opened for writing: always on a first
    /// delivery and for a path directly in the folder a redelivery claimed
    /// ([`DeliveryLedger::move_folder`]); otherwise only when the receipt
    /// lists it as owned or nothing is there yet. A file the app never wrote
    /// is never opened for writing.
    #[must_use]
    pub fn may_write(&self, path: &str, exists: bool) -> bool {
        self.is_first_delivery()
            || !exists
            || self
                .claimed
                .as_deref()
                .is_some_and(|folder| Self::name_in(path, folder).is_some())
            || self
                .previous_files()
                .any(|file| file.ownership == FileOwnership::Owned && file.relative_path == path)
    }

    /// Moves the receipt from `from`, the pinned folder that is no longer
    /// this meeting's, to `to`, the folder this delivery claimed in its
    /// place. Every listed file under `from` leaves the receipt, since this
    /// delivery no longer writes there, and [`DeliveryLedger::may_write`]
    /// allows any path directly in `to`, as on a first delivery: the claim
    /// gave a new folder or one holding this meeting's `meeting.json`.
    /// Swift: none; Swift writes into the pinned folder whatever it holds.
    pub fn move_folder(&mut self, from: &str, to: &str) {
        let prefix = format!("{}/", from.trim_end_matches('/'));
        self.files.retain(|path, _| !path.starts_with(&prefix));
        self.claimed = Some(to.to_owned());
    }

    /// Drops `path` from the receipt: a file of ours that is gone from
    /// disk and that this delivery did not write again.
    /// Swift: none.
    pub fn forget(&mut self, path: &str) {
        self.files.remove(path);
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
        self.files
            .keys()
            .any(|path| Self::name_in(path, folder).is_some_and(&name))
    }

    /// The file name of `path` when it sits directly in `folder`.
    fn name_in<'a>(path: &'a str, folder: &str) -> Option<&'a str> {
        path.strip_prefix(folder)?
            .strip_prefix('/')
            .filter(|name| !name.contains('/'))
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
            warnings: Vec::new(),
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
    /// ([`DeliveryLedger::is_plain_relative`]). A stored path that fails it
    /// would be joined with the root blindly, so such a receipt applies to
    /// nothing.
    fn stays_inside_root(receipt: &DeliveryReceipt) -> bool {
        Self::is_plain_relative(&receipt.folder)
            && receipt
                .files
                .iter()
                .all(|file| Self::is_plain_relative(&file.relative_path))
    }

    /// Non-empty, only named components: no `..`, no root, no drive, no
    /// leading `.` (`Path::components` drops an interior one). The one rule
    /// for a receipt's paths and for the people folder, so a folder the
    /// destination accepts never yields a receipt the ledger refuses.
    pub(crate) fn is_plain_relative(path: &str) -> bool {
        let mut components = Path::new(path).components().peekable();
        components.peek().is_some()
            && components.all(|component| matches!(component, Component::Normal(_)))
    }

    /// [`DeliveryLedger::claim_folder`] with a lookup for the claim: a
    /// candidate that does not exist counts as claimed, and nothing is
    /// created. No delivery calls it; the tests pin the collision rule
    /// through it.
    /// Swift: `DeliveryLedger.resolveFolder`.
    #[must_use]
    pub fn resolve_folder(
        base: &str,
        meeting_id: Uuid,
        exists: impl Fn(&str) -> bool,
        meeting_of: impl Fn(&str) -> Option<Uuid>,
    ) -> String {
        let Ok(folder) = Self::claim_folder(
            base,
            meeting_id,
            |candidate| Ok::<_, Infallible>(!exists(candidate)),
            meeting_of,
        );
        folder
    }

    /// The folder of a delivery without one (a first delivery, or a
    /// redelivery whose pinned folder is no longer this meeting's): `base`,
    /// with `-2`, `-3`, … appended while the candidate is taken, that is,
    /// already there and holding another meeting's `meeting.json` (or
    /// none); a candidate holding `meeting_id` is a crashed or failed
    /// attempt and is reused. `claim` creates the candidate and says whether
    /// it was new (`Ok(true)`, the folder is this delivery's) or something
    /// was already at the path (`Ok(false)`, which `meeting_of` then
    /// decides). Creating is the claim, so two first deliveries with the
    /// same slug, in one process or two, never share a folder. A failed
    /// `claim` ends the search with its error.
    /// Swift: none; `resolveFolder` looks up and the first write creates
    /// the folder.
    pub fn claim_folder<E>(
        base: &str,
        meeting_id: Uuid,
        mut claim: impl FnMut(&str) -> Result<bool, E>,
        meeting_of: impl Fn(&str) -> Option<Uuid>,
    ) -> Result<String, E> {
        let mut candidate = base.to_owned();
        let mut suffix = 2;
        while !claim(&candidate)? {
            if meeting_of(&candidate) == Some(meeting_id) {
                return Ok(candidate);
            }
            candidate = format!("{base}-{suffix}");
            suffix += 1;
        }
        Ok(candidate)
    }
}

/// `root` joined with `folder`: the meeting folder to reveal in the file
/// manager. A folder that is not a plain relative path (a tampered or
/// foreign receipt) is not joined; the root is returned instead, and
/// nothing tells the host the fallback happened.
/// Swift: `DeliveryReceipt.folderURL`.
#[must_use]
pub fn receipt_folder_path(receipt: &DeliveryReceipt) -> PathBuf {
    let root = Path::new(&receipt.root);
    if DeliveryLedger::is_plain_relative(&receipt.folder) {
        root.join(&receipt.folder)
    } else {
        root.to_path_buf()
    }
}
