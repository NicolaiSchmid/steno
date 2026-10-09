//! The host's `Handover` over the phone handover listener, and the file
//! the identity's fingerprint is recorded in ([`FingerprintFile`]). Swift:
//! the handover block of `AppEnvironment.live` in
//! `apps/macos/Steno/AppEnvironment.swift`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};

use steno_core::protocols::BoundaryResult;
use steno_core::{HandoverIntake, HandoverReceipt, PairedDevice, Store};
use steno_handover::{FingerprintRecord, HandoverConfiguration, HandoverIdentity, HandoverService};
use steno_host::services::{Handover, ListenerState, PairingCode};
use uuid::Uuid;

use crate::block_on;
use crate::files::{Access, replace_file};
use crate::swift_import::HandoverGate;

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

/// The host's `Handover` over the listener; blocks on the runtime for the
/// few async calls (paired devices, start, stop, revoke).
///
/// The listener is there from the start ([`ListenerHandover::over`]), or
/// comes later ([`ListenerHandover::waiting`]): when the keyring was still
/// asking the user as the identity was read, the app reads it again once
/// the keyring answers (`App::launch`) and hands the listener over with
/// [`ListenerHandover::set`]. Until then the handover reads as failed with
/// the reason, the paired phones come from the database, and every other
/// call that needs the listener fails or does nothing. Once the app shuts
/// down ([`ListenerHandover::close`]) no listener is handed over.
pub struct ListenerHandover {
    listener: OnceLock<(Arc<HandoverService>, Uuid)>,
    /// Why there is no listener yet, and whether the app shut down; `set`
    /// and `close` hold it throughout.
    waiting: Mutex<Waiting>,
    store: Arc<Store>,
    runtime: tokio::runtime::Handle,
}

struct Waiting {
    reason: String,
    closed: bool,
}

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
        let handover = Self::waiting(String::new(), store, runtime);
        let _ = handover.listener.set((service, mac_id));
        handover
    }

    /// A handover over `store` without its listener yet, for `reason`.
    #[must_use]
    pub fn waiting(reason: String, store: Arc<Store>, runtime: tokio::runtime::Handle) -> Self {
        ListenerHandover {
            listener: OnceLock::new(),
            waiting: Mutex::new(Waiting {
                reason,
                closed: false,
            }),
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
        let waiting = self.lock();
        if waiting.closed || self.listener.set((service, mac_id)).is_err() {
            return false;
        }
        if let Some(service) = self.listener() {
            block_on(&self.runtime, start_if_paired(service));
        }
        true
    }

    /// The app shuts down: no listener is handed over from now on; the one
    /// there, to stop.
    pub fn close(&self) -> Option<&Arc<HandoverService>> {
        self.lock().closed = true;
        self.listener()
    }

    /// Why a waiting handover still has no listener.
    pub fn still_waiting(&self, reason: String) {
        self.lock().reason = reason;
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
}

/// What `start` and `revoke` answer while the import waits.
pub const WAITING_FOR_IMPORT: &str = "Phones can upload again once Steno has brought over this Mac's phone pairing from the previous version.";

/// The host's `Handover` while the Swift import is pending
/// (`crate::swift_import`): no listener and no identity read until the
/// gate opens, then the listener built once the gate opens, which every
/// call goes to. Until then the paired phones come from the store, the
/// listener reads as stopped, and starting it or revoking a phone says
/// that it waits.
pub struct GatedHandover {
    store: Arc<Store>,
    gate: tokio::sync::watch::Receiver<HandoverGate>,
    make: MakeListener,
    listener: std::sync::OnceLock<Arc<ListenerHandover>>,
    /// Why the last build failed, until one succeeds.
    failure: tokio::sync::watch::Sender<Option<String>>,
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
    ) -> Self {
        GatedHandover {
            store,
            gate,
            make,
            listener: std::sync::OnceLock::new(),
            failure: tokio::sync::watch::Sender::new(None),
        }
    }

    /// Why the last build of the listener failed (a guard refused to
    /// mint), `None` before the first build and once one succeeded.
    #[must_use]
    pub fn failure(&self) -> tokio::sync::watch::Receiver<Option<String>> {
        self.failure.subscribe()
    }

    /// Waits for the gate to say [`HandoverGate::Ready`], then builds the
    /// listener, starts it when a phone is paired (as the launch does) and
    /// calls `changed`. Never builds a listener while the gate waits, so
    /// no identity is read or minted before the import put one in place.
    /// A build that fails (a guard refused to mint) waits for the gate to
    /// say ready again and builds again, so whatever puts the identity in
    /// place opens the listener by setting the gate once more. Returns once
    /// the listener is open, or when the gate is gone.
    pub async fn follow(self: Arc<Self>, changed: impl FnOnce() + Send + 'static) {
        let mut gate = self.gate.clone();
        let listener = loop {
            // `wait_for` marks the value it accepted as seen, so the
            // `changed` below waits for the next one.
            if gate.wait_for(|gate| gate.opens_listener()).await.is_err() {
                return;
            }
            let this = self.clone();
            let built = tokio::task::spawn_blocking(move || (this.make)()).await;
            match built.unwrap_or_else(|error| Err(error.to_string())) {
                Ok(listener) => break listener,
                Err(error) => {
                    tracing::warn!(%error, "phone handover is unavailable");
                    self.failure.send_replace(Some(error));
                    if gate.changed().await.is_err() {
                        return;
                    }
                }
            }
        };
        self.failure.send_replace(None);
        let service = listener.listener().cloned();
        let _ = self.listener.set(listener);
        if let Some(service) = service {
            start_if_paired(&service).await;
        }
        changed();
    }

    /// The listener's service, once open.
    #[must_use]
    pub fn service(&self) -> Option<Arc<HandoverService>> {
        self.listener
            .get()
            .and_then(|listener| listener.listener().cloned())
    }
}

fn waiting() -> steno_core::BoxError {
    WAITING_FOR_IMPORT.into()
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
        self.listener.get().ok_or_else(waiting)?.start()
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
        self.listener.get().ok_or_else(waiting)?.revoke(device_id)
    }

    fn receipts(&self) -> Vec<HandoverReceipt> {
        self.listener
            .get()
            .map(|listener| listener.receipts())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use steno_core::testing::FakeHandoverIntake;

    use super::*;
    use crate::swift_import::WaitReason;

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

    /// A build that fails is tried again at the next ready, not dropped:
    /// whatever puts the identity in place opens the listener by setting
    /// the gate once more.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_failed_build_is_tried_again_at_the_next_ready() {
        let store = Arc::new(Store::in_memory().unwrap());
        let (gate, receiver) = tokio::sync::watch::channel(HandoverGate::Pending);
        let (called, mut calls) = tokio::sync::mpsc::unbounded_channel();
        let attempts = AtomicUsize::new(0);
        let runtime = tokio::runtime::Handle::current();
        let listener_store = store.clone();
        let gated = Arc::new(GatedHandover::new(
            store,
            receiver,
            Box::new(move || {
                let attempt = attempts.fetch_add(1, Ordering::SeqCst);
                called.send(attempt).unwrap();
                if attempt == 0 {
                    return Err("refused to mint".to_owned());
                }
                let identity =
                    HandoverIdentity::mint("Steno on a test", chrono::Utc::now()).unwrap();
                let mac_id = identity.mac_id();
                Ok(Arc::new(ListenerHandover::over(
                    Arc::new(service(
                        HandoverConfiguration::default(),
                        listener_store.clone(),
                        Arc::new(FakeHandoverIntake::default()),
                        identity,
                    )),
                    mac_id,
                    listener_store.clone(),
                    runtime.clone(),
                )))
            }),
        ));
        let (opened, mut was_opened) = tokio::sync::oneshot::channel();
        let mut follow = tokio::spawn(gated.clone().follow(move || {
            let _ = opened.send(());
        }));

        gate.send_replace(HandoverGate::Ready);
        assert_eq!(calls.recv().await, Some(0));
        let mut failure = gated.failure();
        failure.wait_for(Option::is_some).await.unwrap();
        assert!(gated.service().is_none());
        gate.send_replace(HandoverGate::Ready);
        tokio::select! {
            call = calls.recv() => assert_eq!(call, Some(1)),
            ended = &mut follow => panic!("follow ended after the failed build: {ended:?}"),
        }
        (&mut was_opened).await.unwrap();
        follow.await.unwrap();
        assert!(gated.service().is_some());
        assert_eq!(*gated.failure().borrow(), None);
    }
}
