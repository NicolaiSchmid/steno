//! The host's `Handover` over the phone handover listener, and the file
//! the identity's fingerprint is recorded in ([`FingerprintFile`]). Swift:
//! the handover block of `AppEnvironment.live` in
//! `apps/macos/Steno/AppEnvironment.swift`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use steno_core::protocols::BoundaryResult;
use steno_core::{HandoverIntake, HandoverReceipt, PairedDevice, Store};
use steno_handover::{FingerprintRecord, HandoverConfiguration, HandoverIdentity, HandoverService};
use steno_host::services::{Handover, ListenerState, PairingCode};
use uuid::Uuid;

use crate::block_on;
use crate::files::{Access, replace_file};

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
///
/// The listener is there from the start ([`ListenerHandover::over`]), or
/// comes later ([`ListenerHandover::waiting`]): when the keyring was still
/// asking the user as the identity was read, the app reads it again once
/// the keyring answers (`App::launch`) and hands the listener over with
/// [`ListenerHandover::set`]. Until then the handover reads as failed with
/// the reason, the paired phones come from the database, and every other
/// call that needs the listener fails or does nothing.
pub struct ListenerHandover {
    listener: OnceLock<(Arc<HandoverService>, Uuid)>,
    /// Why there is no listener yet.
    waiting: Mutex<String>,
    store: Arc<Store>,
    runtime: tokio::runtime::Handle,
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
            waiting: Mutex::new(reason),
            store,
            runtime,
        }
    }

    /// The listener, once there.
    #[must_use]
    pub fn listener(&self) -> Option<&Arc<HandoverService>> {
        self.listener.get().map(|(service, _)| service)
    }

    /// Hands over the listener a waiting handover lacked; false (and
    /// `service` unused) when it has one.
    pub fn set(&self, service: Arc<HandoverService>, mac_id: Uuid) -> bool {
        self.listener.set((service, mac_id)).is_ok()
    }

    /// Why a waiting handover still has no listener.
    pub fn still_waiting(&self, reason: String) {
        *self
            .waiting
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = reason;
    }

    fn unavailable(&self) -> String {
        format!(
            "Phone handover is unavailable: {}",
            self.waiting
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
        )
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
