//! The import over fakes: the launch half against the committed defaults
//! fixtures, the step against a fake keychain that exports the committed
//! test identity, and the gate over a counting secret store.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use p12_keystore::{KeyStore, KeyStoreEntry, Pkcs12ImportPolicy};
use steno_core::testing::InMemorySecretStore;
use steno_handover::HandoverIdentity;
use steno_handover::identity::hex;

use super::*;

/// SHA-256 of `Tests/Fixtures/handover/test-identity.der`
/// (`TestIdentity.fingerprintHex` in the Swift tests).
const FIXTURE_FINGERPRINT: &str =
    "76ac0c0b28f976f25589214d5b2c614d983df9de081b06ec1fe9247bf1e40d92";

fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn fixture(name: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/swift-import")
        .join(name);
    std::fs::read(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

fn identity_der() -> Vec<u8> {
    std::fs::read(repository().join("Tests/Fixtures/handover/test-identity.der")).unwrap()
}

/// The committed test identity as PKCS#12 under `passphrase`, as
/// `SecItemExport` hands it over.
fn identity_pkcs12(passphrase: &str) -> Vec<u8> {
    let committed =
        std::fs::read(repository().join("Tests/Fixtures/handover/test-identity.p12")).unwrap();
    let store =
        KeyStore::from_pkcs12(&committed, "steno-test", Pkcs12ImportPolicy::Relaxed).unwrap();
    let mut again = KeyStore::new();
    for (alias, entry) in store.entries() {
        if let KeyStoreEntry::PrivateKeyChain(_) = entry {
            again.add_entry(alias, entry.clone());
        }
    }
    again.writer(passphrase).write().unwrap()
}

/// The user's account, as the product sees it outside a test.
fn at_home() -> LaunchContext {
    LaunchContext {
        smoke: false,
        home: Some(PathBuf::from("/Users/someone")),
        account_home: Some(PathBuf::from("/Users/someone")),
    }
}

struct FakeDefaults {
    plist: Vec<u8>,
    reads: AtomicUsize,
}

impl FakeDefaults {
    fn new(plist: Vec<u8>) -> Self {
        FakeDefaults {
            plist,
            reads: AtomicUsize::new(0),
        }
    }
}

impl SwiftDefaults for FakeDefaults {
    fn export(&self) -> Result<Vec<u8>, String> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        Ok(self.plist.clone())
    }
}

/// The Swift keychain items a test sets; records every call.
struct FakeKeychain {
    certificate: Mutex<Option<Vec<u8>>>,
    swift_key: bool,
    key: Result<Option<String>, KeychainRefusal>,
    /// The export's refusal; `None` exports the test identity.
    export_refusal: Mutex<Option<KeychainRefusal>>,
    calls: Mutex<Vec<&'static str>>,
}

impl FakeKeychain {
    fn swift_app() -> Self {
        FakeKeychain {
            certificate: Mutex::new(Some(identity_der())),
            swift_key: true,
            key: Ok(Some("sk-swift".to_owned())),
            export_refusal: Mutex::new(None),
            calls: Mutex::new(Vec::new()),
        }
    }

    fn calls(&self) -> Vec<&'static str> {
        self.calls.lock().unwrap().clone()
    }

    fn deny_export(&self) {
        *self.export_refusal.lock().unwrap() = Some(KeychainRefusal {
            denied: true,
            detail: "userCanceledErr".to_owned(),
        });
    }

    fn allow_export(&self) {
        *self.export_refusal.lock().unwrap() = None;
    }
}

impl SwiftKeychain for FakeKeychain {
    fn swift_certificate(&self) -> Result<Option<Vec<u8>>, String> {
        self.calls.lock().unwrap().push("certificate");
        Ok(self.certificate.lock().unwrap().clone())
    }

    fn has_swift_api_key(&self) -> Result<bool, String> {
        self.calls.lock().unwrap().push("has key");
        Ok(self.swift_key)
    }

    fn read_api_key(&self) -> Result<Option<String>, KeychainRefusal> {
        self.calls.lock().unwrap().push("read key");
        self.key.clone()
    }

    fn export_identity(&self, passphrase: &str) -> Result<Vec<u8>, KeychainRefusal> {
        self.calls.lock().unwrap().push("export");
        match self.export_refusal.lock().unwrap().clone() {
            Some(refusal) => Err(refusal),
            None => Ok(identity_pkcs12(passphrase)),
        }
    }
}

fn preferences(dir: &tempfile::TempDir) -> Arc<FilePreferences> {
    Arc::new(FilePreferences::in_support_directory(dir.path()))
}

fn pending(launch: Launch) -> PendingImport {
    match launch {
        Launch::Pending(pending) => pending,
        other => panic!("not pending: {other:?}"),
    }
}

#[test]
fn the_launch_half_copies_the_onboarding_flag_and_the_update_flags_and_nothing_else() {
    let dir = tempfile::tempdir().unwrap();
    let preferences = preferences(&dir);
    let defaults = FakeDefaults::new(fixture("swift-domain.plist"));
    let keychain = Arc::new(FakeKeychain::swift_app());
    let pending = pending(launch(
        &at_home(),
        preferences.clone(),
        &defaults,
        keychain.clone(),
    ));
    assert!(pending.gate_key && pending.read_key);
    let again = FilePreferences::in_support_directory(dir.path());
    assert!(again.flag(OnboardingViewModel::COMPLETED_KEY));
    assert!(again.flag(AUTOMATIC_CHECKS_KEY));
    assert!(again.contains(AUTOMATIC_DOWNLOAD_KEY));
    assert!(!again.flag(AUTOMATIC_DOWNLOAD_KEY));
    for left in [
        "steno.loginItemRegistered",
        "steno.floatingPanel.anchor",
        "steno.systemAudioGranted",
        "SULastCheckTime",
        IMPORT_RAN_KEY,
    ] {
        assert!(!again.contains(left), "{left} was copied");
    }
    assert_eq!(keychain.calls(), ["certificate", "has key"], "nothing read");
}

#[test]
fn a_flag_the_swift_domain_lacks_stays_unset_and_a_key_that_is_not_the_swift_one_is_not_read() {
    let dir = tempfile::tempdir().unwrap();
    let preferences = preferences(&dir);
    let defaults = FakeDefaults::new(fixture("swift-domain-without-update-flags.plist"));
    let keychain = Arc::new(FakeKeychain {
        swift_key: false,
        ..FakeKeychain::swift_app()
    });
    let pending = pending(launch(&at_home(), preferences.clone(), &defaults, keychain));
    assert!(
        pending.gate_key,
        "the key is gated while pending all the same"
    );
    assert!(!pending.read_key);
    assert!(preferences.contains(OnboardingViewModel::COMPLETED_KEY));
    assert!(!preferences.flag(OnboardingViewModel::COMPLETED_KEY));
    assert!(!preferences.contains(AUTOMATIC_CHECKS_KEY));
    assert!(!preferences.contains(AUTOMATIC_DOWNLOAD_KEY));
}

#[test]
fn an_existing_preferences_file_keeps_its_values_and_the_domain_is_not_read() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("preferences.json"),
        br#"{"steno.onboardingCompleted": false, "steno.updates.automaticChecks": false}"#,
    )
    .unwrap();
    let preferences = preferences(&dir);
    let defaults = FakeDefaults::new(fixture("swift-domain.plist"));
    let _ = launch(
        &at_home(),
        preferences.clone(),
        &defaults,
        Arc::new(FakeKeychain::swift_app()),
    );
    assert_eq!(defaults.reads.load(Ordering::SeqCst), 0);
    assert!(!preferences.flag(OnboardingViewModel::COMPLETED_KEY));
    assert!(!preferences.flag(AUTOMATIC_CHECKS_KEY));
    assert!(!preferences.contains(AUTOMATIC_DOWNLOAD_KEY));
}

#[test]
fn without_a_swift_certificate_the_import_is_over_at_launch_and_a_second_launch_does_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let preferences = preferences(&dir);
    let defaults = FakeDefaults::new(fixture("swift-domain.plist"));
    let keychain = Arc::new(FakeKeychain::swift_app());
    *keychain.certificate.lock().unwrap() = None;
    assert!(matches!(
        launch(&at_home(), preferences.clone(), &defaults, keychain.clone()),
        Launch::Done
    ));
    assert!(preferences.flag(IMPORT_RAN_KEY));
    assert!(preferences.flag(OnboardingViewModel::COMPLETED_KEY));

    // The next launch: the import ran, so nothing is read or written,
    // even with a certificate now in the keychain and the flag removed.
    *keychain.certificate.lock().unwrap() = Some(identity_der());
    keychain.calls.lock().unwrap().clear();
    let fresh = Arc::new(FilePreferences::in_support_directory(dir.path()));
    let before = std::fs::read(dir.path().join("preferences.json")).unwrap();
    let second = FakeDefaults::new(fixture("swift-domain.plist"));
    assert!(matches!(
        launch(&at_home(), fresh, &second, keychain.clone()),
        Launch::Done
    ));
    assert_eq!(second.reads.load(Ordering::SeqCst), 0);
    assert_eq!(keychain.calls(), Vec::<&str>::new());
    assert_eq!(
        std::fs::read(dir.path().join("preferences.json")).unwrap(),
        before
    );
}

#[test]
fn a_smoke_run_and_a_foreign_home_skip_the_import_and_touch_nothing() {
    let contexts = [
        (
            LaunchContext {
                smoke: true,
                ..at_home()
            },
            SkipReason::Smoke,
        ),
        (
            LaunchContext {
                home: Some(PathBuf::from("/tmp/scratch-home")),
                ..at_home()
            },
            SkipReason::ForeignHome,
        ),
        (
            LaunchContext {
                home: None,
                ..at_home()
            },
            SkipReason::ForeignHome,
        ),
        (
            LaunchContext {
                account_home: None,
                ..at_home()
            },
            SkipReason::ForeignHome,
        ),
    ];
    for (context, reason) in contexts {
        let dir = tempfile::tempdir().unwrap();
        let defaults = FakeDefaults::new(fixture("swift-domain.plist"));
        let keychain = Arc::new(FakeKeychain::swift_app());
        let outcome = launch(&context, preferences(&dir), &defaults, keychain.clone());
        assert!(
            matches!(outcome, Launch::Skipped(found) if found == reason),
            "{context:?}: {outcome:?}"
        );
        assert_eq!(defaults.reads.load(Ordering::SeqCst), 0);
        assert_eq!(keychain.calls(), Vec::<&str>::new());
        assert!(!dir.path().join("preferences.json").exists());
    }
    assert_eq!(at_home().skip_reason(), None);
}

#[test]
fn the_context_reads_the_smoke_variable() {
    let context = LaunchContext::current("STENO_TEST_SURELY_UNSET_SMOKE_VARIABLE");
    assert!(!context.smoke);
    let context = LaunchContext::current("PATH");
    assert!(context.smoke);
    assert_eq!(context.skip_reason(), Some(SkipReason::Smoke));
}

/// A secret store that counts the reads of each key.
#[derive(Default)]
struct CountingSecrets {
    inner: InMemorySecretStore,
    reads: Mutex<Vec<String>>,
}

#[async_trait]
impl SecretStore for CountingSecrets {
    async fn secret(&self, key: &SecretKey) -> BoundaryResult<Option<String>> {
        self.reads.lock().unwrap().push(key.as_str().to_owned());
        self.inner.secret(key).await
    }

    async fn set_secret(&self, key: &SecretKey, value: Option<&str>) -> BoundaryResult<()> {
        self.inner.set_secret(key, value).await
    }
}

struct Step {
    _dir: tempfile::TempDir,
    preferences: Arc<FilePreferences>,
    keychain: Arc<FakeKeychain>,
    raw: Arc<CountingSecrets>,
    graph: GraphImport,
    import: ImportStep,
    reloads: Arc<AtomicUsize>,
}

fn step(keychain: FakeKeychain, existing_identity: Option<&HandoverIdentity>) -> Step {
    let dir = tempfile::tempdir().unwrap();
    let preferences = preferences(&dir);
    let keychain = Arc::new(keychain);
    let raw = Arc::new(CountingSecrets::default());
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    runtime.block_on(async {
        raw.inner
            .set_secret(&SecretKey::llm_api_key(), Some("sk-swift"))
            .await
            .unwrap();
        if let Some(identity) = existing_identity {
            raw.inner
                .set_secret(
                    &HandoverIdentity::secret_key(),
                    Some(&identity.to_pem().unwrap()),
                )
                .await
                .unwrap();
        }
    });
    if let Some(identity) = existing_identity {
        preferences
            .set_string(IDENTITY_FINGERPRINT_KEY, &hex(&identity.fingerprint()))
            .unwrap();
    }
    let defaults = FakeDefaults::new(fixture("swift-domain.plist"));
    let pending = pending(launch(
        &at_home(),
        preferences.clone(),
        &defaults,
        keychain.clone(),
    ));
    let graph = GraphImport::new(pending, raw.clone());
    let reloads = Arc::new(AtomicUsize::new(0));
    let counted = reloads.clone();
    let import = graph.step(
        RUNTIME.handle().clone(),
        Box::new(move || {
            counted.fetch_add(1, Ordering::SeqCst);
        }),
    );
    Step {
        _dir: dir,
        preferences,
        keychain,
        raw,
        graph,
        import,
        reloads,
    }
}

/// The runtime the step blocks on for the secret store.
static RUNTIME: std::sync::LazyLock<tokio::runtime::Runtime> = std::sync::LazyLock::new(|| {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .build()
        .unwrap()
});

fn read(store: &dyn SecretStore, key: &SecretKey) -> Option<String> {
    RUNTIME.block_on(store.secret(key)).unwrap()
}

#[test]
fn while_pending_the_graph_reads_no_api_key() {
    let step = step(FakeKeychain::swift_app(), None);
    assert_eq!(read(&*step.graph.secrets, &SecretKey::llm_api_key()), None);
    assert!(
        step.raw.reads.lock().unwrap().is_empty(),
        "the keychain was asked"
    );
    assert_eq!(step.graph.gate.handover(), HandoverGate::Pending);
    assert_eq!(step.import.status().stage, SwiftImportStage::Pending);
    assert_eq!(step.import.status().prompts, 2);
}

#[test]
fn the_step_reads_the_key_and_brings_the_identity_over_replacing_a_desktop_id_one() {
    let desktop =
        HandoverIdentity::mint("Steno on a desktop-id build", chrono::Utc::now()).unwrap();
    let step = step(FakeKeychain::swift_app(), Some(&desktop));
    let status = step.import.run();
    assert_eq!(status.stage, SwiftImportStage::Done);
    assert_eq!(status.failure, None);
    assert_eq!(
        step.keychain.calls(),
        [
            "certificate",
            "has key",
            "read key",
            "certificate",
            "export"
        ]
    );
    let stored = read(&*step.raw, &HandoverIdentity::secret_key()).unwrap();
    let imported = HandoverIdentity::from_pem(&stored).unwrap();
    assert_eq!(hex(&imported.fingerprint()), FIXTURE_FINGERPRINT);
    assert_eq!(
        step.preferences.string(IDENTITY_FINGERPRINT_KEY).as_deref(),
        Some(FIXTURE_FINGERPRINT),
        "the recorded fingerprint follows the Swift identity"
    );
    assert!(step.preferences.flag(IMPORT_RAN_KEY));
    assert!(step.preferences.flag(KEY_READ_KEY));
    assert_eq!(step.graph.gate.handover(), HandoverGate::Ready);
    // The key the step read, answered without asking the keychain again.
    step.raw.reads.lock().unwrap().clear();
    assert_eq!(
        read(&*step.graph.secrets, &SecretKey::llm_api_key()).as_deref(),
        Some("sk-swift")
    );
    assert!(step.raw.reads.lock().unwrap().is_empty());
    assert_eq!(step.reloads.load(Ordering::SeqCst), 1);
    assert_eq!(
        step.import.run().stage,
        SwiftImportStage::Done,
        "a no-op now"
    );
}

#[test]
fn a_denied_export_leaves_an_existing_identity_untouched_mints_none_and_try_again_works() {
    let desktop =
        HandoverIdentity::mint("Steno on a desktop-id build", chrono::Utc::now()).unwrap();
    for existing in [Some(&desktop), None] {
        let step = step(FakeKeychain::swift_app(), existing);
        let before = read(&*step.raw, &HandoverIdentity::secret_key());
        step.keychain.deny_export();
        let status = step.import.run();
        assert_eq!(status.stage, SwiftImportStage::Waiting);
        assert_eq!(status.failure.as_deref(), Some(DENIED_EXPORT));
        assert_eq!(
            read(&*step.raw, &HandoverIdentity::secret_key()),
            before,
            "the stored identity, or its absence, is untouched"
        );
        assert_eq!(
            step.preferences.string(IDENTITY_FINGERPRINT_KEY),
            existing.map(|identity| hex(&identity.fingerprint()))
        );
        assert!(!step.preferences.flag(IMPORT_RAN_KEY));
        assert_eq!(
            step.graph.gate.handover(),
            HandoverGate::Waiting(WaitReason::ImportDenied)
        );
        // Try again asks for the identity alone: the key was read.
        assert_eq!(status.prompts, 1);
        step.keychain.allow_export();
        assert_eq!(step.import.run().stage, SwiftImportStage::Done);
        assert_eq!(
            step.keychain
                .calls()
                .iter()
                .filter(|call| **call == "read key")
                .count(),
            1
        );
        assert_eq!(step.graph.gate.handover(), HandoverGate::Ready);
    }
}

#[test]
fn an_export_of_another_certificate_is_refused() {
    let step = step(FakeKeychain::swift_app(), None);
    let other = HandoverIdentity::mint("Another", chrono::Utc::now()).unwrap();
    *step.keychain.certificate.lock().unwrap() = Some(other.certificate_der().to_vec());
    let status = step.import.run();
    assert_eq!(status.stage, SwiftImportStage::Waiting);
    assert_eq!(status.failure.as_deref(), Some(FAILED_EXPORT));
    assert_eq!(read(&*step.raw, &HandoverIdentity::secret_key()), None);
}

#[test]
fn a_denied_key_read_leaves_the_key_empty_and_the_identity_still_comes_over() {
    let step = step(
        FakeKeychain {
            key: Err(KeychainRefusal {
                denied: true,
                detail: "userCanceledErr".to_owned(),
            }),
            ..FakeKeychain::swift_app()
        },
        None,
    );
    assert_eq!(step.import.run().stage, SwiftImportStage::Done);
    assert_eq!(read(&*step.graph.secrets, &SecretKey::llm_api_key()), None);
    assert!(step.preferences.flag(KEY_READ_KEY));
    // A key the user saves in Settings is read from the store again.
    RUNTIME
        .block_on(
            step.graph
                .secrets
                .set_secret(&SecretKey::llm_api_key(), Some("sk-new")),
        )
        .unwrap();
    assert_eq!(
        read(&*step.graph.secrets, &SecretKey::llm_api_key()).as_deref(),
        Some("sk-new")
    );
}

#[test]
fn skipping_the_step_counts_as_both_denied_and_the_next_launch_asks_again() {
    let step = step(FakeKeychain::swift_app(), None);
    let status = step.import.skip();
    assert_eq!(status.stage, SwiftImportStage::Waiting);
    assert_eq!(read(&*step.graph.secrets, &SecretKey::llm_api_key()), None);
    assert!(
        step.raw.reads.lock().unwrap().is_empty(),
        "the keychain was asked"
    );
    assert_eq!(read(&*step.raw, &HandoverIdentity::secret_key()), None);
    assert_eq!(
        step.graph.gate.handover(),
        HandoverGate::Waiting(WaitReason::ImportDenied)
    );
    assert!(!step.preferences.flag(IMPORT_RAN_KEY));
    assert!(!step.preferences.flag(KEY_READ_KEY));
    assert_eq!(
        step.keychain.calls(),
        ["certificate", "has key"],
        "no prompt"
    );

    // The next launch finds the import pending again, with both prompts.
    let defaults = FakeDefaults::new(fixture("swift-domain.plist"));
    let again = pending(launch(
        &at_home(),
        step.preferences.clone(),
        &defaults,
        step.keychain.clone(),
    ));
    assert!(again.read_key);
}

/// `SecItemExport`'s PKCS#12 of the committed test identity, written on a
/// Mac by the keychain test (`tests/swift_keychain.rs`): the legacy
/// encryption Apple writes decodes here on every platform.
#[test]
fn a_pkcs12_the_mac_exported_decodes_into_the_identity() {
    let (identity, bundle) = decode_pkcs12(
        &fixture("test-identity.exported.p12"),
        EXPORTED_PASSPHRASE,
        &identity_der(),
    )
    .unwrap();
    assert_eq!(hex(&identity.fingerprint()), FIXTURE_FINGERPRINT);
    assert!(bundle.starts_with("-----BEGIN CERTIFICATE-----\n"));
    assert!(bundle.contains("-----BEGIN PRIVATE KEY-----\n"));
    assert!(
        decode_pkcs12(
            &fixture("test-identity.exported.p12"),
            "wrong",
            &identity_der()
        )
        .is_err()
    );
}

/// The passphrase of `test-identity.exported.p12`.
const EXPORTED_PASSPHRASE: &str = "steno-export-fixture";

/// The graph over a pending import, end to end on the 0600 secrets file:
/// no API key is read and no listener built (so no identity minted) until
/// the step ran; then the listener comes up with the Swift identity.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_graph_over_a_pending_import_reads_no_key_and_binds_no_listener_until_the_step_ran() {
    use steno_host::services::{Handover as _, ListenerState};

    let dir = tempfile::tempdir().unwrap();
    let paths = steno_core::StenoPaths::new(dir.path().join("support"));
    let secrets_path = paths.support_directory.join("secrets.json");
    let file = crate::FileSecretStore::new(&secrets_path, std::collections::BTreeMap::new());
    file.set_secret(&SecretKey::llm_api_key(), Some("sk-swift"))
        .await
        .unwrap();
    let stored = |key: &str| -> Option<String> {
        let map: std::collections::BTreeMap<String, String> =
            serde_json::from_slice(&std::fs::read(&secrets_path).unwrap()).unwrap();
        map.get(key).cloned()
    };
    let keychain = Arc::new(FakeKeychain::swift_app());
    let pending = pending(launch(
        &at_home(),
        Arc::new(FilePreferences::in_support_directory(
            &paths.support_directory,
        )),
        &FakeDefaults::new(fixture("swift-domain.plist")),
        keychain,
    ));
    let app = crate::build_with_import(
        crate::AppOptions {
            paths,
            database_path: None,
            keyring: false,
            opener: Arc::new(steno_host::fakes::FakeOpener::default()),
            login_item: None,
            runtime: tokio::runtime::Handle::current(),
            version: "0.0.0".to_owned(),
            make_capture_session: Arc::new(|_| Err("no capture in this test".to_owned())),
        },
        Some(pending),
    )
    .unwrap();

    assert!(app.handover.is_none());
    let gated = app.gated_handover.clone().unwrap();
    assert!(gated.service().is_none(), "no listener while pending");
    assert_eq!(gated.state(), ListenerState::Stopped);
    assert!(gated.start().is_err(), "and none starts");
    assert_eq!(
        stored(HandoverIdentity::SECRET_KEY),
        None,
        "no identity minted"
    );
    let host = app.host().unwrap();
    assert_eq!(
        host.snapshot(steno_bridge::BridgeTopic::SettingsSummaries)
            .unwrap()["hasAPIKey"],
        false,
        "the key in the store was not read"
    );
    let summaries_secret = app.secrets.secret(&SecretKey::llm_api_key()).await.unwrap();
    assert_eq!(summaries_secret, None);

    let step = app.services.swift_import.clone().unwrap();
    let status = tokio::task::spawn_blocking(move || step.run())
        .await
        .unwrap();
    assert_eq!(status.stage, SwiftImportStage::Done);
    let (changed, followed) = tokio::sync::oneshot::channel();
    gated
        .clone()
        .follow(move || {
            let _ = changed.send(());
        })
        .await;
    followed.await.unwrap();
    assert_eq!(
        gated.mac_id(),
        "FCF0D2A1-D2CE-48F7-BAFF-E17FD2E9C814",
        "the Mac id the paired phones know"
    );
    let imported =
        HandoverIdentity::from_pem(&stored(HandoverIdentity::SECRET_KEY).unwrap()).unwrap();
    assert_eq!(hex(&imported.fingerprint()), FIXTURE_FINGERPRINT);
    assert_eq!(
        app.secrets
            .secret(&SecretKey::llm_api_key())
            .await
            .unwrap()
            .as_deref(),
        Some("sk-swift")
    );
    app.shutdown();
}
