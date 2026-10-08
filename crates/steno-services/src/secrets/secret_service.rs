//! The Secret Service (`org.freedesktop.secrets`) on the session bus: the
//! keyring GNOME Keyring, `KWallet` and `KeePassXC` serve on Linux. A small
//! client over `zbus`, which the desktop shell already links, so the store
//! adds no crate, no C library and no second async runtime crate.
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
use steno_core::{
    BoxError, SecretKey, SecretPlace, SecretStore, async_trait, protocols::BoundaryResult,
};
use steno_handover::{FingerprintRecord as _, HandoverIdentity};
use tokio::sync::watch;
use zbus::Connection;
use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Type, Value};

use super::{Contents, FileSecretStore, KEYRING_SERVICE, KeyringUnavailable, SecretsUnlocked};
use crate::handover::FingerprintFile;

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
/// [`KeyringUnavailable::Unlocking`], so no read (the app's start on the
/// main thread, the host under its lock, a window's close) waits on the
/// user. [`SecretServiceStore::unlocked_after_prompt`] says when to read
/// again.
///
/// After the choice only a write asks the user, as a write is something
/// the user did: a read of a collection or an item that is locked again
/// fails with [`KeyringUnavailable::Locked`] and dismisses the provider's
/// prompt unseen. A write waits for the answer, up to two minutes; the
/// host saves under its lock, so a keyring locked again while the app runs
/// holds every window and the tray until the user answers the prompt
/// Settings' save raised.
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
/// in the file until an identity is stored (Pair again), or for the user
/// to take by hand. As this computer's phones paired
/// with the file's identity, its fingerprint is recorded first when none
/// is ([`FingerprintFile`]), so the handover reports the service's as
/// replaced instead of adopting it. After the marker the service wins for
/// every key, and a write through the store drops that key's copy from the
/// file at once, so a removal or a change in the launch that moved the
/// entries is never undone by the copy.
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
    /// Where the handover identity's fingerprint is recorded.
    record: FingerprintFile,
    /// How long a prompt may stay unanswered.
    prompt_timeout: Duration,
    backend: OnceLock<Backend>,
    /// Whether the file carries the move's marker, for a choice of the
    /// file: the secrets are then in the keyring.
    marked: AtomicBool,
    phase: watch::Sender<Phase>,
    /// Whether a call failed with [`KeyringUnavailable::Unlocking`]; set
    /// and read under the phase's lock.
    turned_away: AtomicBool,
}

/// Where the choice is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Connecting and moving, without the user.
    Choosing,
    /// The provider's prompt is on screen; back to `Choosing` once it is
    /// answered.
    Asking,
    /// The backend is set; `reread` when a call failed on the way.
    Chosen { reread: bool },
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
    /// The Secret Service on the session bus, falling back to `file`, with
    /// the handover identity's fingerprint in `record`; the choice starts
    /// now, on the store's own thread.
    #[must_use]
    pub fn new(file: FileSecretStore, record: FingerprintFile) -> Self {
        Self::start(file, record, Bus::Session, PROMPT_TIMEOUT)
    }

    /// The Secret Service on the bus at `address`, falling back to `file`.
    #[cfg(test)]
    pub(super) fn on_bus(
        file: FileSecretStore,
        record: FingerprintFile,
        address: &str,
        prompt_timeout: Duration,
    ) -> Self {
        Self::start(
            file,
            record,
            Bus::Address(address.to_owned()),
            prompt_timeout,
        )
    }

    fn start(
        file: FileSecretStore,
        record: FingerprintFile,
        bus: Bus,
        prompt_timeout: Duration,
    ) -> Self {
        let shared = Arc::new(Shared {
            file,
            record,
            prompt_timeout,
            backend: OnceLock::new(),
            marked: AtomicBool::new(false),
            phase: watch::Sender::new(Phase::Choosing),
            turned_away: AtomicBool::new(false),
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

    /// Resolves once the choice is made, the service or the file: true
    /// when a call failed with [`KeyringUnavailable::Unlocking`] on the way,
    /// so now is the moment to make it again.
    ///
    /// Every prompt of the choice counts, whether the provider shows a
    /// window or not (`KeePassXC` answers every `CreateItem` with a prompt
    /// that may show nothing, and the Secret Service API does not say
    /// which); a call is turned away only while one is open.
    pub fn unlocked_after_prompt(&self) -> SecretsUnlocked {
        let mut phase = self.shared.phase.subscribe();
        Box::pin(async move {
            matches!(
                phase
                    .wait_for(|phase| matches!(phase, Phase::Chosen { .. }))
                    .await
                    .as_deref(),
                Ok(Phase::Chosen { reread: true })
            )
        })
    }

    /// The chosen backend, waiting for the choice unless it waits on the
    /// user. The backend is set before the phase turns `Chosen`; a call
    /// turned away is noted under the phase's lock, so the choice either
    /// is made before this look or hears of it.
    async fn backend(&self) -> Result<&Backend, KeyringUnavailable> {
        let mut phase = self.shared.phase.subscribe();
        let _ = phase.wait_for(|phase| *phase != Phase::Choosing).await;
        let mut chosen = false;
        self.shared.phase.send_if_modified(|phase| {
            chosen = matches!(phase, Phase::Chosen { .. });
            if !chosen {
                self.shared.turned_away.store(true, Ordering::Relaxed);
            }
            false
        });
        if !chosen {
            return Err(KeyringUnavailable::Unlocking);
        }
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
        if matches!(backend, Backend::File) {
            let marked = self.file.read().is_ok_and(|contents| contents.moved);
            self.marked.store(marked, Ordering::Relaxed);
        }
        let _ = self.backend.set(backend);
        self.phase.send_modify(|phase| {
            *phase = Phase::Chosen {
                reread: self.turned_away.load(Ordering::Relaxed),
            };
        });
    }

    async fn choose(&self, bus: &Bus) -> Backend {
        let asking = |on: bool| {
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
                let what = if matches!(error, ServiceError::Bus(_)) {
                    "no Secret Service"
                } else {
                    "the Secret Service could not be opened"
                };
                tracing::warn!("secrets: {what} ({error}), keeping secrets with the file");
                return Backend::File;
            }
        };
        let contents = match self.file.read() {
            Ok(contents) => contents,
            Err(error) => {
                tracing::warn!(
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
        match keyring
            .take_over(&self.file, &self.record, &contents, ask)
            .await
        {
            Ok(moved) => {
                tracing::info!(moved, "secrets: moved the file into the Secret Service");
                Backend::Service(keyring)
            }
            Err(error) => {
                tracing::warn!(
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
    /// Never asks the user. A read of the service has one deadline of
    /// 25 s, a D-Bus call's, over all its calls, so a provider that stops
    /// answering holds a caller (the host under its lock) that long once,
    /// not once per call.
    async fn secret(&self, key: &SecretKey) -> BoundaryResult<Option<String>> {
        if let Some(value) = self.shared.file.environment_value(key) {
            return Ok(Some(value.to_owned()));
        }
        match self.backend().await? {
            Backend::Service(keyring) => {
                let read = tokio::time::timeout(CALL_TIMEOUT, keyring.secret(key, Ask::Never))
                    .await
                    .map_err(|_| ServiceError::NoAnswer)?;
                Ok(read?)
            }
            Backend::File => self.shared.file.secret(key).await,
        }
    }

    /// `None` and an empty value remove the item, as the keyring store
    /// does; the file keeps an empty value, so the next move removes the
    /// item too. May ask the user to unlock.
    ///
    /// A write to the service drops the key's copy the move left in the
    /// file: the service holds the newer value, and a later launch would
    /// otherwise write the old one back over a removal, or read it in a run
    /// that cannot open the keyring.
    async fn set_secret(&self, key: &SecretKey, value: Option<&str>) -> BoundaryResult<()> {
        match self.backend().await? {
            Backend::Service(keyring) => {
                keyring.set_secret(key, value, self.ask()).await?;
                let file = &self.shared.file;
                if file.read()?.entries.contains_key(key.as_str()) {
                    file.change(|contents| {
                        contents.entries.remove(key.as_str());
                        Ok(())
                    })?;
                }
                Ok(())
            }
            Backend::File => {
                self.shared
                    .file
                    .set_secret(key, Some(value.unwrap_or_default()))
                    .await
            }
        }
    }

    /// `None` until the choice is made. A marked file says the keyring:
    /// the secrets moved there, though this run could not open it.
    fn place(&self) -> Option<SecretPlace> {
        match self.shared.backend.get()? {
            Backend::Service(_) => Some(SecretPlace::Keyring),
            Backend::File if self.shared.marked.load(Ordering::Relaxed) => {
                Some(SecretPlace::Keyring)
            }
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
    #[error("the Secret Service's prompt was dismissed")]
    Dismissed,
    #[error("nobody answered the Secret Service's prompt")]
    PromptTimedOut,
    #[error("the Secret Service closed its prompt without an answer")]
    PromptClosed,
    #[error("the Secret Service did not answer in time")]
    NoAnswer,
    #[error("the Secret Service holds `{0}` as bytes that are not UTF-8")]
    NotText(String),
    #[error("`{0}` did not read back from the Secret Service as written")]
    ReadBack(String),
    #[error("the handover identity's fingerprint could not be recorded: {0}")]
    Record(BoxError),
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
        record: &FingerprintFile,
        contents: &Contents,
        ask: Ask<'_>,
    ) -> Result<usize, ServiceError> {
        let identity = HandoverIdentity::secret_key();
        let mut copied = 0;
        for (key, value) in &contents.entries {
            let key = SecretKey(key.clone());
            if key == identity
                && let Some(kept) = self.secret(&key, ask).await?
            {
                if kept != *value {
                    tracing::warn!(
                        "secrets: the Secret Service already holds another handover identity; \
                         it stays, and the file keeps its own"
                    );
                    pin_file_identity(record, value)?;
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
        let identity = HandoverIdentity::secret_key();
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
        // A create that needs a prompt answers "no object" and names the
        // item in the prompt's result (`KeePassXC` always does, and updates
        // the item it replaces in place). An item not named either way
        // may be any of `items`, so none is deleted.
        let completed = self.complete(prompt, ask).await?;
        let Some(created) = [Some(created), completed]
            .into_iter()
            .flatten()
            .find(|path| path.as_str() != NO_OBJECT)
        else {
            return Ok(());
        };
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
        self.complete(prompt, ask).await.map(drop)
    }

    /// Shows `prompt` (unless it is none) and waits for the user's answer,
    /// or dismisses it unseen when the call may not ask; the object the
    /// answer names, when it names one (a created item).
    async fn complete(
        &self,
        prompt: OwnedObjectPath,
        ask: Ask<'_>,
    ) -> Result<Option<OwnedObjectPath>, ServiceError> {
        if prompt.as_str() == NO_OBJECT {
            return Ok(None);
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
        let signal = signal.ok_or(ServiceError::PromptClosed)?;
        let answer = signal.args()?;
        if answer.dismissed {
            return Err(ServiceError::Dismissed);
        }
        Ok(OwnedObjectPath::try_from(answer.result.clone()).ok())
    }
}

/// Records the fingerprint of the file's identity, `pem`, when nothing is
/// recorded: this computer's phones paired with it, so the handover's load
/// then finds the service's identity replaced instead of adopting it. A
/// record that cannot be read, or a file identity that does not parse,
/// leaves the record as it is (the load refuses over the first, and the
/// second pins nothing).
fn pin_file_identity(record: &FingerprintFile, pem: &str) -> Result<(), ServiceError> {
    if !matches!(record.recorded(), Ok(None)) {
        return Ok(());
    }
    let Ok(identity) = HandoverIdentity::from_pem(pem) else {
        tracing::warn!("secrets: the file's handover identity does not parse; nothing recorded");
        return Ok(());
    };
    record
        .record(&steno_handover::identity::hex(&identity.fingerprint()))
        .map_err(ServiceError::Record)
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
