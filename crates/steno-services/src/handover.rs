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
//!
//! The waiting state, one row per event. A plain handover
//! ([`ListenerHandover`]) has its listener or waits with a reason; a gated
//! one ([`GatedHandover`]) has no listener until the Swift import's gate
//! opens, then adopts a [`ListenerHandover`], which may itself wait.
//!
//! | Event | Plain handover | Gated handover | Read or written |
//! |-------|----------------|----------------|-----------------|
//! | Launch | the listener, or waiting with the reason | nothing until the gate opens | the identity read; a mint only where the guard allows |
//! | Try again while waiting | the listener once the read succeeds, else the new reason | the gate waits: the import's step runs again; a listener adopted: its Try again | the identity read (or the import's export) |
//! | Try again with a listener | nothing | the adopted listener's: nothing | nothing |
//! | Pair again while waiting | the listener over a new identity | the gate waits: the import ends with a new identity and the gate opens; a listener adopted: its Pair again | the marker, then the next fingerprint noted, then the new identity, then the paired phones removed, then its fingerprint recorded (dropping the note) |
//! | Launch, Try again or gate ready after an unfinished Pair again | finished: the listener over the new identity, no phones; abandoned: as Launch; unknown: waiting, replaced; undecided: waiting, unreadable | as plain, at gate ready | finished: the phones removed, its fingerprint recorded, no mint; abandoned: the note dropped; otherwise nothing |
//! | The keyring answered after asking at the launch (`App::launch`) | as Try again | nothing: Try again reads it | as Try again |
//! | Gate ready | | the listener built and adopted | the identity read, as at the launch |
//! | A failed build (the store's checkpoint) | | no listener; built again at the next gate change | nothing |
//! | Shutdown | no listener handed over from now on | no listener built or adopted from now on | nothing |
//!
//! The cases of an unfinished Pair again are
//! [`HandoverIdentity::load_or_create`]'s.

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
/// replaced atomically. While a Pair again is unfinished the file holds the
/// next fingerprint too, under `steno.handoverIdentityNextFingerprint`
/// ([`FingerprintRecord::note_next`]); the key is written only while it is
/// set, so a finished Pair again leaves the same bytes as a file from
/// before the key, and a file without it reads as before. An older build
/// reads only the first key and drops the second at its next record; it
/// waits as replaced, as it would without the note. Not
/// `preferences.json`: that file holds only flags, is rewritten in place,
/// and a build that reads it as flags would drop every flag over a text
/// value. The Swift app never reads or writes this file, so a rollback to
/// it leaves the record alone. No Swift counterpart.
#[derive(Debug, Clone)]
pub struct FingerprintFile {
    path: PathBuf,
}

impl FingerprintFile {
    /// The key the fingerprint is filed under.
    pub const KEY: &'static str = "steno.handoverIdentityFingerprint";
    /// The key the next fingerprint is filed under while a Pair again is
    /// unfinished.
    pub const NEXT_KEY: &'static str = "steno.handoverIdentityNextFingerprint";

    /// The record under `support_directory`.
    #[must_use]
    pub fn in_support_directory(support_directory: &Path) -> Self {
        FingerprintFile {
            path: support_directory.join("handover-identity.json"),
        }
    }

    /// The file's keys: none when it is missing; one that does not parse
    /// is an error, never "nothing recorded".
    fn values(&self) -> BoundaryResult<BTreeMap<String, String>> {
        match std::fs::read(&self.path) {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
            Err(error) => Err(Box::new(error)),
        }
    }

    /// Replaces the file with `fingerprint` and `next`, each only when
    /// set; with neither the file goes, as before the first record.
    fn write(&self, fingerprint: Option<&str>, next: Option<&str>) -> BoundaryResult<()> {
        let values: BTreeMap<&str, &str> = [(Self::KEY, fingerprint), (Self::NEXT_KEY, next)]
            .into_iter()
            .filter_map(|(key, value)| Some((key, value?)))
            .collect();
        if values.is_empty() {
            return match std::fs::remove_file(&self.path) {
                Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(Box::new(error)),
                _ => Ok(()),
            };
        }
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        replace_file(
            &self.path,
            &serde_json::to_vec_pretty(&values)?,
            Access::Default,
        )?;
        Ok(())
    }
}

impl FingerprintRecord for FingerprintFile {
    fn recorded(&self) -> BoundaryResult<Option<String>> {
        Ok(self.values()?.remove(Self::KEY))
    }

    fn record(&self, fingerprint: &str) -> BoundaryResult<()> {
        self.write(Some(fingerprint), None)
    }

    fn next(&self) -> BoundaryResult<Option<String>> {
        Ok(self.values()?.remove(Self::NEXT_KEY))
    }

    fn note_next(&self, fingerprint: Option<&str>) -> BoundaryResult<()> {
        let recorded = self.recorded()?;
        self.write(recorded.as_deref(), fingerprint)
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
    /// [`Self::mint_anew`] (the next fingerprint noted, the new identity,
    /// the phones removed, its fingerprint recorded) and the listener over
    /// the new identity.
    pub fn pair_again(&self) -> Result<(Arc<HandoverService>, Uuid), String> {
        if let Some(marker) = &self.marker {
            marker
                .mark_import_done()
                .map_err(|error| marker_not_written(&error))?;
        }
        let identity = self.mint_anew()?;
        Ok(self.listener_over(identity))
    }

    /// Pair again's mint alone ([`HandoverIdentity::mint_anew`]): the new
    /// identity's fingerprint is noted as the next one, the identity
    /// written to the secret store, the paired phones removed, then its
    /// fingerprint recorded, which drops the note; a stop after the write
    /// is finished by the next read. Whoever calls it writes the marker
    /// first.
    pub fn mint_anew(&self) -> Result<HandoverIdentity, String> {
        block_on(
            &self.runtime,
            HandoverIdentity::mint_anew(
                self.secrets.as_ref(),
                self.record.as_ref(),
                &self.store,
                &self.common_name,
                chrono::Utc::now(),
            ),
        )
        .map_err(|error| {
            tracing::warn!(%error, "Pair again did not finish");
            match error {
                // A stop after the write, which the next read finishes.
                IdentityError::Store(_) | IdentityError::Record(_) => UNFINISHED.to_owned(),
                error => identity_failure(&error),
            }
        })
    }

    fn listener_over(&self, identity: HandoverIdentity) -> (Arc<HandoverService>, Uuid) {
        let mac_id = identity.mac_id();
        ((self.listen)(identity), mac_id)
    }
}

/// Pair again's answer when the import's marker could not be written
/// (only on the Mac, whose login keychain holds it); the error goes to the
/// log.
pub(crate) fn marker_not_written(error: &str) -> String {
    tracing::warn!(%error, "the import's marker was not written, so Pair again stopped");
    MARKER_NOT_WRITTEN.to_owned()
}

/// What [`marker_not_written`] answers.
pub(crate) const MARKER_NOT_WRITTEN: &str = "Steno made no new pairing, as macOS did not let it save to the login keychain. Choose Pair again, and unlock the keychain if macOS asks.";

/// The word for this computer in the handover's answers.
const COMPUTER: &str = if cfg!(target_os = "macos") {
    "Mac"
} else {
    "computer"
};

/// Why the identity is not available, for the iPhone section's details,
/// in plain words: the keyring's own sentence when it was not opened at
/// start, as that one already names the pairing; the waiting card's first
/// sentence when the guard refused the identity; a sentence of its own when
/// the database or the fingerprint record failed (the caller logs the
/// error); the read's error otherwise.
pub(crate) fn identity_failure(error: &IdentityError) -> String {
    match error {
        IdentityError::Unavailable(Unavailability::Missing) => {
            format!("This {COMPUTER}'s phone pairing is no longer saved.")
        }
        IdentityError::Unavailable(Unavailability::Replaced) => {
            format!(
                "Steno found a different phone pairing on this {COMPUTER} than the one your \
                 phones know."
            )
        }
        IdentityError::Secrets(source) => {
            format!("Steno could not save the new phone pairing on this {COMPUTER} ({source}).")
        }
        IdentityError::Store(_) | IdentityError::Record(_) => {
            format!("Steno could not open its records of this {COMPUTER}'s phone pairing.")
        }
        _ => match keyring_failure(error) {
            Some(not_opened @ KeyringUnavailable::NotOpened(_)) => not_opened.to_string(),
            _ => error.to_string(),
        },
    }
}

/// The keyring's reason when the identity could not be read from it.
fn keyring_failure(error: &IdentityError) -> Option<&KeyringUnavailable> {
    match error {
        IdentityError::Unavailable(Unavailability::Unreadable(source)) => source.downcast_ref(),
        _ => None,
    }
}

/// What Pair again answers when it stopped after the new identity was
/// written (the phones' removal or the record failed).
const UNFINISHED: &str = if cfg!(target_os = "macos") {
    "Steno could not finish the new phone pairing on this Mac. Choose Pair again."
} else {
    "Steno could not finish the new phone pairing on this computer. Choose Pair again."
};

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
    pending: Mutex<Pending>,
    /// Where Try again and Pair again read and mint; `None` for a
    /// handover made over a listener alone.
    source: Option<Arc<IdentitySource>>,
    /// Held by a read or a mint, one at a time.
    attempt: Mutex<()>,
    store: Arc<Store>,
    runtime: tokio::runtime::Handle,
}

/// Why a [`ListenerHandover`] has no listener yet, and whether the app
/// shut down.
struct Pending {
    /// The failure the listener state shows.
    failure: String,
    /// Why the handover waits, for the iPhone section; `None` with a
    /// listener.
    reason: Option<PhoneWaitingReason>,
    closed: bool,
}

/// What Try again and Pair again answer while another one runs.
const ATTEMPT_UNDER_WAY: &str = "Steno is already working on this phone pairing.";
/// What Pair again answers once the phones can send recordings.
pub(crate) const NOT_WAITING: &str = "Your phones can already send recordings.";
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
        let handover = Self::empty(None, store, runtime);
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
        let handover = Self::empty(Some(source), store, runtime);
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

    fn empty(
        source: Option<Arc<IdentitySource>>,
        store: Arc<Store>,
        runtime: tokio::runtime::Handle,
    ) -> Self {
        ListenerHandover {
            listener: OnceLock::new(),
            pending: Mutex::new(Pending {
                failure: String::new(),
                reason: None,
                closed: false,
            }),
            source,
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
    fn set(&self, service: Arc<HandoverService>, mac_id: Uuid) -> bool {
        let mut pending = self.lock();
        if pending.closed || self.listener.set((service, mac_id)).is_err() {
            return false;
        }
        pending.reason = None;
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
    /// listener; `ATTEMPT_UNDER_WAY` while another read or mint runs,
    /// as the handover still waits. Try again, and the launch's reread
    /// once a keyring that was asking the user answered.
    pub fn read_again(&self) -> Result<(), String> {
        if self.listener().is_some() {
            return Ok(());
        }
        let source = self.source.as_ref().ok_or_else(|| self.unavailable())?;
        let _attempt = self.one_at_a_time().ok_or(ATTEMPT_UNDER_WAY)?;
        if self.listener().is_some() {
            return Ok(());
        }
        match source.read() {
            Ok((service, mac_id)) => {
                if self.set(service, mac_id) || self.listener().is_some() {
                    Ok(())
                } else {
                    Err(SHUT_DOWN.to_owned())
                }
            }
            Err(error) => {
                tracing::warn!(%error, "phone handover still waits for its identity");
                self.wait_for(&error);
                Err(identity_failure(&error))
            }
        }
    }

    /// Why the identity is not available, as the listener state and the
    /// iPhone section show it.
    fn wait_for(&self, error: &IdentityError) {
        let mut pending = self.lock();
        pending.failure = identity_failure(error);
        pending.reason = Some(wait_reason(error));
    }

    /// Held for a read or a mint; `None` while another one runs.
    fn one_at_a_time(&self) -> Option<MutexGuard<'_, ()>> {
        match self.attempt.try_lock() {
            Ok(guard) => Some(guard),
            Err(TryLockError::Poisoned(poisoned)) => Some(poisoned.into_inner()),
            Err(TryLockError::WouldBlock) => None,
        }
    }

    fn lock(&self) -> MutexGuard<'_, Pending> {
        self.pending.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn unavailable(&self) -> String {
        format!("Phone handover is unavailable: {}", self.lock().failure)
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
        self.lock().reason
    }

    fn try_again(&self) -> BoundaryResult<()> {
        Ok(self.read_again()?)
    }

    /// Only while the handover waits, and never after the shutdown: the
    /// marker, the mint ([`IdentitySource::pair_again`]), then the
    /// listener over the new identity. Checked again once the attempt is
    /// this one's, as a read that just ended may have handed a listener
    /// over.
    fn pair_again(&self) -> BoundaryResult<()> {
        if self.listener().is_some() {
            return Err(NOT_WAITING.into());
        }
        let source = self.source.as_ref().ok_or_else(|| self.unavailable())?;
        let _attempt = self.one_at_a_time().ok_or(ATTEMPT_UNDER_WAY)?;
        if self.listener().is_some() {
            return Err(NOT_WAITING.into());
        }
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
pub const WAITING_FOR_IMPORT: &str = "Your phones can send recordings again once Steno has brought over this Mac's phone pairing from the previous version, in the welcome step or with Try again in Settings > iPhone.";

/// The Swift import's step as the gated handover drives it while the
/// import's gate waits (`crate::swift_import::ImportStep`).
pub trait WaitingImport: Send + Sync {
    /// Try again: the step runs again (the export and its prompt); its line
    /// when the identity still did not come over.
    fn try_again(&self) -> Result<(), String>;
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
    ///
    /// The latch is checked before the build too, so a shutdown that came
    /// first reads no identity; it is not held during the build, which may
    /// wait on a keychain prompt that the shutdown must not wait for.
    fn build_and_adopt(&self) -> Result<bool, String> {
        if *self.lock_closed() {
            return Ok(false);
        }
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
    /// gate waits, the step runs again. Once the gate opened and a build
    /// failed, `follow` builds again; Settings never offers Try again
    /// then, as [`Handover::waiting`] has no reason to show.
    fn try_again(&self) -> BoundaryResult<()> {
        if let Some(listener) = self.listener.get() {
            return listener.try_again();
        }
        // Copied out: the step sets the gate, which a held borrow would
        // block.
        let gate = *self.gate.borrow();
        match gate {
            HandoverGate::Waiting(_) => Ok(self.import.try_again()?),
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
        // Copied out, as in `try_again`.
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
    use crate::testing::{RefusingRecord, warnings};

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
        assert!(file.next().is_err(), "nor a record without a note");
    }

    /// The bytes a build from before the next fingerprint wrote, exactly.
    const OLDER_FILE: &[u8] = b"{\n  \"steno.handoverIdentityFingerprint\": \"cd34\"\n}";

    /// A file from before the next fingerprint reads as before, with no
    /// note. The note is written beside the recorded fingerprint and kept
    /// by it; recording drops it, which leaves exactly the older bytes, and
    /// dropping it leaves them too, or no file where nothing was recorded.
    #[test]
    fn an_older_fingerprint_file_reads_and_a_finished_pair_again_writes_its_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("handover-identity.json");
        let file = FingerprintFile::in_support_directory(dir.path());
        assert_eq!(file.next().unwrap(), None, "no file, no note");
        std::fs::write(&path, OLDER_FILE).unwrap();
        assert_eq!(file.recorded().unwrap().as_deref(), Some("cd34"));
        assert_eq!(file.next().unwrap(), None);

        file.note_next(Some("ef56")).unwrap();
        assert_eq!(file.recorded().unwrap().as_deref(), Some("cd34"), "kept");
        assert_eq!(file.next().unwrap().as_deref(), Some("ef56"));
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "{\n  \"steno.handoverIdentityFingerprint\": \"cd34\",\n  \
             \"steno.handoverIdentityNextFingerprint\": \"ef56\"\n}"
        );
        file.note_next(None).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), OLDER_FILE, "dropped");

        file.note_next(Some("ef56")).unwrap();
        file.record("cd34").unwrap();
        assert_eq!(file.next().unwrap(), None, "the record drops the note");
        assert_eq!(std::fs::read(&path).unwrap(), OLDER_FILE, "the older bytes");

        // Nothing recorded, as on the first launch after the Swift app.
        std::fs::remove_file(&path).unwrap();
        file.note_next(Some("ef56")).unwrap();
        assert_eq!(file.recorded().unwrap(), None);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "{\n  \"steno.handoverIdentityNextFingerprint\": \"ef56\"\n}"
        );
        file.note_next(None).unwrap();
        assert!(!path.exists(), "nothing left to file");
        file.note_next(None).unwrap();
    }

    /// The answers Try again and Pair again give, pinned as text, as the
    /// tests compare against the constants: each in plain words, with no
    /// developer word in it, and Missing and Replaced in the waiting card's
    /// words.
    #[test]
    fn the_handover_s_answers_are_plain_sentences() {
        assert_eq!(
            ATTEMPT_UNDER_WAY,
            "Steno is already working on this phone pairing."
        );
        assert_eq!(NOT_WAITING, "Your phones can already send recordings.");
        assert_eq!(SHUT_DOWN, "Steno is quitting.");
        let failures = [
            IdentityError::Unavailable(Unavailability::Missing),
            IdentityError::Unavailable(Unavailability::Replaced),
            IdentityError::Store(steno_core::StoreError::from(rusqlite::Error::InvalidQuery)),
            IdentityError::Record("No space left on device".into()),
        ]
        .map(|error| identity_failure(&error));
        let answers = [
            ATTEMPT_UNDER_WAY,
            NOT_WAITING,
            SHUT_DOWN,
            crate::swift_import::STEP_UNDER_WAY,
            MARKER_NOT_WRITTEN,
            WAITING_FOR_IMPORT,
            UNFINISHED,
        ]
        .map(str::to_owned);
        for answer in answers.iter().chain(&failures) {
            for word in [
                "identity",
                "secret",
                "fingerprint",
                "listener",
                "keyring entry",
                "upload",
                "database",
                "record:",
            ] {
                assert!(!answer.to_lowercase().contains(word), "{word}: {answer}");
            }
        }
        for (reason, failure) in [PhoneWaitingReason::Missing, PhoneWaitingReason::Replaced]
            .into_iter()
            .zip(&failures)
        {
            let card = steno_host::settings::snapshots::waiting_message(
                reason,
                steno_core::Platform::CURRENT,
            );
            assert!(card.starts_with(failure.as_str()), "{card} / {failure}");
        }
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

    /// The handover over a [`test_service`] on a newly minted identity.
    fn minted_listener(
        store: &Arc<Store>,
        dir: &Path,
        runtime: &tokio::runtime::Handle,
    ) -> Arc<ListenerHandover> {
        let identity = HandoverIdentity::mint("Steno on a test", chrono::Utc::now()).unwrap();
        let mac_id = identity.mac_id();
        Arc::new(ListenerHandover::over(
            Arc::new(test_service(store, dir, identity)),
            mac_id,
            store.clone(),
            runtime.clone(),
        ))
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
                Ok(minted_listener(&listener_store, &inbox, &runtime))
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
        let call = tokio::time::timeout(crate::testing::PATIENCE, calls.recv())
            .await
            .expect("Try again builds again");
        assert_eq!(call, Some(2));
        (&mut was_opened).await.unwrap();
        follow.await.unwrap();
        assert!(gated.service().is_some());
        assert_eq!(*gated.failure().borrow(), None);
    }

    /// A secret store in memory that refuses every read of the identity
    /// while `refuse` is set, as a denied keychain prompt does, and every
    /// write while `refuse_writes` is; counts the identity's reads and logs
    /// every write it took into `log`.
    #[derive(Default)]
    struct Keychain {
        inner: steno_core::testing::InMemorySecretStore,
        refuse: std::sync::atomic::AtomicBool,
        refuse_writes: std::sync::atomic::AtomicBool,
        reads: AtomicUsize,
        log: Arc<Mutex<Vec<String>>>,
    }

    #[steno_core::async_trait]
    impl SecretStore for Keychain {
        async fn secret(&self, key: &steno_core::SecretKey) -> BoundaryResult<Option<String>> {
            if key.as_str() == HandoverIdentity::SECRET_KEY {
                self.reads.fetch_add(1, Ordering::SeqCst);
                if self.refuse.load(Ordering::SeqCst) {
                    return Err("denied: the user refused the keychain prompt".into());
                }
            }
            self.inner.secret(key).await
        }

        async fn set_secret(
            &self,
            key: &steno_core::SecretKey,
            value: Option<&str>,
        ) -> BoundaryResult<()> {
            if self.refuse_writes.load(Ordering::SeqCst) {
                return Err("denied: the user refused to let Steno change the item".into());
            }
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
    /// `dir`; `stored` puts the identity in the keychain too. `listens`
    /// counts the listeners its source built.
    struct Paired {
        dir: tempfile::TempDir,
        store: Arc<Store>,
        keychain: Arc<Keychain>,
        record: Arc<RefusingRecord>,
        marker: Arc<Marker>,
        identity: HandoverIdentity,
        listens: Arc<AtomicUsize>,
    }

    impl Paired {
        async fn new(stored: bool) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let store = Arc::new(Store::in_memory().unwrap());
            crate::testing::pair_a_phone(&store);
            let keychain = Arc::new(Keychain::default());
            let record = Arc::new(RefusingRecord {
                file: Arc::new(FingerprintFile::in_support_directory(dir.path())),
                refuse: std::sync::atomic::AtomicBool::new(false),
            });
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
                listens: Arc::new(AtomicUsize::new(0)),
            }
        }

        fn source(&self) -> Arc<IdentitySource> {
            let (store, inbox) = (self.store.clone(), self.dir.path().to_path_buf());
            let listens = self.listens.clone();
            Arc::new(IdentitySource {
                store: self.store.clone(),
                secrets: self.keychain.clone(),
                record: self.record.clone(),
                marker: Some(self.marker.clone()),
                common_name: "Steno on a test".to_owned(),
                listen: Box::new(move |identity| {
                    listens.fetch_add(1, Ordering::SeqCst);
                    Arc::new(test_service(&store, &inbox, identity))
                }),
                runtime: tokio::runtime::Handle::current(),
            })
        }

        /// The phone's id.
        fn phone(&self) -> Uuid {
            self.store.paired_devices().unwrap()[0].id
        }

        /// A recording the phone is still sending (its receipt `receiving`,
        /// its chunks in the inbox) and one already admitted as a meeting
        /// (its receipt `complete`, its row in the admission ledger).
        fn seed_recordings(&self) -> Recordings {
            let now = chrono::Utc::now();
            let receipt = |recording_id, state| HandoverReceipt {
                recording_id,
                device_id: self.phone(),
                state,
                byte_count: 4,
                sha256: vec![7; 32],
                chunk_size: 4,
                received_chunks: Vec::new(),
                created_at: now,
                updated_at: now,
            };
            let receiving = Uuid::new_v4();
            self.store
                .save_handover_receipt(&receipt(receiving, steno_core::HandoverState::Receiving))
                .unwrap();
            let inbox = self.dir.path().join("inbox");
            std::fs::create_dir_all(&inbox).unwrap();
            let partial = inbox.join(format!("{}.partial", receiving.hyphenated()));
            std::fs::write(&partial, b"aac").unwrap();
            let meeting = steno_core::Meeting {
                id: Uuid::new_v4(),
                ..steno_core::testing::sample_data::meeting()
            };
            let asset = steno_core::AudioAsset {
                id: Uuid::new_v4(),
                meeting_id: meeting.id,
                url: format!("file:///tmp/Audio/{}/master.m4a", meeting.id),
                format: steno_core::AudioFormat::M4aAac,
                lanes: vec![steno_core::AudioLane::Mixed],
                sidecars_16k: BTreeMap::new(),
                mixdown_url: None,
                retention: steno_core::AudioRetention::KeepForever,
                expires_at: None,
            };
            let admitted = Uuid::new_v4();
            let complete = steno_core::HandoverState::Complete {
                meeting_id: meeting.id,
            };
            self.store
                .save_admission_durably(&receipt(admitted, complete), &meeting, &asset)
                .unwrap();
            Recordings {
                receiving,
                partial,
                admitted,
                meeting: meeting.id,
            }
        }

        /// Keeps the last paired phone whatever deletes it, as a disk that
        /// refuses the write would, until [`Self::let_the_phone_go`]: Pair
        /// again stops right after the write with one phone paired, and
        /// after the first phone with two.
        fn keep_the_phone(&self) {
            self.store
                .write(|transaction| {
                    transaction.execute_batch(
                        "CREATE TRIGGER keep_the_phone BEFORE DELETE ON pairedDevice \
                         WHEN (SELECT COUNT(*) FROM pairedDevice) = 1 \
                         BEGIN SELECT RAISE(ABORT, 'the disk refused the write'); END;",
                    )?;
                    Ok(())
                })
                .unwrap();
        }

        fn let_the_phone_go(&self) {
            self.store
                .write(|transaction| {
                    transaction.execute_batch("DROP TRIGGER keep_the_phone;")?;
                    Ok(())
                })
                .unwrap();
        }

        /// No fingerprint recorded, as on the first launch after the Swift
        /// app, whose phones come from the shared database.
        fn forget_the_record(&self) {
            self.record.file.write(None, None).unwrap();
        }

        /// The stored identity, the recorded fingerprint, the next one and
        /// the paired phones, as they stand.
        async fn places(&self) -> (Option<String>, Option<String>, Option<String>, usize) {
            (
                self.keychain
                    .inner
                    .secret(&HandoverIdentity::secret_key())
                    .await
                    .unwrap(),
                self.record.recorded().unwrap(),
                self.record.next().unwrap(),
                self.store.paired_devices().unwrap().len(),
            )
        }

        /// The identity's fingerprint as recorded, in hex.
        fn fingerprint_of(stored: Option<&String>) -> Option<String> {
            stored.map(|pem| {
                steno_handover::identity::hex(
                    &HandoverIdentity::from_pem(pem).unwrap().fingerprint(),
                )
            })
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
        {
            // A Try again while another read or mint runs says so: the
            // handover still waits.
            let _other = handover.attempt.lock().unwrap();
            let busy = tokio::task::block_in_place(|| handover.try_again()).unwrap_err();
            assert_eq!(busy.to_string(), ATTEMPT_UNDER_WAY);
        }
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

        // Once there, Try again reads nothing and builds no second listener.
        let reads = paired.keychain.reads.load(Ordering::SeqCst);
        tokio::task::block_in_place(|| handover.try_again()).unwrap();
        assert_eq!(paired.keychain.reads.load(Ordering::SeqCst), reads);
        assert_eq!(paired.listens.load(Ordering::SeqCst), 1, "one listener");
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
        let (stored, recorded, next, phones) = paired.places().await;
        let minted = HandoverIdentity::from_pem(&stored.unwrap()).unwrap();
        assert_eq!(next, None, "no note left");
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
        assert_eq!(error.to_string(), MARKER_NOT_WRITTEN, "in plain words");
        assert_eq!(paired.log(), ["marker"], "no identity written");
        assert_eq!(paired.places().await, before);
        assert_eq!(handover.waiting(), Some(PhoneWaitingReason::Missing));
    }

    /// [`Paired::seed_recordings`]' two recordings.
    struct Recordings {
        receiving: Uuid,
        partial: PathBuf,
        admitted: Uuid,
        meeting: Uuid,
    }

    impl Recordings {
        /// Neither recording was marked delivered or dropped on this
        /// computer: the admitted one keeps its ledger row and its meeting,
        /// the one still being sent has no ledger row, and while the phone
        /// is paired (`phone`) both receipts and the chunks stay. Without
        /// the phone the receipts went with it, and the phone, which keeps
        /// its copy, sends the recording again once it paired again.
        fn assert_kept(&self, store: &Store, phone: bool, when: &str) {
            assert_eq!(
                store.admitted_meeting(self.admitted, 4, &[7; 32]).unwrap(),
                Some(self.meeting),
                "{when}: the admitted recording stays delivered"
            );
            assert!(
                store.meeting(self.meeting).unwrap().is_some(),
                "{when}: its meeting stays"
            );
            assert_eq!(
                store.admitted_meeting(self.receiving, 4, &[7; 32]).unwrap(),
                None,
                "{when}: the recording still being sent is not delivered"
            );
            let receiving = store.handover_receipt(self.receiving).unwrap();
            if phone {
                assert_eq!(
                    receiving.map(|receipt| receipt.state),
                    Some(steno_core::HandoverState::Receiving),
                    "{when}"
                );
                assert!(
                    store.handover_receipt(self.admitted).unwrap().is_some(),
                    "{when}"
                );
                assert!(self.partial.exists(), "{when}: its chunks stay");
            } else {
                assert_eq!(receiving, None, "{when}: its receipt went with the phone");
            }
        }
    }

    /// A Pair again whose new identity the keychain refuses to write
    /// changes nothing, whichever reason the handover waits for: the
    /// stored identity, the recorded fingerprint, the paired phone and its
    /// recordings stay, the handover still waits for the same reason, and
    /// so does the next launch.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_refused_write_leaves_pair_again_waiting_with_its_reason() {
        for reason in [
            PhoneWaitingReason::Unreadable,
            PhoneWaitingReason::Missing,
            PhoneWaitingReason::Replaced,
        ] {
            let paired = Paired::new(reason != PhoneWaitingReason::Missing).await;
            match reason {
                PhoneWaitingReason::Unreadable => {
                    paired.keychain.refuse.store(true, Ordering::SeqCst);
                }
                PhoneWaitingReason::Replaced => {
                    let other = HandoverIdentity::mint("Steno elsewhere", chrono::Utc::now())
                        .unwrap()
                        .to_pem()
                        .unwrap();
                    paired
                        .keychain
                        .inner
                        .set_secret(&HandoverIdentity::secret_key(), Some(&other))
                        .await
                        .unwrap();
                }
                _ => {}
            }
            let recordings = paired.seed_recordings();
            let handover = ListenerHandover::read(paired.source());
            assert_eq!(handover.waiting(), Some(reason));
            let before = paired.places().await;

            paired.keychain.refuse_writes.store(true, Ordering::SeqCst);
            let error = tokio::task::block_in_place(|| handover.pair_again()).unwrap_err();
            assert!(
                error
                    .to_string()
                    .starts_with("Steno could not save the new phone pairing"),
                "{reason}: {error}"
            );
            assert_eq!(paired.places().await, before, "{reason}: nothing changed");
            assert_eq!(handover.waiting(), Some(reason), "{reason}: still waiting");
            assert!(handover.listener().is_none());
            recordings.assert_kept(&paired.store, true, &format!("{reason}: a refused write"));

            let next = ListenerHandover::read(paired.source());
            assert_eq!(next.waiting(), Some(reason), "{reason}: the next launch");
            assert_eq!(paired.places().await, before, "{reason}: mints nothing");
        }
    }

    /// A Pair again that stops after the new identity was written (the
    /// phone still paired, one of two removed, or every phone removed and
    /// the record refused) is finished by the next read, at the next
    /// launch or at Try again in the same one, whether or not a fingerprint
    /// was recorded before (none is on the first launch after the Swift
    /// app): the listener comes up over the new identity with no phone
    /// listed, its fingerprint recorded and the note dropped, nothing
    /// minted, and the recordings kept.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_stop_after_the_new_identity_is_finished_at_the_next_launch() {
        for recorded_before in [true, false] {
            for stop in ["after the write", "after phone 1 of 2", "after the phones"] {
                for read in ["the next launch", "Try again"] {
                    let case = format!("recorded before {recorded_before}, {stop}, {read}");
                    let paired = Paired::new(false).await;
                    if !recorded_before {
                        paired.forget_the_record();
                    }
                    if stop == "after phone 1 of 2" {
                        crate::testing::pair_a_phone(&paired.store);
                    }
                    let recordings = paired.seed_recordings();
                    let handover = ListenerHandover::read(paired.source());
                    assert_eq!(handover.waiting(), Some(PhoneWaitingReason::Missing));

                    let refuse_record = stop == "after the phones";
                    if refuse_record {
                        paired.record.refuse.store(true, Ordering::SeqCst);
                    } else {
                        paired.keep_the_phone();
                    }
                    tokio::task::block_in_place(|| handover.pair_again()).unwrap_err();
                    if refuse_record {
                        paired.record.refuse.store(false, Ordering::SeqCst);
                    } else {
                        paired.let_the_phone_go();
                    }
                    let (stored, recorded, next, phones) = paired.places().await;
                    let new_identity = HandoverIdentity::from_pem(
                        stored.as_deref().expect("the new identity was written"),
                    )
                    .unwrap();
                    let new = Paired::fingerprint_of(stored.as_ref());
                    assert_eq!(recorded.is_some(), recorded_before, "{case}");
                    assert_ne!(recorded, new, "{case}: the old record stays");
                    assert_eq!(next, new, "{case}: the note");
                    assert_eq!(phones, usize::from(!refuse_record), "{case}");
                    let phone_stays = phones == 1 && stop != "after phone 1 of 2";
                    recordings.assert_kept(&paired.store, phone_stays, &case);

                    let writes = paired.log().len();
                    let finished = if read == "Try again" {
                        tokio::task::block_in_place(|| handover.try_again()).unwrap();
                        handover
                    } else {
                        ListenerHandover::read(paired.source())
                    };
                    assert_eq!(finished.waiting(), None, "{case}: finished");
                    assert_eq!(
                        paired.places().await,
                        (stored, new.clone(), None, 0),
                        "{case}: no phone listed, the fingerprint recorded, nothing minted"
                    );
                    assert_eq!(paired.log().len(), writes, "{case}: no secret written");
                    assert!(
                        tokio::task::block_in_place(|| finished.paired_devices())
                            .unwrap()
                            .is_empty(),
                        "{case}: Settings lists no phone"
                    );
                    assert_eq!(
                        finished.mac_id(),
                        steno_core::json::uuid_string(new_identity.mac_id()),
                        "{case}: over the new identity"
                    );
                    recordings.assert_kept(&paired.store, false, &case);
                    let after = ListenerHandover::read(paired.source());
                    assert_eq!(after.waiting(), None, "{case}: settled");
                }
            }
        }
    }

    /// The launch over the note of an unfinished Pair again whose identity
    /// the secret store does not hold: the old identity (the note goes,
    /// the listener comes up with the phone), no identity (the note goes,
    /// it waits as missing), another identity (the note stays, it waits as
    /// replaced) or a keychain that refuses the read (the note stays, it
    /// waits as unreadable). None of them writes a secret or removes the
    /// phone.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_unfinished_pair_again_is_decided_at_the_launch() {
        let next = steno_handover::identity::hex(
            &HandoverIdentity::mint("Steno on a test", chrono::Utc::now())
                .unwrap()
                .fingerprint(),
        );
        for (stored, expected, kept) in [
            ("the old identity", None, false),
            ("no identity", Some(PhoneWaitingReason::Missing), false),
            ("another identity", Some(PhoneWaitingReason::Replaced), true),
            ("unreadable", Some(PhoneWaitingReason::Unreadable), true),
        ] {
            let paired = Paired::new(stored != "no identity").await;
            if stored == "another identity" {
                let other = HandoverIdentity::mint("Steno elsewhere", chrono::Utc::now())
                    .unwrap()
                    .to_pem()
                    .unwrap();
                paired
                    .keychain
                    .inner
                    .set_secret(&HandoverIdentity::secret_key(), Some(&other))
                    .await
                    .unwrap();
            }
            paired
                .keychain
                .refuse
                .store(stored == "unreadable", Ordering::SeqCst);
            paired.record.note_next(Some(&next)).unwrap();
            let before = paired.places().await;

            let handover = ListenerHandover::read(paired.source());
            assert_eq!(handover.waiting(), expected, "{stored}");
            let after = paired.places().await;
            assert_eq!(
                (&after.0, &after.1, after.3),
                (&before.0, &before.1, 1),
                "{stored}: nothing minted or removed"
            );
            let note = if kept { Some(next.clone()) } else { None };
            assert_eq!(after.2, note, "{stored}: the note");
            assert!(paired.log().is_empty(), "{stored}: no secret written");
        }
    }

    /// Through every stop of a Pair again and its end, nothing the phone
    /// sent is marked delivered or dropped on this computer while the phone
    /// is paired: the admitted recording keeps its ledger row, the one
    /// still being sent never gets one, and its receipt and chunks stay
    /// until the phone goes. The listener's start then clears the chunks no
    /// receipt names, as the phone sends that recording again.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn pair_again_keeps_the_admission_ledger_and_marks_nothing_delivered() {
        let paired = Paired::new(false).await;
        let recordings = paired.seed_recordings();
        let handover = ListenerHandover::read(paired.source());
        let pair_again = || tokio::task::block_in_place(|| handover.pair_again());

        paired.keychain.refuse_writes.store(true, Ordering::SeqCst);
        pair_again().unwrap_err();
        paired.keychain.refuse_writes.store(false, Ordering::SeqCst);
        recordings.assert_kept(&paired.store, true, "a refused write");

        paired.keep_the_phone();
        pair_again().unwrap_err();
        paired.let_the_phone_go();
        recordings.assert_kept(&paired.store, true, "a stop after the write");

        paired.record.refuse.store(true, Ordering::SeqCst);
        pair_again().unwrap_err();
        paired.record.refuse.store(false, Ordering::SeqCst);
        recordings.assert_kept(&paired.store, false, "a stop after the phones");

        let finished = ListenerHandover::read(paired.source());
        assert_eq!(finished.waiting(), None);
        recordings.assert_kept(&paired.store, false, "the launch that finishes");

        pair_again().unwrap();
        recordings.assert_kept(&paired.store, false, "Pair again");
        tokio::task::block_in_place(|| handover.start()).unwrap();
        assert!(!recordings.partial.exists(), "the start cleared the chunks");
        recordings.assert_kept(&paired.store, false, "the start");
        tokio::task::block_in_place(|| handover.stop());
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
        fn try_again(&self) -> Result<(), String> {
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
        let builds = Arc::new(AtomicUsize::new(0));
        let built = builds.clone();
        let gated = Arc::new(GatedHandover::new(
            paired.store.clone(),
            receiver,
            Box::new(move || {
                built.fetch_add(1, Ordering::SeqCst);
                Ok(Arc::new(ListenerHandover::read(source.clone())))
            }),
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

        // Once there, Try again reads nothing and builds no second listener.
        let reads = paired.keychain.reads.load(Ordering::SeqCst);
        tokio::task::block_in_place(|| gated.try_again()).unwrap();
        assert_eq!(paired.keychain.reads.load(Ordering::SeqCst), reads);
        assert_eq!(builds.load(Ordering::SeqCst), 1, "built once");
        assert_eq!(paired.listens.load(Ordering::SeqCst), 1, "one listener");
        drop(gate);
        if let Some(service) = gated.close() {
            service.stop().await;
        }
    }

    /// A shutdown before the gate opened: `follow` builds nothing, so no
    /// identity is read (nor a prompt brought up) during the quit, and ends
    /// without `changed`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_shutdown_before_the_build_reads_no_identity() {
        let store = Arc::new(Store::in_memory().unwrap());
        let (gate, receiver) = tokio::sync::watch::channel(HandoverGate::Pending);
        let gated = Arc::new(GatedHandover::new(
            store,
            receiver,
            Box::new(|| panic!("no identity is read after the shutdown")),
            Arc::new(FakeImport::default()),
            Box::new(|| unreachable!("no Pair again here")),
        ));
        assert!(gated.close().is_none());
        gate.send_replace(HandoverGate::Ready);
        tokio::time::timeout(
            crate::testing::PATIENCE,
            gated
                .clone()
                .follow(|| panic!("no listener is adopted after the shutdown")),
        )
        .await
        .expect("follow ends");
        assert!(gated.service().is_none());
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
                let listener = minted_listener(&listener_store, &inbox, &runtime);
                *kept.lock().unwrap() = listener.listener().cloned();
                Ok(listener)
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
