//! The Secret Service (`org.freedesktop.secrets`) on the session bus: the
//! keyring GNOME Keyring, `KWallet` and `KeePassXC` serve on Linux. A small
//! client over `zbus`, which the desktop shell already links, so the store
//! adds no crate, no C library and no second async runtime.
//! No Swift counterpart (the Swift app is macOS-only); the label follows
//! `KeychainSecretStore.swift`.
//!
//! The secret crosses the bus in the `plain` algorithm. The session bus is
//! a socket only the user's own processes reach, and reading another
//! client's messages on it (`dbus-monitor`, `BecomeMonitor`) takes that same
//! user, who can read Steno's memory and files anyway, so the `dh-ietf1024`
//! exchange would add a cipher and a key exchange without keeping the
//! secret from anyone who could not already read it. The bus is local; no
//! secret leaves the computer.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use futures_util::StreamExt as _;
use steno_core::{SecretKey, SecretPlace, SecretStore, async_trait, protocols::BoundaryResult};
use tokio::sync::watch;
use zbus::Connection;
use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Type, Value};

use super::{Contents, FileSecretStore, KEYRING_SERVICE, KeyringUnavailable, SecretsUnlocked};

/// The Secret Service when a provider answers on the session bus, else the
/// [`FileSecretStore`] it wraps; `STENO_<KEY>` wins over both.
///
/// The choice is made once, on a thread of the store's own that starts
/// when the store is made, and logged: the store connects, opens a
/// session, finds the default collection and unlocks it and Steno's items
/// in it (the provider may ask the user). No bus, no provider, no default
/// collection, or an unlock the user declines or leaves unanswered leaves
/// every secret of this process with the file, never some here and some
/// there. A call made while the choice runs waits for it, except while the
/// provider's prompt is on screen: then it fails at once with
/// [`KeyringUnavailable::Unlocking`], so no caller (the app's start on the
/// main thread, the host under its lock, a window's close) waits on the
/// user. [`SecretServiceStore::unlocked_after_prompt`] says when to read
/// again.
///
/// After the choice only a write asks the user, as a write is something
/// the user did: a read of a collection or an item that is locked again
/// fails with [`KeyringUnavailable::Locked`] and dismisses the provider's
/// prompt unseen.
///
/// On choosing the service the first time, the store copies what the file
/// holds into it, reads each value back, and then marks the file as moved
/// ([`FileSecretStore`] says what the marker does), keeping the entries: a
/// provider may hold a new item only in memory for a while (`KWallet` and
/// `KeePassXC` save on their own schedule), so the entries go only on a
/// later launch whose own connection reads every value back. A value the
/// provider lost by then is written again. A failure before the marker is
/// written keeps the file for this process.
///
/// A key both hold at the first move: the API key takes the file's value,
/// as only this app writes either store and a file entry is then from
/// before the move or from a run that could not open the keyring (and is
/// the newer value; a removal there leaves an empty value, which removes
/// the item). The handover identity keeps the service's: an identity is
/// never replaced, as the phones pinned one of them, and the file's stays
/// in the file for the user to recover. After the marker the service wins
/// for every key.
///
/// Items are filed under the attributes `service` ([`KEYRING_SERVICE`])
/// and `username` (the key's raw value), the names the `keyring` crate
/// uses, labelled `Steno <key>` for the keyring's window. Where several
/// items match a key (another tool wrote one), the store reads the one
/// with the lowest object path and a write removes the others. A value
/// with a line break (the handover identity's PEM) is stored on one line,
/// base64 behind `steno-base64:`.
pub struct SecretServiceStore {
    shared: Arc<Shared>,
}

struct Shared {
    file: FileSecretStore,
    /// How long a prompt may stay unanswered.
    prompt_timeout: Duration,
    backend: OnceLock<Backend>,
    phase: watch::Sender<Phase>,
    /// Whether the choice showed a prompt.
    asked: AtomicBool,
}

/// Where the choice is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Connecting and moving, without the user.
    Choosing,
    /// The provider's prompt is on screen; back to `Choosing` once it is
    /// answered.
    Asking,
    /// The backend is set: the service or the file, and whether the
    /// choice asked the user on the way.
    Chosen { service: bool, asked: bool },
}

impl std::fmt::Debug for SecretServiceStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SecretServiceStore")
            .field("file", &self.shared.file)
            .field("phase", &*self.shared.phase.borrow())
            .finish()
    }
}

/// Which bus the store looks for a provider on.
enum Bus {
    Session,
    /// A private bus, for the tests.
    #[cfg_attr(not(test), allow(dead_code))]
    Address(String),
}

enum Backend {
    Service(Keyring),
    File,
}

/// How long a D-Bus call may take, libdbus's default.
const CALL_TIMEOUT: Duration = Duration::from_secs(25);
/// How long the store waits for the user to answer the provider's prompt.
const PROMPT_TIMEOUT: Duration = Duration::from_secs(120);

impl SecretServiceStore {
    /// The Secret Service on the session bus, falling back to `file`; the
    /// choice starts now, on the store's own thread.
    #[must_use]
    pub fn new(file: FileSecretStore) -> Self {
        Self::start(file, Bus::Session, PROMPT_TIMEOUT)
    }

    /// The Secret Service on the bus at `address`, falling back to `file`.
    #[cfg(test)]
    pub(super) fn on_bus(file: FileSecretStore, address: &str, prompt_timeout: Duration) -> Self {
        Self::start(file, Bus::Address(address.to_owned()), prompt_timeout)
    }

    fn start(file: FileSecretStore, bus: Bus, prompt_timeout: Duration) -> Self {
        let shared = Arc::new(Shared {
            file,
            prompt_timeout,
            backend: OnceLock::new(),
            phase: watch::Sender::new(Phase::Choosing),
            asked: AtomicBool::new(false),
        });
        let chooser = shared.clone();
        let spawned = std::thread::Builder::new()
            .name("steno-secrets".to_owned())
            .spawn(move || {
                let backend = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime.block_on(chooser.choose(&bus)),
                    Err(error) => {
                        tracing::warn!("secrets: no runtime for the Secret Service ({error})");
                        Backend::File
                    }
                };
                chooser.settle(backend);
            });
        if let Err(error) = spawned {
            tracing::warn!("secrets: no thread for the Secret Service ({error})");
            shared.settle(Backend::File);
        }
        SecretServiceStore { shared }
    }

    /// Resolves once the store chose the service after asking the user,
    /// the moment reads that failed with [`KeyringUnavailable::Unlocking`]
    /// can be made again; never when it chose without asking or chose the
    /// file.
    pub fn unlocked_after_prompt(&self) -> SecretsUnlocked {
        let mut phase = self.shared.phase.subscribe();
        Box::pin(async move {
            let opened_after_asking = matches!(
                phase
                    .wait_for(|phase| matches!(phase, Phase::Chosen { .. }))
                    .await
                    .as_deref(),
                Ok(Phase::Chosen {
                    service: true,
                    asked: true
                })
            );
            if !opened_after_asking {
                std::future::pending::<()>().await;
            }
        })
    }

    /// The chosen backend, waiting for the choice unless it waits on the
    /// user. The backend is set before the phase turns `Chosen`.
    async fn backend(&self) -> Result<&Backend, KeyringUnavailable> {
        let mut phase = self.shared.phase.subscribe();
        let _ = phase.wait_for(|phase| *phase != Phase::Choosing).await;
        self.shared
            .backend
            .get()
            .ok_or(KeyringUnavailable::Unlocking)
    }

    /// Asks with the store's timeout and nothing to note.
    fn ask(&self) -> Ask<'static> {
        fn nothing(_: bool) {}
        Ask::User {
            timeout: self.shared.prompt_timeout,
            asking: &nothing,
        }
    }

    /// Waits until the choice is made, whoever it asks on the way.
    #[cfg(test)]
    async fn chosen(&self) {
        let mut phase = self.shared.phase.subscribe();
        phase
            .wait_for(|phase| matches!(phase, Phase::Chosen { .. }))
            .await
            .unwrap();
    }
}

impl Shared {
    fn settle(&self, backend: Backend) {
        let service = matches!(backend, Backend::Service(_));
        let _ = self.backend.set(backend);
        self.phase.send_replace(Phase::Chosen {
            service,
            asked: self.asked.load(Ordering::Relaxed),
        });
    }

    async fn choose(&self, bus: &Bus) -> Backend {
        let asking = |on: bool| {
            if on {
                self.asked.store(true, Ordering::Relaxed);
            }
            self.phase
                .send_replace(if on { Phase::Asking } else { Phase::Choosing });
        };
        let ask = Ask::User {
            timeout: self.prompt_timeout,
            asking: &asking,
        };
        let keyring = match Keyring::open(bus, ask).await {
            Ok(keyring) => keyring,
            Err(error) => {
                tracing::info!(
                    file = %self.file.path().display(),
                    "secrets: no Secret Service ({error}), keeping secrets with the file"
                );
                return Backend::File;
            }
        };
        let contents = match self.file.read() {
            Ok(contents) => contents,
            Err(error) => {
                tracing::warn!(
                    file = %self.file.path().display(),
                    "secrets: the secrets file could not be read ({error}), keeping secrets \
                     with the file"
                );
                return Backend::File;
            }
        };
        if contents.moved {
            match keyring.tidy(&self.file, &contents, ask).await {
                Ok(removed) => {
                    tracing::info!(removed, "secrets: keeping secrets in the Secret Service");
                }
                Err(error) => {
                    tracing::warn!(
                        "secrets: keeping secrets in the Secret Service; the file's copies \
                         stay for the next start ({error})"
                    );
                }
            }
            return Backend::Service(keyring);
        }
        match keyring.take_over(&self.file, &contents, ask).await {
            Ok(moved) => {
                tracing::info!(moved, "secrets: moved the file into the Secret Service");
                Backend::Service(keyring)
            }
            Err(error) => {
                tracing::warn!(
                    file = %self.file.path().display(),
                    "secrets: moving the file into the Secret Service failed ({error}), \
                     keeping secrets with the file"
                );
                Backend::File
            }
        }
    }
}

#[async_trait]
impl SecretStore for SecretServiceStore {
    /// Never asks the user.
    async fn secret(&self, key: &SecretKey) -> BoundaryResult<Option<String>> {
        if let Some(value) = self.shared.file.environment_value(key) {
            return Ok(Some(value.to_owned()));
        }
        match self.backend().await? {
            Backend::Service(keyring) => Ok(keyring.secret(key, Ask::Never).await?),
            Backend::File => self.shared.file.secret(key).await,
        }
    }

    /// `None` and an empty value remove the item, as the keyring store
    /// does; the file keeps an empty value, so the next move removes the
    /// item too. May ask the user to unlock.
    async fn set_secret(&self, key: &SecretKey, value: Option<&str>) -> BoundaryResult<()> {
        match self.backend().await? {
            Backend::Service(keyring) => Ok(keyring.set_secret(key, value, self.ask()).await?),
            Backend::File => {
                self.shared
                    .file
                    .set_secret(key, Some(value.unwrap_or_default()))
                    .await
            }
        }
    }

    /// `None` until the choice is made.
    fn place(&self) -> Option<SecretPlace> {
        match self.shared.backend.get()? {
            Backend::Service(_) => Some(SecretPlace::Keyring),
            Backend::File => Some(SecretPlace::File),
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
    #[error(
        "the Secret Service has no default keyring; create one in the keyring's manager \
         (Passwords and Keys, KWalletManager, KeePassXC's Secret Service settings)"
    )]
    NoDefaultCollection,
    #[error("the keyring stayed locked: its prompt was dismissed")]
    Dismissed,
    #[error("the keyring stayed locked: nobody answered its prompt")]
    PromptTimedOut,
    #[error("the Secret Service closed its prompt without an answer")]
    PromptClosed,
    #[error("the Secret Service holds `{0}` as bytes that are not UTF-8")]
    NotText(String),
    #[error("`{0}` did not read back from the Secret Service as written")]
    ReadBack(String),
    #[error(transparent)]
    Unavailable(#[from] KeyringUnavailable),
}

/// Whether a call may show the provider's prompt.
#[derive(Clone, Copy)]
enum Ask<'a> {
    /// Dismiss it unseen and fail with [`KeyringUnavailable::Locked`].
    Never,
    /// Show it and wait up to `timeout`; `asking` hears `true` as it
    /// shows and `false` once it is answered.
    User {
        timeout: Duration,
        asking: &'a (dyn Fn(bool) + Sync),
    },
}

/// A connection with an open session and the default collection.
struct Keyring {
    connection: Connection,
    /// The service object, which opens sessions and unlocks.
    service: ServiceProxy<'static>,
    session: OwnedObjectPath,
    collection: CollectionProxy<'static>,
}

/// What a value with a line break is stored behind, base64 after it.
/// GNOME Keyring's unencrypted keyring file (Omarchy's default, which
/// unlocks without a password) keeps each secret on one `secret=` line, and
/// a line break in a value makes the daemon reject the whole file at its
/// next start, every app's secrets with it.
const ONE_LINE_PREFIX: &str = "steno-base64:";

/// `value` as it is stored: itself, or base64 behind [`ONE_LINE_PREFIX`]
/// when it holds a line break.
fn one_line(value: &str) -> String {
    use base64::Engine as _;
    if value.contains(['\n', '\r']) {
        format!(
            "{ONE_LINE_PREFIX}{}",
            base64::engine::general_purpose::STANDARD.encode(value)
        )
    } else {
        value.to_owned()
    }
}

/// The value [`one_line`] stored as `stored`.
fn from_one_line(key: &SecretKey, stored: String) -> Result<String, ServiceError> {
    use base64::Engine as _;
    let Some(encoded) = stored.strip_prefix(ONE_LINE_PREFIX) else {
        return Ok(stored);
    };
    base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .ok_or_else(|| ServiceError::NotText(key.0.clone()))
}

/// The path the Secret Service answers with for "no object".
const NO_OBJECT: &str = "/";
const LABEL: &str = "org.freedesktop.Secret.Item.Label";
const ATTRIBUTES: &str = "org.freedesktop.Secret.Item.Attributes";

impl Keyring {
    /// Connects, opens a session and unlocks the default collection and
    /// every Steno item in it (`KeePassXC` locks items one by one).
    async fn open(bus: &Bus, ask: Ask<'_>) -> Result<Self, ServiceError> {
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
        let keyring = Keyring {
            collection: CollectionProxy::new(&connection, collection).await?,
            connection,
            service,
            session,
        };
        keyring.unlock_collection(ask).await?;
        let items = keyring
            .collection
            .search_items(HashMap::from([("service", KEYRING_SERVICE)]))
            .await?;
        if !items.is_empty() {
            let items: Vec<ObjectPath<'_>> = items.iter().map(ObjectPath::from).collect();
            keyring.unlock(&items, ask).await?;
        }
        Ok(keyring)
    }

    /// The first move: copies every entry of `contents` into the service,
    /// reads each back, then marks the file; how many it copied. The
    /// entries stay in the file until a later launch ([`Keyring::tidy`]).
    async fn take_over(
        &self,
        file: &FileSecretStore,
        contents: &Contents,
        ask: Ask<'_>,
    ) -> Result<usize, ServiceError> {
        let identity = SecretKey::from(steno_handover::HandoverIdentity::SECRET_KEY);
        let mut copied = 0;
        for (key, value) in &contents.entries {
            let key = SecretKey(key.clone());
            if key == identity
                && let Some(kept) = self.secret(&key, ask).await?
            {
                if kept != *value {
                    tracing::warn!(
                        file = %file.path().display(),
                        "secrets: the Secret Service already holds another handover identity; \
                         it stays, and the file keeps its own"
                    );
                }
                continue;
            }
            self.write_and_check(&key, value, ask).await?;
            copied += 1;
        }
        file.change(|contents| {
            contents.moved = true;
            Ok(())
        })?;
        Ok(copied)
    }

    /// A later launch: removes each file entry this connection reads back
    /// from the service, writes again the ones the provider lost, and drops
    /// an API key the service has since replaced; how many it removed. A
    /// handover identity that differs stays in both.
    async fn tidy(
        &self,
        file: &FileSecretStore,
        contents: &Contents,
        ask: Ask<'_>,
    ) -> Result<usize, ServiceError> {
        let identity = SecretKey::from(steno_handover::HandoverIdentity::SECRET_KEY);
        let mut done = Vec::new();
        for (key, value) in &contents.entries {
            let key = SecretKey(key.clone());
            match self.secret(&key, ask).await? {
                Some(held) if held == *value => done.push(key),
                None if value.is_empty() => done.push(key),
                None => {
                    tracing::info!(%key, "secrets: the Secret Service lost an item; writing it again");
                    self.write_and_check(&key, value, ask).await?;
                }
                Some(_) if key == identity => tracing::warn!(
                    file = %file.path().display(),
                    "secrets: the file and the Secret Service hold different handover \
                     identities; both stay"
                ),
                Some(_) => done.push(key),
            }
        }
        if done.is_empty() {
            return Ok(0);
        }
        file.change(|now| {
            for key in &done {
                if now.entries.get(key.as_str()) == contents.entries.get(key.as_str()) {
                    now.entries.remove(key.as_str());
                }
            }
            Ok(())
        })?;
        Ok(done.len())
    }

    /// Writes `value` and reads it back.
    async fn write_and_check(
        &self,
        key: &SecretKey,
        value: &str,
        ask: Ask<'_>,
    ) -> Result<(), ServiceError> {
        self.set_secret(key, Some(value), ask).await?;
        if self.secret(key, ask).await?.unwrap_or_default() != value {
            return Err(ServiceError::ReadBack(key.0.clone()));
        }
        Ok(())
    }

    async fn secret(&self, key: &SecretKey, ask: Ask<'_>) -> Result<Option<String>, ServiceError> {
        let Some(item) = self.items(key, ask).await?.into_iter().next() else {
            return Ok(None);
        };
        self.unlock(&[ObjectPath::from(&item)], ask).await?;
        let secret = ItemProxy::new(&self.connection, item)
            .await?
            .get_secret(&self.session)
            .await?;
        let stored =
            String::from_utf8(secret.value).map_err(|_| ServiceError::NotText(key.0.clone()))?;
        from_one_line(key, stored).map(Some)
    }

    async fn set_secret(
        &self,
        key: &SecretKey,
        value: Option<&str>,
        ask: Ask<'_>,
    ) -> Result<(), ServiceError> {
        let items = self.items(key, ask).await?;
        let Some(value) = value.filter(|value| !value.is_empty()) else {
            return self.delete(items, ask).await;
        };
        let label = format!("Steno {key}");
        let properties = HashMap::from([
            (LABEL, Value::from(label.as_str())),
            (ATTRIBUTES, Value::from(attributes(key))),
        ]);
        let secret = Secret {
            session: self.session.clone(),
            parameters: Vec::new(),
            value: one_line(value).into_bytes(),
            content_type: "text/plain".to_owned(),
        };
        let (created, prompt) = self
            .collection
            .create_item(properties, &secret, true)
            .await?;
        self.complete(prompt, ask).await?;
        let extras = items.into_iter().filter(|item| *item != created).collect();
        self.delete(extras, ask).await
    }

    async fn delete(&self, items: Vec<OwnedObjectPath>, ask: Ask<'_>) -> Result<(), ServiceError> {
        for item in items {
            let prompt = ItemProxy::new(&self.connection, item)
                .await?
                .delete()
                .await?;
            self.complete(prompt, ask).await?;
        }
        Ok(())
    }

    /// The default collection's items filed under `key`, lowest path
    /// first, once it is unlocked.
    async fn items(
        &self,
        key: &SecretKey,
        ask: Ask<'_>,
    ) -> Result<Vec<OwnedObjectPath>, ServiceError> {
        self.unlock_collection(ask).await?;
        let mut items = self.collection.search_items(attributes(key)).await?;
        items.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        Ok(items)
    }

    async fn unlock_collection(&self, ask: Ask<'_>) -> Result<(), ServiceError> {
        self.unlock(std::slice::from_ref(self.collection.inner().path()), ask)
            .await
    }

    /// Unlocks `objects`, which asks the user when one is locked; a no-op
    /// when none is.
    async fn unlock(&self, objects: &[ObjectPath<'_>], ask: Ask<'_>) -> Result<(), ServiceError> {
        let (_, prompt) = self.service.unlock(objects).await?;
        self.complete(prompt, ask).await
    }

    /// Shows `prompt` (unless it is none) and waits for the user's answer,
    /// or dismisses it unseen when the call may not ask.
    async fn complete(&self, prompt: OwnedObjectPath, ask: Ask<'_>) -> Result<(), ServiceError> {
        if prompt.as_str() == NO_OBJECT {
            return Ok(());
        }
        let prompt = PromptProxy::new(&self.connection, prompt).await?;
        let Ask::User { timeout, asking } = ask else {
            let _ = prompt.dismiss().await;
            return Err(KeyringUnavailable::Locked.into());
        };
        let mut completed = prompt.receive_completed().await?;
        asking(true);
        let answer = match prompt.prompt("").await {
            Ok(()) => tokio::time::timeout(timeout, completed.next()).await,
            Err(error) => {
                asking(false);
                return Err(error.into());
            }
        };
        asking(false);
        let Ok(signal) = answer else {
            let _ = prompt.dismiss().await;
            return Err(ServiceError::PromptTimedOut);
        };
        if signal.ok_or(ServiceError::PromptClosed)?.args()?.dismissed {
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

    fn dismiss(&self) -> zbus::Result<()>;

    #[zbus(signal)]
    fn completed(&self, dismissed: bool, result: Value<'_>) -> zbus::Result<()>;
}

#[cfg(test)]
#[path = "secret_service_tests.rs"]
mod tests;
