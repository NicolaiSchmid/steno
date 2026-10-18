//! A secret store that comes back without the handover identity (the Mac's
//! login keychain after a password reset), through the graph and the
//! host's iPhone section: the launch waits, mints nothing and keeps the
//! record and the phones; Pair again, confirmed, is the one way to a new
//! identity. The secrets file stands in for the Keychain.

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
use crate::testing::{Admitted, admit_a_recording, options_under};
use crate::{App, AppOptions, build};

/// The two phones' token hashes.
const TOKEN_HASHES: [[u8; 32]; 2] = [[1; 32], [3; 32]];

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
/// minted, and what the guard must keep.
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
    /// The first phone's recording, with its row in the admission ledger.
    admitted: Admitted,
    marker: Arc<Marker>,
    prompt: Arc<Prompt>,
}

impl Paired {
    /// The first launch mints the identity; two phones pair and the first
    /// delivers a recording; the app quits.
    async fn new() -> Self {
        let mut paired = Paired {
            dir: tempfile::tempdir().unwrap(),
            pem: String::new(),
            fingerprint: String::new(),
            record: Vec::new(),
            mac_id: String::new(),
            phones: Vec::new(),
            tokens: Vec::new(),
            admitted: Admitted {
                recording: uuid::Uuid::nil(),
                meeting: uuid::Uuid::nil(),
            },
            marker: Arc::default(),
            prompt: Arc::default(),
        };
        let first = build(paired.options()).unwrap();
        let handover = first.handover.as_deref().unwrap();
        assert!(handover.listener().is_some(), "minted at the first launch");
        paired.mac_id = handover.mac_id();
        for (name, token) in ["First phone", "Second phone"]
            .into_iter()
            .zip(TOKEN_HASHES)
        {
            let phone = PairedDevice {
                id: uuid::Uuid::new_v4(),
                name: name.to_owned(),
                paired_at: chrono::Utc::now(),
                last_seen_at: None,
            };
            first.store.save_paired_device(&phone, &token).unwrap();
        }
        paired.phones = first.store.paired_devices().unwrap();
        assert_eq!(paired.phones.len(), 2, "two phones");
        paired.tokens = TOKEN_HASHES
            .into_iter()
            .map(|token| {
                let phone = first.store.paired_device_for_token_hash(&token).unwrap();
                (token, phone.expect("the phone's token"))
            })
            .collect();
        paired.admitted = admit_a_recording(&first.store);
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

    fn secrets_path(&self) -> std::path::PathBuf {
        self.support().join("secrets.json")
    }

    /// Create New Keychain: the new keychain holds none of the old items.
    fn empty_the_keychain(&self) {
        std::fs::remove_file(self.secrets_path()).unwrap();
    }

    fn record_path(&self) -> std::path::PathBuf {
        self.support().join("handover-identity.json")
    }

    fn record_file(&self) -> FingerprintFile {
        FingerprintFile::in_support_directory(&self.support())
    }

    fn recorded(&self) -> Option<String> {
        self.record_file().recorded().unwrap()
    }

    /// The next fingerprint an unfinished Pair again noted.
    fn noted(&self) -> Option<String> {
        self.record_file().next().unwrap()
    }

    /// Notes `next` as an unfinished Pair again does; returns the record's
    /// bytes from before the note.
    fn note_next(&mut self, next: &str) -> Vec<u8> {
        self.record_file().note_next(Some(next)).unwrap();
        let noted = std::fs::read(self.record_path()).unwrap();
        std::mem::replace(&mut self.record, noted)
    }

    /// The copies a write set aside beside the record, by name.
    fn set_aside(&self) -> Vec<(String, Vec<u8>)> {
        let mut aside: Vec<_> = std::fs::read_dir(self.support())
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter_map(|path| {
                let name = path.file_name()?.to_string_lossy().into_owned();
                name.starts_with("handover-identity.json.corrupt-")
                    .then(|| (name, std::fs::read(&path).unwrap()))
            })
            .collect();
        aside.sort();
        aside
    }

    /// The next launch, with Pair again's prompt answering [`Self::prompt`].
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
    /// `stored`, nothing listens, and the rest is as [`Self::kept`] says.
    async fn waits_untouched(&self, launched: &Launched, reason: &str, stored: Option<&str>) {
        self.waits(launched, reason);
        assert_eq!(self.stored().await.as_deref(), stored, "nothing minted");
        self.kept(launched);
    }

    /// The handover waits for `reason`: no listener, no mac id, and the card
    /// says why and lists every phone.
    fn waits(&self, launched: &Launched, reason: &str) {
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
    }

    /// The listener is back on the identity the phones paired with, and the
    /// rest is as [`Self::kept`] says.
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

    /// The launch loaded the identity the phones paired with and waits for
    /// nothing, and the rest is as [`Self::kept`] says.
    async fn loads_the_known_identity(&self, launched: &Launched) {
        launched.loaded(&self.mac_id);
        let phone = launched.phone();
        assert_eq!(phone["waiting"], Value::Null, "{phone}");
        assert_eq!(
            self.stored().await.as_deref(),
            Some(self.pem.as_str()),
            "the known identity, unchanged"
        );
        self.kept(launched);
    }

    /// Pair again went through: no phone is paired, the card lists none, and
    /// the admission ledger keeps the delivered recording.
    fn forgot_the_phones(&self, launched: &Launched, case: &str) {
        let store = &launched.app.store;
        assert_eq!(store.paired_devices().unwrap(), [], "{case}: no phone");
        for (token, _) in &self.tokens {
            assert_eq!(
                store.paired_device_for_token_hash(token).unwrap(),
                None,
                "{case}: no phone for its token"
            );
        }
        assert!(
            self.admitted.is_unchanged(store),
            "{case}: the admission ledger"
        );
        let phone = launched.phone();
        assert_eq!(phone["waiting"], Value::Null, "{case}: {phone}");
        assert_eq!(phone["devices"], json!([]), "{case}: {phone}");
    }

    /// The record file is byte for byte the pairing's (with any note), every
    /// phone is still paired, the ledger keeps the recording, and no marker
    /// was written.
    fn kept(&self, launched: &Launched) {
        let store = &launched.app.store;
        assert_eq!(
            std::fs::read(self.record_path()).ok().as_ref(),
            Some(&self.record),
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
        assert!(self.admitted.is_unchanged(store), "the admission ledger");
        assert_eq!(self.marker.writes(), 0, "no marker");
    }
}

/// One launch of the graph and its host; shuts the app down when dropped.
struct Launched {
    app: App,
    host: Host,
    prompt: Arc<Prompt>,
}

impl Launched {
    fn handover(&self) -> &ListenerHandover {
        self.app.handover.as_deref().unwrap()
    }

    /// The launch loaded the identity `mac_id` names and waits for nothing.
    fn loaded(&self, mac_id: &str) {
        let handover = self.handover();
        assert!(handover.listener().is_some(), "loaded at the launch");
        assert_eq!(handover.waiting(), None, "waits for nothing");
        assert_eq!(handover.mac_id(), mac_id, "the identity loaded");
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

/// The record's text when `fingerprint` alone is recorded.
fn record_of(fingerprint: &str) -> String {
    format!("{{\n  \"{}\": \"{fingerprint}\"\n}}", FingerprintFile::KEY)
}

/// An identity minted elsewhere, as a PEM bundle.
fn another_identity() -> String {
    HandoverIdentity::mint("Steno on another computer", chrono::Utc::now())
        .unwrap()
        .to_pem()
        .unwrap()
}

/// The identity is gone: the launch and Try again wait with `missing`, a
/// declined Pair again changes nothing, and once the item is back Try
/// again starts the listener on the same identity.
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
    paired.loads_the_known_identity(&paired.launch()).await;
}

/// Pair again over an emptied store, confirmed, writes the import's
/// marker, mints and records a new identity, and unpairs every phone; the
/// next launch loads that identity.
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
    assert_eq!(
        paired.recorded().as_ref(),
        Some(&fingerprint),
        "the new fingerprint"
    );
    paired.forgot_the_phones(&launched, "Pair again");
    let handover = launched.handover();
    assert_eq!(handover.mac_id(), mac_id, "the new identity");
    assert_eq!(handover.state(), ListenerState::Stopped, "no phone paired");
    drop(launched);

    let again = paired.launch();
    again.loaded(&mac_id);
    assert_eq!(paired.recorded(), Some(fingerprint), "the new fingerprint");
    assert_eq!(
        again.app.store.paired_devices().unwrap(),
        [],
        "no paired phone"
    );
}

/// The store holds another identity than the recorded one: the launch
/// waits with `replaced`, neither adopting it nor minting, until the
/// identity the phones know is back.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn another_identity_in_the_keychain_waits_until_the_known_one_is_back() {
    let paired = Paired::new().await;
    let other = another_identity();
    paired.put(&other).await;

    let launched = paired.launch();
    paired
        .waits_untouched(&launched, "replaced", Some(&other))
        .await;
    launched.try_again();
    paired
        .waits_untouched(&launched, "replaced", Some(&other))
        .await;
    let phone = launched.phone();
    assert_eq!(phone["error"], TRY_AGAIN_FAILED, "{phone}");
    assert!(!launched.pair_again(), "the user said no");
    paired
        .waits_untouched(&launched, "replaced", Some(&other))
        .await;

    paired.put(&paired.pem).await;
    launched.try_again();
    paired.listens_on_the_known_identity(&launched).await;
    drop(launched);
    paired.loads_the_known_identity(&paired.launch()).await;
}

/// A launch that finds the note of an unfinished Pair again decides it by
/// what the keychain holds: the next identity finishes it, the recorded
/// one or none abandons it, another identity or an unreadable keychain
/// keeps the note and waits. None writes the marker.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unfinished_pair_again_is_decided_by_the_keychain_at_the_launch() {
    for case in ["next", "recorded", "none", "other", "unreadable"] {
        let mut paired = Paired::new().await;
        let next = HandoverIdentity::mint("Steno on this computer", chrono::Utc::now()).unwrap();
        let next_fingerprint = hex(&next.fingerprint());
        let other = another_identity();
        match case {
            "next" => paired.put(&next.to_pem().unwrap()).await,
            "none" => paired.empty_the_keychain(),
            "other" => paired.put(&other).await,
            "recorded" | "unreadable" => {}
            _ => unreachable!(),
        }
        let unnoted = paired.note_next(&next_fingerprint);
        if case == "unreadable" {
            std::fs::write(paired.secrets_path(), b"{").unwrap();
        }

        let launched = paired.launch();
        match case {
            "next" => {
                launched.loaded(&uuid_string(next.mac_id()));
                assert_eq!(
                    paired.stored().await,
                    Some(next.to_pem().unwrap()),
                    "{case}: nothing minted"
                );
                assert_eq!(
                    std::fs::read_to_string(paired.record_path()).unwrap(),
                    record_of(&next_fingerprint),
                    "{case}: its fingerprint recorded, the note dropped"
                );
                paired.forgot_the_phones(&launched, case);
                assert_eq!(paired.marker.writes(), 0, "{case}: no marker");
            }
            "recorded" => {
                paired.record = unnoted;
                assert_eq!(paired.noted(), None, "{case}: the note goes");
                paired.loads_the_known_identity(&launched).await;
            }
            "none" => {
                paired.record = unnoted;
                assert_eq!(paired.noted(), None, "{case}: the note goes");
                paired.waits_untouched(&launched, "missing", None).await;
            }
            "other" => {
                assert_eq!(
                    paired.noted(),
                    Some(next_fingerprint),
                    "{case}: the note stays"
                );
                paired
                    .waits_untouched(&launched, "replaced", Some(&other))
                    .await;
            }
            "unreadable" => {
                assert_eq!(
                    paired.noted(),
                    Some(next_fingerprint),
                    "{case}: the note stays"
                );
                paired.waits(&launched, "unreadable");
                assert_eq!(
                    std::fs::read(paired.secrets_path()).unwrap(),
                    b"{",
                    "{case}: nothing written"
                );
                paired.kept(&launched);
            }
            _ => unreachable!(),
        }
    }
}

/// A record file that does not parse is never read as nothing recorded:
/// the launch waits as unreadable and nothing writes the file, until Pair
/// again, confirmed, moves it aside and records the new identity alone.
/// Mended instead, Try again starts the listener without a restart.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_record_with_a_value_that_is_not_a_string_waits_until_pair_again_sets_it_aside() {
    for case in ["fingerprint", "unknown key"] {
        let (paired, stored) = unparsable_record(case).await;
        let launched = paired.launch();
        paired
            .waits_untouched(&launched, "unreadable", Some(&stored))
            .await;
        launched.try_again();
        paired
            .waits_untouched(&launched, "unreadable", Some(&stored))
            .await;
        let phone = launched.phone();
        assert_eq!(phone["error"], TRY_AGAIN_FAILED, "{case}: {phone}");
        drop(launched);
        let launched = paired.launch();
        paired
            .waits_untouched(&launched, "unreadable", Some(&stored))
            .await;
        assert_eq!(paired.set_aside(), [], "{case}: no launch sets it aside");

        paired.prompt.answer.store(true, Ordering::SeqCst);
        assert!(launched.pair_again(), "{case}: the user said yes");
        let minted = HandoverIdentity::from_pem(&paired.stored().await.unwrap()).unwrap();
        let mac_id = uuid_string(minted.mac_id());
        let record = record_of(&hex(&minted.fingerprint()));
        assert_eq!(
            launched.handover().mac_id(),
            mac_id,
            "{case}: the new identity"
        );
        let aside = paired.set_aside();
        assert_eq!(aside.len(), 1, "{case}: {aside:?}");
        assert_eq!(aside[0].1, paired.record, "{case}: its bytes, kept aside");
        assert_eq!(
            std::fs::read_to_string(paired.record_path()).unwrap(),
            record,
            "{case}: a fresh record"
        );
        paired.forgot_the_phones(&launched, case);
        drop(launched);

        let again = paired.launch();
        again.loaded(&mac_id);
        assert_eq!(paired.set_aside(), aside, "{case}: the copy as it was");
        assert_eq!(
            std::fs::read_to_string(paired.record_path()).unwrap(),
            record,
            "{case}: the record as Pair again wrote it"
        );
        drop(again);

        let (mut paired, stored) = unparsable_record(case).await;
        let launched = paired.launch();
        paired
            .waits_untouched(&launched, "unreadable", Some(&stored))
            .await;
        paired.record = record_of(&paired.fingerprint).into_bytes();
        std::fs::write(paired.record_path(), &paired.record).unwrap();
        paired.put(&paired.pem).await;
        launched.try_again();
        paired.listens_on_the_known_identity(&launched).await;
        assert_eq!(paired.set_aside(), [], "{case}: nothing set aside");
    }
}

/// A paired computer whose record holds a value that is not a string,
/// under the fingerprint's key or an unknown one; and the identity the
/// keychain holds.
async fn unparsable_record(case: &str) -> (Paired, String) {
    let mut paired = Paired::new().await;
    let (bad, stored) = if case == "fingerprint" {
        let other = another_identity();
        paired.put(&other).await;
        (format!("{{\"{}\": 7}}", FingerprintFile::KEY), other)
    } else {
        let bad = format!(
            "{{\"{}\": \"{}\", \"steno.later\": 7}}",
            FingerprintFile::KEY,
            paired.fingerprint
        );
        (bad, paired.pem.clone())
    };
    std::fs::write(paired.record_path(), &bad).unwrap();
    paired.record = bad.into_bytes();
    assert!(
        paired.record_file().recorded().is_err(),
        "{case}: recorded() is an error"
    );
    assert!(
        paired.record_file().next().is_err(),
        "{case}: next() is an error"
    );
    (paired, stored)
}
