//! The host's `Handover` over the phone handover listener, the file the
//! identity's fingerprint is recorded in ([`FingerprintFile`]), and the
//! port it listens on ([`listener_port`], Rust only). Swift:
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
            tracing::warn!(
                "the settings could not be read; the phone handover uses its default port"
            );
            tracing::debug!(%error, "settings for the handover's port");
        })
        .ok()
        .and_then(|settings| settings.handover_port)
}

/// The app's listener configuration: the default on the port
/// [`listener_port`] picks from `environment`, the value of
/// [`PORT_VARIABLE`], and [`stored_port`].
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

#[cfg(test)]
mod tests {
    use super::*;

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

    /// The warnings `run` logs on this thread.
    fn warnings(run: impl FnOnce()) -> String {
        #[derive(Clone, Default)]
        struct Logged(Arc<Mutex<Vec<u8>>>);
        impl std::io::Write for Logged {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let logged = Logged::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer({
                let logged = logged.clone();
                move || logged.clone()
            })
            .with_ansi(false)
            .with_max_level(tracing::Level::WARN)
            .finish();
        tracing::subscriber::with_default(subscriber, run);
        String::from_utf8(logged.0.lock().unwrap().clone()).unwrap()
    }

    /// An empty variable is unset, not a value that is not a port.
    #[test]
    fn only_a_variable_that_is_not_a_port_logs_a_warning() {
        let platform = HandoverConfiguration::platform_port();
        let logged = warnings(|| {
            assert_eq!(listener_port(Some(""), None), platform);
            assert_eq!(listener_port(Some(" \n"), None), platform);
            assert_eq!(listener_port(Some("23900"), None), 23900);
        });
        assert_eq!(logged, "");
        let logged = warnings(|| assert_eq!(listener_port(Some("steno"), None), platform));
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
        assert_eq!(configuration(&store, Some("23900")).port, 23900);
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
}
