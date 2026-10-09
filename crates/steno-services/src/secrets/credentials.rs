//! The Windows credential store behind [`KeyringSecretStore`] on Windows:
//! one generic credential per secret, filed as the `keyring` crate (3.x)
//! filed it, so the credentials the app wrote through that crate read
//! unchanged, but kept on this computer (`CRED_PERSIST_LOCAL_MACHINE`)
//! where the crate kept them with the user's roaming profile
//! (`CRED_PERSIST_ENTERPRISE`, its only choice). A roaming credential
//! travels with a roaming profile, and on a domain computer the profile's
//! sync can remove it or bring back an older one; losing the handover
//! identity that way unpairs every phone.
//!
//! The format, as the crate wrote it: the target name `<key>.<service>`
//! (`llm-api-key.uno.schmid.steno.mac`, [`target_name`]), the user name the
//! key, the blob the value in UTF-16 little-endian without a terminating
//! NUL. The target name and the type identify a credential, and
//! `CredWriteW` replaces the credential of the same target name and type,
//! so [`write_secret`] is one `CredWriteW` and never deletes first. An
//! empty value removes the credential, as with the crate.
//!
//! Only a credential that does not exist (`ERROR_NOT_FOUND`) reads as none.
//! Every other failed read, and a blob that is not UTF-16 text, is an
//! error, so the handover identity's load reports it unreadable and never
//! missing (#221).
//!
//! A credential kept any other way (the crate's roaming ones) moves on its
//! first read in [`read_secret`], which is at launch for both secrets. Once
//! the read succeeded, the credential is written again with local
//! persistence and its user name and comment as read, under
//! `CRED_PRESERVE_CREDENTIAL_BLOB`: the credential manager keeps the blob
//! it holds. So the move carries no value and cannot write an old one over
//! a newer one, and where the credential was removed since the read, it
//! fails with `ERROR_NOT_FOUND` and writes nothing. The move is read back
//! and logged, and nothing is written after it: the read-back reads the
//! credential manager's set, so a credential it does not find was removed
//! there, and writing it back would bring a removed credential back. A
//! failed move is logged, the value read is still returned, and the next
//! read tries again. Either persistence reads through the crate too, so a
//! build from before the move still finds its secrets.
//!
//! Microsoft documents what `CredWriteW` replaces, not how or in which
//! order the credential manager writes it to disk, and local and roaming
//! credentials are kept in different folders of the profile. A power loss
//! while it writes the move is therefore the one moment the move does not
//! cover. Should the identity be gone after it, the next launch finds none
//! and #221's guard reports the handover unavailable rather than minting a
//! new identity.
//!
//! The rules above run over [`CredentialSet`], so they are tested on every
//! platform over an in-memory set; the system's set, over `CredReadW`,
//! `CredWriteW` and `CredDeleteW`, is the `windows` module, the one place
//! in the crate allowed `unsafe`. Every call takes the set by `&mut`, and
//! the system's set exists once per process behind a lock
//! (`windows::credential_set`), so this process's reads, moves and writes
//! run one after the other.
//!
//! No Swift counterpart: the Swift app runs on the Mac only.
//!
//! [`KeyringSecretStore`]: super::KeyringSecretStore

use std::io;

use steno_core::SecretKey;

use super::KEYRING_SERVICE;

// The crate's one `unsafe` module (AGENTS.md); every block in it carries a
// SAFETY comment.
#[cfg(windows)]
#[allow(unsafe_code)]
mod windows;
#[cfg(windows)]
pub(super) use windows::credential_set;

/// `CRED_PERSIST_LOCAL_MACHINE`: kept for every later logon of this user
/// on this computer, and on no other computer.
pub(super) const LOCAL_MACHINE: u32 = 2;

/// The most bytes a credential's blob holds
/// (`CRED_MAX_CREDENTIAL_BLOB_SIZE`): 1,280 UTF-16 code units.
const MAX_BLOB_BYTES: usize = 2560;

/// One generic credential, the fields Steno reads and writes. The target
/// alias is empty and the credential carries no attributes, as the
/// `keyring` crate wrote it.
#[derive(Clone, PartialEq, Eq)]
pub(super) struct Credential {
    /// The name the credential is looked up by, with its type.
    pub(super) target_name: String,
    /// The user name, which lookups ignore: the target name and the type
    /// identify a credential.
    pub(super) user_name: String,
    /// The comment Credential Manager shows.
    pub(super) comment: String,
    /// The secret.
    pub(super) blob: Vec<u8>,
    /// The persistence, a `CRED_PERSIST_*` value.
    pub(super) persist: u32,
}

impl std::fmt::Debug for Credential {
    /// Everything but the secret, of which only the length.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credential")
            .field("target_name", &self.target_name)
            .field("user_name", &self.user_name)
            .field("comment", &self.comment)
            .field("blob_bytes", &self.blob.len())
            .field("persist", &self.persist)
            .finish()
    }
}

/// The user's generic credentials.
pub(super) trait CredentialSet {
    /// The credential filed under `target_name`, `None` when there is none.
    fn read(&mut self, target_name: &str) -> io::Result<Option<Credential>>;
    /// Writes `credential`, replacing the one with the same target name.
    fn write(&mut self, credential: &Credential) -> io::Result<()>;
    /// Writes `credential` with local persistence, keeping the blob the
    /// credential filed under its target name holds (`credential.blob` is
    /// not written); false, writing nothing, when there is none.
    fn move_to_this_computer(&mut self, credential: &Credential) -> io::Result<bool>;
    /// Deletes the credential filed under `target_name`; false when there
    /// was none.
    fn delete(&mut self, target_name: &str) -> io::Result<bool>;
}

/// The target name the secret under `key` is filed under, the `keyring`
/// crate's `<user>.<service>`.
pub(super) fn target_name(key: &SecretKey) -> String {
    format!("{}.{KEYRING_SERVICE}", key.as_str())
}

/// The secret under `key`, `None` when there is none. A credential kept
/// other than on this computer moves there first (the module's doc).
pub(super) fn read_secret(
    set: &mut impl CredentialSet,
    key: &SecretKey,
) -> io::Result<Option<String>> {
    let Some(stored) = set.read(&target_name(key))? else {
        return Ok(None);
    };
    let value = text(&stored)?;
    if stored.persist != LOCAL_MACHINE {
        keep_on_this_computer(set, &stored);
    }
    Ok(Some(value))
}

/// Writes `value` under `key`, kept on this computer, over whatever the
/// credential held; `None` and an empty value remove the credential.
pub(super) fn write_secret(
    set: &mut impl CredentialSet,
    key: &SecretKey,
    value: Option<&str>,
) -> io::Result<()> {
    match value.filter(|value| !value.is_empty()) {
        Some(value) => set.write(&Credential {
            target_name: target_name(key),
            user_name: key.as_str().to_owned(),
            comment: format!("Steno {}", key.as_str()),
            blob: blob(value)?,
            persist: LOCAL_MACHINE,
        }),
        None => set.delete(&target_name(key)).map(drop),
    }
}

/// Moves `stored` to this computer, keeping the blob the credential
/// manager holds, and reads it back. Writes nothing else and only logs:
/// the caller holds the value either way.
fn keep_on_this_computer(set: &mut impl CredentialSet, stored: &Credential) {
    let target = &stored.target_name;
    match set.move_to_this_computer(stored) {
        Ok(true) => {}
        Ok(false) => {
            tracing::warn!("secrets: {target} was removed before it moved to this computer");
            return;
        }
        Err(error) => {
            tracing::warn!(
                "secrets: {target} stays with the roaming profile; keeping it on this computer \
                 failed ({error})"
            );
            return;
        }
    }
    match set.read(target) {
        Ok(Some(read)) if read.persist == LOCAL_MACHINE => {
            tracing::info!("secrets: {target} is kept on this computer now");
        }
        // A persistence the system chose: the credential is there, and the
        // next read tries again.
        Ok(Some(read)) => tracing::warn!(
            "secrets: {target} reads back with persistence {} after its move, left as it is",
            read.persist
        ),
        Ok(None) => tracing::warn!(
            "secrets: {target} was removed right after it moved to this computer, left removed"
        ),
        Err(error) => tracing::warn!(
            "secrets: {target} cannot be read back after its move ({error}), left as it is"
        ),
    }
}

/// `value` as the blob: UTF-16 little-endian, no terminating NUL.
fn blob(value: &str) -> io::Result<Vec<u8>> {
    let blob: Vec<u8> = value.encode_utf16().flat_map(u16::to_le_bytes).collect();
    if blob.len() > MAX_BLOB_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "the secret takes {} bytes, more than the {MAX_BLOB_BYTES} the Windows \
                 credential store keeps",
                blob.len()
            ),
        ));
    }
    Ok(blob)
}

/// The text in `credential`'s blob. A blob that is not UTF-16 is an error,
/// never a reason to treat the secret as absent.
fn text(credential: &Credential) -> io::Result<String> {
    let not_text = || {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "the credential {} does not hold UTF-16 text",
                credential.target_name
            ),
        )
    };
    let (units, rest) = credential.blob.as_chunks::<2>();
    if !rest.is_empty() {
        return Err(not_text());
    }
    let units: Vec<u16> = units.iter().map(|unit| u16::from_le_bytes(*unit)).collect();
    String::from_utf16(&units).map_err(|_| not_text())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    /// `CRED_PERSIST_ENTERPRISE`: kept as [`LOCAL_MACHINE`], and also with
    /// the user's roaming profile. The `keyring` crate's persistence.
    const ENTERPRISE: u32 = 3;

    /// What a [`FakeSet`] was asked to do, in order.
    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Call {
        Read(String),
        Write(String, u32),
        Move(String),
        Delete(String),
    }

    /// Changes another program makes to the credentials.
    type Meddling = fn(&mut BTreeMap<String, Credential>);

    /// An in-memory credential set, keyed by the target name as Windows
    /// keys it (case-insensitively), that records every call and fails,
    /// loses or changes what a test asks it to.
    #[derive(Default)]
    struct FakeSet {
        credentials: BTreeMap<String, Credential>,
        calls: Vec<Call>,
        /// How many of the next writes and moves fail.
        failing_writes: usize,
        /// The next reads, one per entry, front first: `None` reads what is
        /// stored, `Some(error)` fails with it.
        failing_reads: Vec<Option<io::Error>>,
        /// Another program's change just before the next move.
        before_move: Option<Meddling>,
        /// Another program's change just after the next move succeeded.
        after_move: Option<Meddling>,
    }

    impl FakeSet {
        fn with(credential: Credential) -> Self {
            let mut set = Self::default();
            set.credentials
                .insert(credential.target_name.to_lowercase(), credential);
            set
        }

        fn stored(&self, target_name: &str) -> Option<&Credential> {
            self.credentials.get(&target_name.to_lowercase())
        }

        fn refuses(&mut self) -> bool {
            let refuses = self.failing_writes > 0;
            self.failing_writes = self.failing_writes.saturating_sub(1);
            refuses
        }
    }

    impl CredentialSet for FakeSet {
        fn read(&mut self, target_name: &str) -> io::Result<Option<Credential>> {
            self.calls.push(Call::Read(target_name.to_owned()));
            if !self.failing_reads.is_empty()
                && let Some(error) = self.failing_reads.remove(0)
            {
                return Err(error);
            }
            Ok(self.stored(target_name).cloned())
        }

        fn write(&mut self, credential: &Credential) -> io::Result<()> {
            self.calls.push(Call::Write(
                credential.target_name.clone(),
                credential.persist,
            ));
            if self.refuses() {
                return Err(io::Error::other("the credential manager refused"));
            }
            self.credentials
                .insert(credential.target_name.to_lowercase(), credential.clone());
            Ok(())
        }

        fn move_to_this_computer(&mut self, credential: &Credential) -> io::Result<bool> {
            self.calls.push(Call::Move(credential.target_name.clone()));
            if let Some(meddle) = self.before_move.take() {
                meddle(&mut self.credentials);
            }
            if self.refuses() {
                return Err(io::Error::other("the credential manager refused"));
            }
            let Some(stored) = self
                .credentials
                .get_mut(&credential.target_name.to_lowercase())
            else {
                return Ok(false);
            };
            *stored = Credential {
                blob: std::mem::take(&mut stored.blob),
                persist: LOCAL_MACHINE,
                ..credential.clone()
            };
            if let Some(meddle) = self.after_move.take() {
                meddle(&mut self.credentials);
            }
            Ok(true)
        }

        fn delete(&mut self, target_name: &str) -> io::Result<bool> {
            self.calls.push(Call::Delete(target_name.to_owned()));
            Ok(self
                .credentials
                .remove(&target_name.to_lowercase())
                .is_some())
        }
    }

    /// What every [`CredentialSet`] does, as Microsoft documents
    /// `CredWriteW` with and without `CRED_PRESERVE_CREDENTIAL_BLOB`: a
    /// move keeps the stored blob and takes the other fields it is given,
    /// and once the credential is deleted it finds nothing to move and
    /// does not bring it back. Run over [`FakeSet`] on every platform and
    /// over the system's set on Windows, so the fake is held to the real
    /// store.
    pub(super) fn moves_as_the_credential_manager_documents(
        set: &mut impl CredentialSet,
        target_name: &str,
        user_name: &str,
    ) {
        let roaming = Credential {
            target_name: target_name.to_owned(),
            user_name: user_name.to_owned(),
            comment: "keyring v3.6.3".to_owned(),
            blob: vec![b'a', 0],
            persist: ENTERPRISE,
        };
        set.write(&roaming).unwrap();
        let without_the_blob = Credential {
            comment: "Steno moved".to_owned(),
            blob: vec![b'x', 0],
            ..roaming.clone()
        };
        assert!(set.move_to_this_computer(&without_the_blob).unwrap());
        assert_eq!(
            set.read(target_name).unwrap(),
            Some(Credential {
                comment: "Steno moved".to_owned(),
                persist: LOCAL_MACHINE,
                ..roaming.clone()
            }),
            "the stored blob is kept, the other fields are written"
        );
        assert!(set.delete(target_name).unwrap());
        assert!(!set.move_to_this_computer(&without_the_blob).unwrap());
        assert_eq!(set.read(target_name).unwrap(), None, "not brought back");
        assert!(!set.delete(target_name).unwrap());
    }

    const TARGET: &str = "llm-api-key.uno.schmid.steno.mac";

    /// A credential as the `keyring` crate 3.6 wrote it: roaming
    /// persistence, its comment, the value in UTF-16 little-endian.
    fn written_by_the_keyring_crate(value: &str) -> Credential {
        Credential {
            target_name: TARGET.to_owned(),
            user_name: "llm-api-key".to_owned(),
            comment: "keyring v3.6.3".to_owned(),
            blob: blob(value).unwrap(),
            persist: ENTERPRISE,
        }
    }

    /// The fake moves as the credential manager does.
    #[test]
    fn the_fake_set_moves_as_the_credential_manager_documents() {
        moves_as_the_credential_manager_documents(&mut FakeSet::default(), TARGET, "llm-api-key");
    }

    /// The credentials are filed where the `keyring` crate filed them,
    /// with its blob format, pinned as literals.
    #[test]
    fn a_secret_is_filed_as_the_keyring_crate_filed_it_and_kept_on_this_computer() {
        assert_eq!(target_name(&SecretKey::llm_api_key()), TARGET);
        assert_eq!(
            target_name(&steno_handover::HandoverIdentity::secret_key()),
            "handover-identity.uno.schmid.steno.mac"
        );
        let mut set = FakeSet::default();
        write_secret(&mut set, &SecretKey::llm_api_key(), Some("sk-é")).unwrap();
        assert_eq!(
            set.stored(TARGET).unwrap(),
            &Credential {
                target_name: TARGET.to_owned(),
                user_name: "llm-api-key".to_owned(),
                comment: "Steno llm-api-key".to_owned(),
                blob: vec![b's', 0, b'k', 0, b'-', 0, 0xE9, 0],
                persist: LOCAL_MACHINE,
            }
        );
        assert_eq!(
            read_secret(&mut set, &SecretKey::llm_api_key())
                .unwrap()
                .as_deref(),
            Some("sk-é")
        );
        assert_eq!(
            set.calls,
            [
                Call::Write(TARGET.to_owned(), LOCAL_MACHINE),
                Call::Read(TARGET.to_owned())
            ],
            "a local credential is read once and not written again"
        );
    }

    /// A roaming credential reads and moves to this computer with its
    /// blob, user name and comment; nothing is deleted or written on the
    /// way, and the next read finds it moved.
    #[test]
    fn a_roaming_credential_reads_and_moves_to_this_computer_once() {
        let roaming = written_by_the_keyring_crate("sk-1");
        let mut set = FakeSet::with(roaming.clone());
        assert_eq!(
            read_secret(&mut set, &SecretKey::llm_api_key())
                .unwrap()
                .as_deref(),
            Some("sk-1")
        );
        assert_eq!(
            set.stored(TARGET).unwrap(),
            &Credential {
                persist: LOCAL_MACHINE,
                ..roaming
            }
        );
        assert_eq!(
            set.calls,
            [
                Call::Read(TARGET.to_owned()),
                Call::Move(TARGET.to_owned()),
                Call::Read(TARGET.to_owned()),
            ]
        );
        set.calls.clear();
        read_secret(&mut set, &SecretKey::llm_api_key()).unwrap();
        assert_eq!(set.calls, [Call::Read(TARGET.to_owned())], "moved once");
    }

    /// A failed read is an error, never a missing secret, and nothing is
    /// moved or written.
    #[test]
    fn a_failed_read_is_an_error_and_writes_nothing() {
        let roaming = written_by_the_keyring_crate("sk-1");
        let mut set = FakeSet::with(roaming.clone());
        set.failing_reads = vec![Some(io::Error::other("the credential manager is busy"))];
        let error = read_secret(&mut set, &SecretKey::llm_api_key()).unwrap_err();
        assert_eq!(error.to_string(), "the credential manager is busy");
        assert_eq!(set.calls, [Call::Read(TARGET.to_owned())]);
        assert_eq!(set.stored(TARGET), Some(&roaming));
    }

    /// A move the credential manager refuses leaves the roaming credential
    /// as it was, and the value is still read; the next read tries again.
    #[test]
    fn a_refused_move_keeps_the_roaming_credential_and_still_reads_it() {
        let roaming = written_by_the_keyring_crate("sk-1");
        let mut set = FakeSet::with(roaming.clone());
        set.failing_writes = 1;
        assert_eq!(
            read_secret(&mut set, &SecretKey::llm_api_key())
                .unwrap()
                .as_deref(),
            Some("sk-1")
        );
        assert_eq!(set.stored(TARGET), Some(&roaming));
        read_secret(&mut set, &SecretKey::llm_api_key()).unwrap();
        assert_eq!(set.stored(TARGET).unwrap().persist, LOCAL_MACHINE);
    }

    /// A credential another program removes between the read and the move
    /// stays removed; one it rewrites keeps the newer value, as the move
    /// carries none. The value read is still returned.
    #[test]
    fn a_move_never_brings_back_or_overwrites_what_changed_since_the_read() {
        let mut set = FakeSet::with(written_by_the_keyring_crate("sk-1"));
        set.before_move = Some(BTreeMap::clear);
        assert_eq!(
            read_secret(&mut set, &SecretKey::llm_api_key())
                .unwrap()
                .as_deref(),
            Some("sk-1")
        );
        assert_eq!(
            set.calls,
            [Call::Read(TARGET.to_owned()), Call::Move(TARGET.to_owned())]
        );
        assert_eq!(set.stored(TARGET), None, "not brought back");

        let mut set = FakeSet::with(written_by_the_keyring_crate("sk-1"));
        set.before_move = Some(|credentials| {
            credentials.insert(TARGET.to_owned(), written_by_the_keyring_crate("sk-2"));
        });
        read_secret(&mut set, &SecretKey::llm_api_key()).unwrap();
        let stored = set.stored(TARGET).unwrap();
        assert_eq!(text(stored).unwrap(), "sk-2", "the newer key stays");
        assert_eq!(stored.persist, LOCAL_MACHINE);
    }

    /// Nothing is written after a move, whatever its read-back finds: a
    /// credential removed right after it stays removed, a newer one stays,
    /// and a failed read-back leaves the moved credential as it is.
    #[test]
    fn nothing_is_written_after_a_move_whatever_its_read_back_finds() {
        let removed: Meddling = BTreeMap::clear;
        let rewritten: Meddling = |credentials| {
            credentials.insert(TARGET.to_owned(), written_by_the_keyring_crate("sk-2"));
        };
        for (after_move, failing_reads, left) in [
            (Some(removed), vec![], None),
            (
                Some(rewritten),
                vec![],
                Some(written_by_the_keyring_crate("sk-2")),
            ),
            (
                None,
                vec![None, Some(io::Error::other("busy"))],
                Some(Credential {
                    persist: LOCAL_MACHINE,
                    ..written_by_the_keyring_crate("sk-1")
                }),
            ),
        ] {
            let mut set = FakeSet::with(written_by_the_keyring_crate("sk-1"));
            set.after_move = after_move;
            set.failing_reads = failing_reads;
            assert_eq!(
                read_secret(&mut set, &SecretKey::llm_api_key())
                    .unwrap()
                    .as_deref(),
                Some("sk-1")
            );
            assert_eq!(
                set.calls,
                [
                    Call::Read(TARGET.to_owned()),
                    Call::Move(TARGET.to_owned()),
                    Call::Read(TARGET.to_owned()),
                ]
            );
            assert_eq!(set.stored(TARGET), left.as_ref());
        }
    }

    /// A write over a roaming credential replaces it, kept on this
    /// computer, without deleting it first.
    #[test]
    fn a_write_over_a_roaming_credential_replaces_it_without_a_delete() {
        let mut set = FakeSet::with(written_by_the_keyring_crate("sk-1"));
        write_secret(&mut set, &SecretKey::llm_api_key(), Some("sk-2")).unwrap();
        assert_eq!(set.calls, [Call::Write(TARGET.to_owned(), LOCAL_MACHINE)]);
        let stored = set.stored(TARGET).unwrap();
        assert_eq!(text(stored).unwrap(), "sk-2");
        assert_eq!(stored.persist, LOCAL_MACHINE);
    }

    /// `None` and an empty value remove the credential; removing a missing
    /// one is no error. A missing credential reads as `None`.
    #[test]
    fn none_and_an_empty_value_remove_the_credential() {
        for removal in [None, Some("")] {
            let mut set = FakeSet::with(written_by_the_keyring_crate("sk-1"));
            write_secret(&mut set, &SecretKey::llm_api_key(), removal).unwrap();
            assert_eq!(set.stored(TARGET), None);
            assert_eq!(
                read_secret(&mut set, &SecretKey::llm_api_key()).unwrap(),
                None
            );
            write_secret(&mut set, &SecretKey::llm_api_key(), removal).unwrap();
        }
    }

    /// A blob that is not UTF-16 text is an error, and the credential is
    /// left as it is: not moved, not deleted.
    #[test]
    fn a_blob_that_is_not_text_is_an_error_and_left_alone() {
        for blob in [vec![b's', 0, b'k'], vec![0x00, 0xD8]] {
            let foreign = Credential {
                blob,
                ..written_by_the_keyring_crate("")
            };
            let mut set = FakeSet::with(foreign.clone());
            let error = read_secret(&mut set, &SecretKey::llm_api_key()).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
            assert_eq!(set.calls, [Call::Read(TARGET.to_owned())]);
            assert_eq!(set.stored(TARGET), Some(&foreign));
        }
    }

    /// A value longer than the credential store keeps is refused before
    /// anything is written; the credential keeps what it held.
    #[test]
    fn a_value_too_long_for_the_store_is_refused_before_a_write() {
        let mut set = FakeSet::with(written_by_the_keyring_crate("sk-1"));
        let long = "k".repeat(MAX_BLOB_BYTES / 2 + 1);
        let error = write_secret(&mut set, &SecretKey::llm_api_key(), Some(&long)).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(set.calls, []);
        assert_eq!(text(set.stored(TARGET).unwrap()).unwrap(), "sk-1");
        write_secret(
            &mut set,
            &SecretKey::llm_api_key(),
            Some(&long[..MAX_BLOB_BYTES / 2]),
        )
        .unwrap();
    }

    /// The handover identity, minted for a computer name as long as a DNS
    /// label, fits the credential store's blob.
    #[test]
    fn a_minted_identity_fits_the_credential_store() {
        let identity = steno_handover::HandoverIdentity::mint(
            &format!("Steno on {}", "n".repeat(63)),
            chrono::Utc::now(),
        )
        .unwrap();
        let pem = identity.to_pem().unwrap();
        let bytes = blob(&pem).unwrap().len();
        assert!(bytes <= MAX_BLOB_BYTES, "{bytes} bytes");
    }
}
