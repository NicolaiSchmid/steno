//! A secret store that comes back without the handover identity, end to
//! end through the graph ([`crate::build`]) and the host's iPhone section:
//! on the Mac, the login keychain after a password reset when the user
//! chose Create New Keychain (stable plan P35). The computer had an
//! identity, its recorded fingerprint and a paired phone; the next launch
//! waits, mints nothing, keeps the record and the phone, and listens on
//! nothing. Try again brings the listener back once the same identity is
//! in the store again, and Pair again, confirmed, is the one way to a new
//! identity.
//!
//! The secrets file stands in for the Keychain: the graph reads both
//! through `SecretStore`, and `KeyringSecretStore` answers an item the new
//! keychain lacks (`keyring::Error::NoEntry`) with `None`, as the file
//! answers a key it lacks. The Mac shell's import marker is a fake that
//! counts its writes. Rust only: the Swift app has no guard.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use serde_json::Value;
use steno_bridge::{BridgeHost as _, BridgeTopic};
use steno_core::{PairedDevice, SecretStore as _};
use steno_handover::identity::hex;
use steno_handover::{FingerprintRecord as _, HandoverIdentity};
use steno_host::host::Host;
use steno_host::services::{Handover as _, ListenerState};

use crate::handover::{FingerprintFile, ListenerHandover};
use crate::secrets::FileSecretStore;
use crate::swift_import::ImportMarker;
use crate::{App, AppOptions, build};

/// The import's `swift-import-done` marker, which Pair again writes before
/// it mints; counts the writes.
#[derive(Default)]
struct Marker(AtomicUsize);

impl Marker {
    fn writes(&self) -> usize {
        self.0.load(Ordering::SeqCst)
    }
}

impl ImportMarker for Marker {
    fn mark_import_done(&self) -> Result<(), String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

/// A computer whose phone paired with the identity its first launch
/// minted, and what the guard must keep: the identity's bundle, its
/// recorded fingerprint as the file holds it, and the paired phones.
struct Paired {
    dir: tempfile::TempDir,
    pem: String,
    fingerprint: String,
    record: Vec<u8>,
    mac_id: String,
    phones: Vec<PairedDevice>,
    marker: Arc<Marker>,
    /// What Pair again's prompt answers.
    confirm: Arc<AtomicBool>,
}

impl Paired {
    /// The first launch mints the identity and records its fingerprint; a
    /// phone pairs; the app quits.
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let marker = Arc::new(Marker::default());
        let confirm = Arc::new(AtomicBool::new(false));
        let mut paired = Paired {
            dir,
            pem: String::new(),
            fingerprint: String::new(),
            record: Vec::new(),
            mac_id: String::new(),
            phones: Vec::new(),
            marker,
            confirm,
        };
        let first = build(paired.options()).unwrap();
        let handover = first.handover.as_deref().unwrap();
        assert!(handover.listener().is_some(), "minted at the first launch");
        paired.mac_id = handover.mac_id();
        crate::testing::pair_a_phone(&first.store);
        paired.phones = first.store.paired_devices().unwrap();
        first.shutdown();
        drop(first);
        paired.pem = paired.stored().await.expect("the minted identity");
        paired.fingerprint = hex(&HandoverIdentity::from_pem(&paired.pem)
            .unwrap()
            .fingerprint());
        paired.record = std::fs::read(paired.record_path()).unwrap();
        assert_eq!(
            paired.recorded().as_deref(),
            Some(paired.fingerprint.as_str())
        );
        paired
    }

    fn support(&self) -> std::path::PathBuf {
        self.dir.path().join("support")
    }

    fn options(&self) -> AppOptions {
        AppOptions {
            import_marker: Some(self.marker.clone()),
            ..crate::testing::options_under(&self.support())
        }
    }

    /// The secret store the graph reads, as the test reads and writes it.
    fn keychain(&self) -> FileSecretStore {
        FileSecretStore::in_support_directory(&self.support())
    }

    async fn stored(&self) -> Option<String> {
        self.keychain()
            .secret(&HandoverIdentity::secret_key())
            .await
            .unwrap()
    }

    async fn put(&self, pem: &str) {
        self.keychain()
            .set_secret(&HandoverIdentity::secret_key(), Some(pem))
            .await
            .unwrap();
    }

    /// Create New Keychain: the new keychain holds none of the old items.
    fn empty_the_keychain(&self) {
        std::fs::remove_file(self.support().join("secrets.json")).unwrap();
    }

    fn record_path(&self) -> std::path::PathBuf {
        self.support().join("handover-identity.json")
    }

    fn recorded(&self) -> Option<String> {
        FingerprintFile::in_support_directory(&self.support())
            .recorded()
            .unwrap()
    }

    /// The next launch, and its host with Pair again's prompt answering
    /// [`Self::confirm`].
    fn launch(&self) -> Launched {
        let app = build(self.options()).unwrap();
        let confirm = self.confirm.clone();
        let host = app.host().unwrap().with_dialogs(
            Box::new(move |_| confirm.load(Ordering::SeqCst)),
            Box::new(|_| None),
        );
        Launched { app, host }
    }

    /// The handover waits for `reason` and mints nothing: the store holds
    /// `stored` (none, or the identity found there), no listener is built
    /// (so nothing listens), and the record, the phone and the marker are
    /// as [`Self::kept`] says.
    async fn waits_untouched(&self, launched: &Launched, reason: &str, stored: Option<&str>) {
        let handover = launched.handover();
        assert!(handover.listener().is_none(), "nothing listens");
        assert!(matches!(handover.state(), ListenerState::Failed(_)));
        assert_eq!(handover.mac_id(), "");
        let phone = launched.phone();
        assert_eq!(phone["waiting"]["reason"], reason, "{phone}");
        assert_eq!(phone["devices"].as_array().unwrap().len(), 1, "{phone}");
        assert_eq!(self.stored().await.as_deref(), stored, "nothing minted");
        self.kept(launched);
    }

    /// The listener is back on the identity the phone paired with, started
    /// for the paired phone, and the record, the phone and the marker are
    /// as [`Self::kept`] says.
    async fn listens_on_the_known_identity(&self, launched: &Launched) {
        let handover = launched.handover();
        assert!(
            matches!(handover.state(), ListenerState::Listening(_)),
            "{:?}",
            handover.state()
        );
        assert_eq!(
            handover.mac_id(),
            self.mac_id,
            "the identity the phone knows"
        );
        let phone = launched.phone();
        assert_eq!(phone["waiting"], Value::Null, "{phone}");
        assert_eq!(self.stored().await.as_deref(), Some(self.pem.as_str()));
        self.kept(launched);
    }

    /// The record file is the one the phone's pairing left, byte for byte,
    /// the phone is still paired, and no marker was written.
    fn kept(&self, launched: &Launched) {
        assert_eq!(std::fs::read(self.record_path()).unwrap(), self.record);
        assert_eq!(launched.app.store.paired_devices().unwrap(), self.phones);
        assert_eq!(self.marker.writes(), 0, "no marker");
    }
}

struct Launched {
    app: App,
    host: Host,
}

impl Launched {
    fn handover(&self) -> &ListenerHandover {
        self.app.handover.as_deref().unwrap()
    }

    fn phone(&self) -> Value {
        tokio::task::block_in_place(|| self.host.snapshot(BridgeTopic::SettingsPhone).unwrap())
    }

    fn try_again(&self) {
        tokio::task::block_in_place(|| self.host.settings_phone_try_again()).unwrap();
    }

    fn pair_again(&self) -> bool {
        tokio::task::block_in_place(|| self.host.settings_phone_pair_again())
            .unwrap()
            .confirmed
    }
}

impl Drop for Launched {
    fn drop(&mut self) {
        self.app.shutdown();
    }
}

/// The identity is gone from the store: the launch waits with `missing`,
/// and so does Try again while the store is still empty. Pair again that
/// the user declines changes nothing. Once the item is back, Try again
/// starts the listener on the same identity without a restart, and the
/// phone stays paired.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_emptied_keychain_waits_until_the_identity_is_back() {
    let paired = Paired::new().await;
    paired.empty_the_keychain();

    let launched = paired.launch();
    paired.waits_untouched(&launched, "missing", None).await;

    launched.try_again();
    paired.waits_untouched(&launched, "missing", None).await;
    let phone = launched.phone();
    assert_eq!(
        phone["error"],
        steno_host::settings::phones::TRY_AGAIN_FAILED,
        "{phone}"
    );

    assert!(!launched.pair_again(), "the user said no");
    paired.waits_untouched(&launched, "missing", None).await;

    paired.put(&paired.pem).await;
    launched.try_again();
    paired.listens_on_the_known_identity(&launched).await;
}

/// Pair again over an emptied store, once the user confirms the warning:
/// the import's marker first, then a new identity in the store with its
/// fingerprint recorded over the old one. The paired phones are removed,
/// as each must pair again (`HandoverIdentity::pair_again`), and the
/// listener comes up on the new identity, stopped until a phone pairs.
/// The next launch loads that identity and waits for nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pair_again_over_an_emptied_keychain_mints_once_the_user_confirms() {
    let paired = Paired::new().await;
    paired.empty_the_keychain();
    let launched = paired.launch();
    paired.waits_untouched(&launched, "missing", None).await;

    paired.confirm.store(true, Ordering::SeqCst);
    assert!(launched.pair_again());
    assert_eq!(paired.marker.writes(), 1, "the marker");
    let minted = HandoverIdentity::from_pem(&paired.stored().await.unwrap()).unwrap();
    let mac_id = steno_core::json::uuid_string(minted.mac_id());
    let fingerprint = hex(&minted.fingerprint());
    assert_ne!(fingerprint, paired.fingerprint, "a new identity");
    assert_eq!(paired.recorded(), Some(fingerprint));
    assert_eq!(launched.app.store.paired_devices().unwrap(), []);
    let handover = launched.handover();
    assert_eq!(handover.mac_id(), mac_id);
    assert_eq!(handover.state(), ListenerState::Stopped, "no phone paired");
    let phone = launched.phone();
    assert_eq!(phone["waiting"], Value::Null, "{phone}");
    assert_eq!(phone["devices"], serde_json::json!([]), "{phone}");
    drop(launched);

    let again = paired.launch();
    let handover = again.handover();
    assert!(handover.listener().is_some());
    assert_eq!(handover.waiting(), None);
    assert_eq!(handover.mac_id(), mac_id);
}

/// The store holds another identity than the one whose fingerprint is
/// recorded (a keychain restored from elsewhere, or another build's
/// mint): the launch waits with `replaced`, keeps that identity as it
/// found it, and neither adopts it nor mints; Try again and a declined
/// Pair again keep it so. Once the identity the phone knows is back, Try
/// again starts the listener on it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn another_identity_in_the_keychain_waits_until_the_known_one_is_back() {
    let paired = Paired::new().await;
    let other = HandoverIdentity::mint("Steno on another computer", chrono::Utc::now())
        .unwrap()
        .to_pem()
        .unwrap();
    paired.put(&other).await;

    let launched = paired.launch();
    paired
        .waits_untouched(&launched, "replaced", Some(&other))
        .await;
    launched.try_again();
    paired
        .waits_untouched(&launched, "replaced", Some(&other))
        .await;
    assert!(!launched.pair_again());
    paired
        .waits_untouched(&launched, "replaced", Some(&other))
        .await;

    paired.put(&paired.pem).await;
    launched.try_again();
    paired.listens_on_the_known_identity(&launched).await;
}
