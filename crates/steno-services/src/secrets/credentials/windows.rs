//! The user's generic credentials through `CredReadW`, `CredWriteW` and
//! `CredDeleteW`: the system's [`CredentialSet`], once per process behind
//! a lock ([`credential_set`]). A read copies the credential out of the
//! buffer `CredReadW` allocates, and [`Found`] zeroes its blob and frees it
//! with `CredFree`, once. Only `ERROR_NOT_FOUND` ([`is_not_found`]) is a
//! credential that does not exist; every other failure is an error. The
//! tests run against the runner's real credential store, under target
//! names of their own (`steno-test-<uuid>.uno.schmid.steno.mac`).

use std::io;
use std::sync::{Mutex, MutexGuard, PoisonError};

use windows_sys::Win32::Foundation::ERROR_NOT_FOUND;
use windows_sys::Win32::Security::Credentials::{
    CRED_MAX_CREDENTIAL_BLOB_SIZE, CRED_PERSIST_LOCAL_MACHINE, CRED_PRESERVE_CREDENTIAL_BLOB,
    CRED_TYPE_GENERIC, CREDENTIALW, CredDeleteW, CredFree, CredReadW, CredWriteW,
};

use super::{Credential, CredentialSet, LOCAL_MACHINE, MAX_BLOB_BYTES};

const _: () = assert!(LOCAL_MACHINE == CRED_PERSIST_LOCAL_MACHINE);
const _: () = assert!(MAX_BLOB_BYTES == CRED_MAX_CREDENTIAL_BLOB_SIZE as usize);

/// The user's credential set. Made only by [`credential_set`].
pub(in crate::secrets) struct System(());

/// The user's credential set, locked for this process: every read and
/// write of a Steno credential in this process runs under it, one after
/// the other.
pub(in crate::secrets) fn credential_set() -> MutexGuard<'static, System> {
    static SET: Mutex<System> = Mutex::new(System(()));
    SET.lock().unwrap_or_else(PoisonError::into_inner)
}

impl CredentialSet for System {
    fn read(&mut self, target_name: &str) -> io::Result<Option<Credential>> {
        let target = wide(target_name)?;
        let mut found: *mut CREDENTIALW = std::ptr::null_mut();
        // SAFETY: `target` is a NUL-terminated UTF-16 string without an
        // interior NUL (`wide`), owned by this frame and alive until the
        // call returns; `CredReadW` only reads it. `found` is a valid place
        // for the one pointer `CredReadW` writes, which it sets only when it
        // succeeds.
        let read = unsafe { CredReadW(target.as_ptr(), CRED_TYPE_GENERIC, 0, &raw mut found) };
        if read == 0 {
            let error = io::Error::last_os_error();
            return if is_not_found(&error) {
                Ok(None)
            } else {
                Err(error)
            };
        }
        if found.is_null() {
            return Err(io::Error::other("CredReadW succeeded without a credential"));
        }
        let found = Found(found);
        // SAFETY: on success `found.0` points to one `CREDENTIALW` the
        // credential manager allocated, which stays valid and unchanged
        // until `CredFree` in `Found`'s drop. `found` drops at the end of
        // this function, after the `Credential` below is built from the
        // last use of `credential`, so this borrow ends first.
        let credential = unsafe { &*found.0 };
        // SAFETY: the strings of a credential `CredReadW` returned are
        // NUL-terminated or null, and its blob is `CredentialBlobSize`
        // bytes or null; all live in the allocation `found` frees, which
        // outlives these reads (each is copied out).
        let (target_name, user_name, comment, blob) = unsafe {
            (
                string(credential.TargetName),
                string(credential.UserName),
                string(credential.Comment),
                bytes(credential.CredentialBlob, credential.CredentialBlobSize).to_vec(),
            )
        };
        Ok(Some(Credential {
            target_name,
            user_name,
            comment,
            blob,
            persist: credential.Persist,
        }))
    }

    fn write(&mut self, credential: &Credential) -> io::Result<()> {
        put(credential, credential.persist, Some(&credential.blob))
    }

    fn move_to_this_computer(&mut self, credential: &Credential) -> io::Result<bool> {
        match put(credential, CRED_PERSIST_LOCAL_MACHINE, None) {
            Ok(()) => Ok(true),
            Err(error) if is_not_found(&error) => Ok(false),
            Err(error) => Err(error),
        }
    }

    fn delete(&mut self, target_name: &str) -> io::Result<bool> {
        let target = wide(target_name)?;
        // SAFETY: `target` is a NUL-terminated UTF-16 string without an
        // interior NUL (`wide`), owned by this frame and alive until the
        // call returns; `CredDeleteW` only reads it.
        if unsafe { CredDeleteW(target.as_ptr(), CRED_TYPE_GENERIC, 0) } != 0 {
            return Ok(true);
        }
        let error = io::Error::last_os_error();
        if is_not_found(&error) {
            Ok(false)
        } else {
            Err(error)
        }
    }
}

/// One `CredWriteW` of `credential` with `persist`: with `blob`, which
/// replaces the stored blob, or with none under
/// `CRED_PRESERVE_CREDENTIAL_BLOB`, which keeps the stored blob and fails
/// with `ERROR_NOT_FOUND` when no credential is filed under the target name.
fn put(credential: &Credential, persist: u32, blob: Option<&[u8]>) -> io::Result<()> {
    let mut target = wide(&credential.target_name)?;
    let mut user = wide(&credential.user_name)?;
    let mut comment = wide(&credential.comment)?;
    let (flags, mut blob) = match blob {
        Some(blob) => (0, blob.to_vec()),
        None => (CRED_PRESERVE_CREDENTIAL_BLOB, Vec::new()),
    };
    let size = u32::try_from(blob.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "the secret is too long"))?;
    let raw = CREDENTIALW {
        Type: CRED_TYPE_GENERIC,
        TargetName: target.as_mut_ptr(),
        Comment: comment.as_mut_ptr(),
        CredentialBlobSize: size,
        CredentialBlob: if blob.is_empty() {
            std::ptr::null_mut()
        } else {
            blob.as_mut_ptr()
        },
        Persist: persist,
        UserName: user.as_mut_ptr(),
        // No credential flags (the call's own are `flags`), no attributes,
        // no target alias; `LastWritten` is ignored on a write.
        ..CREDENTIALW::default()
    };
    // SAFETY: every pointer in `raw` points into a buffer owned by this
    // frame and alive until the call returns: `target`, `user` and
    // `comment` are NUL-terminated UTF-16 strings without an interior NUL
    // (`wide`), the blob is `size` bytes, or null with a size of zero when
    // empty, which `CRED_PRESERVE_CREDENTIAL_BLOB` requires, and
    // `Attributes` and `TargetAlias` are null. `CredWriteW` only reads them
    // (the binding's pointers are mutable, the call writes through none)
    // and keeps no pointer to them after it returns.
    if unsafe { CredWriteW(&raw const raw, flags) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// A credential `CredReadW` allocated: its blob is zeroed and it is freed
/// on drop.
struct Found(*mut CREDENTIALW);

impl Drop for Found {
    fn drop(&mut self) {
        // SAFETY: `self.0` is the pointer a successful `CredReadW`
        // returned, non-null as `read` makes a `Found` only after its null
        // check, not freed yet and freed only here, once. `CredReadW`
        // returns the credential in one buffer this process owns until
        // `CredFree`, so its blob is `CredentialBlobSize` writable bytes of
        // it, or null; no borrow of the credential outlives `read`, which
        // ends before this drop.
        unsafe {
            let credential = &*self.0;
            if !credential.CredentialBlob.is_null() {
                std::ptr::write_bytes(
                    credential.CredentialBlob,
                    0,
                    credential.CredentialBlobSize as usize,
                );
            }
            CredFree(self.0.cast_const().cast());
        }
    }
}

/// Whether `error` is `ERROR_NOT_FOUND`, a credential that does not exist.
fn is_not_found(error: &io::Error) -> bool {
    error.raw_os_error() == Some(ERROR_NOT_FOUND.cast_signed())
}

/// `text` as a NUL-terminated UTF-16 string; text with a NUL inside is an
/// error, as Windows would cut it there.
fn wide(text: &str) -> io::Result<Vec<u16>> {
    if text.contains('\0') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "a credential's names hold no NUL",
        ));
    }
    Ok(text.encode_utf16().chain(std::iter::once(0)).collect())
}

/// The NUL-terminated UTF-16 string at `text`, empty when null; an
/// unpaired surrogate reads as U+FFFD.
///
/// # Safety
///
/// `text` is null or points to a NUL-terminated UTF-16 string that stays
/// valid for the call.
unsafe fn string(text: *const u16) -> String {
    if text.is_null() {
        return String::new();
    }
    let mut length = 0;
    // SAFETY: the string is NUL-terminated (the caller's contract), so
    // every unit up to and including the NUL is in bounds.
    while unsafe { *text.add(length) } != 0 {
        length += 1;
    }
    // SAFETY: the `length` units before the NUL are in bounds (above).
    String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(text, length) })
}

/// The `size` bytes at `blob`, empty when null or zero.
///
/// # Safety
///
/// `blob` is null or points to `size` readable bytes that stay valid and
/// unchanged for the returned borrow.
unsafe fn bytes<'a>(blob: *const u8, size: u32) -> &'a [u8] {
    if blob.is_null() || size == 0 {
        return &[];
    }
    // SAFETY: `size` readable bytes at `blob` (the caller's contract).
    unsafe { std::slice::from_raw_parts(blob, size as usize) }
}

/// The `on_windows_` tests run against the runner's real credential store,
/// each under target names of its own
/// (`steno-test-<uuid>.uno.schmid.steno.mac`), which it deletes again
/// however it ends; no other credential is touched.
#[cfg(test)]
mod tests {
    use steno_core::SecretKey;
    use windows_sys::Win32::Foundation::{ERROR_ACCESS_DENIED, ERROR_NO_SUCH_LOGON_SESSION};
    use windows_sys::Win32::Security::Credentials::CRED_PERSIST_ENTERPRISE;

    use super::super::tests::moves_as_the_credential_manager_documents;
    use super::super::{read_secret, target_name, write_secret};
    use super::*;
    use crate::secrets::KEYRING_SERVICE;

    /// A key of the test's own, whose credential is deleted when it drops.
    struct TestKey(SecretKey);

    impl TestKey {
        fn new() -> Self {
            Self(SecretKey(format!("steno-test-{}", uuid::Uuid::new_v4())))
        }

        /// The credential as `CredReadW` returns it.
        fn stored(&self) -> Option<Credential> {
            credential_set().read(&target_name(&self.0)).unwrap()
        }

        /// The `keyring` crate's entry for the key.
        fn keyring_entry(&self) -> keyring::Entry {
            keyring::Entry::new(KEYRING_SERVICE, self.0.as_str()).unwrap()
        }
    }

    impl Drop for TestKey {
        fn drop(&mut self) {
            let _ = credential_set().delete(&target_name(&self.0));
        }
    }

    /// Only `ERROR_NOT_FOUND` is a credential that does not exist; a read
    /// that fails any other way is an error, never a missing secret.
    #[test]
    fn only_a_credential_that_does_not_exist_reads_as_none() {
        let code = |code: u32| io::Error::from_raw_os_error(code.cast_signed());
        assert!(is_not_found(&code(ERROR_NOT_FOUND)));
        assert!(!is_not_found(&code(ERROR_NO_SUCH_LOGON_SESSION)));
        assert!(!is_not_found(&code(ERROR_ACCESS_DENIED)));
        assert!(!is_not_found(&io::Error::other("not an OS error")));
    }

    /// A secret written here is kept on this computer, filed and encoded
    /// as the `keyring` crate files it, so the crate (and a build from
    /// before this store) reads it too; a removal deletes it.
    #[test]
    fn on_windows_a_written_secret_is_kept_on_this_computer_and_reads_through_the_keyring_crate() {
        let key = TestKey::new();
        write_secret(&mut *credential_set(), &key.0, Some("sk-é")).unwrap();
        let stored = key.stored().unwrap();
        assert_eq!(stored.persist, CRED_PERSIST_LOCAL_MACHINE);
        assert_eq!(stored.target_name, target_name(&key.0));
        assert_eq!(stored.user_name, key.0.as_str());
        assert_eq!(stored.blob, [b's', 0, b'k', 0, b'-', 0, 0xE9, 0]);
        assert_eq!(key.keyring_entry().get_password().unwrap(), "sk-é");
        assert_eq!(
            read_secret(&mut *credential_set(), &key.0)
                .unwrap()
                .as_deref(),
            Some("sk-é")
        );

        write_secret(&mut *credential_set(), &key.0, Some("sk-2")).unwrap();
        assert_eq!(key.keyring_entry().get_password().unwrap(), "sk-2");
        write_secret(&mut *credential_set(), &key.0, None).unwrap();
        assert!(key.stored().is_none());
        assert_eq!(read_secret(&mut *credential_set(), &key.0).unwrap(), None);
        write_secret(&mut *credential_set(), &key.0, None).unwrap();
    }

    /// A credential the `keyring` crate wrote roams
    /// (`CRED_PERSIST_ENTERPRISE`); its first read returns it and moves it
    /// to this computer with its blob, user name and comment, under the
    /// same target name, where the crate still reads it.
    #[test]
    fn on_windows_a_credential_the_keyring_crate_wrote_reads_and_moves_to_this_computer() {
        let key = TestKey::new();
        let identity = steno_handover::HandoverIdentity::mint("Steno on test", chrono::Utc::now())
            .unwrap()
            .to_pem()
            .unwrap();
        key.keyring_entry().set_password(&identity).unwrap();
        let roaming = key.stored().unwrap();
        assert_eq!(roaming.persist, CRED_PERSIST_ENTERPRISE);

        assert_eq!(
            read_secret(&mut *credential_set(), &key.0)
                .unwrap()
                .as_deref(),
            Some(identity.as_str())
        );
        let moved = key.stored().unwrap();
        assert_eq!(
            moved,
            Credential {
                persist: CRED_PERSIST_LOCAL_MACHINE,
                ..roaming
            }
        );
        assert_eq!(key.keyring_entry().get_password().unwrap(), identity);
    }

    /// `CredWriteW` replaces the credential of the same target name,
    /// whatever its persistence: one credential per target name and type,
    /// never two.
    #[test]
    fn on_windows_a_write_replaces_the_credential_of_the_same_target_name() {
        let key = TestKey::new();
        let roaming = Credential {
            target_name: target_name(&key.0),
            user_name: key.0.as_str().to_owned(),
            comment: "keyring v3.6.3".to_owned(),
            blob: vec![b'a', 0],
            persist: CRED_PERSIST_ENTERPRISE,
        };
        credential_set().write(&roaming).unwrap();
        // The target name is case-insensitive: the same credential.
        let local = Credential {
            target_name: target_name(&key.0).to_uppercase(),
            blob: vec![b'b', 0],
            persist: CRED_PERSIST_LOCAL_MACHINE,
            ..roaming.clone()
        };
        credential_set().write(&local).unwrap();
        let stored = key.stored().unwrap();
        assert_eq!(stored.blob, [b'b', 0]);
        assert_eq!(stored.persist, CRED_PERSIST_LOCAL_MACHINE);
        assert!(credential_set().delete(&roaming.target_name).unwrap());
        assert!(key.stored().is_none(), "no second credential was left");
        assert!(!credential_set().delete(&roaming.target_name).unwrap());
    }

    /// A move under `CRED_PRESERVE_CREDENTIAL_BLOB` keeps the blob the
    /// credential manager holds and never brings back a deleted credential,
    /// as the in-memory set's does.
    #[test]
    fn on_windows_a_move_keeps_the_stored_blob_and_never_brings_back_a_deleted_credential() {
        let key = TestKey::new();
        moves_as_the_credential_manager_documents(
            &mut *credential_set(),
            &target_name(&key.0),
            key.0.as_str(),
        );
    }
}
