//! `SecretStore` implementations: the platform keyring (the Keychain, the
//! Windows credential store) through the `keyring` crate, the Secret
//! Service on Linux (`secret_service::SecretServiceStore`), and the 0600
//! JSON file the Swift CLI used where no keyring is reachable
//! (`STENO_<KEY>` wins over the file and over the Secret Service); and
//! [`KeepsApiKey`], the app's store over them, which keeps the API key for
//! the pipeline's rebuilds.
//! Swift: `apps/macos/Steno/Services/KeychainSecretStore.swift`,
//! `Sources/StenoCore/Testing/FileSecretStore.swift`.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use steno_core::{
    SecretKey, SecretPlace, SecretStore, StenoPaths, async_trait, protocols::BoundaryResult,
};

use crate::files::{Access, replace_file, restrict_new_file};

#[cfg(all(test, target_os = "linux"))]
mod fake_service;
#[cfg(target_os = "linux")]
mod secret_service;
#[cfg(target_os = "linux")]
pub use secret_service::SecretServiceStore;

/// The service every Steno keyring entry is filed under, on every
/// platform: the Swift app's (`KeychainSecretStore.defaultService` in
/// `apps/macos/Steno/Services/KeychainSecretStore.swift`), so the Rust app
/// reads the API key the Swift app stored on the Mac. The account is the
/// `SecretKey`'s raw value, as in Swift. Swift also labels the item
/// `Steno <key>`; the `keyring` crate cannot set a label (the Keychain
/// then shows the service), and lookups match on service and account only.
/// The Secret Service item carries the label (`SecretServiceStore` on
/// Linux).
pub const KEYRING_SERVICE: &str = "uno.schmid.steno.mac";

/// The platform keyring when `keyring` is set: the Keychain on macOS, the
/// credential store on Windows, the Secret Service on Linux when a
/// provider answers on the session bus (else the secrets file, decided on
/// a thread of the store's own as soon as it is made; see
/// `SecretServiceStore`). Without `keyring` (the CLI), the secrets file
/// under the support directory.
#[must_use]
pub fn secret_store(keyring: bool, paths: &StenoPaths) -> Arc<dyn SecretStore> {
    secret_store_with_unlock(keyring, paths).0
}

/// [`secret_store`], and on Linux what resolves once the Secret Service
/// store made its choice, true when a read failed with
/// [`KeyringUnavailable::Unlocking`] on the way while the keyring asked the
/// user: the app reads its secrets again then. `None` where no store asks.
#[must_use]
pub fn secret_store_with_unlock(
    keyring: bool,
    paths: &StenoPaths,
) -> (Arc<dyn SecretStore>, Option<SecretsUnlocked>) {
    let file = || FileSecretStore::in_support_directory(&paths.support_directory);
    if !keyring {
        return (Arc::new(file()), None);
    }
    #[cfg(target_os = "linux")]
    {
        let record =
            crate::handover::FingerprintFile::in_support_directory(&paths.support_directory);
        let store = SecretServiceStore::new(file(), record);
        let unlocked = store.unlocked_after_prompt();
        (Arc::new(store), Some(unlocked))
    }
    #[cfg(not(target_os = "linux"))]
    {
        (Arc::new(KeyringSecretStore), None)
    }
}

/// Resolves once the secret store chose where the secrets are, the
/// keyring or the file (so also when nothing was unlocked): true when it
/// turned a read away while the keyring asked the user, so the caller
/// reads again.
pub type SecretsUnlocked = std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send>>;

/// A secret Steno cannot reach because the keyring that holds it is
/// locked, still waiting for the user to answer its prompt, or was not
/// open when the app started. Never a reason to treat the secret as
/// absent: a caller that would mint or delete on `None` stops instead.
/// No Swift counterpart (the Keychain store reported its `OSStatus`).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum KeyringUnavailable {
    /// The keyring is waiting for the user to answer its prompt.
    #[error(
        "the keyring is waiting for an answer in its window; Steno reads its secrets again \
         once it is answered"
    )]
    Unlocking,
    /// The keyring was locked again while the app ran, refused Steno
    /// access (`KeePassXC`'s access dialog denied), or stopped answering.
    #[error(
        "the keyring is locked or did not let Steno in; unlock it or allow Steno, then start \
         Steno again"
    )]
    Locked,
    /// The secrets moved into the keyring, which could not be opened at
    /// start: locked, its prompt dismissed, no provider running, or no
    /// answer within two seconds. Holds the key's raw value; the message
    /// names it in plain words.
    #[error(
        "{} is kept in the keyring, which Steno could not open when it started; unlock the \
         keyring and start Steno again",
        plain_name(.0)
    )]
    NotOpened(String),
}

/// What the user calls the secret filed under `key`.
fn plain_name(key: &str) -> &'static str {
    match key {
        SecretKey::LLM_API_KEY => "the API key",
        steno_handover::HandoverIdentity::SECRET_KEY => "this computer's phone pairing",
        _ => "a secret",
    }
}

/// The platform keyring.
#[derive(Debug, Default)]
pub struct KeyringSecretStore;

impl KeyringSecretStore {
    fn entry(key: &SecretKey) -> Result<keyring::Entry, keyring::Error> {
        keyring::Entry::new(KEYRING_SERVICE, key.as_str())
    }
}

#[async_trait]
impl SecretStore for KeyringSecretStore {
    async fn secret(&self, key: &SecretKey) -> BoundaryResult<Option<String>> {
        match Self::entry(key)?.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(Box::new(error)),
        }
    }

    /// `None` and an empty value remove the entry, as in Swift.
    async fn set_secret(&self, key: &SecretKey, value: Option<&str>) -> BoundaryResult<()> {
        let entry = Self::entry(key)?;
        match value.filter(|value| !value.is_empty()) {
            Some(value) => entry.set_password(value)?,
            None => match entry.delete_credential() {
                Ok(()) | Err(keyring::Error::NoEntry) => {}
                Err(error) => return Err(Box::new(error)),
            },
        }
        Ok(())
    }

    fn place(&self) -> Option<SecretPlace> {
        Some(SecretPlace::Keyring)
    }
}

/// A JSON object of secrets in one file, owner-only (0600) where the
/// platform has modes. An environment variable `STENO_<KEY>`
/// (`llm-api-key` becomes `STENO_LLM_API_KEY`) overrides the file, so a
/// CLI run never has to store a key.
///
/// Every read goes to the file; nothing is cached, so two processes over
/// one file see each other's writes (the CLI and an app on Linux that
/// found no Secret Service provider). A write is a
/// read-modify-write under an advisory lock on `<file>.lock`, so two
/// processes writing different keys both keep theirs, and it replaces the
/// file by renaming a complete, synced temporary file over it, so a crash
/// or a full disk leaves the old file or the new one, never a torn one.
/// An empty file reads as no secrets, as in Swift, and an empty value as
/// no value.
///
/// Once the Linux app moved the entries into the Secret Service, the file
/// carries `"movedToSecretService": true` for good. From then on it is no
/// longer a store: a key it does not hold is an error
/// ([`KeyringUnavailable::NotOpened`]), not `None`, and a write fails, so
/// a run that cannot open the keyring never mints a fresh handover
/// identity or drops the API key. The entries the move left behind are
/// still read until a later launch deletes them, or the app saves that
/// key. A build from before the marker cannot parse the file (the marker
/// is a boolean, and that build reads only text values), so it fails every
/// secret read and write instead of minting.
pub struct FileSecretStore {
    path: PathBuf,
    environment: BTreeMap<String, String>,
}

/// The overriding variables' names only, never a value or another
/// variable.
impl std::fmt::Debug for FileSecretStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileSecretStore")
            .field("path", &self.path)
            .field(
                "overrides",
                &self
                    .environment
                    .keys()
                    .filter(|name| name.starts_with("STENO_"))
                    .collect::<Vec<_>>(),
            )
            .finish()
    }
}

/// The file's JSON object: the entries, and the marker the move into the
/// Secret Service leaves.
#[derive(Debug, Default, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Contents {
    #[serde(
        rename = "movedToSecretService",
        default,
        skip_serializing_if = "is_false"
    )]
    pub moved: bool,
    #[serde(flatten)]
    pub entries: BTreeMap<String, String>,
}

#[allow(clippy::trivially_copy_pass_by_ref)] // serde's `skip_serializing_if` passes a reference
fn is_false(value: &bool) -> bool {
    !*value
}

impl FileSecretStore {
    #[must_use]
    pub fn new(path: impl Into<PathBuf>, environment: BTreeMap<String, String>) -> Self {
        FileSecretStore {
            path: path.into(),
            environment,
        }
    }

    /// `secrets.json` under the support directory, reading the process
    /// environment's Unicode variables (see `text_variables`).
    #[must_use]
    pub fn in_support_directory(support_directory: &Path) -> Self {
        Self::new(
            support_directory.join("secrets.json"),
            text_variables(std::env::vars_os()),
        )
    }

    /// The variable that overrides `key`. Swift: `environmentVariable(for:)`.
    #[must_use]
    pub fn environment_variable(key: &SecretKey) -> String {
        format!("STENO_{}", key.as_str().to_uppercase().replace('-', "_"))
    }

    /// The value `STENO_<KEY>` sets for `key`, unless it is empty.
    fn environment_value(&self, key: &SecretKey) -> Option<&str> {
        self.environment
            .get(&Self::environment_variable(key))
            .map(String::as_str)
            .filter(|value| !value.is_empty())
    }

    /// Applies `change` to the contents under the lock and writes them
    /// back, deleting the file when nothing is left in it. The lock file
    /// stays: deleting it while another process waits on it would let the
    /// next writer lock a new file beside the old one.
    #[cfg(target_os = "linux")]
    pub(crate) fn change(
        &self,
        change: impl FnOnce(&mut Contents) -> std::io::Result<()>,
    ) -> std::io::Result<()> {
        self.locked(|contents| {
            change(contents)?;
            if *contents == Contents::default() {
                if let Err(error) = std::fs::remove_file(&self.path)
                    && error.kind() != std::io::ErrorKind::NotFound
                {
                    return Err(error);
                }
                if let Some(parent) = self.path.parent()
                    && let Ok(directory) = std::fs::File::open(parent)
                {
                    let _ = directory.sync_all();
                }
                return Ok(());
            }
            self.write(contents)
        })
    }

    pub(crate) fn read(&self) -> std::io::Result<Contents> {
        match std::fs::read(&self.path) {
            Ok(bytes) if bytes.is_empty() => Ok(Contents::default()),
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(std::io::Error::other),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Contents::default()),
            Err(error) => Err(error),
        }
    }

    /// The file next to the secrets with `suffix` appended to its name.
    fn beside(&self, suffix: &str) -> PathBuf {
        let mut name = self.path.file_name().unwrap_or_default().to_os_string();
        name.push(suffix);
        self.path.with_file_name(name)
    }

    /// Replaces the file with `contents`, owner-only.
    fn write(&self, contents: &Contents) -> std::io::Result<()> {
        let data = serde_json::to_vec_pretty(contents).map_err(std::io::Error::other)?;
        replace_file(&self.path, &data, Access::OwnerOnly)
    }

    /// Runs `write` on the stored contents under the advisory lock on
    /// `<file>.lock`.
    fn locked(
        &self,
        write: impl FnOnce(&mut Contents) -> std::io::Result<()>,
    ) -> std::io::Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let lock = restrict_new_file(std::fs::OpenOptions::new().create(true).write(true))
            .open(self.beside(".lock"))?;
        lock.lock()?;
        write(&mut self.read()?)
    }
}

/// The variables of `variables` whose name and value are both Unicode, as
/// a secret override is text. `std::env::vars()` panics on the first one
/// that is not, which would stop the app and the CLI from starting; the
/// path variables are read with `var_os` and used as they are
/// (`StenoPaths::support_directory`, `CodexCredentialStore::default_home`).
fn text_variables(
    variables: impl IntoIterator<Item = (OsString, OsString)>,
) -> BTreeMap<String, String> {
    variables
        .into_iter()
        .filter_map(|(name, value)| Some((name.into_string().ok()?, value.into_string().ok()?)))
        .collect()
}

#[async_trait]
impl SecretStore for FileSecretStore {
    async fn secret(&self, key: &SecretKey) -> BoundaryResult<Option<String>> {
        if let Some(value) = self.environment_value(key) {
            return Ok(Some(value.to_owned()));
        }
        let contents = self.read()?;
        match contents.entries.get(key.as_str()) {
            Some(value) => Ok(Some(value.clone()).filter(|value| !value.is_empty())),
            None if contents.moved => Err(Box::new(KeyringUnavailable::NotOpened(key.0.clone()))),
            None => Ok(None),
        }
    }

    /// Fails once the entries moved into the Secret Service.
    async fn set_secret(&self, key: &SecretKey, value: Option<&str>) -> BoundaryResult<()> {
        self.locked(|contents| {
            if contents.moved {
                return Err(std::io::Error::other(KeyringUnavailable::NotOpened(
                    key.0.clone(),
                )));
            }
            match value {
                Some(value) => {
                    contents
                        .entries
                        .insert(key.as_str().to_owned(), value.to_owned());
                }
                None => {
                    contents.entries.remove(key.as_str());
                }
            }
            self.write(contents)
        })?;
        Ok(())
    }

    fn place(&self) -> Option<SecretPlace> {
        Some(SecretPlace::File)
    }
}

/// The app's secret store, which keeps the last API key read or written
/// through it for the pipeline's rebuilds: a keyring locked again while
/// the app runs (`KeePassXC` locks with the session) fails a rebuild's
/// read, and the pipeline then keeps the key it had
/// ([`KeepsApiKey::kept_api_key`]) instead of running without summaries
/// until the next start. Every call passes through unchanged, so Settings
/// still see the failure. A write of the key replaces the kept one, a
/// removal or a failed write clears it, and a read that a write overtook
/// keeps nothing, so the kept key never outlives a removal or a change.
/// No Swift counterpart (the Keychain does not lock while the app runs).
pub struct KeepsApiKey {
    inner: Arc<dyn SecretStore>,
    kept: Mutex<Kept>,
}

#[derive(Default)]
struct Kept {
    key: Option<String>,
    /// How many writes of the key went through, so a read that began
    /// before one keeps nothing.
    writes: u64,
}

impl KeepsApiKey {
    #[must_use]
    pub fn new(inner: Arc<dyn SecretStore>) -> Self {
        KeepsApiKey {
            inner,
            kept: Mutex::default(),
        }
    }

    /// The API key the last successful read or write through the store
    /// saw; `None` after a removal, a failed write, or a read of no key.
    #[must_use]
    pub fn kept_api_key(&self) -> Option<String> {
        self.kept().key.clone()
    }

    fn kept(&self) -> MutexGuard<'_, Kept> {
        self.kept.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl std::fmt::Debug for KeepsApiKey {
    /// Never the key.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KeepsApiKey").finish_non_exhaustive()
    }
}

#[async_trait]
impl SecretStore for KeepsApiKey {
    async fn secret(&self, key: &SecretKey) -> BoundaryResult<Option<String>> {
        if key.as_str() != SecretKey::LLM_API_KEY {
            return self.inner.secret(key).await;
        }
        let writes = self.kept().writes;
        let read = self.inner.secret(key).await;
        if let Ok(value) = &read {
            let mut kept = self.kept();
            if kept.writes == writes {
                kept.key.clone_from(value);
            }
        }
        read
    }

    async fn set_secret(&self, key: &SecretKey, value: Option<&str>) -> BoundaryResult<()> {
        let written = self.inner.set_secret(key, value).await;
        if key.as_str() == SecretKey::LLM_API_KEY {
            let mut kept = self.kept();
            kept.writes += 1;
            kept.key = value
                .filter(|value| written.is_ok() && !value.is_empty())
                .map(str::to_owned);
        }
        written
    }

    fn place(&self) -> Option<SecretPlace> {
        self.inner.place()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use steno_core::testing::InMemorySecretStore;

    /// The kept key follows every read and write of the key, and a failed
    /// read leaves it as it was.
    #[tokio::test]
    async fn the_kept_api_key_follows_reads_and_writes_and_survives_a_failed_read() {
        let key = SecretKey::llm_api_key();
        let memory = Arc::new(InMemorySecretStore::with([(
            key.clone(),
            "sk-1".to_owned(),
        )]));
        let store = KeepsApiKey::new(memory.clone());
        assert_eq!(store.kept_api_key(), None, "nothing read yet");
        store.secret(&key).await.unwrap();
        assert_eq!(store.kept_api_key().as_deref(), Some("sk-1"));

        memory.fail_reads(Some("the keyring is locked"));
        assert!(
            store.secret(&key).await.is_err(),
            "the failure passes through"
        );
        assert_eq!(store.kept_api_key().as_deref(), Some("sk-1"));
        store.set_secret(&key, Some("sk-2")).await.unwrap();
        assert_eq!(store.kept_api_key().as_deref(), Some("sk-2"));
        store.set_secret(&key, None).await.unwrap();
        assert_eq!(store.kept_api_key(), None, "a removal clears it");

        memory.fail_reads(None);
        store.set_secret(&key, Some("sk-3")).await.unwrap();
        store.secret(&key).await.unwrap();
        assert_eq!(store.kept_api_key().as_deref(), Some("sk-3"));
        memory.set_secret(&key, None).await.unwrap();
        store.secret(&key).await.unwrap();
        assert_eq!(store.kept_api_key(), None, "a read of no key clears it");

        let identity = SecretKey::from(steno_handover::HandoverIdentity::SECRET_KEY);
        store.set_secret(&identity, Some("pem")).await.unwrap();
        store.secret(&identity).await.unwrap();
        assert_eq!(store.kept_api_key(), None, "only the API key is kept");
    }

    /// A write the store refuses clears the kept key, as the stored one
    /// may be gone; a read that began before a write keeps nothing.
    #[tokio::test]
    async fn a_failed_write_or_a_read_overtaken_by_a_write_keeps_no_old_key() {
        let key = SecretKey::llm_api_key();
        let refusing = KeepsApiKey::new(Arc::new(RefusingWrites(InMemorySecretStore::with([(
            key.clone(),
            "sk-1".to_owned(),
        )]))));
        refusing.secret(&key).await.unwrap();
        assert!(refusing.set_secret(&key, Some("sk-2")).await.is_err());
        assert_eq!(refusing.kept_api_key(), None);

        let (reading, release) = (
            Arc::new(tokio::sync::Notify::new()),
            Arc::new(tokio::sync::Notify::new()),
        );
        let slow = Arc::new(KeepsApiKey::new(Arc::new(SlowReads {
            inner: InMemorySecretStore::with([(key.clone(), "sk-1".to_owned())]),
            reading: reading.clone(),
            release: release.clone(),
        })));
        let read = tokio::spawn({
            let (slow, key) = (slow.clone(), key.clone());
            async move { slow.secret(&key).await.unwrap() }
        });
        reading.notified().await;
        slow.set_secret(&key, None).await.unwrap();
        release.notify_one();
        assert_eq!(
            read.await.unwrap().as_deref(),
            Some("sk-1"),
            "read before the removal"
        );
        assert_eq!(slow.kept_api_key(), None, "the removal wins");
    }

    /// A store whose writes fail.
    struct RefusingWrites(InMemorySecretStore);

    #[async_trait]
    impl SecretStore for RefusingWrites {
        async fn secret(&self, key: &SecretKey) -> BoundaryResult<Option<String>> {
            self.0.secret(key).await
        }

        async fn set_secret(&self, _key: &SecretKey, _value: Option<&str>) -> BoundaryResult<()> {
            Err("the keyring's prompt was dismissed".into())
        }
    }

    /// A store whose reads take the value, then wait for `release`.
    struct SlowReads {
        inner: InMemorySecretStore,
        reading: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
    }

    #[async_trait]
    impl SecretStore for SlowReads {
        async fn secret(&self, key: &SecretKey) -> BoundaryResult<Option<String>> {
            let value = self.inner.secret(key).await;
            self.reading.notify_one();
            self.release.notified().await;
            value
        }

        async fn set_secret(&self, key: &SecretKey, value: Option<&str>) -> BoundaryResult<()> {
            self.inner.set_secret(key, value).await
        }
    }

    #[tokio::test]
    async fn the_file_store_round_trips_and_the_environment_wins_for_every_key() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileSecretStore::new(dir.path().join("secrets.json"), BTreeMap::new());
        let key = SecretKey::llm_api_key();
        assert_eq!(store.secret(&key).await.unwrap(), None);
        store.set_secret(&key, Some("sk-1")).await.unwrap();
        assert_eq!(store.secret(&key).await.unwrap().as_deref(), Some("sk-1"));
        let again = FileSecretStore::new(dir.path().join("secrets.json"), BTreeMap::new());
        assert_eq!(again.secret(&key).await.unwrap().as_deref(), Some("sk-1"));
        store.set_secret(&key, None).await.unwrap();
        assert_eq!(store.secret(&key).await.unwrap(), None);

        assert_eq!(
            FileSecretStore::environment_variable(&key),
            "STENO_LLM_API_KEY"
        );
        let identity = SecretKey::from("handover-identity");
        assert_eq!(
            FileSecretStore::environment_variable(&identity),
            "STENO_HANDOVER_IDENTITY"
        );
        let env = FileSecretStore::new(
            dir.path().join("secrets.json"),
            BTreeMap::from([
                ("STENO_LLM_API_KEY".to_owned(), "sk-env".to_owned()),
                ("STENO_HANDOVER_IDENTITY".to_owned(), "pem-env".to_owned()),
            ]),
        );
        assert_eq!(env.secret(&key).await.unwrap().as_deref(), Some("sk-env"));
        assert_eq!(
            env.secret(&identity).await.unwrap().as_deref(),
            Some("pem-env")
        );
    }

    /// The keyring's message names each secret as the user knows it.
    #[test]
    fn a_secret_kept_in_the_keyring_is_named_in_plain_words() {
        let not_opened = |key: &str| KeyringUnavailable::NotOpened(key.to_owned()).to_string();
        assert!(
            not_opened(SecretKey::LLM_API_KEY).starts_with("the API key is kept in the keyring")
        );
        assert!(
            not_opened(steno_handover::HandoverIdentity::SECRET_KEY)
                .starts_with("this computer's phone pairing is kept in the keyring")
        );
        assert!(not_opened("other").starts_with("a secret is kept in the keyring"));
    }

    #[tokio::test]
    async fn after_the_move_a_missing_key_and_every_write_fail() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.json");
        std::fs::write(
            &path,
            br#"{"movedToSecretService": true, "handover-identity": "pem"}"#,
        )
        .unwrap();
        let store = FileSecretStore::new(&path, BTreeMap::new());
        let (identity, key) = (
            SecretKey::from("handover-identity"),
            SecretKey::llm_api_key(),
        );
        assert_eq!(
            store.secret(&identity).await.unwrap().as_deref(),
            Some("pem"),
            "an entry the move left behind still reads"
        );
        assert_eq!(
            store.secret(&key).await.unwrap_err().to_string(),
            KeyringUnavailable::NotOpened(key.0.clone()).to_string()
        );
        assert!(store.set_secret(&key, Some("sk-1")).await.is_err());
        assert!(store.set_secret(&identity, None).await.is_err());
        let env = FileSecretStore::new(
            &path,
            BTreeMap::from([("STENO_LLM_API_KEY".to_owned(), "sk-env".to_owned())]),
        );
        assert_eq!(env.secret(&key).await.unwrap().as_deref(), Some("sk-env"));
        assert!(
            serde_json::from_slice::<BTreeMap<String, String>>(&std::fs::read(&path).unwrap())
                .is_err(),
            "a build from before the marker cannot read the file at all"
        );
    }

    #[tokio::test]
    async fn an_empty_value_reads_as_none() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileSecretStore::new(dir.path().join("secrets.json"), BTreeMap::new());
        let key = SecretKey::llm_api_key();
        store.set_secret(&key, Some("")).await.unwrap();
        assert_eq!(store.secret(&key).await.unwrap(), None);
    }

    #[test]
    fn the_debug_output_names_the_overrides_and_no_value() {
        let store = FileSecretStore::new(
            "secrets.json",
            BTreeMap::from([
                ("STENO_LLM_API_KEY".to_owned(), "sk-secret".to_owned()),
                ("OTHER_TOKEN".to_owned(), "t0ken".to_owned()),
            ]),
        );
        let text = format!("{store:?}");
        assert!(text.contains("STENO_LLM_API_KEY"), "{text}");
        for hidden in ["sk-secret", "OTHER_TOKEN", "t0ken"] {
            assert!(!text.contains(hidden), "{text}");
        }
    }

    #[tokio::test]
    async fn an_empty_file_reads_as_no_secrets_and_takes_a_write() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.json");
        std::fs::write(&path, b"").unwrap();
        let store = FileSecretStore::new(&path, BTreeMap::new());
        let key = SecretKey::llm_api_key();
        assert_eq!(store.secret(&key).await.unwrap(), None);
        store.set_secret(&key, Some("sk-1")).await.unwrap();
        assert_eq!(store.secret(&key).await.unwrap().as_deref(), Some("sk-1"));
    }

    #[tokio::test]
    async fn a_store_sees_what_another_store_wrote() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.json");
        let first = FileSecretStore::new(&path, BTreeMap::new());
        let second = FileSecretStore::new(&path, BTreeMap::new());
        let (a, b) = (SecretKey::from("a"), SecretKey::from("b"));
        assert_eq!(first.secret(&a).await.unwrap(), None);
        second.set_secret(&a, Some("1")).await.unwrap();
        assert_eq!(first.secret(&a).await.unwrap().as_deref(), Some("1"));
        first.set_secret(&b, Some("2")).await.unwrap();
        assert_eq!(second.secret(&a).await.unwrap().as_deref(), Some("1"));
        assert_eq!(second.secret(&b).await.unwrap().as_deref(), Some("2"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_file_is_owner_only_whatever_mode_it_had() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.json");
        let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        let store = FileSecretStore::new(&path, BTreeMap::new());
        let key = SecretKey::llm_api_key();
        store
            .set_secret(&key, Some("a-rather-long-key-value"))
            .await
            .unwrap();
        assert_eq!(mode(&path), 0o600);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        store.set_secret(&key, Some("k")).await.unwrap();
        assert_eq!(
            mode(&path),
            0o600,
            "a write replaces the file, mode and all"
        );
        assert_eq!(mode(&store.beside(".lock")), 0o600);
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            serde_json::from_str::<BTreeMap<String, String>>(&text).unwrap(),
            BTreeMap::from([(key.as_str().to_owned(), "k".to_owned())])
        );
    }

    /// Keys per writer in the concurrency tests.
    const ROUNDS: usize = 20;
    const WRITER_PATH: &str = "STENO_TEST_SECRETS_WRITER_PATH";
    const WRITER_PREFIX: &str = "STENO_TEST_SECRETS_WRITER_PREFIX";

    async fn write_keys(path: &Path, prefix: &str) {
        let store = FileSecretStore::new(path, BTreeMap::new());
        for round in 0..ROUNDS {
            store
                .set_secret(&SecretKey(format!("{prefix}-{round}")), Some("value"))
                .await
                .unwrap();
        }
    }

    /// Reads the file until `done`, failing on a torn read.
    fn watch_for_tears(path: &Path, mut done: impl FnMut() -> bool) {
        while !done() {
            if let Ok(bytes) = std::fs::read(path) {
                assert!(
                    serde_json::from_slice::<BTreeMap<String, String>>(&bytes).is_ok(),
                    "torn read: {:?}",
                    String::from_utf8_lossy(&bytes)
                );
            }
        }
    }

    fn assert_every_key(path: &Path, prefixes: &[&str]) {
        let map: BTreeMap<String, String> =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        for prefix in prefixes {
            for round in 0..ROUNDS {
                assert!(
                    map.contains_key(&format!("{prefix}-{round}")),
                    "{prefix}-{round} was lost"
                );
            }
        }
    }

    #[test]
    fn two_threads_writing_different_keys_lose_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.json");
        let writers: Vec<_> = ["a", "b"]
            .into_iter()
            .map(|prefix| {
                let path = path.clone();
                std::thread::spawn(move || {
                    tokio::runtime::Builder::new_current_thread()
                        .build()
                        .unwrap()
                        .block_on(write_keys(&path, prefix));
                })
            })
            .collect();
        watch_for_tears(&path, || {
            writers.iter().all(std::thread::JoinHandle::is_finished)
        });
        for writer in writers {
            writer.join().unwrap();
        }
        assert_every_key(&path, &["a", "b"]);
    }

    /// One writer of [`two_processes_writing_different_keys_lose_nothing`];
    /// does nothing unless that test started this process.
    #[tokio::test]
    async fn a_writer_process_writes_its_keys_only_when_the_two_process_test_starts_it() {
        if let (Ok(path), Ok(prefix)) = (std::env::var(WRITER_PATH), std::env::var(WRITER_PREFIX)) {
            write_keys(Path::new(&path), &prefix).await;
        }
    }

    #[test]
    fn two_processes_writing_different_keys_lose_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.json");
        let mut writers: Vec<_> = ["a", "b"]
            .into_iter()
            .map(|prefix| {
                std::process::Command::new(std::env::current_exe().unwrap())
                    .args([
                        "secrets::tests::a_writer_process_writes_its_keys_only_when_the_two_process_test_starts_it",
                        "--exact",
                        "--test-threads=1",
                        "--quiet",
                    ])
                    .env(WRITER_PATH, &path)
                    .env(WRITER_PREFIX, prefix)
                    .stdout(std::process::Stdio::null())
                    .spawn()
                    .unwrap()
            })
            .collect();
        watch_for_tears(&path, || {
            writers
                .iter_mut()
                .all(|writer| writer.try_wait().unwrap().is_some())
        });
        for writer in &mut writers {
            assert!(writer.wait().unwrap().success());
        }
        assert_every_key(&path, &["a", "b"]);
    }

    /// Reads a Swift source from the repository root.
    fn swift_source(path: &str) -> String {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        std::fs::read_to_string(root.join(path)).unwrap_or_else(|error| panic!("{path}: {error}"))
    }

    #[test]
    fn keyring_entries_are_filed_as_the_swift_app_files_them() {
        let keychain = swift_source("apps/macos/Steno/Services/KeychainSecretStore.swift");
        assert!(
            keychain.contains(&format!(
                "static let defaultService = \"{KEYRING_SERVICE}\""
            )),
            "KeychainSecretStore.defaultService is not {KEYRING_SERVICE}"
        );
        assert!(
            keychain.contains("kSecAttrAccount as String: key.rawValue"),
            "the Swift account is no longer the key's raw value"
        );
        let keys = swift_source("Sources/StenoCore/Protocols/SecretStore.swift");
        assert!(
            keys.contains(&format!(
                "static let llmAPIKey = SecretKey(rawValue: \"{}\")",
                SecretKey::LLM_API_KEY
            )),
            "SecretKey.llmAPIKey is not {}",
            SecretKey::LLM_API_KEY
        );
    }

    /// Text no platform reads as Unicode: a lone continuation byte on
    /// Unix, an unpaired surrogate on Windows.
    fn not_unicode() -> OsString {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;
            OsString::from_vec(vec![b'a', 0x80])
        }
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStringExt;
            OsString::from_wide(&[u16::from(b'a'), 0xD800])
        }
    }

    #[test]
    fn a_variable_whose_name_or_value_is_not_unicode_is_skipped() {
        let kept = text_variables([
            (OsString::from("STENO_LLM_API_KEY"), OsString::from("sk-1")),
            (not_unicode(), OsString::from("a value")),
            (OsString::from("STENO_OTHER_KEY"), not_unicode()),
        ]);
        assert_eq!(
            kept,
            BTreeMap::from([("STENO_LLM_API_KEY".to_owned(), "sk-1".to_owned())])
        );
    }
}
