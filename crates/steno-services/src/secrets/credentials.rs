//! The Windows credential store behind [`KeyringSecretStore`] on Windows:
//! one generic credential per secret, filed as the `keyring` crate (3.x)
//! filed it, so the entries the app wrote through that crate read
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
//! `CredWriteW` replaces the credential of the same target name and type in
//! place, so a write never deletes first. An empty value removes the entry,
//! as with the crate.
//!
//! A credential kept any other way (the crate's roaming ones) moves on
//! its first read, which is at launch for both secrets: once the read
//! succeeded, the same credential, byte for byte, is written again with
//! local persistence, replacing it in place. The move is read back; where
//! the read-back finds nothing or fails, the credential is written back as
//! it was read. A failed move is logged, the value read is still returned,
//! and the next read tries again. Either persistence reads through the
//! crate too, so a build from before the move still finds its secrets.
//!
//! The rules above run over [`CredentialSet`], so they are tested on every
//! platform over an in-memory set; the system's set, over `CredReadW`,
//! `CredWriteW` and `CredDeleteW`, is the `windows` module, the one place
//! in the crate allowed `unsafe`. Every call takes the set by `&mut`, and
//! the system's set exists once per process behind a lock
//! (`windows::credential_set`), so a read's move can never write an old
//! value over a newer one this process wrote in between.
//!
//! No Swift counterpart: the Swift app runs on the Mac only.
//!
//! [`KeyringSecretStore`]: super::KeyringSecretStore

use std::io;

use steno_core::SecretKey;

use super::KEYRING_SERVICE;

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
    /// The user name, which the credential manager ignores for a generic
    /// credential.
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
    /// Writes `credential`, replacing the one with the same target name in
    /// place.
    fn write(&mut self, credential: &Credential) -> io::Result<()>;
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
/// entry held; `None` and an empty value remove the entry.
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

/// Writes `stored` again with local persistence, which replaces it in
/// place, and reads it back; where the read-back finds nothing or fails,
/// writes `stored` back as it was read. Logs and returns on any failure:
/// the caller holds the value either way.
fn keep_on_this_computer(set: &mut impl CredentialSet, stored: &Credential) {
    let target = &stored.target_name;
    let local = Credential {
        persist: LOCAL_MACHINE,
        ..stored.clone()
    };
    if let Err(error) = set.write(&local) {
        tracing::warn!(
            "secrets: {target} stays with the roaming profile; keeping it on this computer \
             failed ({error})"
        );
        return;
    }
    let missing = match set.read(target) {
        Ok(Some(read)) if read.blob == stored.blob && read.persist == LOCAL_MACHINE => {
            tracing::info!("secrets: {target} is kept on this computer now");
            return;
        }
        // Someone else's write, or a persistence the system chose: the
        // credential is there, so it stays as it is.
        Ok(Some(read)) => {
            tracing::warn!(
                "secrets: {target} reads back other than it was written (persistence {}), \
                 left as it is",
                read.persist
            );
            return;
        }
        Ok(None) => "is not found".to_owned(),
        Err(error) => format!("cannot be read ({error})"),
    };
    tracing::warn!(
        "secrets: {target} {missing} after it was written to stay on this computer; writing \
         it back as it was"
    );
    if let Err(error) = set.write(stored) {
        tracing::error!("secrets: writing {target} back failed ({error})");
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
        Delete(String),
    }

    /// An in-memory credential set, keyed by the target name as Windows
    /// keys it (case-insensitively), that records every call and fails or
    /// loses what a test asks it to.
    #[derive(Default)]
    struct FakeSet {
        credentials: BTreeMap<String, Credential>,
        calls: Vec<Call>,
        /// How many of the next writes fail.
        failing_writes: usize,
        /// The next reads, one per entry, front first: `None` reads what is
        /// stored, `Some(Ok(()))` finds nothing, `Some(Err)` fails.
        odd_reads: Vec<Option<io::Result<()>>>,
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
    }

    impl CredentialSet for FakeSet {
        fn read(&mut self, target_name: &str) -> io::Result<Option<Credential>> {
            self.calls.push(Call::Read(target_name.to_owned()));
            if !self.odd_reads.is_empty() {
                match self.odd_reads.remove(0) {
                    Some(Err(error)) => return Err(error),
                    Some(Ok(())) => return Ok(None),
                    None => {}
                }
            }
            Ok(self.stored(target_name).cloned())
        }

        fn write(&mut self, credential: &Credential) -> io::Result<()> {
            self.calls.push(Call::Write(
                credential.target_name.clone(),
                credential.persist,
            ));
            if self.failing_writes > 0 {
                self.failing_writes -= 1;
                return Err(io::Error::other("the credential manager refused"));
            }
            self.credentials
                .insert(credential.target_name.to_lowercase(), credential.clone());
            Ok(())
        }

        fn delete(&mut self, target_name: &str) -> io::Result<bool> {
            self.calls.push(Call::Delete(target_name.to_owned()));
            Ok(self
                .credentials
                .remove(&target_name.to_lowercase())
                .is_some())
        }
    }

    const TARGET: &str = "llm-api-key.uno.schmid.steno.mac";

    /// An entry as the `keyring` crate 3.6 wrote it: roaming persistence,
    /// its comment, the value in UTF-16 little-endian.
    fn written_by_the_keyring_crate(value: &str) -> Credential {
        Credential {
            target_name: TARGET.to_owned(),
            user_name: "llm-api-key".to_owned(),
            comment: "keyring v3.6.3".to_owned(),
            blob: blob(value).unwrap(),
            persist: ENTERPRISE,
        }
    }

    /// The entries are filed where the `keyring` crate filed them, with
    /// its blob format, pinned as literals.
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
            "a local entry is read once and not written again"
        );
    }

    /// A roaming entry reads, and is written again in place, byte for byte
    /// and with its user name and comment, kept on this computer; nothing
    /// is deleted on the way.
    #[test]
    fn a_roaming_entry_reads_and_is_rewritten_in_place_on_this_computer() {
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
                Call::Write(TARGET.to_owned(), LOCAL_MACHINE),
                Call::Read(TARGET.to_owned()),
            ]
        );
        set.calls.clear();
        read_secret(&mut set, &SecretKey::llm_api_key()).unwrap();
        assert_eq!(set.calls, [Call::Read(TARGET.to_owned())], "moved once");
    }

    /// A move the credential manager refuses leaves the roaming entry as
    /// it was, and the value is still read; the next read tries again.
    #[test]
    fn a_refused_move_keeps_the_roaming_entry_and_still_reads_it() {
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

    /// Where the moved entry does not read back, or its read-back fails,
    /// the entry is written back as it was read.
    #[test]
    fn a_move_that_does_not_read_back_is_written_back_as_it_was() {
        for read_back in [
            Ok(()),
            Err(io::Error::other("the credential manager is busy")),
        ] {
            let roaming = written_by_the_keyring_crate("sk-1");
            let mut set = FakeSet::with(roaming.clone());
            set.odd_reads = vec![None, Some(read_back)];
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
                    Call::Write(TARGET.to_owned(), LOCAL_MACHINE),
                    Call::Read(TARGET.to_owned()),
                    Call::Write(TARGET.to_owned(), ENTERPRISE),
                ]
            );
            assert_eq!(set.stored(TARGET), Some(&roaming));
        }
    }

    /// A read-back that finds a credential other than the one written
    /// leaves it alone: it is there, and may be newer.
    #[test]
    fn a_move_that_reads_back_another_credential_leaves_it_alone() {
        struct Overwritten(FakeSet);
        impl CredentialSet for Overwritten {
            fn read(&mut self, target_name: &str) -> io::Result<Option<Credential>> {
                self.0.read(target_name)
            }
            fn write(&mut self, credential: &Credential) -> io::Result<()> {
                self.0.write(credential)?;
                // Another process writes a new key right after the move.
                let newer = Credential {
                    blob: blob("sk-2").unwrap(),
                    ..credential.clone()
                };
                self.0.credentials.insert(TARGET.to_owned(), newer);
                Ok(())
            }
            fn delete(&mut self, target_name: &str) -> io::Result<bool> {
                self.0.delete(target_name)
            }
        }
        let mut set = Overwritten(FakeSet::with(written_by_the_keyring_crate("sk-1")));
        read_secret(&mut set, &SecretKey::llm_api_key()).unwrap();
        assert_eq!(
            text(set.0.stored(TARGET).unwrap()).unwrap(),
            "sk-2",
            "the newer key stays"
        );
        assert_eq!(set.0.calls.len(), 3, "no write back: {:?}", set.0.calls);
    }

    /// A write over a roaming entry replaces it in place, kept on this
    /// computer, without deleting it first.
    #[test]
    fn a_write_over_a_roaming_entry_replaces_it_without_a_delete() {
        let mut set = FakeSet::with(written_by_the_keyring_crate("sk-1"));
        write_secret(&mut set, &SecretKey::llm_api_key(), Some("sk-2")).unwrap();
        assert_eq!(set.calls, [Call::Write(TARGET.to_owned(), LOCAL_MACHINE)]);
        let stored = set.stored(TARGET).unwrap();
        assert_eq!(text(stored).unwrap(), "sk-2");
        assert_eq!(stored.persist, LOCAL_MACHINE);
    }

    /// `None` and an empty value remove the entry; removing a missing one
    /// is no error. A missing entry reads as `None`.
    #[test]
    fn none_and_an_empty_value_remove_the_entry() {
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

    /// A blob that is not UTF-16 text is an error, and the entry is left
    /// as it is: not moved, not deleted.
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
    /// anything is written; the entry keeps what it held.
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
