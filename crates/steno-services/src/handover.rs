//! The host's `Handover` over the phone handover listener, and the file
//! the identity's fingerprint is recorded in ([`FingerprintFile`]). Swift:
//! the handover block of `AppEnvironment.live` in
//! `apps/macos/Steno/AppEnvironment.swift`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

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
