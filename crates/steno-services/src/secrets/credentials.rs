//! The Windows credential store behind [`KeyringSecretStore`], through
//! `keyring-core`'s Windows store: one generic credential per secret under
//! `<key>.<service>`, as the `keyring` crate 3 filed it, but kept on this
//! computer, not with the roaming profile whose sync can lose it. A roaming
//! credential is written back as local on its first read.
//!
//! [`KeyringSecretStore`]: super::KeyringSecretStore

use std::collections::HashMap;
use std::io;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use keyring_core::api::CredentialStoreApi as _;
use keyring_core::{Entry, Error};
use steno_core::SecretKey;
use windows_native_keyring_store::Store;

use super::KEYRING_SERVICE;

/// Every read and write of a Steno credential in this process runs under
/// this lock, so a read's move never writes back over a newer value.
static LOCK: Mutex<()> = Mutex::new(());

/// The entry for `key`, written with local persistence.
fn entry(key: &SecretKey) -> io::Result<Entry> {
    static STORE: OnceLock<Arc<Store>> = OnceLock::new();
    let store = if let Some(store) = STORE.get() {
        store
    } else {
        let store = Store::new().map_err(|error| other(&error))?;
        STORE.get_or_init(|| store)
    };
    store
        .build(
            KEYRING_SERVICE,
            key.as_str(),
            Some(&HashMap::from([("persistence", "local")])),
        )
        .map_err(|error| other(&error))
}

/// The secret under `key`, `None` when there is none. A credential kept
/// other than on this computer moves there first; a failed move only logs.
pub(super) fn read_secret(key: &SecretKey) -> io::Result<Option<String>> {
    let _held = LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let entry = entry(key)?;
    let value = match entry.get_password() {
        Ok(value) => value,
        Err(Error::NoEntry) => return Ok(None),
        Err(Error::BadEncoding(_)) => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("the credential for {key} does not hold UTF-16 text"),
            ));
        }
        Err(error) => return Err(other(&error)),
    };
    let persistence = entry
        .get_attributes()
        .map(|attributes| attributes.get("persistence").cloned());
    if !matches!(&persistence, Ok(Some(persistence)) if persistence == "Local") {
        match entry.set_password(&value) {
            Ok(()) => tracing::info!("secrets: {key} is kept on this computer now"),
            Err(error) => tracing::warn!(
                "secrets: {key} stays with the roaming profile; keeping it on this computer \
                 failed ({error})"
            ),
        }
    }
    Ok(Some(value))
}

/// Writes `value` under `key`, kept on this computer; `None` and an empty
/// value remove the credential.
pub(super) fn write_secret(key: &SecretKey, value: Option<&str>) -> io::Result<()> {
    let _held = LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let entry = entry(key)?;
    match value.filter(|value| !value.is_empty()) {
        Some(value) => entry.set_password(value).map_err(|error| other(&error)),
        None => match entry.delete_credential() {
            Ok(()) | Err(Error::NoEntry) => Ok(()),
            Err(error) => Err(other(&error)),
        },
    }
}

fn other(error: &Error) -> io::Error {
    io::Error::other(error.to_string())
}

/// Against the runner's real credential store, each test under target
/// names of its own (`steno-test-<uuid>.uno.schmid.steno.mac`), deleted
/// however it ends.
#[cfg(test)]
mod tests {
    use super::*;

    struct TestKey(SecretKey);

    impl TestKey {
        fn new() -> Self {
            TestKey(SecretKey(format!("steno-test-{}", uuid::Uuid::new_v4())))
        }

        /// The credential as `keyring` 3 wrote it, with roaming persistence.
        fn roaming(&self) -> Entry {
            Store::new()
                .unwrap()
                .build(
                    KEYRING_SERVICE,
                    self.0.as_str(),
                    Some(&HashMap::from([("persistence", "enterprise")])),
                )
                .unwrap()
        }

        fn attributes(&self) -> HashMap<String, String> {
            entry(&self.0).unwrap().get_attributes().unwrap()
        }
    }

    impl Drop for TestKey {
        fn drop(&mut self) {
            let _ = entry(&self.0).map(|entry| entry.delete_credential());
        }
    }

    #[test]
    fn a_written_secret_is_filed_as_before_and_kept_on_this_computer() {
        let key = TestKey::new();
        assert_eq!(read_secret(&key.0).unwrap(), None);
        write_secret(&key.0, Some("sk-é")).unwrap();
        let attributes = key.attributes();
        assert_eq!(
            attributes["target_name"],
            format!("{}.uno.schmid.steno.mac", key.0.as_str())
        );
        assert_eq!(attributes["persistence"], "Local");
        assert_eq!(read_secret(&key.0).unwrap().as_deref(), Some("sk-é"));
        for removal in [None, Some("")] {
            write_secret(&key.0, Some("sk-1")).unwrap();
            write_secret(&key.0, removal).unwrap();
            assert_eq!(read_secret(&key.0).unwrap(), None);
            write_secret(&key.0, removal).unwrap();
        }
    }

    #[test]
    fn a_roaming_credential_reads_and_moves_to_this_computer() {
        let key = TestKey::new();
        key.roaming().set_password("sk-1").unwrap();
        assert_eq!(key.attributes()["persistence"], "Enterprise");
        assert_eq!(read_secret(&key.0).unwrap().as_deref(), Some("sk-1"));
        assert_eq!(key.attributes()["persistence"], "Local");
        assert_eq!(read_secret(&key.0).unwrap().as_deref(), Some("sk-1"));
    }

    #[test]
    fn a_blob_that_is_not_text_is_an_error_and_left_alone() {
        let key = TestKey::new();
        key.roaming().set_secret(&[b's', 0, b'k']).unwrap();
        let error = read_secret(&key.0).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(key.attributes()["persistence"], "Enterprise");
    }

    /// The handover identity, minted for a computer name as long as a DNS
    /// label, fits the credential store; one byte past its limit does not.
    #[test]
    fn a_minted_identity_fits_the_credential_store() {
        let key = TestKey::new();
        let identity = steno_handover::HandoverIdentity::mint(
            &format!("Steno on {}", "n".repeat(63)),
            chrono::Utc::now(),
        )
        .unwrap();
        write_secret(&key.0, Some(&identity.to_pem().unwrap())).unwrap();
        assert!(write_secret(&key.0, Some(&"k".repeat(1281))).is_err());
    }
}
