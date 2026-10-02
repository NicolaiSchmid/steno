//! The host's `Handover` over the phone handover listener.

use std::sync::Arc;

use chrono::Utc;
use steno_core::{HandoverReceipt, PairedDevice, SecretKey, SecretStore, Store, HandoverIntake};
use steno_handover::{HandoverConfiguration, HandoverIdentity, HandoverService};
use steno_host::services::{Handover, ListenerState, PairingCode};
use uuid::Uuid;

/// Loads the identity from the secret store or mints one and stores it,
/// as the Swift app kept it in the login keychain.
pub async fn load_or_mint_identity(
    secrets: &dyn SecretStore,
    common_name: &str,
) -> Result<HandoverIdentity, String> {
    let key = HandoverIdentity::secret_key();
    if let Some(bundle) = secrets.secret(&key).await.map_err(|e| e.to_string())? {
        return HandoverIdentity::from_pem(&bundle).map_err(|e| e.to_string());
    }
    let identity = HandoverIdentity::mint(common_name, Utc::now()).map_err(|e| e.to_string())?;
    secrets
        .set_secret(&key, Some(&identity.to_pem()))
        .await
        .map_err(|e| e.to_string())?;
    let _ = SecretKey::llm_api_key();
    Ok(identity)
}

/// The listener over the store and the recording intake.
pub fn service(
    store: Arc<Store>,
    intake: Arc<dyn HandoverIntake>,
    identity: HandoverIdentity,
) -> HandoverService {
    HandoverService::with_wall_clock(HandoverConfiguration::default(), store, intake, Arc::new(identity))
}

pub struct RealHandover {
    pub service: Arc<HandoverService>,
    pub mac_id: Uuid,
    pub runtime: tokio::runtime::Handle,
}

impl RealHandover {
    fn block<T>(&self, future: impl std::future::Future<Output = T>) -> T {
        tokio::task::block_in_place(|| self.runtime.block_on(future))
    }
}

impl Handover for RealHandover {
    fn state(&self) -> ListenerState {
        match self.service.state() {
            steno_handover::ListenerState::Stopped => ListenerState::Stopped,
            steno_handover::ListenerState::Listening { port } => ListenerState::Listening(port),
            steno_handover::ListenerState::Failed(reason) => ListenerState::Failed(reason),
        }
    }

    fn mac_id(&self) -> String {
        steno_core::json::uuid_string(self.mac_id)
    }

    fn paired_devices(&self) -> Result<Vec<PairedDevice>, String> {
        self.block(self.service.paired_devices())
            .map_err(|error| error.to_string())
    }

    fn start(&self) -> Result<(), String> {
        self.block(self.service.start()).map_err(|error| error.to_string())
    }

    fn stop(&self) {
        self.block(self.service.stop());
    }

    fn begin_pairing(&self) -> PairingCode {
        let payload = self.service.begin_pairing();
        PairingCode {
            expires_at: payload.expires_at,
            url_string: payload.url_string(),
        }
    }

    fn cancel_pairing(&self) {
        self.service.cancel_pairing();
    }

    fn revoke(&self, device_id: Uuid) -> Result<(), String> {
        self.block(self.service.revoke(device_id))
            .map_err(|error| error.to_string())
    }

    fn receipts(&self) -> Vec<HandoverReceipt> {
        self.service.receipts().borrow().clone()
    }
}
