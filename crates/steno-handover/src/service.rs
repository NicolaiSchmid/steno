//! The computer's side of the handover as the host and the CLI see it: one
//! object that owns the listener, the pairing session and the receipt
//! stream. The host renders `begin_pairing().url_string()` as a QR code,
//! lists `paired_devices()`, calls `revoke`, and watches `states` and
//! `receipts`. Swift: `HandoverService.swift`.

use std::sync::Arc;

use chrono::Utc;
use steno_core::{HandoverIntake, HandoverReceipt, PairedDevice, Store, store};
use tokio::sync::{Mutex, watch};
use uuid::Uuid;

use crate::configuration::{Clock, HandoverConfiguration};
use crate::engine::Engine;
use crate::identity::HandoverIdentity;
use crate::pairing::PairingPayload;
use crate::server::{HandoverServer, ServerError, ServerMetrics};

/// Where the listener stands. Named `ListenerState` because core's
/// `HandoverState` is the per-recording receipt state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListenerState {
    Stopped,
    Listening { port: u16 },
    Failed(String),
}

/// The computer's side of the handover: one per host, created with
/// [`HandoverService::new`], started and stopped with the app. The fields
/// are what the host and the tests read; the listener itself is private.
pub struct HandoverService {
    pub configuration: HandoverConfiguration,
    pub identity: Arc<HandoverIdentity>,
    pub engine: Arc<Engine>,
    pub metrics: Arc<ServerMetrics>,
    server: Mutex<Option<HandoverServer>>,
    listener_states: watch::Sender<ListenerState>,
    receipt_updates: watch::Sender<Vec<HandoverReceipt>>,
}

impl std::fmt::Debug for HandoverService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HandoverService")
            .field("configuration", &self.configuration)
            .field("state", &self.state())
            .finish_non_exhaustive()
    }
}

impl HandoverService {
    /// The service before `start`. `now` is the one time source.
    ///
    /// ```no_run
    /// use std::path::Path;
    /// use std::sync::Arc;
    ///
    /// use chrono::Utc;
    /// use steno_core::{
    ///     BoundaryResult, HandoverIntake, PairedDevice, RecordingMetadata, Store, async_trait,
    /// };
    /// use steno_handover::{HandoverConfiguration, HandoverIdentity, HandoverService};
    /// use uuid::Uuid;
    ///
    /// /// The host's intake: moves the verified file into the audio folder
    /// /// and enqueues the meeting.
    /// struct Intake;
    ///
    /// #[async_trait]
    /// impl HandoverIntake for Intake {
    ///     async fn admit(
    ///         &self,
    ///         _file: &Path,
    ///         _metadata: &RecordingMetadata,
    ///         _device: &PairedDevice,
    ///     ) -> BoundaryResult<Uuid> {
    ///         Ok(Uuid::new_v4())
    ///     }
    /// }
    ///
    /// # async fn run() -> Result<(), Box<dyn std::error::Error>> {
    /// let store = Arc::new(Store::in_memory()?);
    /// let identity = Arc::new(HandoverIdentity::mint("Steno", Utc::now())?);
    /// let configuration = HandoverConfiguration {
    ///     advertise: false,
    ///     ..HandoverConfiguration::default()
    /// };
    /// let service = HandoverService::new(
    ///     configuration,
    ///     store,
    ///     Arc::new(Intake),
    ///     identity,
    ///     Arc::new(Utc::now),
    /// );
    /// service.start().await?;
    /// let qr = service.begin_pairing().url_string();
    /// # let _ = qr;
    /// # Ok(())
    /// # }
    /// ```
    pub fn new(
        configuration: HandoverConfiguration,
        store: Arc<Store>,
        intake: Arc<dyn HandoverIntake>,
        identity: Arc<HandoverIdentity>,
        now: Clock,
    ) -> Self {
        let (listener_states, _) = watch::channel(ListenerState::Stopped);
        let (receipt_updates, _) = watch::channel(Vec::new());
        let engine = Arc::new(Engine::new(
            configuration.clone(),
            identity.clone(),
            store,
            intake,
            receipt_updates.clone(),
            now,
        ));
        HandoverService {
            configuration,
            identity,
            engine,
            metrics: Arc::new(ServerMetrics::default()),
            server: Mutex::new(None),
            listener_states,
            receipt_updates,
        }
    }

    /// [`HandoverService::new`] on the wall clock.
    pub fn with_wall_clock(
        configuration: HandoverConfiguration,
        store: Arc<Store>,
        intake: Arc<dyn HandoverIntake>,
        identity: Arc<HandoverIdentity>,
    ) -> Self {
        Self::new(configuration, store, intake, identity, Arc::new(Utc::now))
    }

    // Pairing and devices

    /// Opens a pairing window and returns what the QR code shows
    /// (`url_string`). Replaces any open session; the secret is single use
    /// and expires after `configuration.pairing_window`.
    pub fn begin_pairing(&self) -> PairingPayload {
        self.engine.begin_pairing()
    }

    /// Closes the open pairing window, if any: the QR code on screen pairs
    /// nothing from now on.
    pub fn cancel_pairing(&self) {
        self.engine.cancel_pairing();
    }

    /// Every paired phone, oldest pairing first.
    pub async fn paired_devices(&self) -> store::Result<Vec<PairedDevice>> {
        self.engine.with_store(Store::paired_devices).await
    }

    /// Forgets the phone: its next request is answered 401, which the phone
    /// shows as unpaired.
    pub async fn revoke(&self, device_id: Uuid) -> store::Result<()> {
        self.engine.revoke(device_id).await
    }

    // Observation

    #[must_use]
    pub fn state(&self) -> ListenerState {
        self.listener_states.borrow().clone()
    }

    /// The current state and every change after it.
    #[must_use]
    pub fn states(&self) -> watch::Receiver<ListenerState> {
        self.listener_states.subscribe()
    }

    /// Every handover receipt touched since start, oldest first, updated as
    /// chunks arrive and a recording completes. The UI reads it directly;
    /// `received_bytes ≈ received_chunks.len() * chunk_size`.
    #[must_use]
    pub fn receipts(&self) -> watch::Receiver<Vec<HandoverReceipt>> {
        self.receipt_updates.subscribe()
    }

    // Lifecycle

    /// Binds the listener (and advertises when configured). Idempotent.
    /// Sweeps orphaned inbox files first.
    pub async fn start(&self) -> Result<(), ServerError> {
        let mut server = self.server.lock().await;
        if server.is_some() {
            return Ok(());
        }
        self.engine.sweep_orphans().await;
        match HandoverServer::start(
            &self.configuration,
            &self.identity,
            self.engine.clone(),
            self.metrics.clone(),
        )
        .await
        {
            Ok(started) => {
                let port = started.port;
                *server = Some(started);
                self.listener_states
                    .send_replace(ListenerState::Listening { port });
                Ok(())
            }
            Err(error) => {
                self.listener_states
                    .send_replace(ListenerState::Failed(error.to_string()));
                Err(error)
            }
        }
    }

    /// Stops the listener ([`HandoverServer::stop`]) and reports
    /// [`ListenerState::Stopped`]. Idempotent.
    pub async fn stop(&self) {
        let server = self.server.lock().await.take();
        if let Some(server) = server {
            server.stop().await;
            self.listener_states.send_replace(ListenerState::Stopped);
        }
    }
}
