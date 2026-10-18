//! The Secret Service (`org.freedesktop.secrets`) on the session bus, a
//! small client over `zbus`. The secret crosses the bus in the `plain`
//! algorithm: only the user's own processes reach the session bus, and
//! they can read Steno's memory anyway. A bus that is not a `unix:` socket
//! is never used, so no secret leaves the computer.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use futures_util::StreamExt as _;
use steno_core::{
    BoxError, SecretKey, SecretPlace, SecretStore, Store, async_trait, protocols::BoundaryResult,
};
use steno_handover::identity::decision::{
    self, Caller, Found, Identities, Observations, Outcome, Phones, Reason, Write,
};
use steno_handover::{FingerprintRecord as _, HandoverIdentity};
use tokio::sync::watch;
use zbus::Connection;
use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Type, Value};

use super::{
    Bus, Contents, FileSecretStore, KEYRING_SERVICE, KeyringUnavailable, SecretsUnlocked,
    plain_name,
};
use crate::handover::FingerprintFile;

/// The Secret Service when a provider answers on the session bus, else the
/// [`FileSecretStore`] it wraps, or, once the file is marked, the secrets
/// unavailable for the run; `STENO_<KEY>` wins either way.
///
/// The choice is made once, on the store's own thread, and logged: the
/// store connects (two seconds for the connection and the session), finds
/// the default collection and unlocks it and Steno's items (the provider
/// may ask the user). Any failure leaves every secret of this process with
/// the file. A call during the choice waits for it, except while the
/// provider's prompt is on screen: then it fails at once with
/// [`KeyringUnavailable::Unlocking`], so no read waits on the user;
/// [`SecretServiceStore::unlocked_after_prompt`] says when to read again.
/// After the choice only a write asks the user.
///
/// On choosing the service the first time, the store copies the file's
/// entries into it, reads each back, and marks the file as moved, keeping
/// the entries: providers may hold a new item only in memory for a while,
/// so the entries go only on a later launch that reads every value back.
/// At that first move the API key takes the file's value; the handover
/// identity keeps the service's (phones pinned one), and the file's
/// fingerprint is recorded first so the handover reports the service's as
/// replaced (`pin_file_identity`). After the marker a write drops that
/// key's copy from the file at once.
///
/// Items are filed under `service` ([`KEYRING_SERVICE`]) and `username`
/// (the key's raw value), as the `keyring` crate files them. Where several
/// match a key, the lowest object path is read and a write removes the
/// others. A value with a line break is stored base64 behind
/// `steno-base64:`.
pub struct SecretServiceStore {
    shared: Arc<Shared>,
}

struct Shared {
    file: FileSecretStore,
    /// Where the handover identity's fingerprint is recorded.
    record: FingerprintFile,
    /// The database the paired phones are read from when the move pins the
    /// file's identity.
    phones: Option<Arc<Store>>,
    /// How long a prompt may stay unanswered.
    prompt_timeout: Duration,
    backend: OnceLock<Backend>,
    /// Whether the file carries the move's marker.
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

enum Backend {
    Service(Keyring),
    File,
}

/// How long a D-Bus call may take, libdbus's default.
const CALL_TIMEOUT: Duration = Duration::from_secs(25);
/// How long the connection and its first call (the session) may take
/// together: the method timeout does not cover the connection's set-up, so
/// a bus that never answers would hold the choice for good.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
/// How long the store waits for the user to answer the provider's prompt.
const PROMPT_TIMEOUT: Duration = Duration::from_secs(120);

impl SecretServiceStore {
    /// The Secret Service on `bus`, falling back to `file`, with the handover
    /// identity's fingerprint in `record` and the paired phones in `phones`;
    /// the choice starts now, on the store's own thread.
    #[must_use]
    pub(super) fn new(
        file: FileSecretStore,
        record: FingerprintFile,
        phones: Option<Arc<Store>>,
        bus: Bus,
    ) -> Self {
        Self::start(file, record, phones, PROMPT_TIMEOUT, move |shared| {
            shared.choose_on_own_runtime(&bus)
        })
    }

    /// The Secret Service on the bus at `address`, falling back to `file`,
    /// with no database.
    #[cfg(test)]
    pub(super) fn on_bus(
        file: FileSecretStore,
        record: FingerprintFile,
        address: &str,
        prompt_timeout: Duration,
    ) -> Self {
        Self::on_bus_with_phones(file, record, None, address, prompt_timeout)
    }

    /// [`Self::on_bus`] with the paired phones in `phones`.
    #[cfg(test)]
    pub(super) fn on_bus_with_phones(
        file: FileSecretStore,
        record: FingerprintFile,
        phones: Option<Arc<Store>>,
        address: &str,
        prompt_timeout: Duration,
    ) -> Self {
        let bus = Bus::Address(address.to_owned());
        Self::start(file, record, phones, prompt_timeout, move |shared| {
            shared.choose_on_own_runtime(&bus)
        })
    }

    /// The store, with `choose` started on the store's own thread; a
    /// `choose` that panics settles on the file.
    fn start(
        file: FileSecretStore,
        record: FingerprintFile,
        phones: Option<Arc<Store>>,
        prompt_timeout: Duration,
        choose: impl FnOnce(&Shared) -> Backend + Send + 'static,
    ) -> Self {
        let shared = Arc::new(Shared {
            file,
            record,
            phones,
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
                let backend = chooser.or_file_on_panic(choose);
                chooser.settle(backend);
            });
        if let Err(error) = spawned {
            tracing::warn!("secrets: no thread for the Secret Service ({error})");
            shared.settle(Backend::File);
        }
        SecretServiceStore { shared }
    }

    /// Resolves once the choice is made: true when a call failed with
    /// [`KeyringUnavailable::Unlocking`] on the way, so now is the moment to
    /// make it again. Every prompt counts, shown or not (`KeePassXC` may show
    /// nothing).
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

    /// The chosen backend, waiting for the choice unless it waits on the user.
    /// A call turned away is noted under the phase's lock, so the choice
    /// either is made before this look or hears of it.
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

    /// Drops `key`'s copy from the file, when it holds one.
    fn drop_file_copy(&self, key: &SecretKey) -> Result<(), ServiceError> {
        let file = &self.shared.file;
        if file.read()?.entries.contains_key(key.as_str()) {
            file.change(|contents| {
                contents.entries.remove(key.as_str());
                Ok(())
            })?;
        }
        Ok(())
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

    /// The backend `choose` gives, or the file when it panics, so the choice
    /// still settles.
    fn or_file_on_panic(&self, choose: impl FnOnce(&Self) -> Backend) -> Backend {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| choose(self))).unwrap_or_else(
            |_| {
                tracing::warn!(
                    "secrets: choosing the Secret Service panicked, {}",
                    self.fallback_note()
                );
                Backend::File
            },
        )
    }

    /// Where the secrets are when the choice falls back to the file.
    fn fallback_note(&self) -> &'static str {
        if self.file.read().is_ok_and(|contents| contents.moved) {
            "the secrets stay in the keyring, unavailable this run"
        } else {
            "keeping secrets with the file"
        }
    }

    /// [`Self::choose`] on a runtime of the calling thread's own.
    fn choose_on_own_runtime(&self, bus: &Bus) -> Backend {
        match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime.block_on(self.choose(bus)),
            Err(error) => {
                tracing::warn!("secrets: no runtime for the Secret Service ({error})");
                Backend::File
            }
        }
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
                let what = if matches!(error, ServiceError::Bus(_) | ServiceError::NotLocal) {
                    "no Secret Service"
                } else {
                    "the Secret Service could not be opened"
                };
                tracing::warn!("secrets: {what} ({error}), {}", self.fallback_note());
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
            .take_over(
                &self.file,
                &self.record,
                self.phones.as_deref(),
                &contents,
                ask,
            )
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
    /// Never asks the user. One deadline of one D-Bus call (25 s) over all of
    /// a read's calls; a read that times out fails as a locked keyring.
    async fn secret(&self, key: &SecretKey) -> BoundaryResult<Option<String>> {
        if let Some(value) = self.shared.file.environment_value(key) {
            return Ok(Some(value.to_owned()));
        }
        match self.backend().await? {
            Backend::Service(keyring) => {
                let read = tokio::time::timeout(CALL_TIMEOUT, keyring.secret(key, Ask::Never))
                    .await
                    .unwrap_or(Err(ServiceError::NoAnswer));
                Ok(read.map_err(as_locked)?)
            }
            Backend::File => self.shared.file.secret(key).await,
        }
    }

    /// `None` and an empty value remove the item; the file keeps an empty
    /// value, so the next move removes the item too. May ask the user.
    ///
    /// A write drops the key's copy the move left in the file, so a later
    /// launch never writes the old one back. A removal drops the copy first;
    /// a write drops it after the service holds the value, as the copy may be
    /// the only one the provider saved.
    async fn set_secret(&self, key: &SecretKey, value: Option<&str>) -> BoundaryResult<()> {
        match self.backend().await? {
            Backend::Service(keyring) => {
                let removal = value.is_none_or(str::is_empty);
                if removal {
                    self.drop_file_copy(key)?;
                }
                keyring
                    .set_secret(key, value, self.ask())
                    .await
                    .map_err(as_locked)?;
                if !removal {
                    self.drop_file_copy(key)?;
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

    /// `None` until the choice is made. A marked file says the keyring.
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
    #[error("the keyring's prompt was dismissed")]
    Dismissed,
    #[error("nobody answered the keyring's prompt")]
    PromptTimedOut,
    #[error("the keyring closed its prompt without an answer")]
    PromptClosed,
    #[error("the keyring did not answer in time")]
    NoAnswer,
    #[error("the session bus is not a local socket")]
    NotLocal,
    /// Holds the key's raw value; the message names it in plain words.
    #[error("the keyring holds {} in a form Steno cannot read", plain_name(.0))]
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

/// What a value with a line break is stored behind, base64 after it:
/// GNOME Keyring's unencrypted keyring file keeps each secret on one line,
/// and a line break makes the daemon reject the whole file.
const ONE_LINE_PREFIX: &str = "steno-base64:";

/// `value` as it is stored: itself, or base64 behind [`ONE_LINE_PREFIX`]
/// when it holds a line break or starts with the prefix itself.
fn one_line(value: &str) -> String {
    use base64::Engine as _;
    if value.contains(['\n', '\r']) || value.starts_with(ONE_LINE_PREFIX) {
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
        let local = match bus {
            Bus::Session => session_is_local(std::env::var("DBUS_SESSION_BUS_ADDRESS")),
            Bus::Address(address) => local_bus(address),
        };
        if !local {
            return Err(ServiceError::NotLocal);
        }
        let Connected {
            connection,
            service,
            session,
        } = connect_within(bus.clone(), CONNECT_TIMEOUT).await?;
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
    /// reads each back, then marks the file; how many it copied.
    async fn take_over(
        &self,
        file: &FileSecretStore,
        record: &FingerprintFile,
        phones: Option<&Store>,
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
                    pin_file_identity(record, value, phones, chrono::Utc::now())?;
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

    /// A later launch: removes each file entry read back from the service,
    /// writes again the ones the provider lost, and drops an API key the
    /// service replaced; how many it removed.
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
        // A create that needs a prompt names the item in the prompt's result. An
        // item not named either way may be any of `items`, so none is deleted.
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

    /// Shows `prompt` and waits for the answer, or dismisses it unseen when
    /// the call may not ask; the object the answer names, if any.
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

/// Records the fingerprint of the file's identity, `pem`, where the
/// handover identity's decision ([`decision::decide`]) pins it over the
/// phones paired in `phones`, judged at `now`, so the handover's load finds
/// the service's identity replaced instead of adopting it. Where no phone
/// can know the file's identity it records nothing. A Pair again racing the
/// move waits as replaced; it never mints.
fn pin_file_identity(
    record: &FingerprintFile,
    pem: &str,
    phones: Option<&Store>,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<(), ServiceError> {
    let identities = if let Ok(identity) = HandoverIdentity::from_pem(pem) {
        Identities::Found(vec![Found {
            fingerprint: steno_handover::identity::hex(&identity.fingerprint()),
            start: identity.not_valid_before().ok(),
        }])
    } else {
        tracing::warn!("secrets: the file's handover identity does not parse; nothing recorded");
        Identities::Malformed
    };
    let phones = match phones.map(Store::paired_devices) {
        Some(Ok(devices)) => {
            Phones::Paired(devices.iter().map(|device| device.paired_at).collect())
        }
        Some(Err(error)) => {
            tracing::warn!(%error, "secrets: the paired phones could not be read for the move");
            Phones::Unreadable
        }
        None => Phones::Unreadable,
    };
    let decided = decision::decide(&Observations {
        caller: Caller::Move,
        identities,
        record: steno_handover::identity::observe_record(record).0,
        phones,
        now,
    });
    if decided.outcome == Outcome::Wait(Reason::Replaced) {
        tracing::info!(
            "secrets: every paired phone predates the file's handover identity; nothing recorded"
        );
    }
    for write in decided.writes {
        match write {
            Write::Record(fingerprint) => {
                record.record(&fingerprint).map_err(ServiceError::Record)?;
            }
            // The move's decision writes nothing else.
            Write::DropNote | Write::RemovePhones => {}
        }
    }
    Ok(())
}

/// A call's failure after the choice as the user reads it: a bus error or
/// a provider that stopped answering reads as a locked keyring; the bus's
/// error goes to the log.
fn as_locked(error: ServiceError) -> ServiceError {
    match error {
        ServiceError::Bus(_) | ServiceError::NoAnswer => {
            tracing::info!("secrets: a keyring call failed ({error}); it reads as locked");
            KeyringUnavailable::Locked.into()
        }
        error => error,
    }
}

/// A connection with an open `plain` session.
struct Connected {
    connection: Connection,
    service: ServiceProxy<'static>,
    session: OwnedObjectPath,
}

/// [`connect`] on a thread of its own, waited for `patience` at most, then
/// [`ServiceError::NoAnswer`]. The thread holds nothing of the store's, so
/// one the bus never answers waits there alone.
async fn connect_within(bus: Bus, patience: Duration) -> Result<Connected, ServiceError> {
    let deadline = Instant::now() + patience;
    let (done, outcome) = tokio::sync::oneshot::channel();
    std::thread::Builder::new()
        .name("steno-secrets-connect".to_owned())
        .spawn(move || {
            let connected = match tokio::runtime::Builder::new_current_thread().build() {
                Ok(runtime) => runtime.block_on(connect(&bus, deadline)),
                Err(error) => Err(zbus::Error::InputOutput(Arc::new(error)).into()),
            };
            let _ = done.send(connected);
        })
        .map_err(|error| zbus::Error::InputOutput(Arc::new(error)))?;
    tokio::time::timeout(patience, outcome)
        .await
        .map_or(Err(ServiceError::NoAnswer), |sent| {
            sent.unwrap_or(Err(ServiceError::NoAnswer))
        })
}

/// Connects to `bus` and opens a `plain` session, unless the connection
/// came after `deadline`.
async fn connect(bus: &Bus, deadline: Instant) -> Result<Connected, ServiceError> {
    let builder = match bus {
        Bus::Session => zbus::connection::Builder::session()?,
        Bus::Address(address) => zbus::connection::Builder::address(address.as_str())?,
    };
    let connection = builder.method_timeout(CALL_TIMEOUT).build().await?;
    if Instant::now() >= deadline {
        return Err(ServiceError::NoAnswer);
    }
    let service = ServiceProxy::new(&connection).await?;
    let (_, session) = service.open_session("plain", &Value::from("")).await?;
    Ok(Connected {
        connection,
        service,
        session,
    })
}

/// Whether the session bus at `address` is local. Unset, `zbus` uses the
/// socket in `XDG_RUNTIME_DIR`, which is.
fn session_is_local(address: Result<String, std::env::VarError>) -> bool {
    address.map_or(true, |address| local_bus(&address))
}

/// Whether every address in a D-Bus address list is a `unix:` socket, so
/// the `plain` session's secrets stay on this computer.
fn local_bus(address: &str) -> bool {
    address
        .split(';')
        .filter(|entry| !entry.is_empty())
        .all(|entry| entry.starts_with("unix:"))
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
