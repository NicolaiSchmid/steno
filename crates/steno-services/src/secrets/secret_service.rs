//! The Secret Service (`org.freedesktop.secrets`) on the session bus: the
//! keyring GNOME Keyring, `KWallet` and `KeePassXC` serve on Linux. A small
//! client over `zbus`, which the desktop shell already links, so the store
//! adds no crate, no C library and no second async runtime.
//!
//! The secret crosses the bus in the `plain` algorithm. The session bus is
//! a socket only the user's own processes reach, and any of them may ask
//! the service for an unlocked secret anyway, so the `dh-ietf1024`
//! exchange would add a cipher and a key exchange without keeping the
//! secret from anyone who could read it on the bus. The bus is local; no
//! secret leaves the computer.

use std::collections::HashMap;
use std::time::Duration;

use futures_util::StreamExt as _;
use steno_core::{SecretKey, SecretStore, async_trait, protocols::BoundaryResult};
use tokio::sync::OnceCell;
use zbus::Connection;
use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Type, Value};

use super::{FileSecretStore, KEYRING_SERVICE};

/// The Secret Service when a provider answers on the session bus, else the
/// [`FileSecretStore`] it wraps; `STENO_<KEY>` wins over both.
///
/// The choice is made once, on the first read or write, and logged: the
/// store connects, opens a session, finds the default collection and
/// unlocks it (the provider may ask the user). No bus, no provider, no
/// default collection, or an unlock the user declines leaves every secret
/// of this process in the file, never some here and some there.
///
/// On choosing the service, the store first moves what the file holds
/// into it: each entry is written to the default collection and read back,
/// and only once every entry reads back does the file lose them (and is
/// deleted when nothing is left), so a crash or a failure on the way
/// leaves every secret readable from the file at least. A failure there
/// keeps the file for this process. A key that both hold takes the file's
/// value: only this app writes either store, one instance at a time, and a
/// file entry exists only from before the move, from a run that fell back
/// to the file (and is then the newer value), or from a move cut short (and
/// is then the same value).
///
/// Items are filed under the attributes `service` ([`KEYRING_SERVICE`])
/// and `username` (the key's raw value), the names the `keyring` crate
/// uses, labelled `Steno <key>` for the keyring's window.
pub struct SecretServiceStore {
    file: FileSecretStore,
    bus: Bus,
    backend: OnceCell<Backend>,
}

impl std::fmt::Debug for SecretServiceStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SecretServiceStore")
            .field("file", &self.file)
            .field("bus", &self.bus)
            .field(
                "backend",
                &self.backend.get().map(|backend| match backend {
                    Backend::Service(_) => "Secret Service",
                    Backend::File => "file",
                }),
            )
            .finish()
    }
}

/// Which bus the store looks for a provider on.
#[derive(Debug)]
enum Bus {
    Session,
    /// A private bus, for the tests.
    #[cfg_attr(not(test), allow(dead_code))]
    Address(String),
}

enum Backend {
    Service(Service),
    File,
}

/// How long a D-Bus call may take, libdbus's default.
const CALL_TIMEOUT: Duration = Duration::from_secs(25);
/// How long the store waits for the user to answer the provider's unlock
/// prompt.
const PROMPT_TIMEOUT: Duration = Duration::from_secs(120);

impl SecretServiceStore {
    /// The Secret Service on the session bus, falling back to `file`.
    #[must_use]
    pub fn new(file: FileSecretStore) -> Self {
        SecretServiceStore {
            file,
            bus: Bus::Session,
            backend: OnceCell::new(),
        }
    }

    /// The Secret Service on the bus at `address`, falling back to `file`.
    #[cfg(test)]
    pub(super) fn on_bus(file: FileSecretStore, address: &str) -> Self {
        SecretServiceStore {
            file,
            bus: Bus::Address(address.to_owned()),
            backend: OnceCell::new(),
        }
    }

    async fn backend(&self) -> &Backend {
        self.backend.get_or_init(|| self.choose()).await
    }

    async fn choose(&self) -> Backend {
        let service = match Service::open(&self.bus).await {
            Ok(service) => service,
            Err(error) => {
                tracing::info!(
                    file = %self.file.path.display(),
                    "secrets: no Secret Service ({error}), keeping secrets in the file"
                );
                return Backend::File;
            }
        };
        match service.take_over(&self.file).await {
            Ok(moved) => {
                tracing::info!(moved, "secrets: keeping secrets in the Secret Service");
                Backend::Service(service)
            }
            Err(error) => {
                tracing::warn!(
                    file = %self.file.path.display(),
                    "secrets: moving the file into the Secret Service failed ({error}), \
                     keeping secrets in the file"
                );
                Backend::File
            }
        }
    }
}

#[async_trait]
impl SecretStore for SecretServiceStore {
    async fn secret(&self, key: &SecretKey) -> BoundaryResult<Option<String>> {
        if let Some(value) = self.file.environment_value(key) {
            return Ok(Some(value.to_owned()));
        }
        match self.backend().await {
            Backend::Service(service) => Ok(service.secret(key).await?),
            Backend::File => self.file.secret(key).await,
        }
    }

    /// `None` and an empty value remove the item, as the keyring store
    /// does; the file keeps an empty value, as in Swift.
    async fn set_secret(&self, key: &SecretKey, value: Option<&str>) -> BoundaryResult<()> {
        match self.backend().await {
            Backend::Service(service) => Ok(service.set_secret(key, value).await?),
            Backend::File => self.file.set_secret(key, value).await,
        }
    }
}

/// What can go wrong talking to the Secret Service.
#[derive(Debug, thiserror::Error)]
pub(super) enum ServiceError {
    #[error(transparent)]
    Bus(#[from] zbus::Error),
    #[error("the secrets file: {0}")]
    File(#[from] std::io::Error),
    #[error("the Secret Service has no default collection")]
    NoDefaultCollection,
    #[error("the keyring stayed locked: the unlock prompt was dismissed")]
    Dismissed,
    #[error("the keyring stayed locked: nobody answered the unlock prompt")]
    PromptTimedOut,
    #[error("the Secret Service closed the unlock prompt without an answer")]
    PromptClosed,
    #[error("the Secret Service holds `{0}` as bytes that are not UTF-8")]
    NotText(String),
    #[error("`{0}` did not read back from the Secret Service as written")]
    ReadBack(String),
}

/// A connection with an open session and the default collection.
struct Service {
    connection: Connection,
    session: OwnedObjectPath,
    collection: OwnedObjectPath,
}

/// The path the Secret Service answers with for "no object".
const NO_OBJECT: &str = "/";
const LABEL: &str = "org.freedesktop.Secret.Item.Label";
const ATTRIBUTES: &str = "org.freedesktop.Secret.Item.Attributes";

impl Service {
    async fn open(bus: &Bus) -> Result<Self, ServiceError> {
        let builder = match bus {
            Bus::Session => zbus::connection::Builder::session()?,
            Bus::Address(address) => zbus::connection::Builder::address(address.as_str())?,
        };
        let connection = builder.method_timeout(CALL_TIMEOUT).build().await?;
        let service = ServiceProxy::new(&connection).await?;
        let (_, session) = service.open_session("plain", &Value::from("")).await?;
        let collection = service.read_alias("default").await?;
        if collection.as_str() == NO_OBJECT {
            return Err(ServiceError::NoDefaultCollection);
        }
        let service = Service {
            connection,
            session,
            collection,
        };
        service.unlock().await?;
        Ok(service)
    }

    /// Moves every entry of `file` into the service; how many it moved.
    async fn take_over(&self, file: &FileSecretStore) -> Result<usize, ServiceError> {
        let entries = file.read()?;
        for (key, value) in &entries {
            let key = SecretKey(key.clone());
            self.set_secret(&key, Some(value)).await?;
            if self.secret(&key).await?.unwrap_or_default() != *value {
                return Err(ServiceError::ReadBack(key.0));
            }
        }
        if !entries.is_empty() {
            file.remove_moved(&entries)?;
        }
        Ok(entries.len())
    }

    async fn secret(&self, key: &SecretKey) -> Result<Option<String>, ServiceError> {
        let Some(item) = self.items(key).await?.into_iter().next() else {
            return Ok(None);
        };
        let secret = ItemProxy::new(&self.connection, item)
            .await?
            .get_secret(&self.session)
            .await?;
        String::from_utf8(secret.value)
            .map(Some)
            .map_err(|_| ServiceError::NotText(key.0.clone()))
    }

    async fn set_secret(&self, key: &SecretKey, value: Option<&str>) -> Result<(), ServiceError> {
        let Some(value) = value.filter(|value| !value.is_empty()) else {
            for item in self.items(key).await? {
                let prompt = ItemProxy::new(&self.connection, item)
                    .await?
                    .delete()
                    .await?;
                self.complete(prompt).await?;
            }
            return Ok(());
        };
        let label = format!("Steno {key}");
        let properties = HashMap::from([
            (LABEL, Value::from(label.as_str())),
            (ATTRIBUTES, Value::from(attributes(key))),
        ]);
        let secret = Secret {
            session: self.session.clone(),
            parameters: Vec::new(),
            value: value.as_bytes().to_vec(),
            content_type: "text/plain".to_owned(),
        };
        self.unlock().await?;
        let (_, prompt) = self
            .collection()
            .await?
            .create_item(properties, &secret, true)
            .await?;
        self.complete(prompt).await
    }

    /// The default collection's items filed under `key`, once it is
    /// unlocked.
    async fn items(&self, key: &SecretKey) -> Result<Vec<OwnedObjectPath>, ServiceError> {
        self.unlock().await?;
        Ok(self
            .collection()
            .await?
            .search_items(attributes(key))
            .await?)
    }

    async fn collection(&self) -> zbus::Result<CollectionProxy<'_>> {
        CollectionProxy::new(&self.connection, &self.collection).await
    }

    /// Unlocks the default collection, which asks the user when it is
    /// locked; a no-op when it is not.
    async fn unlock(&self) -> Result<(), ServiceError> {
        let (_, prompt) = ServiceProxy::new(&self.connection)
            .await?
            .unlock(&[self.collection.as_ref()])
            .await?;
        self.complete(prompt).await
    }

    /// Shows `prompt` (unless it is none) and waits for the user's answer.
    async fn complete(&self, prompt: OwnedObjectPath) -> Result<(), ServiceError> {
        if prompt.as_str() == NO_OBJECT {
            return Ok(());
        }
        let prompt = PromptProxy::new(&self.connection, prompt).await?;
        let mut completed = prompt.receive_completed().await?;
        prompt.prompt("").await?;
        let signal = tokio::time::timeout(PROMPT_TIMEOUT, completed.next())
            .await
            .map_err(|_| ServiceError::PromptTimedOut)?
            .ok_or(ServiceError::PromptClosed)?;
        if signal.args()?.dismissed {
            return Err(ServiceError::Dismissed);
        }
        Ok(())
    }
}

/// The attributes an item for `key` is filed and found under.
fn attributes(key: &SecretKey) -> HashMap<&str, &str> {
    HashMap::from([("service", KEYRING_SERVICE), ("username", key.as_str())])
}

/// A secret on the wire (`(oayays)`).
#[derive(Debug, serde::Serialize, serde::Deserialize, Type)]
pub(super) struct Secret {
    pub session: OwnedObjectPath,
    pub parameters: Vec<u8>,
    pub value: Vec<u8>,
    pub content_type: String,
}

#[zbus::proxy(
    interface = "org.freedesktop.Secret.Service",
    default_service = "org.freedesktop.secrets",
    default_path = "/org/freedesktop/secrets",
    gen_blocking = false
)]
trait Service {
    fn open_session(
        &self,
        algorithm: &str,
        input: &Value<'_>,
    ) -> zbus::Result<(OwnedValue, OwnedObjectPath)>;

    fn read_alias(&self, name: &str) -> zbus::Result<OwnedObjectPath>;

    fn unlock(
        &self,
        objects: &[ObjectPath<'_>],
    ) -> zbus::Result<(Vec<OwnedObjectPath>, OwnedObjectPath)>;
}

#[zbus::proxy(
    interface = "org.freedesktop.Secret.Collection",
    default_service = "org.freedesktop.secrets",
    gen_blocking = false
)]
trait Collection {
    fn search_items(&self, attributes: HashMap<&str, &str>) -> zbus::Result<Vec<OwnedObjectPath>>;

    fn create_item(
        &self,
        properties: HashMap<&str, Value<'_>>,
        secret: &Secret,
        replace: bool,
    ) -> zbus::Result<(OwnedObjectPath, OwnedObjectPath)>;
}

#[zbus::proxy(
    interface = "org.freedesktop.Secret.Item",
    default_service = "org.freedesktop.secrets",
    gen_blocking = false
)]
trait Item {
    fn get_secret(&self, session: &ObjectPath<'_>) -> zbus::Result<Secret>;

    fn delete(&self) -> zbus::Result<OwnedObjectPath>;
}

#[zbus::proxy(
    interface = "org.freedesktop.Secret.Prompt",
    default_service = "org.freedesktop.secrets",
    gen_blocking = false
)]
trait Prompt {
    fn prompt(&self, window_id: &str) -> zbus::Result<()>;

    #[zbus(signal)]
    fn completed(&self, dismissed: bool, result: Value<'_>) -> zbus::Result<()>;
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::Path;

    use super::super::fake_service::{Daemon, Shared, State, serve};
    use super::*;

    fn file_at(path: &Path, environment: &[(&str, &str)]) -> FileSecretStore {
        FileSecretStore::new(
            path,
            environment
                .iter()
                .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
                .collect(),
        )
    }

    fn entries(path: &Path) -> BTreeMap<String, String> {
        file_at(path, &[]).read().unwrap()
    }

    fn write_file(path: &Path, entries: &[(&str, &str)]) {
        let map: BTreeMap<_, _> = entries.iter().copied().collect();
        std::fs::write(path, serde_json::to_vec(&map).unwrap()).unwrap();
    }

    /// A store over a fresh fake: the daemon, the fake's connection (kept
    /// for its lifetime), the fake's state and the secrets file's folder.
    struct Setup {
        daemon: Daemon,
        _fake: Option<Connection>,
        state: Shared,
        folder: tempfile::TempDir,
    }

    impl Setup {
        /// None when `dbus-daemon` is missing (the test skips).
        async fn new(provider: bool, state: State) -> Option<Self> {
            let daemon = Daemon::start()?;
            let state = Shared::new(std::sync::Mutex::new(state));
            let fake = if provider {
                Some(serve(&daemon, state.clone()).await)
            } else {
                None
            };
            Some(Setup {
                daemon,
                _fake: fake,
                state,
                folder: tempfile::tempdir().unwrap(),
            })
        }

        fn path(&self) -> std::path::PathBuf {
            self.folder.path().join("secrets.json")
        }

        fn store(&self, environment: &[(&str, &str)]) -> SecretServiceStore {
            SecretServiceStore::on_bus(file_at(&self.path(), environment), &self.daemon.address)
        }

        fn values(&self, key: &str) -> Vec<String> {
            self.state.lock().unwrap().values(key)
        }

        fn chose_service(store: &SecretServiceStore) -> bool {
            matches!(store.backend.get(), Some(Backend::Service(_)))
        }
    }

    #[tokio::test]
    async fn secrets_are_read_written_and_removed_through_the_service() {
        let Some(setup) = Setup::new(true, State::default()).await else {
            return;
        };
        let store = setup.store(&[]);
        let key = SecretKey::llm_api_key();
        assert_eq!(store.secret(&key).await.unwrap(), None);
        assert!(Setup::chose_service(&store));
        store.set_secret(&key, Some("sk-1")).await.unwrap();
        store.set_secret(&key, Some("sk-2")).await.unwrap();
        assert_eq!(store.secret(&key).await.unwrap().as_deref(), Some("sk-2"));
        {
            let state = setup.state.lock().unwrap();
            let items: Vec<_> = state.items.values().collect();
            assert_eq!(items.len(), 1, "a second write replaces the item");
            assert_eq!(items[0].label, "Steno llm-api-key");
            assert_eq!(
                items[0].attributes,
                BTreeMap::from([
                    ("service".to_owned(), KEYRING_SERVICE.to_owned()),
                    ("username".to_owned(), "llm-api-key".to_owned()),
                ])
            );
        }
        let identity = SecretKey::from("handover-identity");
        store.set_secret(&identity, Some("pem")).await.unwrap();
        store.set_secret(&key, None).await.unwrap();
        assert_eq!(store.secret(&key).await.unwrap(), None);
        assert_eq!(setup.values("handover-identity"), ["pem"]);
        store.set_secret(&identity, Some("")).await.unwrap();
        assert!(setup.state.lock().unwrap().items.is_empty());
        assert!(!setup.path().exists(), "the file is never written");
    }

    #[tokio::test]
    async fn without_a_provider_every_secret_stays_in_the_file() {
        let Some(setup) = Setup::new(false, State::default()).await else {
            return;
        };
        write_file(&setup.path(), &[("llm-api-key", "sk-file")]);
        let store = setup.store(&[]);
        let key = SecretKey::llm_api_key();
        assert_eq!(
            store.secret(&key).await.unwrap().as_deref(),
            Some("sk-file")
        );
        assert!(matches!(store.backend.get(), Some(Backend::File)));
        store.set_secret(&key, Some("sk-2")).await.unwrap();
        assert_eq!(
            entries(&setup.path()),
            BTreeMap::from([("llm-api-key".to_owned(), "sk-2".to_owned())])
        );
    }

    #[tokio::test]
    async fn the_files_entries_move_into_the_service_and_the_file_goes() {
        let Some(setup) = Setup::new(true, State::default()).await else {
            return;
        };
        write_file(
            &setup.path(),
            &[("llm-api-key", "sk-file"), ("handover-identity", "pem")],
        );
        let store = setup.store(&[]);
        assert_eq!(
            store
                .secret(&SecretKey::from("handover-identity"))
                .await
                .unwrap()
                .as_deref(),
            Some("pem")
        );
        assert_eq!(setup.values("llm-api-key"), ["sk-file"]);
        assert_eq!(setup.values("handover-identity"), ["pem"]);
        assert!(!setup.path().exists(), "the emptied file is deleted");
    }

    #[tokio::test]
    async fn a_key_both_hold_takes_the_files_newer_value() {
        let Some(setup) = Setup::new(true, State::default()).await else {
            return;
        };
        let key = SecretKey::llm_api_key();
        setup
            .store(&[])
            .set_secret(&key, Some("sk-old"))
            .await
            .unwrap();
        write_file(&setup.path(), &[("llm-api-key", "sk-new")]);
        let store = setup.store(&[]);
        assert_eq!(store.secret(&key).await.unwrap().as_deref(), Some("sk-new"));
        assert_eq!(setup.values("llm-api-key"), ["sk-new"]);
        assert!(!setup.path().exists());
    }

    #[tokio::test]
    async fn without_a_default_collection_every_secret_stays_in_the_file() {
        let state = State {
            no_default: true,
            ..State::default()
        };
        let Some(setup) = Setup::new(true, state).await else {
            return;
        };
        let store = setup.store(&[]);
        let key = SecretKey::llm_api_key();
        store.set_secret(&key, Some("sk-1")).await.unwrap();
        assert!(matches!(store.backend.get(), Some(Backend::File)));
        assert_eq!(
            entries(&setup.path()),
            BTreeMap::from([("llm-api-key".to_owned(), "sk-1".to_owned())])
        );
        assert!(setup.state.lock().unwrap().items.is_empty());
    }

    #[tokio::test]
    async fn a_refused_move_keeps_the_file_for_every_secret() {
        let state = State {
            refuse_writes: true,
            ..State::default()
        };
        let Some(setup) = Setup::new(true, state).await else {
            return;
        };
        write_file(&setup.path(), &[("llm-api-key", "sk-file")]);
        let store = setup.store(&[]);
        let key = SecretKey::llm_api_key();
        assert_eq!(
            store.secret(&key).await.unwrap().as_deref(),
            Some("sk-file")
        );
        assert!(matches!(store.backend.get(), Some(Backend::File)));
        store
            .set_secret(&SecretKey::from("other"), Some("x"))
            .await
            .unwrap();
        assert_eq!(
            entries(&setup.path()),
            BTreeMap::from([
                ("llm-api-key".to_owned(), "sk-file".to_owned()),
                ("other".to_owned(), "x".to_owned()),
            ])
        );
    }

    #[tokio::test]
    async fn a_locked_keyring_is_unlocked_through_its_prompt() {
        let state = State {
            locked: true,
            ..State::default()
        };
        let Some(setup) = Setup::new(true, state).await else {
            return;
        };
        let store = setup.store(&[]);
        let key = SecretKey::llm_api_key();
        store.set_secret(&key, Some("sk-1")).await.unwrap();
        assert!(Setup::chose_service(&store));
        assert_eq!(setup.state.lock().unwrap().prompts, 1);

        // Locked again while the app runs: the next read asks again.
        setup.state.lock().unwrap().locked = true;
        assert_eq!(store.secret(&key).await.unwrap().as_deref(), Some("sk-1"));
        assert_eq!(setup.state.lock().unwrap().prompts, 2);
    }

    #[tokio::test]
    async fn a_dismissed_unlock_keeps_every_secret_in_the_file() {
        let state = State {
            locked: true,
            dismiss: true,
            ..State::default()
        };
        let Some(setup) = Setup::new(true, state).await else {
            return;
        };
        write_file(&setup.path(), &[("llm-api-key", "sk-file")]);
        let store = setup.store(&[]);
        let key = SecretKey::llm_api_key();
        assert_eq!(
            store.secret(&key).await.unwrap().as_deref(),
            Some("sk-file")
        );
        assert!(matches!(store.backend.get(), Some(Backend::File)));
        assert_eq!(setup.state.lock().unwrap().prompts, 1, "asked once");
        assert!(setup.state.lock().unwrap().items.is_empty());
    }

    #[tokio::test]
    async fn the_environment_wins_over_the_service() {
        let Some(setup) = Setup::new(true, State::default()).await else {
            return;
        };
        let key = SecretKey::llm_api_key();
        setup
            .store(&[])
            .set_secret(&key, Some("sk-service"))
            .await
            .unwrap();
        let store = setup.store(&[("STENO_LLM_API_KEY", "sk-env")]);
        assert_eq!(store.secret(&key).await.unwrap().as_deref(), Some("sk-env"));
        assert_eq!(setup.values("llm-api-key"), ["sk-service"]);
    }
}
