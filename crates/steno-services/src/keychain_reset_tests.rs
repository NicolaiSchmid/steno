//! A secret store that comes back without the handover identity, end to
//! end through the graph ([`crate::build`]) and the host's iPhone section:
//! on the Mac, the login keychain after a password reset when the user
//! chose Create New Keychain (stable plan P35). The computer had an
//! identity, its recorded fingerprint and two paired phones; the next
//! launch waits, mints nothing, keeps the record and the phones, and
//! listens on nothing. Try again brings the listener back once the same
//! identity is in the store again, and Pair again, confirmed, is the one
//! way to a new identity.
//!
//! The secrets file stands in for the Keychain: the graph reads both
//! through `SecretStore`, and `KeyringSecretStore` answers an item the new
//! keychain lacks (`keyring::Error::NoEntry`) with `None`, as the file
//! answers a key it lacks. Any other Keychain error (access denied, a
//! cancelled prompt, a locked keychain) is an `Err`, which waits as
//! `unreadable`; `handover.rs` and `app.rs` test that answer. The Mac
//! shell's import marker is a fake that counts its writes. The Swift app's
//! guard is its own change, with its own record file.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use serde_json::{Value, json};
use steno_bridge::{BridgeHost as _, BridgeTopic};
use steno_core::json::uuid_string;
use steno_core::{PairedDevice, SecretStore as _};
use steno_handover::identity::hex;
use steno_handover::{FingerprintRecord as _, HandoverIdentity};
use steno_host::host::Host;
use steno_host::services::{Handover as _, ListenerState};
use steno_host::settings::phones::TRY_AGAIN_FAILED;

use crate::handover::{FingerprintFile, ListenerHandover};
use crate::secrets::FileSecretStore;
use crate::swift_import::ImportMarker;
use crate::testing::{PHONE_TOKEN_HASH, options_under, pair_a_phone};
use crate::{App, AppOptions, build};

/// The second phone's token hash, beside [`PHONE_TOKEN_HASH`].
const SECOND_PHONE_TOKEN_HASH: [u8; 32] = [3; 32];

/// The import's `swift-import-done` marker, which Pair again writes;
/// counts the writes.
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

/// Pair again's prompt: what it answers, and how often it was shown.
#[derive(Default)]
struct Prompt {
    answer: AtomicBool,
    shown: AtomicUsize,
}

/// A computer whose two phones paired with the identity its first launch
/// minted, and what the guard must keep: the identity's bundle, its
/// recorded fingerprint as the file holds it, and the paired phones, each
/// with its token hash.
struct Paired {
    dir: tempfile::TempDir,
    pem: String,
    fingerprint: String,
    record: Vec<u8>,
    mac_id: String,
    /// Every paired phone's row, as `paired_devices` lists them.
    phones: Vec<PairedDevice>,
    /// Each token hash and the phone it authenticates.
    tokens: Vec<([u8; 32], PairedDevice)>,
    marker: Arc<Marker>,
    prompt: Arc<Prompt>,
}

impl Paired {
    /// The first launch mints the identity and records its fingerprint; two
    /// phones pair; the app quits.
    async fn new() -> Self {
        let mut paired = Paired {
            dir: tempfile::tempdir().unwrap(),
            pem: String::new(),
            fingerprint: String::new(),
            record: Vec::new(),
            mac_id: String::new(),
            phones: Vec::new(),
            tokens: Vec::new(),
            marker: Arc::default(),
            prompt: Arc::default(),
        };
        let first = build(paired.options()).unwrap();
        let handover = first.handover.as_deref().unwrap();
        assert!(handover.listener().is_some(), "minted at the first launch");
        paired.mac_id = handover.mac_id();
        pair_a_phone(&first.store);
        let second = PairedDevice {
            id: uuid::Uuid::new_v4(),
            name: "Second phone".to_owned(),
            paired_at: chrono::Utc::now(),
            last_seen_at: None,
        };
        first
            .store
            .save_paired_device(&second, &SECOND_PHONE_TOKEN_HASH)
            .unwrap();
        paired.phones = first.store.paired_devices().unwrap();
        assert_eq!(paired.phones.len(), 2, "two phones");
        paired.tokens = [PHONE_TOKEN_HASH, SECOND_PHONE_TOKEN_HASH]
            .into_iter()
            .map(|token| {
                let phone = first.store.paired_device_for_token_hash(&token).unwrap();
                (token, phone.expect("the phone's token"))
            })
            .collect();
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
            ..options_under(&self.support())
        }
    }

    /// The secrets file, the Keychain's stand-in, as the graph reads it.
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
    /// [`Self::prompt`].
    fn launch(&self) -> Launched {
        let app = build(self.options()).unwrap();
        let prompt = self.prompt.clone();
        let host = app.host().unwrap().with_dialogs(
            Box::new(move |_| {
                prompt.shown.fetch_add(1, Ordering::SeqCst);
                prompt.answer.load(Ordering::SeqCst)
            }),
            Box::new(|_| None),
        );
        Launched {
            app,
            host,
            prompt: self.prompt.clone(),
        }
    }

    /// The handover waits for `reason` and mints nothing: the store holds
    /// `stored` (none, or the identity found there), no listener is built
    /// (so nothing listens), and the record, the phones and the marker are
    /// as [`Self::kept`] says.
    async fn waits_untouched(&self, launched: &Launched, reason: &str, stored: Option<&str>) {
        let handover = launched.handover();
        assert!(handover.listener().is_none(), "nothing listens");
        assert!(
            matches!(handover.state(), ListenerState::Failed(_)),
            "{:?}",
            handover.state()
        );
        assert_eq!(handover.mac_id(), "", "no mac id while waiting");
        let phone = launched.phone();
        assert_eq!(phone["waiting"]["reason"], reason, "{phone}");
        assert_eq!(
            phone["devices"].as_array().unwrap().len(),
            self.phones.len(),
            "{phone}"
        );
        assert_eq!(self.stored().await.as_deref(), stored, "nothing minted");
        self.kept(launched);
    }

    /// The listener is back on the identity the phones paired with, started
    /// for the paired phones, no error shows, and the record, the phones
    /// and the marker are as [`Self::kept`] says.
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
            "the identity the phones know"
        );
        let phone = launched.phone();
        assert_eq!(phone["waiting"], Value::Null, "{phone}");
        assert_eq!(phone["error"], Value::Null, "{phone}");
        assert_eq!(
            self.stored().await.as_deref(),
            Some(self.pem.as_str()),
            "the known identity, unchanged"
        );
        self.kept(launched);
    }

    /// A restart after the identity came back: the launch loads it and
    /// waits for nothing, and the record, the phones and the marker are as
    /// [`Self::kept`] says.
    async fn relaunch_loads_the_known_identity(&self) {
        let again = self.launch();
        let handover = again.handover();
        assert!(handover.listener().is_some(), "loaded at the launch");
        assert_eq!(handover.waiting(), None, "waits for nothing");
        assert_eq!(
            handover.mac_id(),
            self.mac_id,
            "the identity the phones know"
        );
        let phone = again.phone();
        assert_eq!(phone["waiting"], Value::Null, "{phone}");
        assert_eq!(
            self.stored().await.as_deref(),
            Some(self.pem.as_str()),
            "the known identity, unchanged"
        );
        self.kept(&again);
    }

    /// The record file is the one the phones' pairing left, byte for byte,
    /// every phone is still paired, each row and token hash unchanged, and
    /// no marker was written.
    fn kept(&self, launched: &Launched) {
        let store = &launched.app.store;
        assert_eq!(
            std::fs::read(self.record_path()).unwrap(),
            self.record,
            "the record, byte for byte"
        );
        assert_eq!(
            store.paired_devices().unwrap(),
            self.phones,
            "the paired phones"
        );
        for (token, phone) in &self.tokens {
            assert_eq!(
                store.paired_device_for_token_hash(token).unwrap().as_ref(),
                Some(phone),
                "the phone's token"
            );
        }
        assert_eq!(self.marker.writes(), 0, "no marker");
    }
}

struct Launched {
    app: App,
    host: Host,
    prompt: Arc<Prompt>,
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

    /// Pair again, which shows the prompt once; what the user answered.
    fn pair_again(&self) -> bool {
        let shown = self.prompt.shown.load(Ordering::SeqCst);
        let confirmed = tokio::task::block_in_place(|| self.host.settings_phone_pair_again())
            .unwrap()
            .confirmed;
        assert_eq!(
            self.prompt.shown.load(Ordering::SeqCst),
            shown + 1,
            "the prompt was shown"
        );
        confirmed
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
/// starts the listener on the same identity without a restart, the
/// phones stay paired, and the next launch loads that identity.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_emptied_keychain_waits_until_the_identity_is_back() {
    let paired = Paired::new().await;
    paired.empty_the_keychain();

    let launched = paired.launch();
    paired.waits_untouched(&launched, "missing", None).await;

    launched.try_again();
    paired.waits_untouched(&launched, "missing", None).await;
    let phone = launched.phone();
    assert_eq!(phone["error"], TRY_AGAIN_FAILED, "{phone}");

    assert!(!launched.pair_again(), "the user said no");
    paired.waits_untouched(&launched, "missing", None).await;

    paired.put(&paired.pem).await;
    launched.try_again();
    paired.listens_on_the_known_identity(&launched).await;
    drop(launched);
    paired.relaunch_loads_the_known_identity().await;
}

/// Pair again over an emptied store, once the user confirms the warning,
/// ends with the import's marker written, a new identity in the store with
/// its fingerprint recorded over the old one, and no paired phone, as each
/// must pair again (`HandoverIdentity::pair_again`); the listener is up on
/// the new identity, stopped until a phone pairs. The next launch loads
/// that identity and waits for nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pair_again_over_an_emptied_keychain_mints_once_the_user_confirms() {
    let paired = Paired::new().await;
    paired.empty_the_keychain();
    let launched = paired.launch();
    paired.waits_untouched(&launched, "missing", None).await;

    paired.prompt.answer.store(true, Ordering::SeqCst);
    assert!(launched.pair_again(), "the user said yes");
    assert_eq!(paired.marker.writes(), 1, "the marker");
    let minted = HandoverIdentity::from_pem(&paired.stored().await.unwrap()).unwrap();
    let mac_id = uuid_string(minted.mac_id());
    let fingerprint = hex(&minted.fingerprint());
    assert_ne!(fingerprint, paired.fingerprint, "a new identity");
    assert_eq!(paired.recorded(), Some(fingerprint), "the new fingerprint");
    let store = &launched.app.store;
    assert_eq!(store.paired_devices().unwrap(), [], "no paired phone");
    for (token, _) in &paired.tokens {
        assert_eq!(store.paired_device_for_token_hash(token).unwrap(), None);
    }
    let handover = launched.handover();
    assert_eq!(handover.mac_id(), mac_id, "the new identity");
    assert_eq!(handover.state(), ListenerState::Stopped, "no phone paired");
    let phone = launched.phone();
    assert_eq!(phone["waiting"], Value::Null, "{phone}");
    assert_eq!(phone["devices"], json!([]), "{phone}");
    drop(launched);

    let again = paired.launch();
    let handover = again.handover();
    assert!(handover.listener().is_some(), "loaded at the launch");
    assert_eq!(handover.waiting(), None, "waits for nothing");
    assert_eq!(handover.mac_id(), mac_id, "the new identity");
}

/// The store holds another identity than the one whose fingerprint is
/// recorded (a keychain restored from elsewhere, or another build's
/// mint): the launch waits with `replaced`, keeps that identity as it
/// found it, and neither adopts it nor mints; Try again and a declined
/// Pair again keep it so. Once the identity the phones know is back, Try
/// again starts the listener on it, and the next launch loads it.
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
    assert!(!launched.pair_again(), "the user said no");
    paired
        .waits_untouched(&launched, "replaced", Some(&other))
        .await;

    paired.put(&paired.pem).await;
    launched.try_again();
    paired.listens_on_the_known_identity(&launched).await;
    drop(launched);
    paired.relaunch_loads_the_known_identity().await;
}
