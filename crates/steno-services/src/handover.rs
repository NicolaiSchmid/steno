//! The host's `Handover` over the phone handover listener
//! ([`ListenerHandover`], and [`GatedHandover`] behind the Swift import's
//! gate), where the identity is read and made anew ([`IdentitySource`]),
//! the file the identity's fingerprint is recorded in ([`FingerprintFile`]),
//! and the listener's port ([`listener_port`], [`PORT_VARIABLE`]; Rust
//! only). Swift: the handover block of `AppEnvironment.live` in
//! `apps/macos/Steno/AppEnvironment.swift`.
//!
//! While the identity the paired phones know cannot be read, or the guard
//! refuses it, the handover waits with the reason ([`Handover::waiting`])
//! and never mints over it; Try again reads it again and Pair again makes
//! a new one, the import's marker written first (stable plan S6).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError, TryLockError};

use steno_bridge::PhoneWaitingReason;
use steno_core::protocols::BoundaryResult;
use steno_core::{HandoverIntake, HandoverReceipt, PairedDevice, SecretStore, Store};
use steno_handover::{
    FingerprintRecord, HandoverConfiguration, HandoverIdentity, HandoverService, IdentityError,
    Unavailability,
};
use steno_host::services::{Handover, ListenerState, PairingCode};
use uuid::Uuid;

use crate::block_on;
use crate::files::{Access, replace_file};
use crate::secrets::KeyringUnavailable;
use crate::swift_import::{HandoverGate, ImportMarker};

/// The handover identity's fingerprint in `handover-identity.json` under
/// the support directory, as `{"steno.handoverIdentityFingerprint": "<hex>"}`,
/// replaced atomically. Not `preferences.json`: that file holds only
/// flags, is rewritten in place, and a build that reads it as flags would
/// drop every flag over a text value. The Swift app never reads or writes
/// this file, so a rollback to it leaves the record alone. No Swift
/// counterpart.
#[derive(Debug, Clone)]
pub struct FingerprintFile {
    path: PathBuf,
}

impl FingerprintFile {
    /// The key the fingerprint is filed under.
    pub const KEY: &'static str = "steno.handoverIdentityFingerprint";

    /// The record under `support_directory`.
    #[must_use]
    pub fn in_support_directory(support_directory: &Path) -> Self {
        FingerprintFile {
            path: support_directory.join("handover-identity.json"),
        }
    }
}

impl FingerprintRecord for FingerprintFile {
    /// A missing file records nothing; one that does not parse is an
    /// error, never "nothing recorded".
    fn recorded(&self) -> BoundaryResult<Option<String>> {
        let bytes = match std::fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(Box::new(error)),
        };
        let mut values: BTreeMap<String, String> = serde_json::from_slice(&bytes)?;
        Ok(values.remove(Self::KEY))
    }

    fn record(&self, fingerprint: &str) -> BoundaryResult<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let values = BTreeMap::from([(Self::KEY, fingerprint)]);
        replace_file(
            &self.path,
            &serde_json::to_vec_pretty(&values)?,
            Access::Default,
        )?;
        Ok(())
    }

    fn forget(&self) -> BoundaryResult<()> {
        match std::fs::remove_file(&self.path) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(Box::new(error)),
            _ => Ok(()),
        }
    }
}

/// The variable that sets the handover's port, over the stored setting:
/// the NixOS module sets it to the port it opens in the firewall.
pub const PORT_VARIABLE: &str = "STENO_HANDOVER_PORT";

/// The port the app's listener binds (stable plan X4): [`PORT_VARIABLE`]
/// from the environment, else the stored [`Settings::handover_port`], else
/// [`HandoverConfiguration::platform_port`]. An empty variable counts as
/// unset; one that is not a port is ignored with a warning. Rust only.
///
/// [`Settings::handover_port`]: steno_core::Settings::handover_port
#[must_use]
pub fn listener_port(environment: Option<&str>, stored: Option<u16>) -> u16 {
    let from_environment = environment
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .and_then(|value| {
            let port = value.parse().ok();
            if port.is_none() {
                tracing::warn!("{PORT_VARIABLE} is not a port from 0 to 65535, so it is ignored");
            }
            port
        });
    from_environment
        .or(stored)
        .unwrap_or(HandoverConfiguration::platform_port())
}

/// The stored [`Settings::handover_port`]; `None` when there is none or
/// the settings cannot be read, with a warning. Rust only.
///
/// [`Settings::handover_port`]: steno_core::Settings::handover_port
pub(crate) fn stored_port(store: &Store) -> Option<u16> {
    store
        .settings()
        .inspect_err(|error| {
            tracing::warn!("the settings could not be read, so a stored handoverPort is ignored");
            tracing::debug!(%error, "settings for the handover's port");
        })
        .ok()
        .and_then(|settings| settings.handover_port)
}

/// The default listener configuration on the port [`listener_port`] picks
/// from `environment` (the value of [`PORT_VARIABLE`]) and [`stored_port`].
pub(crate) fn configuration(store: &Store, environment: Option<&str>) -> HandoverConfiguration {
    HandoverConfiguration {
        port: listener_port(environment, stored_port(store)),
        ..HandoverConfiguration::default()
    }
}

/// The listener over the store and the recording intake.
pub fn service(
    configuration: HandoverConfiguration,
    store: Arc<Store>,
    intake: Arc<dyn HandoverIntake>,
    identity: HandoverIdentity,
) -> HandoverService {
    HandoverService::with_wall_clock(configuration, store, intake, Arc::new(identity))
}

/// Starts `handover`'s listener when a phone is paired.
pub(crate) async fn start_if_paired(handover: &HandoverService) {
    let paired = handover.paired_devices().await.unwrap_or_default();
    if !paired.is_empty()
        && let Err(error) = handover.start().await
    {
        tracing::warn!(%error, "handover listener did not start");
    }
}

/// Builds the listener over a loaded or minted identity (the intake, the
/// configuration and the service; `App`'s build supplies it).
pub type Listen = Box<dyn Fn(HandoverIdentity) -> Arc<HandoverService> + Send + Sync>;

/// Where a handover reads its identity and makes a new one: the secret
/// store, the fingerprint record, the database's paired phones, the
/// import's marker Pair again writes first, and the listener's build.
/// Every read and every mint of the identity in the app goes through
/// here. Rust only.
pub struct IdentitySource {
    pub store: Arc<Store>,
    pub secrets: Arc<dyn SecretStore>,
    pub record: Arc<dyn FingerprintRecord>,
    /// Where Pair again writes the import's `swift-import-done` marker
    /// before it mints (the Mac's login keychain); `None` where no import
    /// can run.
    pub marker: Option<Arc<dyn ImportMarker>>,
    /// The new identity's common name (`Steno on <computer name>`).
    pub common_name: String,
    pub listen: Listen,
    pub runtime: tokio::runtime::Handle,
}

impl IdentitySource {
    /// The listener over the stored identity, or over one minted where
    /// the guard allows ([`HandoverIdentity::load_or_create`]), with the
    /// identity's computer id.
    pub fn read(&self) -> Result<(Arc<HandoverService>, Uuid), IdentityError> {
        let identity = block_on(
            &self.runtime,
            HandoverIdentity::load_or_create(
                self.secrets.as_ref(),
                self.record.as_ref(),
                &self.store,
                &self.common_name,
                chrono::Utc::now(),
            ),
        )?;
        Ok(self.listener_over(identity))
    }

    /// Pair again: the import's marker first, so a later launch never
    /// brings the Swift identity back over the new one; a marker that
    /// cannot be written stops here, before anything is removed. Then
    /// [`Self::mint_anew`] and the listener over the new identity.
    pub fn pair_again(&self) -> Result<(Arc<HandoverService>, Uuid), String> {
        if let Some(marker) = &self.marker {
            marker
                .mark_import_done()
                .map_err(|error| marker_not_written(&error))?;
        }
        let identity = self.mint_anew()?;
        Ok(self.listener_over(identity))
    }

    /// Pair again's mint alone ([`HandoverIdentity::pair_again`]): the
    /// paired phones and the recorded fingerprint go, a new identity is
    /// stored. Whoever calls it writes the marker first.
    pub fn mint_anew(&self) -> Result<HandoverIdentity, String> {
        block_on(
            &self.runtime,
            HandoverIdentity::pair_again(
                self.secrets.as_ref(),
                self.record.as_ref(),
                &self.store,
                &self.common_name,
                chrono::Utc::now(),
            ),
        )
        .map_err(|error| identity_failure(&error))
    }

    fn listener_over(&self, identity: HandoverIdentity) -> (Arc<HandoverService>, Uuid) {
        let mac_id = identity.mac_id();
        ((self.listen)(identity), mac_id)
    }
}

/// Pair again's answer when the marker could not be written.
pub(crate) fn marker_not_written(error: &str) -> String {
    format!("Steno made no new pairing, as it could not note that the earlier one is gone: {error}")
}

/// Why the identity is not available, for the Phones settings' details:
/// the keyring's own sentence when it was not opened at start, as that one
/// already names the pairing.
pub(crate) fn identity_failure(error: &IdentityError) -> String {
    match keyring_failure(error) {
        Some(not_opened @ KeyringUnavailable::NotOpened(_)) => not_opened.to_string(),
        _ => error.to_string(),
    }
}

/// The keyring's reason when the identity could not be read from it.
fn keyring_failure(error: &IdentityError) -> Option<&KeyringUnavailable> {
    match error {
        IdentityError::Unavailable(Unavailability::Unreadable(source)) => source.downcast_ref(),
        _ => None,
    }
}

/// What the iPhone section says the handover waits for, by why the
/// identity was not available: a stored identity that does not parse, a
/// record that does not, and a write that failed read as "could not be
/// read" too, as Try again may get past each.
fn wait_reason(error: &IdentityError) -> PhoneWaitingReason {
    match error {
        IdentityError::Unavailable(Unavailability::Missing) => PhoneWaitingReason::Missing,
        IdentityError::Unavailable(Unavailability::Replaced) => PhoneWaitingReason::Replaced,
        _ => PhoneWaitingReason::Unreadable,
    }
}

/// The host's `Handover` over the listener; blocks on the runtime for the
/// few async calls (paired devices, start, stop, revoke).
///
/// The listener is there from the start, or comes later: while the
/// identity cannot be read or the guard refuses it
/// ([`HandoverIdentity::load_or_create`]), the handover waits
/// ([`Handover::waiting`]), mints nothing, and reads the identity again
/// at Try again ([`Handover::try_again`]) or once a keyring that was
/// asking the user answered (`App::launch`); Pair again
/// ([`Handover::pair_again`]) makes a new identity. Until then the
/// handover reads as failed with the reason, the paired phones come from
/// the database, and every other call that needs the listener fails or
/// does nothing. Once the app shuts down ([`ListenerHandover::close`]) no
/// listener is handed over.
pub struct ListenerHandover {
    listener: OnceLock<(Arc<HandoverService>, Uuid)>,
    /// Why there is no listener yet, and whether the app shut down; `set`
    /// and `close` hold it throughout.
    waiting: Mutex<Waiting>,
    /// Where Try again and Pair again read and mint; `None` for a
    /// handover made over a listener alone.
    identity: Option<Arc<IdentitySource>>,
    /// Held by a read or a mint, one at a time.
    attempt: Mutex<()>,
    store: Arc<Store>,
    runtime: tokio::runtime::Handle,
}

struct Waiting {
    /// The failure the listener state shows.
    reason: String,
    /// Why the handover waits, for the iPhone section; `None` with a
    /// listener.
    wait: Option<PhoneWaitingReason>,
    closed: bool,
}

/// What Try again and Pair again answer while another one runs.
const ATTEMPT_UNDER_WAY: &str = "Steno is already reading this computer's phone pairing.";
/// What Pair again answers once the listener is there.
const NOT_WAITING: &str = "Phone handover is not waiting for a new pairing.";
/// What Try again and Pair again answer once the app shuts down.
const SHUT_DOWN: &str = "Steno is quitting.";

impl ListenerHandover {
    /// The handover over `service`, whose identity's computer id is
    /// `mac_id`.
    #[must_use]
    pub fn over(
        service: Arc<HandoverService>,
        mac_id: Uuid,
        store: Arc<Store>,
        runtime: tokio::runtime::Handle,
    ) -> Self {
        let handover = Self::empty(String::new(), None, None, store, runtime);
        let _ = handover.listener.set((service, mac_id));
        handover
    }

    /// The handover over the identity `source` reads: its listener when
    /// the read succeeds, else one that waits with the reason and reads
    /// again at Try again. Logs why it waits.
    #[must_use]
    pub fn read(source: Arc<IdentitySource>) -> Self {
        let (store, runtime) = (source.store.clone(), source.runtime.clone());
        let read = source.read();
        let handover = Self::empty(String::new(), None, Some(source), store, runtime);
        match read {
            Ok((service, mac_id)) => {
                let _ = handover.listener.set((service, mac_id));
            }
            Err(error) => {
                tracing::warn!(%error, "phone handover waits for its identity");
                handover.wait_for(&error);
            }
        }
        handover
    }

    /// A handover over `store` without its listener, for `reason`, and no
    /// identity to read again.
    #[must_use]
    pub fn waiting(reason: String, store: Arc<Store>, runtime: tokio::runtime::Handle) -> Self {
        Self::empty(reason, None, None, store, runtime)
    }

    fn empty(
        reason: String,
        wait: Option<PhoneWaitingReason>,
        identity: Option<Arc<IdentitySource>>,
        store: Arc<Store>,
        runtime: tokio::runtime::Handle,
    ) -> Self {
        ListenerHandover {
            listener: OnceLock::new(),
            waiting: Mutex::new(Waiting {
                reason,
                wait,
                closed: false,
            }),
            identity,
            attempt: Mutex::new(()),
            store,
            runtime,
        }
    }

    /// The listener, once there.
    #[must_use]
    pub fn listener(&self) -> Option<&Arc<HandoverService>> {
        self.listener.get().map(|(service, _)| service)
    }

    /// Hands over the listener a waiting handover lacked and starts it
    /// when a phone is paired; false (and `service` unused) when it has one
    /// or the app shut down. The start runs under the lock `close` takes,
    /// so a shutdown either stops the started listener or comes first and
    /// keeps it from starting.
    pub fn set(&self, service: Arc<HandoverService>, mac_id: Uuid) -> bool {
        let mut waiting = self.lock();
        if waiting.closed || self.listener.set((service, mac_id)).is_err() {
            return false;
        }
        waiting.wait = None;
        self.start_if_paired();
        true
    }

    /// Starts the listener when a phone is paired, blocking on the runtime.
    pub(crate) fn start_if_paired(&self) {
        if let Some(service) = self.listener() {
            block_on(&self.runtime, start_if_paired(service));
        }
    }

    /// The app shuts down: no listener is handed over from now on; the one
    /// there, to stop.
    pub fn close(&self) -> Option<&Arc<HandoverService>> {
        self.lock().closed = true;
        self.listener()
    }

    /// Reads the identity again while the handover waits and hands the
    /// listener over when the read succeeds; the failure's reason, kept
    /// for the iPhone section, when it does not. Nothing to do with a
    /// listener, or while another read or mint runs. Try again, and the
    /// launch's reread once a keyring that was asking the user answered.
    pub fn read_again(&self) -> Result<(), String> {
        if self.listener().is_some() {
            return Ok(());
        }
        let source = self.identity.as_ref().ok_or_else(|| self.unavailable())?;
        let Some(_attempt) = self.one_at_a_time() else {
            return Ok(());
        };
        match source.read() {
            Ok((service, mac_id)) => {
                if self.set(service, mac_id) || self.listener().is_some() {
                    Ok(())
                } else {
                    Err(SHUT_DOWN.to_owned())
                }
            }
            Err(error) => {
                self.wait_for(&error);
                Err(identity_failure(&error))
            }
        }
    }

    /// Why the identity is not available, as the listener state and the
    /// iPhone section show it.
    fn wait_for(&self, error: &IdentityError) {
        let mut waiting = self.lock();
        waiting.reason = identity_failure(error);
        waiting.wait = Some(wait_reason(error));
    }

    /// Held for a read or a mint; `None` while another one runs.
    fn one_at_a_time(&self) -> Option<MutexGuard<'_, ()>> {
        match self.attempt.try_lock() {
            Ok(guard) => Some(guard),
            Err(TryLockError::Poisoned(poisoned)) => Some(poisoned.into_inner()),
            Err(TryLockError::WouldBlock) => None,
        }
    }

    fn lock(&self) -> MutexGuard<'_, Waiting> {
        self.waiting.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn unavailable(&self) -> String {
        format!("Phone handover is unavailable: {}", self.lock().reason)
    }
}

impl Handover for ListenerHandover {
    fn state(&self) -> ListenerState {
        let Some(service) = self.listener() else {
            return ListenerState::Failed(self.unavailable());
        };
        match service.state() {
            steno_handover::ListenerState::Stopped => ListenerState::Stopped,
            steno_handover::ListenerState::Listening { port } => ListenerState::Listening(port),
            steno_handover::ListenerState::Failed(reason) => ListenerState::Failed(reason),
        }
    }

    /// Empty until the listener is there.
    fn mac_id(&self) -> String {
        self.listener
            .get()
            .map(|(_, mac_id)| steno_core::json::uuid_string(*mac_id))
            .unwrap_or_default()
    }

    fn paired_devices(&self) -> BoundaryResult<Vec<PairedDevice>> {
        match self.listener() {
            Some(service) => Ok(block_on(&self.runtime, service.paired_devices())?),
            None => Ok(self.store.paired_devices()?),
        }
    }

    fn start(&self) -> BoundaryResult<()> {
        let service = self.listener().ok_or_else(|| self.unavailable())?;
        Ok(block_on(&self.runtime, service.start())?)
    }

    fn stop(&self) {
        if let Some(service) = self.listener() {
            block_on(&self.runtime, service.stop());
        }
    }

    /// Without the listener (which the host never meets, as it opens a
    /// pairing only after `start` succeeded), a code that has run out.
    fn begin_pairing(&self) -> PairingCode {
        let Some(service) = self.listener() else {
            return PairingCode {
                expires_at: chrono::DateTime::UNIX_EPOCH,
                url_string: String::new(),
            };
        };
        let payload = service.begin_pairing();
        PairingCode {
            expires_at: payload.expires_at,
            url_string: payload.url_string(),
        }
    }

    fn cancel_pairing(&self) {
        if let Some(service) = self.listener() {
            service.cancel_pairing();
        }
    }

    fn revoke(&self, device_id: Uuid) -> BoundaryResult<()> {
        let service = self.listener().ok_or_else(|| self.unavailable())?;
        Ok(block_on(&self.runtime, service.revoke(device_id))?)
    }

    fn receipts(&self) -> Vec<HandoverReceipt> {
        self.listener()
            .map(|service| service.receipts().borrow().clone())
            .unwrap_or_default()
    }

    fn waiting(&self) -> Option<PhoneWaitingReason> {
        if self.listener().is_some() {
            return None;
        }
        self.lock().wait
    }

    fn try_again(&self) -> BoundaryResult<()> {
        Ok(self.read_again()?)
    }

    /// Only while the handover waits, and never after the shutdown: the
    /// marker, the mint ([`IdentitySource::pair_again`]), then the
    /// listener over the new identity.
    fn pair_again(&self) -> BoundaryResult<()> {
        if self.listener().is_some() {
            return Err(NOT_WAITING.into());
        }
        let source = self.identity.as_ref().ok_or_else(|| self.unavailable())?;
        let _attempt = self.one_at_a_time().ok_or(ATTEMPT_UNDER_WAY)?;
        if self.lock().closed {
            return Err(SHUT_DOWN.into());
        }
        let (service, mac_id) = source.pair_again()?;
        self.set(service, mac_id);
        Ok(())
    }
}

/// What `start` and `revoke` answer while the import waits, before a
/// listener build failed.
pub const WAITING_FOR_IMPORT: &str = "Phones can upload again once Steno has brought over this Mac's phone pairing from the previous version. This happens in the welcome step when Steno next starts.";

/// The Swift import's step as the gated handover drives it while the
/// import's gate waits (`crate::swift_import::ImportStep`).
pub trait WaitingImport: Send + Sync {
    /// Try again: the step runs again (the export and its prompt); its line
    /// when the identity still did not come over.
    fn run_again(&self) -> Result<(), String>;
    /// Pair again ends the import: the marker first (one that cannot be
    /// written stops it), then `mint`, then the gate opens.
    fn pair_again(&self, mint: &dyn Fn() -> Result<(), String>) -> Result<(), String>;
}

/// Pair again's mint while the import waits ([`IdentitySource::mint_anew`]).
pub type MintAnew = Box<dyn Fn() -> Result<(), String> + Send + Sync>;

/// The host's `Handover` while the Swift import is pending
/// (`crate::swift_import`): no listener and no identity read until the
/// gate opens, then the listener built once the gate opens, which every
/// call goes to. Until then the paired phones come from the store, the
/// listener reads as stopped, and starting it or revoking a phone says
/// that it waits. While the gate waits after the step, Try again runs the
/// step again and Pair again ends the import with a new identity
/// ([`WaitingImport`]). Once the app shuts down ([`GatedHandover::close`])
/// no listener is adopted.
pub struct GatedHandover {
    store: Arc<Store>,
    gate: tokio::sync::watch::Receiver<HandoverGate>,
    make: MakeListener,
    import: Arc<dyn WaitingImport>,
    mint: MintAnew,
    listener: std::sync::OnceLock<Arc<ListenerHandover>>,
    /// Why the last build failed, until one succeeds.
    failure: tokio::sync::watch::Sender<Option<String>>,
    /// Try again after a failed build: `follow` builds again.
    retry: tokio::sync::Notify,
    /// Set once the app shut down; held while a built listener is adopted
    /// and started.
    closed: Mutex<bool>,
}

/// Builds the listener once the gate opened: loads the identity the
/// import stored, as the graph's build does without an import.
pub type MakeListener = Box<dyn Fn() -> Result<Arc<ListenerHandover>, String> + Send + Sync>;

impl GatedHandover {
    #[must_use]
    pub fn new(
        store: Arc<Store>,
        gate: tokio::sync::watch::Receiver<HandoverGate>,
        make: MakeListener,
        import: Arc<dyn WaitingImport>,
        mint: MintAnew,
    ) -> Self {
        GatedHandover {
            store,
            gate,
            make,
            import,
            mint,
            listener: std::sync::OnceLock::new(),
            failure: tokio::sync::watch::Sender::new(None),
            retry: tokio::sync::Notify::new(),
            closed: Mutex::new(false),
        }
    }

    /// Why the last build of the listener failed (the store's checkpoint),
    /// `None` before the first build and once one succeeded.
    #[must_use]
    pub fn failure(&self) -> tokio::sync::watch::Receiver<Option<String>> {
        self.failure.subscribe()
    }

    /// Waits for the gate to say [`HandoverGate::Ready`], then builds the
    /// listener, starts it when a phone is paired (as the launch does) and
    /// calls `changed`. Never builds a listener while the gate waits, so
    /// no identity is read or minted before the import put one in place.
    /// A build that fails waits for the gate to say ready again, or for Try
    /// again, and builds again. Returns once the listener is adopted (it
    /// may itself wait for its identity, [`ListenerHandover::read`]), when
    /// the gate is gone, or after the shutdown, which adopts nothing and
    /// calls no `changed`.
    pub async fn follow(self: Arc<Self>, changed: impl FnOnce() + Send + 'static) {
        let mut gate = self.gate.clone();
        loop {
            // `wait_for` marks the value it accepted as seen, so the
            // `changed` below waits for the next one.
            if gate.wait_for(|gate| gate.opens_listener()).await.is_err() {
                return;
            }
            let this = self.clone();
            let built = tokio::task::spawn_blocking(move || this.build_and_adopt()).await;
            match built.unwrap_or_else(|error| Err(error.to_string())) {
                Ok(true) => break,
                Ok(false) => return,
                Err(error) => {
                    tracing::warn!(%error, "phone handover is unavailable");
                    self.failure.send_replace(Some(error));
                    tokio::select! {
                        changed = gate.changed() => if changed.is_err() {
                            return;
                        },
                        () = self.retry.notified() => {}
                    }
                }
            }
        }
        self.failure.send_replace(None);
        changed();
    }

    /// Builds the listener, then adopts it and starts it when a phone is
    /// paired, under the latch [`Self::close`] takes: `false`, with nothing
    /// adopted or started, once the app shut down.
    fn build_and_adopt(&self) -> Result<bool, String> {
        let listener = (self.make)()?;
        let closed = self.lock_closed();
        if *closed {
            listener.close();
            return Ok(false);
        }
        let _ = self.listener.set(listener.clone());
        listener.start_if_paired();
        Ok(true)
    }

    /// The app shuts down: no listener is adopted from now on, and the
    /// adopted one hands over none; the listener there, to stop.
    pub fn close(&self) -> Option<Arc<HandoverService>> {
        *self.lock_closed() = true;
        self.listener
            .get()
            .and_then(|listener| listener.close().cloned())
    }

    /// The listener's service, once open.
    #[must_use]
    pub fn service(&self) -> Option<Arc<HandoverService>> {
        self.listener
            .get()
            .and_then(|listener| listener.listener().cloned())
    }

    fn lock_closed(&self) -> MutexGuard<'_, bool> {
        self.closed.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// What `start` and `revoke` answer without a listener: why the last
    /// build failed, once the gate opened and the build failed (the
    /// import is over by then), else that the handover waits for the
    /// import.
    fn waiting_error(&self) -> steno_core::BoxError {
        self.failure
            .borrow()
            .clone()
            .unwrap_or_else(|| WAITING_FOR_IMPORT.to_owned())
            .into()
    }
}

impl Handover for GatedHandover {
    fn state(&self) -> ListenerState {
        self.listener
            .get()
            .map_or(ListenerState::Stopped, |listener| listener.state())
    }

    fn mac_id(&self) -> String {
        self.listener
            .get()
            .map(|listener| listener.mac_id())
            .unwrap_or_default()
    }

    fn paired_devices(&self) -> BoundaryResult<Vec<PairedDevice>> {
        match self.listener.get() {
            Some(listener) => listener.paired_devices(),
            None => Ok(self.store.paired_devices()?),
        }
    }

    fn start(&self) -> BoundaryResult<()> {
        self.listener
            .get()
            .ok_or_else(|| self.waiting_error())?
            .start()
    }

    fn stop(&self) {
        if let Some(listener) = self.listener.get() {
            listener.stop();
        }
    }

    fn begin_pairing(&self) -> PairingCode {
        match self.listener.get() {
            Some(listener) => listener.begin_pairing(),
            // Unreachable from the Phones section, which starts the
            // listener first: a code that has already run out.
            None => PairingCode {
                expires_at: chrono::DateTime::UNIX_EPOCH,
                url_string: String::new(),
            },
        }
    }

    fn cancel_pairing(&self) {
        if let Some(listener) = self.listener.get() {
            listener.cancel_pairing();
        }
    }

    fn revoke(&self, device_id: Uuid) -> BoundaryResult<()> {
        self.listener
            .get()
            .ok_or_else(|| self.waiting_error())?
            .revoke(device_id)
    }

    fn receipts(&self) -> Vec<HandoverReceipt> {
        self.listener
            .get()
            .map(|listener| listener.receipts())
            .unwrap_or_default()
    }

    /// The adopted listener's reason; before one, the import's while its
    /// gate waits after the step. Not while the step is pending: the
    /// onboarding window shows it.
    fn waiting(&self) -> Option<PhoneWaitingReason> {
        if let Some(listener) = self.listener.get() {
            return listener.waiting();
        }
        match *self.gate.borrow() {
            HandoverGate::Waiting(reason) => Some(reason.into()),
            HandoverGate::Pending | HandoverGate::Ready => None,
        }
    }

    /// The adopted listener reads its identity again, so a listener left
    /// waiting on the keyring gets past an unlock; before one, while the
    /// gate waits, the step runs again, and once the gate opened and a
    /// build failed, `follow` builds again.
    fn try_again(&self) -> BoundaryResult<()> {
        if let Some(listener) = self.listener.get() {
            return listener.try_again();
        }
        let gate = *self.gate.borrow();
        match gate {
            HandoverGate::Waiting(_) => Ok(self.import.run_again()?),
            HandoverGate::Pending => Err(WAITING_FOR_IMPORT.into()),
            HandoverGate::Ready => {
                self.retry.notify_one();
                Ok(())
            }
        }
    }

    /// The adopted listener's Pair again; before one, while the gate
    /// waits, the import's ([`WaitingImport::pair_again`]), whose open gate
    /// brings the listener over the new identity.
    fn pair_again(&self) -> BoundaryResult<()> {
        if let Some(listener) = self.listener.get() {
            return listener.pair_again();
        }
        if *self.lock_closed() {
            return Err(SHUT_DOWN.into());
        }
        let gate = *self.gate.borrow();
        match gate {
            HandoverGate::Waiting(_) => Ok(self.import.pair_again(&*self.mint)?),
            HandoverGate::Pending | HandoverGate::Ready => Err(NOT_WAITING.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use steno_core::testing::FakeHandoverIntake;

    use super::*;
    use crate::swift_import::WaitReason;
    use crate::testing::warnings;

    #[test]
    fn the_port_is_the_environment_s_else_the_stored_one_else_the_platform_s() {
        let platform = HandoverConfiguration::platform_port();
        assert_eq!(listener_port(None, None), platform);
        assert_eq!(listener_port(None, Some(40000)), 40000);
        assert_eq!(listener_port(None, Some(0)), 0, "the system chooses");
        assert_eq!(listener_port(Some("23900"), Some(40000)), 23900);
        assert_eq!(listener_port(Some(" 23900\n"), None), 23900);
        assert_eq!(listener_port(Some("0"), None), 0);
        assert_eq!(listener_port(Some(""), Some(40000)), 40000, "unset");
        assert_eq!(listener_port(Some("65536"), Some(40000)), 40000);
        assert_eq!(listener_port(Some("steno"), None), platform);
    }

    /// An empty variable is unset, not a value that is not a port.
    #[test]
    fn only_a_variable_that_is_not_a_port_logs_a_warning() {
        let platform = HandoverConfiguration::platform_port();
        let ((), logged) = warnings(|| {
            assert_eq!(listener_port(Some(""), None), platform);
            assert_eq!(listener_port(Some(" \n"), None), platform);
            assert_eq!(listener_port(Some("23900"), None), 23900);
        });
        assert_eq!(logged, "");
        let ((), logged) = warnings(|| assert_eq!(listener_port(Some("steno"), None), platform));
        assert!(
            logged.contains("STENO_HANDOVER_PORT is not a port from 0 to 65535, so it is ignored"),
            "{logged}"
        );
    }

    #[test]
    fn the_variable_and_the_stored_port_reach_the_configuration() {
        let platform = HandoverConfiguration::platform_port();
        let store = Store::in_memory().unwrap();
        assert_eq!(stored_port(&store), None);
        assert_eq!(configuration(&store, None).port, platform);

        let mut settings = store.settings().unwrap();
        settings.handover_port = Some(40000);
        store.save_settings(&settings).unwrap();
        assert_eq!(stored_port(&store), Some(40000));
        assert_eq!(configuration(&store, None).port, 40000);
        assert_eq!(configuration(&store, Some("")).port, 40000);
        assert_eq!(
            configuration(&store, Some("23900")),
            HandoverConfiguration {
                port: 23900,
                ..HandoverConfiguration::default()
            },
            "the default otherwise"
        );
    }

    /// Settings that cannot be read leave the stored port out.
    #[test]
    fn settings_that_cannot_be_read_leave_the_stored_port_out() {
        let store = Store::in_memory().unwrap();
        store
            .write(|transaction| {
                transaction.execute(
                    "INSERT INTO setting (key, value) VALUES ('handoverPort', '40000'), ('launchAtLogin', 'yes')",
                    [],
                )?;
                Ok(())
            })
            .unwrap();
        assert!(store.settings().is_err());
        assert_eq!(stored_port(&store), None);
        assert_eq!(
            configuration(&store, None).port,
            HandoverConfiguration::platform_port()
        );
    }

    #[test]
    fn the_fingerprint_file_records_and_reads_back() {
        let dir = tempfile::tempdir().unwrap();
        let file = FingerprintFile::in_support_directory(&dir.path().join("support"));
        assert_eq!(file.recorded().unwrap(), None);
        file.record("ab12").unwrap();
        file.record("cd34").unwrap();
        assert_eq!(file.recorded().unwrap().as_deref(), Some("cd34"));
        let text =
            std::fs::read_to_string(dir.path().join("support/handover-identity.json")).unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&text).unwrap(),
            serde_json::json!({ "steno.handoverIdentityFingerprint": "cd34" })
        );
        std::fs::write(dir.path().join("support/handover-identity.json"), b"{").unwrap();
        assert!(file.recorded().is_err(), "a damaged record is no empty one");
    }

    /// Only a gate that says ready opens the listener.
    #[test]
    fn only_a_ready_gate_opens_the_listener() {
        assert!(HandoverGate::Ready.opens_listener());
        assert!(!HandoverGate::Pending.opens_listener());
        assert!(!HandoverGate::Waiting(WaitReason::ImportDenied).opens_listener());
    }

    /// A loopback-free test listener: unadvertised, on a port the system
    /// chooses, its inbox in `dir`.
    fn test_service(store: &Arc<Store>, dir: &Path, identity: HandoverIdentity) -> HandoverService {
        service(
            HandoverConfiguration {
                advertise: false,
                inbox_directory: dir.join("inbox"),
                ..HandoverConfiguration::default()
            },
            store.clone(),
            Arc::new(FakeHandoverIntake::default()),
            identity,
        )
    }

    /// A build that fails is tried again at the next ready, and at Try
    /// again, not dropped: whatever puts the identity in place opens the
    /// listener by setting the gate once more, and Try again builds again
    /// after a failure that was not the import's. Until then starting the
    /// listener or revoking a phone answers why the build failed, not that
    /// the import waits.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_failed_build_is_tried_again_at_the_next_ready_and_at_try_again() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::in_memory().unwrap());
        let (gate, receiver) = tokio::sync::watch::channel(HandoverGate::Pending);
        let (called, mut calls) = tokio::sync::mpsc::unbounded_channel();
        let attempts = AtomicUsize::new(0);
        let runtime = tokio::runtime::Handle::current();
        let (listener_store, inbox) = (store.clone(), dir.path().to_path_buf());
        let gated = Arc::new(GatedHandover::new(
            store,
            receiver,
            Box::new(move || {
                let attempt = attempts.fetch_add(1, Ordering::SeqCst);
                called.send(attempt).unwrap();
                if attempt < 2 {
                    return Err("the store could not be synced".to_owned());
                }
                let identity =
                    HandoverIdentity::mint("Steno on a test", chrono::Utc::now()).unwrap();
                let mac_id = identity.mac_id();
                Ok(Arc::new(ListenerHandover::over(
                    Arc::new(test_service(&listener_store, &inbox, identity)),
                    mac_id,
                    listener_store.clone(),
                    runtime.clone(),
                )))
            }),
            Arc::new(FakeImport::default()),
            Box::new(|| unreachable!("no Pair again here")),
        ));
        let (opened, mut was_opened) = tokio::sync::oneshot::channel();
        let mut follow = tokio::spawn(gated.clone().follow(move || {
            let _ = opened.send(());
        }));

        assert_eq!(
            gated.start().unwrap_err().to_string(),
            WAITING_FOR_IMPORT,
            "before the gate opened"
        );
        gate.send_replace(HandoverGate::Ready);
        assert_eq!(calls.recv().await, Some(0));
        let mut failure = gated.failure();
        failure.wait_for(Option::is_some).await.unwrap();
        assert!(gated.service().is_none());
        // The import is over: the answer is why the build failed.
        let failed = "the store could not be synced";
        assert_eq!(gated.start().unwrap_err().to_string(), failed);
        assert_eq!(gated.revoke(Uuid::nil()).unwrap_err().to_string(), failed);
        assert_eq!(gated.waiting(), None, "no identity to wait for");
        gate.send_replace(HandoverGate::Ready);
        tokio::select! {
            call = calls.recv() => assert_eq!(call, Some(1)),
            ended = &mut follow => panic!("follow ended after the failed build: {ended:?}"),
        }
        gated.try_again().unwrap();
        tokio::select! {
            call = calls.recv() => assert_eq!(call, Some(2)),
            ended = &mut follow => panic!("follow ended after the second failed build: {ended:?}"),
        }
        (&mut was_opened).await.unwrap();
        follow.await.unwrap();
        assert!(gated.service().is_some());
        assert_eq!(*gated.failure().borrow(), None);
    }

    /// A secret store in memory that refuses every read of the identity
    /// while `refuse` is set, as a denied keychain prompt does, and logs
    /// every write into `log`.
    #[derive(Default)]
    struct Keychain {
        inner: steno_core::testing::InMemorySecretStore,
        refuse: std::sync::atomic::AtomicBool,
        log: Arc<Mutex<Vec<String>>>,
    }

    #[steno_core::async_trait]
    impl SecretStore for Keychain {
        async fn secret(&self, key: &steno_core::SecretKey) -> BoundaryResult<Option<String>> {
            if key.as_str() == HandoverIdentity::SECRET_KEY && self.refuse.load(Ordering::SeqCst) {
                return Err("denied: the user refused the keychain prompt".into());
            }
            self.inner.secret(key).await
        }

        async fn set_secret(
            &self,
            key: &steno_core::SecretKey,
            value: Option<&str>,
        ) -> BoundaryResult<()> {
            self.log
                .lock()
                .unwrap()
                .push(format!("set {}", key.as_str()));
            self.inner.set_secret(key, value).await
        }
    }

    /// The import's marker: logs every write into the keychain's log, and
    /// fails while `fail` is set.
    #[derive(Default)]
    struct Marker {
        log: Arc<Mutex<Vec<String>>>,
        fail: std::sync::atomic::AtomicBool,
        marked: std::sync::atomic::AtomicBool,
    }

    impl ImportMarker for Marker {
        fn mark_import_done(&self) -> Result<(), String> {
            self.log.lock().unwrap().push("marker".to_owned());
            if self.fail.load(Ordering::SeqCst) {
                return Err("the keychain is locked".to_owned());
            }
            self.marked.store(true, Ordering::SeqCst);
            Ok(())
        }
    }

    /// A computer whose phone paired with `identity`, recorded under
    /// `dir`; `stored` puts the identity in the keychain too.
    struct Paired {
        dir: tempfile::TempDir,
        store: Arc<Store>,
        keychain: Arc<Keychain>,
        record: Arc<FingerprintFile>,
        marker: Arc<Marker>,
        identity: HandoverIdentity,
    }

    impl Paired {
        async fn new(stored: bool) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let store = Arc::new(Store::in_memory().unwrap());
            crate::testing::pair_a_phone(&store);
            let keychain = Arc::new(Keychain::default());
            let record = Arc::new(FingerprintFile::in_support_directory(dir.path()));
            let identity = HandoverIdentity::mint("Steno on a test", chrono::Utc::now()).unwrap();
            if stored {
                identity.store(&keychain.inner, &*record).await.unwrap();
            } else {
                record
                    .record(&steno_handover::identity::hex(&identity.fingerprint()))
                    .unwrap();
            }
            let marker = Arc::new(Marker {
                log: keychain.log.clone(),
                ..Marker::default()
            });
            Paired {
                dir,
                store,
                keychain,
                record,
                marker,
                identity,
            }
        }

        fn source(&self) -> Arc<IdentitySource> {
            let (store, inbox) = (self.store.clone(), self.dir.path().to_path_buf());
            Arc::new(IdentitySource {
                store: self.store.clone(),
                secrets: self.keychain.clone(),
                record: self.record.clone(),
                marker: Some(self.marker.clone()),
                common_name: "Steno on a test".to_owned(),
                listen: Box::new(move |identity| Arc::new(test_service(&store, &inbox, identity))),
                runtime: tokio::runtime::Handle::current(),
            })
        }

        /// The stored identity, the recorded fingerprint and the paired
        /// phones, as they stand.
        async fn places(&self) -> (Option<String>, Option<String>, usize) {
            (
                self.keychain
                    .inner
                    .secret(&HandoverIdentity::secret_key())
                    .await
                    .unwrap(),
                self.record.recorded().unwrap(),
                self.store.paired_devices().unwrap().len(),
            )
        }

        fn log(&self) -> Vec<String> {
            self.keychain.log.lock().unwrap().clone()
        }
    }

    /// A read the keychain refuses mints nothing: the stored identity, the
    /// recorded fingerprint and the paired phone stay as they were, at the
    /// launch and at a Try again that is refused too. Once the keychain
    /// answers, Try again hands the listener over without a restart, on
    /// the identity the phone knows, and starts it for the paired phone.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_refused_read_mints_nothing_and_try_again_starts_the_listener_once_it_reads() {
        let paired = Paired::new(true).await;
        let before = paired.places().await;
        paired.keychain.refuse.store(true, Ordering::SeqCst);
        let handover = ListenerHandover::read(paired.source());
        assert!(handover.listener().is_none());
        assert_eq!(handover.waiting(), Some(PhoneWaitingReason::Unreadable));
        let ListenerState::Failed(reason) = handover.state() else {
            panic!("a waiting handover reads as failed");
        };
        assert!(reason.contains("refused the keychain prompt"), "{reason}");
        assert_eq!(
            paired.places().await,
            before,
            "nothing minted at the launch"
        );
        let refused = tokio::task::block_in_place(|| handover.try_again()).unwrap_err();
        assert!(
            refused.to_string().contains("could not be read"),
            "{refused}"
        );
        assert_eq!(paired.places().await, before, "nor at Try again");
        assert!(
            paired.log().is_empty(),
            "nothing written: {:?}",
            paired.log()
        );

        paired.keychain.refuse.store(false, Ordering::SeqCst);
        tokio::task::block_in_place(|| handover.try_again()).unwrap();
        assert_eq!(handover.waiting(), None);
        assert_eq!(
            handover.mac_id(),
            steno_core::json::uuid_string(paired.identity.mac_id()),
            "the identity the phone knows"
        );
        assert!(
            matches!(handover.state(), ListenerState::Listening(_)),
            "{:?}",
            handover.state()
        );
        assert_eq!(paired.places().await, before);
        tokio::task::block_in_place(|| handover.stop());
    }

    /// The identity the phone knows is gone, so the handover waits (and
    /// says so) instead of minting. Pair again writes the import's marker
    /// first, before the new identity is stored, and keeps it; the paired
    /// phone and the old fingerprint go, and the listener comes up on the
    /// new identity.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn pair_again_writes_the_marker_before_the_new_identity_and_keeps_it() {
        let paired = Paired::new(false).await;
        let handover = ListenerHandover::read(paired.source());
        assert_eq!(handover.waiting(), Some(PhoneWaitingReason::Missing));
        assert_eq!(paired.places().await.0, None, "nothing minted");

        tokio::task::block_in_place(|| handover.pair_again()).unwrap();
        assert_eq!(
            paired.log(),
            ["marker", "set handover-identity"],
            "the marker before the identity"
        );
        assert!(
            paired.marker.marked.load(Ordering::SeqCst),
            "the marker stays"
        );
        let (stored, recorded, phones) = paired.places().await;
        let minted = HandoverIdentity::from_pem(&stored.unwrap()).unwrap();
        assert_ne!(minted.fingerprint(), paired.identity.fingerprint());
        assert_eq!(
            recorded,
            Some(steno_handover::identity::hex(&minted.fingerprint()))
        );
        assert_eq!(phones, 0, "every phone pairs again");
        assert_eq!(handover.waiting(), None);
        assert_eq!(
            handover.mac_id(),
            steno_core::json::uuid_string(minted.mac_id())
        );
        assert_eq!(
            tokio::task::block_in_place(|| handover.pair_again())
                .unwrap_err()
                .to_string(),
            NOT_WAITING,
            "only while waiting"
        );
    }

    /// A marker that cannot be written stops Pair again before anything
    /// goes: no identity is minted, and the phone and the fingerprint stay.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_marker_that_cannot_be_written_stops_pair_again() {
        let paired = Paired::new(false).await;
        let before = paired.places().await;
        let handover = ListenerHandover::read(paired.source());
        paired.marker.fail.store(true, Ordering::SeqCst);
        let error = tokio::task::block_in_place(|| handover.pair_again()).unwrap_err();
        assert!(
            error.to_string().contains("the keychain is locked"),
            "{error}"
        );
        assert_eq!(paired.log(), ["marker"], "no identity written");
        assert_eq!(paired.places().await, before);
        assert_eq!(handover.waiting(), Some(PhoneWaitingReason::Missing));
    }

    /// The import's step as a test drives it: counts the runs and the
    /// Pair agains, and answers `run` as set.
    #[derive(Default)]
    struct FakeImport {
        runs: AtomicUsize,
        pairings: AtomicUsize,
        refuse_run: std::sync::atomic::AtomicBool,
    }

    impl WaitingImport for FakeImport {
        fn run_again(&self) -> Result<(), String> {
            self.runs.fetch_add(1, Ordering::SeqCst);
            if self.refuse_run.load(Ordering::SeqCst) {
                return Err("macOS did not let Steno read this Mac's phone pairing.".to_owned());
            }
            Ok(())
        }

        fn pair_again(&self, mint: &dyn Fn() -> Result<(), String>) -> Result<(), String> {
            self.pairings.fetch_add(1, Ordering::SeqCst);
            mint()
        }
    }

    /// While the import's gate waits, Try again runs the step again and
    /// Pair again goes through the import with the mint; while the step is
    /// pending neither does anything, as the onboarding window shows it.
    #[test]
    fn while_the_gate_waits_try_again_and_pair_again_go_to_the_import() {
        let store = Arc::new(Store::in_memory().unwrap());
        let (gate, receiver) = tokio::sync::watch::channel(HandoverGate::Pending);
        let import = Arc::new(FakeImport::default());
        let mints = Arc::new(AtomicUsize::new(0));
        let minted = mints.clone();
        let gated = GatedHandover::new(
            store,
            receiver,
            Box::new(|| unreachable!("no build while the gate waits")),
            import.clone(),
            Box::new(move || {
                minted.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }),
        );
        assert_eq!(gated.waiting(), None, "the step is pending");
        assert!(gated.try_again().is_err());
        assert!(gated.pair_again().is_err());
        assert_eq!(import.runs.load(Ordering::SeqCst), 0);

        gate.send_replace(HandoverGate::Waiting(WaitReason::ImportDenied));
        assert_eq!(gated.waiting(), Some(PhoneWaitingReason::ImportDenied));
        import.refuse_run.store(true, Ordering::SeqCst);
        assert!(
            gated
                .try_again()
                .unwrap_err()
                .to_string()
                .starts_with("macOS did not let Steno")
        );
        import.refuse_run.store(false, Ordering::SeqCst);
        gated.try_again().unwrap();
        assert_eq!(import.runs.load(Ordering::SeqCst), 2);
        gated.pair_again().unwrap();
        assert_eq!(import.pairings.load(Ordering::SeqCst), 1);
        assert_eq!(mints.load(Ordering::SeqCst), 1);
    }

    /// The listener built behind the gate may itself wait, on a keychain
    /// that refused its read: the gated handover says so, and Try again
    /// reads it again through the adopted listener, which an unlock alone
    /// never did.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_gated_listener_left_waiting_reads_again_at_try_again() {
        let paired = Paired::new(true).await;
        paired.keychain.refuse.store(true, Ordering::SeqCst);
        let (gate, receiver) = tokio::sync::watch::channel(HandoverGate::Ready);
        let source = paired.source();
        let gated = Arc::new(GatedHandover::new(
            paired.store.clone(),
            receiver,
            Box::new(move || Ok(Arc::new(ListenerHandover::read(source.clone())))),
            Arc::new(FakeImport::default()),
            Box::new(|| unreachable!("no Pair again here")),
        ));
        gated.clone().follow(|| {}).await;
        assert!(gated.service().is_none());
        assert_eq!(gated.waiting(), Some(PhoneWaitingReason::Unreadable));

        paired.keychain.refuse.store(false, Ordering::SeqCst);
        tokio::task::block_in_place(|| gated.try_again()).unwrap();
        let service = gated.service().expect("the listener after Try again");
        assert!(matches!(
            service.state(),
            steno_handover::ListenerState::Listening { .. }
        ));
        assert_eq!(gated.waiting(), None);
        drop(gate);
        if let Some(service) = gated.close() {
            service.stop().await;
        }
    }

    /// A shutdown while `follow` builds the listener: the build finishes
    /// after the shutdown, and its listener is neither adopted nor started,
    /// though a phone is paired; `follow` ends without `changed`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_shutdown_while_the_listener_builds_adopts_and_starts_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::in_memory().unwrap());
        crate::testing::pair_a_phone(&store);
        let (_gate, receiver) = tokio::sync::watch::channel(HandoverGate::Ready);
        let (building, is_building) = std::sync::mpsc::channel();
        let (finish, may_finish) = std::sync::mpsc::channel::<()>();
        let may_finish = Mutex::new(may_finish);
        let built = Arc::new(Mutex::new(None::<Arc<HandoverService>>));
        let runtime = tokio::runtime::Handle::current();
        let (listener_store, inbox, kept) =
            (store.clone(), dir.path().to_path_buf(), built.clone());
        let gated = Arc::new(GatedHandover::new(
            store,
            receiver,
            Box::new(move || {
                building.send(()).unwrap();
                may_finish.lock().unwrap().recv().unwrap();
                let identity =
                    HandoverIdentity::mint("Steno on a test", chrono::Utc::now()).unwrap();
                let mac_id = identity.mac_id();
                let service = Arc::new(test_service(&listener_store, &inbox, identity));
                *kept.lock().unwrap() = Some(service.clone());
                Ok(Arc::new(ListenerHandover::over(
                    service,
                    mac_id,
                    listener_store.clone(),
                    runtime.clone(),
                )))
            }),
            Arc::new(FakeImport::default()),
            Box::new(|| unreachable!("no Pair again here")),
        ));
        let follow = tokio::spawn(
            gated
                .clone()
                .follow(|| panic!("no listener is adopted after the shutdown")),
        );
        tokio::task::spawn_blocking(move || is_building.recv().unwrap())
            .await
            .unwrap();
        assert!(gated.close().is_none(), "nothing to stop yet");
        finish.send(()).unwrap();
        follow.await.unwrap();
        assert!(gated.service().is_none(), "not adopted");
        let service = built.lock().unwrap().clone().expect("the build finished");
        assert_eq!(
            service.state(),
            steno_handover::ListenerState::Stopped,
            "not started"
        );
    }
}
