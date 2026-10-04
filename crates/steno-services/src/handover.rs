//! The host's `Handover` over the phone handover listener. Swift: the
//! handover block of `AppEnvironment.live` in
//! `apps/macos/Steno/AppEnvironment.swift`.

use std::sync::Arc;

use steno_core::protocols::BoundaryResult;
use steno_core::{HandoverIntake, HandoverReceipt, PairedDevice, Store};
use steno_handover::{HandoverConfiguration, HandoverIdentity, HandoverService};
use steno_host::services::{Handover, ListenerState, PairingCode};
use uuid::Uuid;

use crate::block_on;

/// The listener over the store and the recording intake.
pub fn service(
    store: Arc<Store>,
    intake: Arc<dyn HandoverIntake>,
    identity: HandoverIdentity,
) -> HandoverService {
    HandoverService::with_wall_clock(
        HandoverConfiguration::default(),
        store,
        intake,
        Arc::new(identity),
    )
}

/// The host's `Handover` over the listener; blocks on the runtime for the
/// few async calls (paired devices, start, stop, revoke).
pub struct ListenerHandover {
    pub service: Arc<HandoverService>,
    pub mac_id: Uuid,
    pub runtime: tokio::runtime::Handle,
}

impl Handover for ListenerHandover {
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

    fn paired_devices(&self) -> BoundaryResult<Vec<PairedDevice>> {
        Ok(block_on(&self.runtime, self.service.paired_devices())?)
    }

    fn start(&self) -> BoundaryResult<()> {
        Ok(block_on(&self.runtime, self.service.start())?)
    }

    fn stop(&self) {
        block_on(&self.runtime, self.service.stop());
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

    fn revoke(&self, device_id: Uuid) -> BoundaryResult<()> {
        Ok(block_on(&self.runtime, self.service.revoke(device_id))?)
    }

    fn receipts(&self) -> Vec<HandoverReceipt> {
        self.service.receipts().borrow().clone()
    }
}
