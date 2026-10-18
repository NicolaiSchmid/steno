//! The computer's side of the handover as the host and the CLI see it: one
//! object that owns the listener, the pairing session and the receipt
//! stream.

use std::sync::Arc;

use chrono::Utc;
use steno_core::{HandoverIntake, HandoverReceipt, PairedDevice, Store, StoreError, store};
use tokio::sync::{Mutex, watch};
use uuid::Uuid;

use crate::configuration::{Clock, HandoverConfiguration};
use crate::engine::Engine;
use crate::identity::HandoverIdentity;
use crate::pairing::PairingPayload;
use crate::server::{HandoverServer, ServerError, ServerMetrics};

/// Where the listener stands; core's `HandoverState` is the per-recording
/// receipt state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListenerState {
    Stopped,
    Listening { port: u16 },
    Failed(String),
}

/// [`HandoverService::checkpoint_store`] failed: the store's commits are not
/// known to be on the disk.
#[derive(Debug, thiserror::Error)]
#[error("the database could not be synced to the disk: {0}")]
pub struct StoreNotSynced(pub StoreError);

/// The computer's side of the handover: one per host, started and stopped
/// with the app.
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
    /// The launch checkpoint, run before the identity is read and the service
    /// built: [`Store::checkpoint_durably`] copies every commit into the synced
    /// database file and restarts the WAL. A retried `complete` is answered
    /// from a stored receipt with no write of its own, so an admission whose
    /// WAL sync failed must reach the disk before any phone is answered. On an
    /// error the caller builds no listener.
    pub fn checkpoint_store(store: &Store) -> Result<(), StoreNotSynced> {
        store.checkpoint_durably().map_err(StoreNotSynced)
    }

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
        let engine = Engine::new(
            configuration.clone(),
            identity.clone(),
            store,
            intake,
            receipt_updates.clone(),
            now,
        );
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

    /// Opens a pairing window, replacing any open one, and returns what the QR
    /// code shows. The secret is single use and expires after
    /// `configuration.pairing_window`.
    pub fn begin_pairing(&self) -> PairingPayload {
        self.engine.begin_pairing()
    }

    /// Closes the open pairing window, if any.
    pub fn cancel_pairing(&self) {
        self.engine.cancel_pairing();
    }

    /// Every paired phone, oldest pairing first.
    pub async fn paired_devices(&self) -> store::Result<Vec<PairedDevice>> {
        self.engine.with_store(Store::paired_devices).await
    }

    /// Forgets the phone: its next request is answered 401, and it shows as
    /// unpaired.
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
    /// chunks arrive and a recording completes.
    #[must_use]
    pub fn receipts(&self) -> watch::Receiver<Vec<HandoverReceipt>> {
        self.receipt_updates.subscribe()
    }

    // Lifecycle

    /// Sweeps orphaned inbox files, then binds the listener (and advertises
    /// when configured). Idempotent.
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

    /// Stops the listener and reports [`ListenerState::Stopped`]. Idempotent.
    /// Holds the lock across the stop, so a concurrent start binds only once
    /// the port is free.
    pub async fn stop(&self) {
        let mut guard = self.server.lock().await;
        if let Some(server) = guard.take() {
            server.stop().await;
            self.listener_states.send_replace(ListenerState::Stopped);
        }
    }
}
