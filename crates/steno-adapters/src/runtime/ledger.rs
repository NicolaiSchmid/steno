//! The delivery policy with no I/O in it.

use std::collections::{BTreeMap, HashSet};
use std::convert::Infallible;
use std::path::{Component, Path, PathBuf};

use steno_core::content_hash::sha256;
use steno_core::{DeliveredFile, DeliveryReceipt, FileOwnership};
use uuid::Uuid;

/// Which receipt applies to this root, which paths may be opened for
/// writing, what was written, and the resulting receipt. The rules live
/// here once, so files the app never wrote are never opened for writing by
/// any destination.
#[derive(Debug, Clone)]
pub struct DeliveryLedger {
    /// The destination root (the vault path).
    root: String,
    /// The receipt that applies to `root`: `None` on a first delivery, and
    /// for a receipt from another root or with paths that leave it.
    previous: Option<DeliveryReceipt>,
    /// The folder a redelivery claimed in place of its pinned one.
    claimed: Option<String>,
    files: BTreeMap<String, DeliveredFile>,
    warnings: Vec<String>,
}

impl DeliveryLedger {
    /// The ledger for `root`; `previous` applies only when its root is
    /// this one, lexically ([`Self::same_root`]) or by `is_root`, and it
    /// stays inside.
    #[must_use]
    pub fn new(
        previous: Option<&DeliveryReceipt>,
        root: &str,
        is_root: impl Fn(&str) -> bool,
    ) -> Self {
        let previous = previous
            .filter(|receipt| Self::same_root(&receipt.root, root) || is_root(&receipt.root))
            .filter(|receipt| Self::stays_inside_root(receipt))
            .cloned();
        // Previous files stay listed unless rewritten or forgotten, so an
        // opted-out audio copy keeps its entry.
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
            warnings: Vec::new(),
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

    /// The files written so far plus those carried over.
    #[must_use]
    pub fn files(&self) -> &BTreeMap<String, DeliveredFile> {
        &self.files
    }

    fn previous_files(&self) -> impl Iterator<Item = &DeliveredFile> {
        self.previous.iter().flat_map(|receipt| &receipt.files)
    }

    /// Whether an owned path may be opened for writing: always on a first
    /// delivery and directly in a claimed folder; otherwise only when the
    /// receipt lists it as owned or nothing is there yet.
    #[must_use]
    pub fn may_write(&self, path: &str, exists: bool) -> bool {
        self.is_first_delivery()
            || !exists
            || self
                .claimed
                .as_deref()
                .is_some_and(|folder| Self::name_in(path, folder).is_some())
            || self.delivered_hash(path).is_some()
    }

    /// Moves the receipt from the pinned folder `from`, no longer this
    /// meeting's, to the claimed `to`: files under `from` leave the
    /// receipt and any path directly in `to` may be written.
    pub fn move_folder(&mut self, from: &str, to: &str) {
        let prefix = format!("{}/", from.trim_end_matches('/'));
        self.files.retain(|path, _| !path.starts_with(&prefix));
        self.claimed = Some(to.to_owned());
    }

    /// Drops `path`, a file of ours gone from disk and not rewritten.
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

    /// The previous receipt's managed-block pages not in `rendered`.
    #[must_use]
    pub fn stale_managed_pages(&self, rendered: &HashSet<String>) -> Vec<String> {
        self.previous_files()
            .filter(|file| file.ownership == FileOwnership::ManagedBlock)
            .filter(|file| !rendered.contains(&file.relative_path))
            .map(|file| file.relative_path.clone())
            .collect()
    }

    /// Whether a listed file sits directly in `folder` with a name
    /// satisfying `name`.
    #[must_use]
    pub fn lists(&self, folder: &str, name: impl Fn(&str) -> bool) -> bool {
        self.files
            .keys()
            .any(|path| Self::name_in(path, folder).is_some_and(&name))
    }

    /// The file name of `path` when it sits directly in `folder`.
    pub(crate) fn name_in<'a>(path: &'a str, folder: &str) -> Option<&'a str> {
        path.strip_prefix(folder)?
            .strip_prefix('/')
            .filter(|name| !name.contains('/'))
    }

    /// The receipt of this delivery, with the renderer version and the
    /// warnings given ([`Self::warn`]).
    #[must_use]
    pub fn receipt(&self, folder: &str, renderer_version: i64) -> DeliveryReceipt {
        DeliveryReceipt {
            root: self.root.clone(),
            folder: folder.to_owned(),
            files: self.files.values().cloned().collect(),
            renderer_version,
            warnings: self.warnings.clone(),
        }
    }

    /// The hash the previous delivery wrote to the owned file at `path`.
    #[must_use]
    pub fn delivered_hash(&self, path: &str) -> Option<&[u8]> {
        self.previous_files()
            .find(|file| file.ownership == FileOwnership::Owned && file.relative_path == path)
            .map(|file| file.sha256.as_slice())
    }

    /// Keeps the entry for `path`, an edited file left alone, when Steno
    /// may write there: with the hash Steno last wrote, else this render's,
    /// never the user's, so the next delivery still sees the edit. No entry
    /// for a file Steno never wrote: a rolled-back Swift app writes any
    /// path the receipt lists.
    pub fn keep(&mut self, path: &str, data: &[u8]) {
        if !self.may_write(path, true) {
            return;
        }
        self.files
            .entry(path.to_owned())
            .or_insert_with(|| DeliveredFile {
                relative_path: path.to_owned(),
                ownership: FileOwnership::Owned,
                sha256: sha256(data),
            });
    }

    /// Whether `hash` is what the previous delivery wrote to `path` or a
    /// copy beside it ([`Self::copy_stamp`]).
    #[must_use]
    pub fn wrote(&self, path: &str, hash: &[u8]) -> bool {
        self.previous_files().any(|file| {
            file.ownership == FileOwnership::Owned
                && file.sha256 == hash
                && (file.relative_path == path
                    || Self::copy_stamp(path, &file.relative_path).is_some())
        })
    }

    /// The listed copies beside `path`, newest first by date and number.
    #[must_use]
    pub fn listed_copies(&self, path: &str) -> Vec<String> {
        let mut copies: Vec<_> = self
            .previous_files()
            .filter(|file| file.ownership == FileOwnership::Owned)
            .filter_map(|file| Some((Self::copy_stamp(path, &file.relative_path)?, file)))
            .collect();
        copies.sort_by(|(left, _), (right, _)| right.cmp(left));
        copies
            .into_iter()
            .map(|(_, file)| file.relative_path.clone())
            .collect()
    }

    /// The path of a copy beside `path`: `<stem> (Steno <date>).<ext>`,
    /// with ` <number>` after the date from 2 on.
    #[must_use]
    pub fn copy_beside_path(path: &str, date: &str, number: u32) -> String {
        let (stem, extension) = split_extension(path);
        if number == 1 {
            format!("{stem} (Steno {date}){extension}")
        } else {
            format!("{stem} (Steno {date} {number}){extension}")
        }
    }

    /// The date and number (1 when unnumbered) of `copy` when it is a copy
    /// beside `path`. ISO dates order as strings.
    #[must_use]
    pub fn copy_stamp<'a>(path: &str, copy: &'a str) -> Option<(&'a str, u32)> {
        let (stem, extension) = split_extension(path);
        let stamp = copy
            .strip_prefix(stem)?
            .strip_prefix(" (Steno ")?
            .strip_suffix(extension)?
            .strip_suffix(')')?;
        match stamp.split_once(' ') {
            Some((date, number)) => Some((date, number.parse().ok()?)),
            None => Some((stamp, 1)),
        }
    }

    pub fn warn(&mut self, warning: String) {
        self.warnings.push(warning);
    }

    /// Two spellings of one root, compared lexically; symlinks are not
    /// resolved.
    #[must_use]
    pub fn same_root(left: &str, right: &str) -> bool {
        Self::standardized(left) == Self::standardized(right)
    }

    /// `..` pops a plain component, vanishes against the root and
    /// otherwise stays.
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

    /// Whether the receipt's folder and file paths are all plain relative
    /// paths; a receipt failing it applies to nothing.
    fn stays_inside_root(receipt: &DeliveryReceipt) -> bool {
        Self::is_plain_relative(&receipt.folder)
            && receipt
                .files
                .iter()
                .all(|file| Self::is_plain_relative(&file.relative_path))
    }

    /// Non-empty, only named components: no `..`, root, drive or leading
    /// `.`. The one rule for receipt paths and the people folder.
    pub(crate) fn is_plain_relative(path: &str) -> bool {
        let mut components = Path::new(path).components().peekable();
        components.peek().is_some()
            && components.all(|component| matches!(component, Component::Normal(_)))
    }

    /// [`DeliveryLedger::claim_folder`] with a lookup: a missing candidate
    /// counts as claimed and nothing is created. For tests.
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

    /// The folder of a delivery without one: `base`, then `-2`, `-3`, …
    /// while the candidate holds another meeting's `meeting.json` (or
    /// none); one holding `meeting_id` is reused. `claim` creates the
    /// candidate, `Ok(true)` when new, so two deliveries never share a
    /// folder. A failed `claim` ends the search.
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

/// `path` split before its last component's extension.
fn split_extension(path: &str) -> (&str, &str) {
    let name_start = path.rfind('/').map_or(0, |slash| slash + 1);
    match path[name_start..].rfind('.') {
        Some(dot) if dot > 0 => path.split_at(name_start + dot),
        _ => (path, ""),
    }
}

/// `root` joined with `folder`, the meeting folder to reveal. A folder
/// that is not plain relative yields the root instead.
#[must_use]
pub fn receipt_folder_path(receipt: &DeliveryReceipt) -> PathBuf {
    let root = Path::new(&receipt.root);
    if DeliveryLedger::is_plain_relative(&receipt.folder) {
        root.join(&receipt.folder)
    } else {
        root.to_path_buf()
    }
}
