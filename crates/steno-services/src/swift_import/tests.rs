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

/// The fingerprint record under `support`, as the app keeps it.
fn record_in(support: &Path) -> Arc<crate::handover::FingerprintFile> {
    Arc::new(crate::handover::FingerprintFile::in_support_directory(
        support,
    ))
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

/// What [`FakeKeychain`] calls at each of its calls, with the call's name.
type OnCall = Arc<dyn Fn(&'static str) + Send + Sync>;

/// The Swift keychain items a test sets; records every call.
struct FakeKeychain {
    certificate: Mutex<Option<Vec<u8>>>,
    /// The certificate query's failure, which wins over `certificate`.
    certificate_error: Mutex<Option<String>>,
    key_item: Result<ApiKeyItem, String>,
    /// A `handover-identity` entry is filed (a desktop-id build's).
    stored_identity: Result<bool, String>,
    key: Result<Option<String>, KeychainRefusal>,
    /// The `swift-import-done` marker: whether it is filed, or the query's
    /// failure.
    marker: Mutex<Result<bool, String>>,
    /// The export's refusal; `None` exports the test identity.
    export_refusal: Mutex<Option<KeychainRefusal>>,
    /// The certificate each export was asked for.
    exported: Mutex<Vec<Vec<u8>>>,
    calls: Mutex<Vec<&'static str>>,
    /// Runs at every call once it is recorded, outside the fake's locks.
    on_call: Mutex<Option<OnCall>>,
}

impl FakeKeychain {
    fn swift_app() -> Self {
        FakeKeychain {
            certificate: Mutex::new(Some(identity_der())),
            certificate_error: Mutex::new(None),
            key_item: Ok(ApiKeyItem::Swift),
            stored_identity: Ok(false),
            key: Ok(Some("sk-swift".to_owned())),
            marker: Mutex::new(Ok(false)),
            export_refusal: Mutex::new(None),
            exported: Mutex::new(Vec::new()),
            calls: Mutex::new(Vec::new()),
            on_call: Mutex::new(None),
        }
    }

    /// The Swift app's items, with the key's read refused.
    fn denying_the_key() -> Self {
        FakeKeychain {
            key: Err(user_canceled()),
            ..FakeKeychain::swift_app()
        }
    }

    fn calls(&self) -> Vec<&'static str> {
        self.calls.lock().unwrap().clone()
    }

    /// How often `call` was made.
    fn count(&self, call: &str) -> usize {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|made| **made == call)
            .count()
    }

    fn record(&self, call: &'static str) {
        self.calls.lock().unwrap().push(call);
        let on_call = self.on_call.lock().unwrap().clone();
        if let Some(on_call) = on_call {
            on_call(call);
        }
    }

    fn on_call(&self, on_call: impl Fn(&'static str) + Send + Sync + 'static) {
        *self.on_call.lock().unwrap() = Some(Arc::new(on_call));
    }

    fn deny_export(&self) {
        *self.export_refusal.lock().unwrap() = Some(user_canceled());
    }

    fn allow_export(&self) {
        *self.export_refusal.lock().unwrap() = None;
    }
}

/// The refusal of a prompt the user denied.
fn user_canceled() -> KeychainRefusal {
    KeychainRefusal {
        denied: true,
        detail: "userCanceledErr".to_owned(),
    }
}

impl SwiftKeychain for FakeKeychain {
    fn swift_certificate(&self) -> Result<Option<Vec<u8>>, String> {
        self.record("certificate");
        if let Some(error) = self.certificate_error.lock().unwrap().clone() {
            return Err(error);
        }
        Ok(self.certificate.lock().unwrap().clone())
    }

    fn api_key_item(&self) -> Result<ApiKeyItem, String> {
        self.record("key item");
        self.key_item.clone()
    }

    fn has_stored_identity(&self) -> Result<bool, String> {
        self.record("identity entry");
        self.stored_identity.clone()
    }

    fn import_done(&self) -> Result<bool, String> {
        self.record("marker");
        self.marker.lock().unwrap().clone()
    }

    fn mark_import_done(&self) -> Result<(), String> {
        self.record("mark done");
        *self.marker.lock().unwrap() = Ok(true);
        Ok(())
    }

    fn read_api_key(&self) -> Result<Option<String>, KeychainRefusal> {
        self.record("read key");
        self.key.clone()
    }

    fn export_identity(
        &self,
        certificate_der: &[u8],
        passphrase: &str,
    ) -> Result<Vec<u8>, KeychainRefusal> {
        self.record("export");
        self.exported.lock().unwrap().push(certificate_der.to_vec());
        match self.export_refusal.lock().unwrap().clone() {
            Some(refusal) => Err(refusal),
            None => Ok(identity_pkcs12(passphrase)),
        }
    }
}

fn preferences(dir: &tempfile::TempDir) -> Arc<FilePreferences> {
    Arc::new(FilePreferences::in_support_directory(dir.path()))
}

/// The launch half at the user's home against the Swift domain fixture.
fn launch_at_home(preferences: Arc<FilePreferences>, keychain: Arc<dyn SwiftKeychain>) -> Launch {
    launch(
        &at_home(),
        preferences,
        &FakeDefaults::new(fixture("swift-domain.plist")),
        keychain,
    )
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
    assert_eq!(pending.key, LaunchKey::Unread(ApiKeyItem::Swift));
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
    assert_eq!(
        keychain.calls(),
        ["marker", "certificate", "key item", "identity entry"],
        "nothing read"
    );
    assert_flags_only(dir.path());
}

#[test]
fn a_flag_the_swift_domain_lacks_stays_unset_and_a_key_that_is_not_the_swift_one_is_not_read() {
    let dir = tempfile::tempdir().unwrap();
    let preferences = preferences(&dir);
    let defaults = FakeDefaults::new(fixture("swift-domain-without-update-flags.plist"));
    let keychain = Arc::new(FakeKeychain {
        key_item: Ok(ApiKeyItem::Other),
        ..FakeKeychain::swift_app()
    });
    let pending = pending(launch(&at_home(), preferences.clone(), &defaults, keychain));
    assert_eq!(
        pending.key,
        LaunchKey::Unread(ApiKeyItem::Other),
        "the key is gated while pending all the same"
    );
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
    // even with a certificate now in the keychain.
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

/// A secret store that counts the reads of each key, and refuses the
/// identity's writes while `refuse_identity` is set and the API key's
/// while `refuse_key` is.
#[derive(Default)]
struct CountingSecrets {
    inner: InMemorySecretStore,
    reads: Mutex<Vec<String>>,
    refuse_identity: std::sync::atomic::AtomicBool,
    refuse_key: std::sync::atomic::AtomicBool,
}

impl CountingSecrets {
    /// Whether the API key was read.
    fn read_the_key(&self) -> bool {
        self.reads
            .lock()
            .unwrap()
            .iter()
            .any(|read| read == SecretKey::LLM_API_KEY)
    }
}

#[async_trait]
impl SecretStore for CountingSecrets {
    async fn secret(&self, key: &SecretKey) -> BoundaryResult<Option<String>> {
        self.reads.lock().unwrap().push(key.as_str().to_owned());
        self.inner.secret(key).await
    }

    async fn set_secret(&self, key: &SecretKey, value: Option<&str>) -> BoundaryResult<()> {
        let refused = match key.as_str() {
            HandoverIdentity::SECRET_KEY => &self.refuse_identity,
            SecretKey::LLM_API_KEY => &self.refuse_key,
            _ => return self.inner.set_secret(key, value).await,
        };
        if refused.load(Ordering::SeqCst) {
            return Err("errSecAuthFailed".into());
        }
        self.inner.set_secret(key, value).await
    }
}

/// `preferences.json` stays a map of booleans, which every earlier build
/// reads: a value of another type would make such a build drop every flag
/// after a rollback.
fn assert_flags_only(support_directory: &Path) {
    let bytes = std::fs::read(support_directory.join("preferences.json")).unwrap();
    serde_json::from_slice::<std::collections::BTreeMap<String, bool>>(&bytes)
        .unwrap_or_else(|error| panic!("{error}: {}", String::from_utf8_lossy(&bytes)));
}

struct Step {
    dir: tempfile::TempDir,
    preferences: Arc<FilePreferences>,
    keychain: Arc<FakeKeychain>,
    raw: Arc<CountingSecrets>,
    record: Arc<crate::handover::FingerprintFile>,
    graph: GraphImport,
    import: Arc<ImportStep>,
    reloads: Arc<AtomicUsize>,
}

fn step(keychain: FakeKeychain, existing_identity: Option<&HandoverIdentity>) -> Step {
    let dir = tempfile::tempdir().unwrap();
    let preferences = preferences(&dir);
    let keychain = Arc::new(FakeKeychain {
        stored_identity: Ok(existing_identity.is_some()),
        ..keychain
    });
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
    let pending = pending(launch_at_home(preferences.clone(), keychain.clone()));
    let record = record_in(dir.path());
    let graph = GraphImport::new(pending, raw.clone(), record.clone());
    let reloads = Arc::new(AtomicUsize::new(0));
    let counted = reloads.clone();
    let import = Arc::new(graph.step(
        RUNTIME.handle().clone(),
        Box::new(move || {
            counted.fetch_add(1, Ordering::SeqCst);
        }),
    ));
    Step {
        dir,
        preferences,
        keychain,
        raw,
        record,
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

fn set(store: &dyn SecretStore, key: &SecretKey, value: Option<&str>) {
    RUNTIME.block_on(store.set_secret(key, value)).unwrap();
}

/// The fingerprint of the `handover-identity` entry in `store`.
fn stored_fingerprint(store: &dyn SecretStore) -> String {
    let stored = read(store, &HandoverIdentity::secret_key()).unwrap();
    hex(&HandoverIdentity::from_pem(&stored).unwrap().fingerprint())
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
    assert_eq!(
        step.import.status().prompts,
        3,
        "the key, the export and the entry it replaces"
    );
    let status = step.import.run();
    assert_eq!(status.stage, SwiftImportStage::Done);
    assert_eq!(status.error, None);
    assert_eq!(
        step.keychain.calls(),
        [
            "marker",
            "certificate",
            "key item",
            "identity entry",
            "read key",
            "certificate",
            "export",
            "mark done"
        ]
    );
    assert_eq!(stored_fingerprint(&*step.raw), FIXTURE_FINGERPRINT);
    assert!(step.preferences.flag(IMPORT_RAN_KEY));
    assert_flags_only(step.dir.path());
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
        assert_eq!(status.error.as_deref(), Some(DENIED_EXPORT));
        assert_eq!(
            read(&*step.raw, &HandoverIdentity::secret_key()),
            before,
            "the stored identity, or its absence, is untouched"
        );
        assert!(!step.preferences.flag(IMPORT_RAN_KEY));
        assert_eq!(
            step.graph.gate.handover(),
            HandoverGate::Waiting(WaitReason::ImportDenied)
        );
        // Try again asks for the identity alone (and the entry it
        // replaces): the key was read.
        assert_eq!(status.prompts, 1 + u8::from(existing.is_some()));
        step.keychain.allow_export();
        assert_eq!(step.import.run().stage, SwiftImportStage::Done);
        assert_eq!(step.keychain.count("read key"), 1);
        assert_eq!(step.graph.gate.handover(), HandoverGate::Ready);
    }
}

/// The export is of the certificate the step's lookup found, so two
/// certificates under the Swift label cannot make the two calls disagree.
#[test]
fn the_export_is_of_the_certificate_the_lookup_found() {
    let step = step(FakeKeychain::swift_app(), None);
    assert_eq!(step.import.run().stage, SwiftImportStage::Done);
    assert_eq!(*step.keychain.exported.lock().unwrap(), [identity_der()]);
}

/// A desktop-id build's key item and identity entry each count as a
/// prompt the step announces. Not now reads neither; Continue lets the
/// graph read the key.
#[test]
fn a_desktop_id_key_and_identity_count_as_prompts_and_not_now_reads_neither() {
    let desktop =
        HandoverIdentity::mint("Steno on a desktop-id build", chrono::Utc::now()).unwrap();
    let keychain = FakeKeychain {
        key_item: Ok(ApiKeyItem::Other),
        ..FakeKeychain::swift_app()
    };
    let step = step(keychain, Some(&desktop));
    assert_eq!(step.import.status().prompts, 3);
    step.import.skip();
    let key = SecretKey::llm_api_key();
    assert_eq!(read(&*step.graph.secrets, &key), None);
    assert!(
        step.raw.reads.lock().unwrap().is_empty(),
        "Not now read a key"
    );
    assert!(!step.keychain.calls().contains(&"read key"));
    assert_eq!(step.import.run().stage, SwiftImportStage::Done);
    assert!(
        !step.keychain.calls().contains(&"read key"),
        "not the Swift key"
    );
    assert_eq!(
        read(&*step.graph.secrets, &key).as_deref(),
        Some("sk-swift")
    );
}

/// A store that fails while it replaces a stored entry reads as that
/// prompt denied; Try again stores the identity it kept, without a second
/// export. Without an entry to replace the store simply failed.
#[test]
fn a_refused_replace_counts_as_denied_and_try_again_repeats_only_the_store() {
    let desktop =
        HandoverIdentity::mint("Steno on a desktop-id build", chrono::Utc::now()).unwrap();
    for existing in [Some(&desktop), None] {
        let step = step(FakeKeychain::swift_app(), existing);
        step.raw.refuse_identity.store(true, Ordering::SeqCst);
        let status = step.import.run();
        assert_eq!(status.stage, SwiftImportStage::Waiting);
        assert!(
            !step.preferences.flag(IMPORT_RAN_KEY),
            "the import is over without the identity"
        );
        let expected = if existing.is_some() {
            DENIED_EXPORT
        } else {
            FAILED_EXPORT
        };
        assert_eq!(status.error.as_deref(), Some(expected));
        assert_eq!(
            status.prompts,
            u8::from(existing.is_some()),
            "the store alone is left"
        );
        assert_eq!(
            step.graph.gate.handover(),
            HandoverGate::Waiting(WaitReason::ImportDenied)
        );
        step.raw.refuse_identity.store(false, Ordering::SeqCst);
        assert_eq!(step.import.run().stage, SwiftImportStage::Done);
        assert_eq!(step.keychain.count("export"), 1, "Try again exported again");
        assert_eq!(stored_fingerprint(&*step.raw), FIXTURE_FINGERPRINT);
    }
}

/// A fingerprint record that refuses every write while `refuse` is set,
/// over the app's file.
struct RefusingRecord {
    file: Arc<crate::handover::FingerprintFile>,
    refuse: std::sync::atomic::AtomicBool,
}

impl FingerprintRecord for RefusingRecord {
    fn recorded(&self) -> BoundaryResult<Option<String>> {
        self.file.recorded()
    }

    fn record(&self, fingerprint: &str) -> BoundaryResult<()> {
        if self.refuse.load(Ordering::SeqCst) {
            return Err("No space left on device".into());
        }
        self.file.record(fingerprint)
    }
}

/// The handover's own load, as the listener does it, over `secrets` and
/// `record` with no phone paired.
fn load_identity(
    secrets: &dyn SecretStore,
    record: &dyn FingerprintRecord,
) -> Result<HandoverIdentity, steno_handover::IdentityError> {
    let store = steno_core::Store::in_memory().unwrap();
    RUNTIME.block_on(HandoverIdentity::load_or_create(
        secrets,
        record,
        &store,
        "Steno on a test",
        chrono::Utc::now(),
    ))
}

/// The Swift identity replaces a desktop-id build's, whose fingerprint
/// that build recorded. Its secret is written but its fingerprint is not:
/// the step does not count that as done (Waiting with Try again, no
/// marker, no `IMPORT_RAN`), and the handover's load refuses the stored
/// identity rather than mint. Try again stores it again, now with its
/// fingerprint, and the load accepts the Swift identity.
#[test]
fn a_fingerprint_not_recorded_after_the_store_is_surfaced_and_try_again_records_it() {
    let dir = tempfile::tempdir().unwrap();
    let desktop =
        HandoverIdentity::mint("Steno on a desktop-id build", chrono::Utc::now()).unwrap();
    let raw = Arc::new(CountingSecrets::default());
    let file = record_in(dir.path());
    RUNTIME.block_on(desktop.store(&raw.inner, &*file)).unwrap();
    let keychain = Arc::new(FakeKeychain {
        stored_identity: Ok(true),
        ..FakeKeychain::swift_app()
    });
    let preferences = preferences(&dir);
    let record = Arc::new(RefusingRecord {
        file: file.clone(),
        refuse: std::sync::atomic::AtomicBool::new(true),
    });
    let graph = GraphImport::new(
        pending(launch_at_home(preferences.clone(), keychain.clone())),
        raw.clone(),
        record.clone(),
    );
    let step = graph.step(RUNTIME.handle().clone(), Box::new(|| {}));

    let status = step.run();
    assert_eq!(status.stage, SwiftImportStage::Waiting);
    assert_eq!(status.error.as_deref(), Some(FAILED_EXPORT));
    assert_eq!(
        stored_fingerprint(&*raw),
        FIXTURE_FINGERPRINT,
        "the secret was written"
    );
    assert_eq!(
        file.recorded().unwrap(),
        Some(hex(&desktop.fingerprint())),
        "the desktop-id fingerprint is still the recorded one"
    );
    assert_eq!(*keychain.marker.lock().unwrap(), Ok(false), "no marker");
    assert_eq!(keychain.count("mark done"), 0);
    assert!(!preferences.flag(IMPORT_RAN_KEY));
    assert_eq!(
        graph.gate.handover(),
        HandoverGate::Waiting(WaitReason::ImportDenied)
    );
    assert!(
        matches!(
            load_identity(&raw.inner, &*file),
            Err(steno_handover::IdentityError::Unavailable(
                steno_handover::Unavailability::Replaced
            ))
        ),
        "the listener refuses it, and mints nothing"
    );
    assert_eq!(stored_fingerprint(&*raw), FIXTURE_FINGERPRINT);

    record.refuse.store(false, Ordering::SeqCst);
    assert_eq!(step.run().stage, SwiftImportStage::Done);
    assert_eq!(
        keychain.count("export"),
        1,
        "Try again repeats the store only"
    );
    assert_eq!(
        file.recorded().unwrap().as_deref(),
        Some(FIXTURE_FINGERPRINT)
    );
    assert_eq!(*keychain.marker.lock().unwrap(), Ok(true));
    assert!(preferences.flag(IMPORT_RAN_KEY));
    assert_eq!(graph.gate.handover(), HandoverGate::Ready);
    let loaded = load_identity(&raw.inner, &*file).unwrap();
    assert_eq!(hex(&loaded.fingerprint()), FIXTURE_FINGERPRINT);
}

/// A certificate query that failed may have hidden the Swift identity:
/// the import stays pending rather than end, so the listener cannot mint
/// over it.
#[test]
fn a_failed_certificate_query_leaves_the_import_pending() {
    let dir = tempfile::tempdir().unwrap();
    let preferences = preferences(&dir);
    let keychain = Arc::new(FakeKeychain::swift_app());
    *keychain.certificate_error.lock().unwrap() = Some("errSecInteractionNotAllowed".to_owned());
    let launched = launch_at_home(preferences.clone(), keychain);
    assert!(matches!(launched, Launch::Pending(_)), "{launched:?}");
    assert!(!preferences.flag(IMPORT_RAN_KEY));
    assert!(!preferences.contains(IMPORT_RAN_KEY));
}

#[test]
fn an_export_of_another_certificate_is_refused() {
    let step = step(FakeKeychain::swift_app(), None);
    let other = HandoverIdentity::mint("Another", chrono::Utc::now()).unwrap();
    *step.keychain.certificate.lock().unwrap() = Some(other.certificate_der().to_vec());
    let status = step.import.run();
    assert_eq!(status.stage, SwiftImportStage::Waiting);
    assert_eq!(status.error.as_deref(), Some(FAILED_EXPORT));
    assert_eq!(read(&*step.raw, &HandoverIdentity::secret_key()), None);
}

#[test]
fn a_denied_key_read_leaves_the_key_empty_and_the_identity_still_comes_over() {
    let step = step(FakeKeychain::denying_the_key(), None);
    assert_eq!(step.import.run().stage, SwiftImportStage::Done);
    assert_eq!(read(&*step.graph.secrets, &SecretKey::llm_api_key()), None);
    assert!(step.preferences.flag(KEY_READ_KEY));
    // A key the user saves in Settings is read from the store again.
    set(
        &*step.graph.secrets,
        &SecretKey::llm_api_key(),
        Some("sk-new"),
    );
    assert_eq!(
        read(&*step.graph.secrets, &SecretKey::llm_api_key()).as_deref(),
        Some("sk-new")
    );
}

/// The gate's `Debug` never prints the key the step read.
#[test]
fn the_gate_s_debug_leaves_the_key_out() {
    let step = step(FakeKeychain::swift_app(), None);
    step.import.run();
    let printed = format!("{:?}", step.graph.gate);
    assert!(printed.contains("Read(Some(<redacted>))"), "{printed}");
    assert!(!printed.contains("sk-swift"), "{printed}");
}

/// While the gate hides the key (before the step, after Not now), a
/// cleared key field or a keyless preset saves no key: the write never
/// reaches the store, so the Swift app's item stays, and the gate stays
/// shut. A key the user types goes through.
#[test]
fn a_gated_empty_key_write_leaves_the_stored_key_in_place() {
    let step = step(FakeKeychain::swift_app(), None);
    let key = SecretKey::llm_api_key();
    let write = |value: Option<&str>| set(&*step.graph.secrets, &key, value);
    for skipped in [false, true] {
        if skipped {
            step.import.skip();
        }
        write(None);
        write(Some(""));
        assert_eq!(
            read(&*step.raw, &key).as_deref(),
            Some("sk-swift"),
            "skipped: {skipped}"
        );
        assert_eq!(read(&*step.graph.secrets, &key), None, "skipped: {skipped}");
    }
    write(Some("sk-typed"));
    assert_eq!(read(&*step.raw, &key).as_deref(), Some("sk-typed"));
    assert_eq!(
        read(&*step.graph.secrets, &key).as_deref(),
        Some("sk-typed")
    );
    // Open now: a cleared field removes the key, as without an import.
    write(None);
    assert_eq!(read(&*step.raw, &key), None);
}

/// The graph's store is the import's gate over the app's `KeepsApiKey`,
/// as the graph's one wiring function builds them
/// (`crate::app::gated_secrets`): an empty key write the gate swallows
/// never reaches `KeepsApiKey`, so the key it kept for the pipeline stays;
/// once a saved key opened the gate, `KeepsApiKey` keeps and drops the key
/// as it does without an import.
#[test]
fn a_write_the_gate_swallows_leaves_the_kept_key_and_an_open_gate_writes_through() {
    let dir = tempfile::tempdir().unwrap();
    let key = SecretKey::llm_api_key();
    let memory = Arc::new(InMemorySecretStore::with([(
        key.clone(),
        "sk-swift".to_owned(),
    )]));
    let (_graph, wired) = wired(dir.path(), FakeKeychain::swift_app(), memory.clone());
    let kept = wired.kept.clone();
    assert_eq!(
        read(&*kept, &key).as_deref(),
        Some("sk-swift"),
        "KeepsApiKey reads the store behind no gate"
    );
    for cleared in [None, Some("")] {
        set(&*wired.secrets, &key, cleared);
        assert_eq!(
            kept.kept_api_key().as_deref(),
            Some("sk-swift"),
            "{cleared:?}"
        );
        assert_eq!(read(&*memory, &key).as_deref(), Some("sk-swift"));
    }

    set(&*wired.secrets, &key, Some("sk-typed"));
    assert_eq!(kept.kept_api_key().as_deref(), Some("sk-typed"));
    memory.fail_reads(Some("the keychain is locked"));
    assert!(RUNTIME.block_on(wired.secrets.secret(&key)).is_err());
    assert_eq!(
        kept.kept_api_key().as_deref(),
        Some("sk-typed"),
        "a failed read keeps the key"
    );
    memory.fail_reads(None);
    set(&*wired.secrets, &key, None);
    assert_eq!(kept.kept_api_key(), None, "an open gate's removal drops it");
    assert_eq!(read(&*memory, &key), None);
}

/// The pipeline reads the key through the gate, as the graph's one wiring
/// function builds it (`crate::app::gated_secrets`): behind a pending
/// import its build asks the store behind the gate for no API key, though
/// the `KeepsApiKey` it falls back on wraps that store.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_pipeline_reads_the_key_through_the_gate_and_not_the_store_behind_it() {
    let (dir, store) = crate::testing::temp_store();
    let paths = steno_core::StenoPaths::new(dir.path().join("support"));
    let mut settings = store.settings().unwrap();
    settings.llm_provider = steno_core::LlmProvider::Endpoint;
    settings.llm_base_url = Some("http://127.0.0.1:9/v1".to_owned());
    settings.llm_model = Some("model".to_owned());
    store.save_settings(&settings).unwrap();
    let raw = Arc::new(CountingSecrets::default());
    raw.inner
        .set_secret(&SecretKey::llm_api_key(), Some("sk-swift"))
        .await
        .unwrap();
    let (_graph, wired) = wired(dir.path(), FakeKeychain::swift_app(), raw.clone());
    let built = built_over(&store, &paths, &wired);
    assert!(built.dependencies.summarizer.is_none());
    assert!(
        !raw.read_the_key(),
        "the build read the key behind the gate"
    );
}

/// The gate says where the store behind it keeps the secrets, so Settings
/// word the key's place as without an import.
#[test]
fn the_gate_reports_the_place_of_the_store_behind_it() {
    let dir = tempfile::tempdir().unwrap();
    let file = Arc::new(crate::FileSecretStore::new(
        dir.path().join("secrets.json"),
        std::collections::BTreeMap::new(),
    ));
    let graph = GraphImport::new(
        pending(launch_at_home(
            preferences(&dir),
            Arc::new(FakeKeychain::swift_app()),
        )),
        file,
        record_in(dir.path()),
    );
    assert_eq!(graph.secrets.place(), Some(SecretPlace::File));
}

/// A refused key read is remembered: the next launch, whose import still
/// waits for the identity, answers no key without asking the keychain
/// (so nothing prompts before the step), and so does every launch after
/// the import is over, until the user saves a key.
#[test]
fn a_refused_key_read_is_never_asked_again_until_a_key_is_saved() {
    let first = step(FakeKeychain::denying_the_key(), None);
    first.keychain.deny_export();
    let status = first.import.run();
    assert_eq!(status.stage, SwiftImportStage::Waiting);
    assert!(first.preferences.flag(KEY_DENIED_KEY));

    // The next launch: still pending, the key stays unread.
    let keychain = Arc::new(FakeKeychain::denying_the_key());
    let again = pending(launch_at_home(first.preferences.clone(), keychain.clone()));
    assert_eq!(again.key, LaunchKey::Denied);
    let graph = GraphImport::new(again, first.raw.clone(), first.record.clone());
    first.raw.reads.lock().unwrap().clear();
    let key = SecretKey::llm_api_key();
    assert_eq!(read(&*graph.secrets, &key), None);
    let step = graph.step(RUNTIME.handle().clone(), Box::new(|| {}));
    assert_eq!(step.status().prompts, 1, "the identity alone");
    assert_eq!(step.run().stage, SwiftImportStage::Done);
    assert_eq!(read(&*graph.secrets, &key), None);
    assert!(
        !first.raw.read_the_key(),
        "the keychain was asked for the key"
    );
    assert!(!keychain.calls().contains(&"read key"));

    // The import is over: the graph's store still answers no key...
    let over = key_denied_secrets(&first.preferences, first.raw.clone()).0;
    assert_eq!(read(&*over, &key), None);
    assert!(
        !first.raw.read_the_key(),
        "the keychain was asked for the key"
    );
    // ...until the user saves one, which clears the flag for good.
    set(&*over, &key, Some("sk-new"));
    assert_eq!(read(&*over, &key).as_deref(), Some("sk-new"));
    assert!(!first.preferences.flag(KEY_DENIED_KEY));
    assert_flags_only(first.dir.path());
    let later = key_denied_secrets(&first.preferences, first.raw.clone()).0;
    assert_eq!(read(&*later, &key).as_deref(), Some("sk-new"));
}

/// A refused keychain read hides the keychain's key only: a graph over the
/// secrets file (the CLI's on the Mac) still answers the key it holds.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_graph_over_the_secrets_file_reads_its_key_after_a_refused_keychain_read() {
    let dir = tempfile::tempdir().unwrap();
    let paths = steno_core::StenoPaths::new(dir.path().join("support"));
    std::fs::create_dir_all(&paths.support_directory).unwrap();
    std::fs::write(
        paths.support_directory.join("preferences.json"),
        format!(
            r#"{{"{IMPORT_RAN_KEY}": true, "{KEY_READ_KEY}": true, "{KEY_DENIED_KEY}": true}}"#
        ),
    )
    .unwrap();
    let file = crate::FileSecretStore::new(
        paths.support_directory.join("secrets.json"),
        std::collections::BTreeMap::new(),
    );
    let key = SecretKey::llm_api_key();
    file.set_secret(&key, Some("sk-swift")).await.unwrap();
    let app = crate::build(test_options(paths)).unwrap();
    assert_eq!(
        app.secrets.secret(&key).await.unwrap().as_deref(),
        Some("sk-swift")
    );
    app.shutdown();
}

/// Not now reads nothing and brings up no prompt; unlike a refusal it
/// writes no flag, so the next launch asks again.
#[test]
fn skipping_the_step_reads_nothing_and_the_next_launch_asks_again() {
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
        ["marker", "certificate", "key item", "identity entry"],
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
    assert_eq!(again.key, LaunchKey::Unread(ApiKeyItem::Swift));
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
    let app = build_over(paths, keychain);

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
    // The step and the host write through one `preferences.json`: a flag
    // the app sets after the step keeps the step's flags beside it.
    app.services
        .preferences
        .set_flag(OnboardingViewModel::COMPLETED_KEY, true);
    let on_disk = FilePreferences::in_support_directory(&app.paths.support_directory);
    assert!(on_disk.flag(IMPORT_RAN_KEY), "the step's flag was lost");
    assert!(on_disk.flag(KEY_READ_KEY), "the step's flag was lost");
    assert!(on_disk.flag(OnboardingViewModel::COMPLETED_KEY));
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

/// After a denied export the gate waits: the gated listener stays closed
/// and no identity is minted, though no phone is paired, until Try again
/// brings the Swift identity over; then the listener opens on it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_waiting_gate_keeps_the_listener_closed_until_the_identity_came_over() {
    use std::task::{Context, Poll, Waker};
    use steno_host::services::Handover as _;

    let dir = tempfile::tempdir().unwrap();
    let paths = steno_core::StenoPaths::new(dir.path().join("support"));
    let secrets_path = paths.support_directory.join("secrets.json");
    let keychain = Arc::new(FakeKeychain::swift_app());
    keychain.deny_export();
    let app = build_over(paths, keychain.clone());
    let step = app.services.swift_import.clone().unwrap();
    let run = || {
        let step = step.clone();
        tokio::task::spawn_blocking(move || step.run())
    };
    assert_eq!(run().await.unwrap().stage, SwiftImportStage::Waiting);

    let gated = app.gated_handover.clone().unwrap();
    let (opened, was_opened) = tokio::sync::oneshot::channel();
    let mut follow = std::pin::pin!(gated.clone().follow(move || {
        let _ = opened.send(());
    }));
    let mut context = Context::from_waker(Waker::noop());
    assert_eq!(follow.as_mut().poll(&mut context), Poll::Pending);
    assert!(gated.service().is_none(), "no listener while waiting");
    assert!(
        !std::fs::read_to_string(&secrets_path)
            .unwrap_or_default()
            .contains(HandoverIdentity::SECRET_KEY),
        "no identity minted"
    );

    keychain.allow_export();
    assert_eq!(run().await.unwrap().stage, SwiftImportStage::Done);
    follow.await;
    was_opened.await.unwrap();
    assert_eq!(
        gated.mac_id(),
        "FCF0D2A1-D2CE-48F7-BAFF-E17FD2E9C814",
        "the listener is over the Swift identity"
    );
    app.shutdown();
}

/// D5 through the handover's guard: a desktop-id build ran first, so the
/// secret store holds the identity it minted and its fingerprint is
/// recorded. The Swift identity replaces it at the step, with its
/// fingerprint, so the listener built behind the gate loads the Swift
/// identity instead of refusing it as replaced.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_swift_identity_that_replaced_a_desktop_id_one_is_the_one_the_listener_loads() {
    use steno_handover::FingerprintRecord as _;
    use steno_host::services::Handover as _;

    let dir = tempfile::tempdir().unwrap();
    let paths = steno_core::StenoPaths::new(dir.path().join("support"));
    let desktop = crate::build(test_options(paths.clone())).unwrap();
    let desktop_id = desktop.handover.as_deref().unwrap().mac_id();
    assert!(
        !desktop_id.is_empty(),
        "the desktop-id build minted an identity"
    );
    desktop.shutdown();
    drop(desktop);
    let record = crate::handover::FingerprintFile::in_support_directory(&paths.support_directory);
    let recorded = record
        .recorded()
        .unwrap()
        .expect("its fingerprint is recorded");
    assert_ne!(recorded, FIXTURE_FINGERPRINT);

    let keychain = Arc::new(FakeKeychain {
        stored_identity: Ok(true),
        ..FakeKeychain::swift_app()
    });
    let app = build_over(paths, keychain);
    let step = app.services.swift_import.clone().unwrap();
    let status = tokio::task::spawn_blocking(move || step.run())
        .await
        .unwrap();
    assert_eq!(status.stage, SwiftImportStage::Done);
    assert_eq!(
        record.recorded().unwrap().as_deref(),
        Some(FIXTURE_FINGERPRINT)
    );
    let gated = app.gated_handover.clone().unwrap();
    let mut failure = gated.failure();
    let mut follow = tokio::spawn(gated.clone().follow(|| {}));
    tokio::select! {
        failed = failure.wait_for(Option::is_some) => {
            panic!("the listener refused the Swift identity: {:?}", *failed.unwrap());
        }
        opened = &mut follow => opened.unwrap(),
    }
    assert_eq!(gated.mac_id(), "FCF0D2A1-D2CE-48F7-BAFF-E17FD2E9C814");
    assert_ne!(gated.mac_id(), desktop_id);
    app.shutdown();
}

/// A paired phone proves that an identity existed: with `preferences.json`
/// gone, the Swift certificate gone before the step and the stored
/// identity missing or unreadable, the gate opens but no listener is built
/// and no identity is minted.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_paired_phone_without_a_readable_identity_gets_no_minted_one() {
    for unreadable in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let paths = steno_core::StenoPaths::new(dir.path().join("support"));
        std::fs::create_dir_all(&paths.support_directory).unwrap();
        let secrets_path = paths.support_directory.join("secrets.json");
        if unreadable {
            std::fs::write(&secrets_path, b"not json").unwrap();
        }
        let keychain = Arc::new(FakeKeychain::swift_app());
        let app = build_over(paths, keychain.clone());
        crate::testing::pair_a_phone(&app.store);
        std::fs::remove_file(dir.path().join("support/preferences.json")).unwrap();
        *keychain.certificate.lock().unwrap() = None;
        let step = app.services.swift_import.clone().unwrap();
        let status = tokio::task::spawn_blocking(move || step.run())
            .await
            .unwrap();
        assert_eq!(
            status.stage,
            SwiftImportStage::Done,
            "nothing left to import"
        );
        let gated = app.gated_handover.clone().unwrap();
        let mut failure = gated.failure();
        let mut follow = tokio::spawn(gated.clone().follow(|| panic!("no listener opens")));
        // A listener that opens panics inside `follow`: the test fails on
        // the ended task instead of waiting for a failure that never comes.
        tokio::select! {
            failed = failure.wait_for(Option::is_some) => {
                failed.unwrap();
            }
            ended = &mut follow => panic!("follow ended: {ended:?}"),
        }
        assert!(gated.service().is_none(), "unreadable: {unreadable}");
        assert!(!follow.is_finished(), "follow waits for the next ready");
        follow.abort();
        let secrets = std::fs::read(&secrets_path).unwrap_or_default();
        assert!(
            !String::from_utf8_lossy(&secrets).contains(HandoverIdentity::SECRET_KEY),
            "no identity minted (unreadable: {unreadable})"
        );
        app.shutdown();
    }
}

/// The meeting detail asks what the pipeline asks, over the real gate:
/// behind a pending import whose key is withheld, a ready meeting without
/// a summary says the key is not available yet and offers no re-run for
/// an endpoint that needs the key; for `ChatGPT` summaries (the Codex
/// backend), which need none and keep running, the row is the runnable one
/// and Run summary is on offer.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_detail_withholds_the_summary_only_for_an_endpoint_that_needs_the_key() {
    use steno_bridge::{BridgeHost as _, BridgeTopic, MeetingIdParams};

    let dir = tempfile::tempdir().unwrap();
    let paths = steno_core::StenoPaths::new(dir.path().join("support"));
    let app = build_over(paths, Arc::new(FakeKeychain::swift_app()));
    let gate = app.import_gate.clone().unwrap();
    assert!(gate.key_withheld());
    let meeting = steno_core::testing::sample_data::meeting();
    app.store.save_meeting(&meeting).unwrap();
    let segment = steno_core::TranscriptSegment {
        id: uuid::Uuid::from_u128(1),
        meeting_id: meeting.id,
        start: 0.0,
        end: 2.0,
        speaker_id: None,
        lane: steno_core::AudioLane::Mic,
        text: "hello there".to_owned(),
        raw_text: "hello there".to_owned(),
    };
    app.store
        .replace_transcript(&meeting, &[segment], &[])
        .unwrap();
    let mut settings = app.store.settings().unwrap();
    settings.llm_provider = steno_core::LlmProvider::Endpoint;
    settings.llm_base_url = Some("http://127.0.0.1:9/v1".to_owned());
    settings.llm_model = Some("model".to_owned());
    app.store.save_settings(&settings).unwrap();
    let host = app.host().unwrap();
    let _ = host.snapshot(BridgeTopic::MeetingsList);
    host.meetings_select(MeetingIdParams {
        meeting_id: meeting.id,
    })
    .unwrap();

    let detail = host.snapshot(BridgeTopic::MeetingDetail).unwrap();
    assert_eq!(
        detail["summaryStatus"]["body"],
        steno_host::setup::copy::SUMMARY_KEY_WITHHELD_BODY,
        "endpoint: {detail}"
    );
    assert_eq!(detail["canRerunSummary"], false, "endpoint");

    settings.llm_provider = steno_core::LlmProvider::Codex;
    settings.codex_model = Some("gpt-5".to_owned());
    settings.codex_confirmed_at = Some(chrono::Utc::now());
    app.store.save_settings(&settings).unwrap();
    host.store_changed();
    assert!(gate.key_withheld(), "the key is still withheld");
    let detail = host.snapshot(BridgeTopic::MeetingDetail).unwrap();
    assert_eq!(detail["summaryStatus"]["kind"], "skippedRunnable", "Codex");
    assert_ne!(
        detail["summaryStatus"]["body"],
        steno_host::setup::copy::SUMMARY_KEY_WITHHELD_BODY
    );
    assert_eq!(detail["canRerunSummary"], true, "Codex");
    app.shutdown();
}

fn test_options(paths: steno_core::StenoPaths) -> crate::AppOptions {
    crate::AppOptions {
        paths,
        database_path: None,
        keyring: false,
        opener: Arc::new(steno_host::fakes::FakeOpener::default()),
        login_item: None,
        runtime: tokio::runtime::Handle::current(),
        version: "0.0.0".to_owned(),
        make_capture_session: Arc::new(|_| Err("no capture in this test".to_owned())),
        lock_patience: std::time::Duration::ZERO,
        update_source: None,
        install_gate: Arc::new(crate::updates::NeverIdle),
    }
}

/// The graph under `paths` over the launch half against the Swift domain
/// fixture and `keychain`, which must leave the import pending.
fn build_over(paths: steno_core::StenoPaths, keychain: Arc<FakeKeychain>) -> crate::App {
    crate::build_with_import(test_options(paths), |preferences| {
        Some(pending(launch_at_home(preferences, keychain)))
    })
    .unwrap()
}

/// An update source that finds nothing, for a graph whose updater is the
/// update schedule (S4).
struct NoUpdates;

#[async_trait]
impl crate::updates::UpdateSource for NoUpdates {
    async fn check(&self) -> Result<Option<String>, String> {
        Ok(None)
    }
    async fn download(&self, _version: &str) -> Result<Vec<u8>, String> {
        Ok(Vec::new())
    }
    async fn install(&self, _version: &str, _package: Vec<u8>) -> Result<(), String> {
        Ok(())
    }
    async fn relaunch(&self) {}
    async fn ask(&self, _question: crate::updates::Question<'_>) -> bool {
        false
    }
    fn tell_install_failed(&self, _message: &str) {}
    fn announce(&self, _version: &str) {}
}

/// Sparkle's two choices come over into exactly the flags the update
/// schedule (S4) reads, through the graph's one `preferences.json`: with
/// the Swift app's automatic checks off and automatic downloads on (both
/// the opposite of the schedule's defaults), the host's updater answers
/// the Swift app's choices, with the import pending or over at launch,
/// and a flag the updater sets after the step keeps the step's flags
/// beside it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_update_schedule_reads_the_flags_the_launch_half_copied() {
    let domain = br#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>SUAutomaticallyUpdate</key>
	<true/>
	<key>SUEnableAutomaticChecks</key>
	<false/>
	<key>steno.onboardingCompleted</key>
	<true/>
</dict>
</plist>
"#;
    for certificate in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        let paths = steno_core::StenoPaths::new(dir.path().join("support"));
        let keychain = Arc::new(FakeKeychain::swift_app());
        if !certificate {
            *keychain.certificate.lock().unwrap() = None;
        }
        let options = crate::AppOptions {
            update_source: Some(Arc::new(NoUpdates)),
            ..test_options(paths.clone())
        };
        let app = crate::build_with_import(options, |preferences| {
            match launch(
                &at_home(),
                preferences,
                &FakeDefaults::new(domain.to_vec()),
                keychain,
            ) {
                Launch::Pending(pending) => Some(pending),
                _ => None,
            }
        })
        .unwrap();
        assert_eq!(app.import_gate.is_some(), certificate);
        let updater = app.services.updater.clone();
        assert!(!updater.automatically_checks(), "pending: {certificate}");
        assert!(updater.automatically_downloads(), "pending: {certificate}");

        if let Some(step) = app.services.swift_import.clone() {
            let status = tokio::task::spawn_blocking(move || step.run())
                .await
                .unwrap();
            assert_eq!(status.stage, SwiftImportStage::Done);
        }
        updater.set_automatically_checks(true);
        let on_disk = FilePreferences::in_support_directory(&paths.support_directory);
        assert_eq!(on_disk.stored_flag(AUTOMATIC_CHECKS_KEY), Some(true));
        assert_eq!(on_disk.stored_flag(AUTOMATIC_DOWNLOAD_KEY), Some(true));
        assert!(on_disk.flag(IMPORT_RAN_KEY), "pending: {certificate}");
        assert!(on_disk.flag(OnboardingViewModel::COMPLETED_KEY));
        app.shutdown();
    }
}

/// A second app that the database's lock refuses (#225) runs no launch
/// half: it reads neither the Swift domain nor the keychain and writes no
/// `preferences.json` beside the first app's.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_app_the_database_lock_refuses_runs_no_launch_half() {
    let dir = tempfile::tempdir().unwrap();
    let paths = steno_core::StenoPaths::new(dir.path().join("support"));
    std::fs::create_dir_all(&paths.support_directory).unwrap();
    let _first = steno_core::DatabaseLock::acquire(&paths.database_path()).unwrap();
    let ran = AtomicUsize::new(0);
    let refused = crate::build_with_import(test_options(paths.clone()), |_| {
        ran.fetch_add(1, Ordering::SeqCst);
        None
    })
    .err()
    .unwrap();
    assert!(
        matches!(
            refused,
            crate::BuildError::Lock(steno_core::DatabaseLockError::Held(_))
        ),
        "{refused:?}"
    );
    assert_eq!(ran.load(Ordering::SeqCst), 0, "the launch half ran");
    assert!(!paths.support_directory.join("preferences.json").exists());
}

/// How long a call that must not wait may take before the test fails
/// instead of hanging.
const NO_WAIT: std::time::Duration = std::time::Duration::from_secs(30);

/// `call` on another thread, from inside the fake's export (as if while
/// its prompt is up). Once the export ran, the closure this returns gives
/// what `call` answered, or the timeout when it was still waiting after
/// [`NO_WAIT`].
fn during_the_export_prompt(
    step: &Step,
    call: impl Fn(&ImportStep) -> SwiftImportStatus + Send + Sync + 'static,
) -> impl FnOnce() -> Result<SwiftImportStatus, std::sync::mpsc::RecvTimeoutError> {
    let answered = Arc::new(Mutex::new(None));
    let seen = answered.clone();
    let import = Arc::downgrade(&step.import);
    let call = Arc::new(call);
    step.keychain.on_call(move |name| {
        let Some(import) = import.upgrade() else {
            return;
        };
        if name != "export" || seen.lock().unwrap().is_some() {
            return;
        }
        let (sender, receiver) = std::sync::mpsc::channel();
        let call = call.clone();
        std::thread::spawn(move || {
            let _ = sender.send(call(&import));
        });
        *seen.lock().unwrap() = Some(receiver.recv_timeout(NO_WAIT));
    });
    move || answered.lock().unwrap().take().expect("no export ran")
}

/// Closing the onboarding window while the export's prompt is up skips the
/// step on the main thread: the skip answers at once, without waiting for
/// the prompt, and changes nothing; the run then ends as the user answers.
#[test]
fn a_skip_while_the_export_prompt_is_up_answers_at_once_and_mints_nothing() {
    let step = step(FakeKeychain::swift_app(), None);
    let skipped = during_the_export_prompt(&step, ImportStep::skip);
    let status = step.import.run();
    let skipped = skipped().expect("the skip waited for the prompt");
    assert_eq!(skipped.stage, SwiftImportStage::Pending, "the run decides");
    assert_eq!(status.stage, SwiftImportStage::Done);
    assert_eq!(step.graph.gate.handover(), HandoverGate::Ready);
    assert_eq!(
        stored_fingerprint(&*step.raw),
        FIXTURE_FINGERPRINT,
        "the Swift identity, nothing minted"
    );
}

/// A second Continue while the first run's prompt is up reads nothing and
/// brings up no second prompt.
#[test]
fn a_second_run_while_the_export_prompt_is_up_reads_nothing() {
    let step = step(FakeKeychain::swift_app(), None);
    step.keychain.deny_export();
    let second = during_the_export_prompt(&step, ImportStep::run);
    let status = step.import.run();
    let second = second().expect("the second run waited for the first");
    assert_eq!(second.stage, SwiftImportStage::Pending);
    assert_eq!(status.stage, SwiftImportStage::Waiting);
    let calls = step.keychain.calls();
    assert_eq!(step.keychain.count("export"), 1, "{calls:?}");
    assert_eq!(step.keychain.count("read key"), 1);
}

/// A key the user saved in Settings before the step is the key: the run
/// does not read the Swift item over it (so a refusal there cannot hide
/// it), and Not now leaves it too.
#[test]
fn a_key_saved_before_the_step_is_not_read_over() {
    for skipping in [false, true] {
        let step = step(FakeKeychain::denying_the_key(), None);
        let key = SecretKey::llm_api_key();
        set(&*step.graph.secrets, &key, Some("sk-saved"));
        if skipping {
            step.import.skip();
        } else {
            assert_eq!(step.import.run().stage, SwiftImportStage::Done);
            assert!(step.preferences.flag(KEY_READ_KEY));
        }
        assert!(
            !step.keychain.calls().contains(&"read key"),
            "skipping: {skipping}"
        );
        assert!(
            !step.preferences.flag(KEY_DENIED_KEY),
            "skipping: {skipping}"
        );
        assert_eq!(
            read(&*step.graph.secrets, &key).as_deref(),
            Some("sk-saved"),
            "skipping: {skipping}"
        );
    }
}

/// Clearing the key the step read drops it at this launch, though the write
/// never reaches the store (the item the Swift app shares stays).
#[test]
fn clearing_the_key_the_step_read_drops_it_for_this_launch() {
    let step = step(FakeKeychain::swift_app(), None);
    assert_eq!(step.import.run().stage, SwiftImportStage::Done);
    let key = SecretKey::llm_api_key();
    assert_eq!(
        read(&*step.graph.secrets, &key).as_deref(),
        Some("sk-swift")
    );
    set(&*step.graph.secrets, &key, None);
    assert_eq!(read(&*step.graph.secrets, &key), None);
    assert_eq!(read(&*step.raw, &key).as_deref(), Some("sk-swift"));
}

/// No flag is on disk while the key's prompt is up, so an app quit then
/// leaves the next launch with the key unread, and the step asks again;
/// only the answer writes `KEY_READ_KEY` (and `KEY_DENIED_KEY`).
#[test]
fn no_flag_is_written_while_the_key_prompt_is_up() {
    for denied in [false, true] {
        let keychain = if denied {
            FakeKeychain::denying_the_key()
        } else {
            FakeKeychain::swift_app()
        };
        let step = step(keychain, None);
        let flags = Arc::new(Mutex::new(None));
        let seen = flags.clone();
        let support = step.dir.path().to_owned();
        step.keychain.on_call(move |name| {
            if name == "read key" {
                let on_disk = FilePreferences::in_support_directory(&support);
                *seen.lock().unwrap() = Some((
                    on_disk.contains(KEY_READ_KEY),
                    on_disk.contains(KEY_DENIED_KEY),
                ));
            }
        });
        step.import.run();
        assert_eq!(
            flags.lock().unwrap().take(),
            Some((false, false)),
            "denied: {denied}"
        );
        assert!(step.preferences.flag(KEY_READ_KEY));
        assert_eq!(step.preferences.flag(KEY_DENIED_KEY), denied);
    }
}

/// A key save the store refuses keeps the refused read on record: the key
/// stays unasked until a save goes through.
#[test]
fn a_refused_key_save_keeps_the_refused_read_flag() {
    let dir = tempfile::tempdir().unwrap();
    let preferences = preferences(&dir);
    preferences.set_flag(KEY_DENIED_KEY, true);
    let raw = Arc::new(CountingSecrets::default());
    raw.refuse_key.store(true, Ordering::SeqCst);
    let gated = key_denied_secrets(&preferences, raw.clone()).0;
    let key = SecretKey::llm_api_key();
    assert!(
        RUNTIME
            .block_on(gated.set_secret(&key, Some("sk-new")))
            .is_err()
    );
    assert!(preferences.flag(KEY_DENIED_KEY));
    assert_eq!(read(&*gated, &key), None);
    assert!(
        FilePreferences::in_support_directory(dir.path()).flag(KEY_DENIED_KEY),
        "the flag on disk"
    );
}

/// Attribute queries that fail count as items that may prompt: the key as
/// one that is not the Swift app's (the graph reads it after the step), the
/// identity entry as one the import replaces.
#[test]
fn failed_attribute_queries_count_as_items_that_may_prompt() {
    let dir = tempfile::tempdir().unwrap();
    let keychain = Arc::new(FakeKeychain {
        key_item: Err("errSecInteractionNotAllowed".to_owned()),
        stored_identity: Err("errSecInteractionNotAllowed".to_owned()),
        ..FakeKeychain::swift_app()
    });
    let pending = pending(launch_at_home(preferences(&dir), keychain.clone()));
    assert_eq!(pending.key, LaunchKey::Unread(ApiKeyItem::Other));
    assert!(pending.replaces_identity);
    let graph = GraphImport::new(
        pending,
        Arc::new(CountingSecrets::default()),
        record_in(dir.path()),
    );
    let step = graph.step(RUNTIME.handle().clone(), Box::new(|| {}));
    assert_eq!(
        step.status().prompts,
        3,
        "the key, the export and the entry"
    );
    assert_eq!(step.run().stage, SwiftImportStage::Done);
    assert!(!keychain.calls().contains(&"read key"), "not the Swift key");
}

/// The marker is written second, after the identity, and before
/// `IMPORT_RAN_KEY`: at its write the store already holds the Swift
/// identity and `preferences.json` on disk has no `IMPORT_RAN_KEY` yet, so
/// no crash leaves the flag without the marker (a `preferences.json` set
/// aside later would then bring the import back). A crash between the
/// identity and the marker leaves neither, and the rerun stores the same
/// Swift identity again (harmless) and then the marker.
#[test]
fn the_marker_comes_after_the_identity_and_a_crash_between_them_stores_the_same_identity_again() {
    let step = step(FakeKeychain::swift_app(), None);
    let raw = step.raw.clone();
    let support = step.dir.path().to_path_buf();
    let at_marker = Arc::new(Mutex::new(None));
    let seen = at_marker.clone();
    step.keychain.on_call(move |name| {
        let mut seen = seen.lock().unwrap();
        if name == "mark done" && seen.is_none() {
            let on_disk = FilePreferences::in_support_directory(&support);
            *seen = Some((
                read(&*raw, &HandoverIdentity::secret_key()).is_some(),
                on_disk.contains(IMPORT_RAN_KEY),
            ));
        }
    });
    assert_eq!(step.import.run().stage, SwiftImportStage::Done);
    let (identity_first, flag_first) = at_marker.lock().unwrap().take().unwrap();
    assert!(identity_first, "the marker came before the identity");
    assert!(!flag_first, "IMPORT_RAN came before the marker");
    assert!(step.preferences.flag(IMPORT_RAN_KEY));
    assert_eq!(step.keychain.count("mark done"), 1);

    // The crash window: the identity is stored, the marker and the flag
    // are not.
    let dir = tempfile::tempdir().unwrap();
    let keychain = Arc::new(FakeKeychain::swift_app());
    let raw = Arc::new(CountingSecrets::default());
    let (_, bundle) = decode_pkcs12(&identity_pkcs12("p"), "p", &identity_der()).unwrap();
    RUNTIME
        .block_on(store_imported_identity(
            &*raw,
            &*record_in(dir.path()),
            &bundle,
        ))
        .unwrap();
    let graph = GraphImport::new(
        pending(launch_at_home(preferences(&dir), keychain.clone())),
        raw.clone(),
        record_in(dir.path()),
    );
    let rerun = graph.step(RUNTIME.handle().clone(), Box::new(|| {}));
    assert_eq!(rerun.run().stage, SwiftImportStage::Done);
    assert_eq!(stored_fingerprint(&*raw), FIXTURE_FINGERPRINT);
    assert_eq!(*keychain.marker.lock().unwrap(), Ok(true));
}

/// A marker query that fails is not a first run: the run replaces nothing
/// (no export prompt, the stored identity stays) and waits with Try
/// again, until a query answers.
#[test]
fn an_unreadable_marker_replaces_nothing_until_a_query_answers() {
    let desktop =
        HandoverIdentity::mint("Steno on a desktop-id build", chrono::Utc::now()).unwrap();
    for later in [Ok(false), Ok(true)] {
        let keychain = FakeKeychain::swift_app();
        *keychain.marker.lock().unwrap() = Err("errSecInteractionNotAllowed".to_owned());
        let step = step(keychain, Some(&desktop));
        let before = read(&*step.raw, &HandoverIdentity::secret_key());
        let status = step.import.run();
        assert_eq!(status.stage, SwiftImportStage::Waiting);
        assert_eq!(status.error.as_deref(), Some(FAILED_EXPORT));
        assert_eq!(step.keychain.count("export"), 0, "an export prompt came up");
        assert_eq!(read(&*step.raw, &HandoverIdentity::secret_key()), before);
        assert!(!step.preferences.flag(IMPORT_RAN_KEY));
        assert_eq!(
            step.graph.gate.handover(),
            HandoverGate::Waiting(WaitReason::ImportDenied)
        );

        let found = later == Ok(true);
        *step.keychain.marker.lock().unwrap() = later;
        assert_eq!(step.import.run().stage, SwiftImportStage::Done);
        let fingerprint = stored_fingerprint(&*step.raw);
        if found {
            assert_eq!(step.keychain.count("export"), 0);
            assert_eq!(fingerprint, hex(&desktop.fingerprint()), "replaced");
        } else {
            assert_eq!(fingerprint, FIXTURE_FINGERPRINT, "a first run replaces");
        }
    }
}

/// Pair again (Settings) stores a new identity and keeps the marker: with
/// `preferences.json` set aside since, the next launch finds the marker,
/// asks nothing and leaves that identity in place, though the Swift
/// certificate is still in the keychain.
#[test]
fn after_the_import_a_set_aside_preferences_file_brings_no_replace_and_no_prompt() {
    let step = step(FakeKeychain::swift_app(), None);
    assert_eq!(step.import.run().stage, SwiftImportStage::Done);
    let paired_again = HandoverIdentity::mint("Steno, paired again", chrono::Utc::now()).unwrap();
    set(
        &*step.raw,
        &HandoverIdentity::secret_key(),
        Some(&paired_again.to_pem().unwrap()),
    );
    std::fs::remove_file(step.dir.path().join("preferences.json")).unwrap();
    step.keychain.calls.lock().unwrap().clear();

    let fresh = preferences(&step.dir);
    assert!(matches!(
        launch_at_home(fresh.clone(), step.keychain.clone()),
        Launch::Done
    ));
    assert_eq!(step.keychain.calls(), ["marker"], "a prompt may come up");
    assert!(fresh.flag(IMPORT_RAN_KEY));
    assert_eq!(
        stored_fingerprint(&*step.raw),
        hex(&paired_again.fingerprint())
    );
}

/// A key saved in Settings while the Swift key's prompt is up wins over a
/// Deny there: the refused read sets no flag that would hide the saved
/// key at this launch or the next.
#[test]
fn a_key_saved_while_the_key_prompt_is_up_survives_a_deny() {
    let step = step(FakeKeychain::denying_the_key(), None);
    let gated = step.graph.secrets.clone();
    step.keychain.on_call(move |name| {
        if name == "read key" {
            set(&*gated, &SecretKey::llm_api_key(), Some("sk-saved"));
        }
    });
    step.import.run();
    let key = SecretKey::llm_api_key();
    assert!(!step.preferences.flag(KEY_DENIED_KEY));
    assert_eq!(
        read(&*step.graph.secrets, &key).as_deref(),
        Some("sk-saved")
    );
    let next = key_denied_secrets(&step.preferences, step.raw.clone()).0;
    assert_eq!(read(&*next, &key).as_deref(), Some("sk-saved"));
}

/// The graph's secrets over `platform`, wired as the graph wires them
/// (`crate::app::gated_secrets`), behind the import the launch half leaves
/// pending under `support` with `keychain`; with that import.
fn wired(
    support: &Path,
    keychain: FakeKeychain,
    platform: Arc<dyn SecretStore>,
) -> (GraphImport, crate::app::GraphSecrets) {
    let preferences = Arc::new(FilePreferences::in_support_directory(support));
    let (import, secrets) = crate::app::gated_secrets(
        Some(pending(launch_at_home(
            preferences.clone(),
            Arc::new(keychain),
        ))),
        &preferences,
        platform,
        &steno_core::StenoPaths::new(support.to_path_buf()),
        false,
    );
    (import.expect("the import is pending"), secrets)
}

/// The pipeline's dependencies over the graph's secrets, with the stored
/// settings.
fn built_over(
    store: &Arc<steno_core::Store>,
    paths: &steno_core::StenoPaths,
    secrets: &crate::app::GraphSecrets,
) -> crate::pipeline::BuiltPipeline {
    crate::app::pipeline_dependencies(
        store,
        &crate::speech::SpeechEngines::new(crate::speech::SpeechSetup::new(
            &store.settings().unwrap(),
            paths,
        )),
        secrets,
        &crate::llm::codex_store(),
        &steno_pipeline::MeetingEventBus::new(),
        &tokio::runtime::Handle::current(),
    )
    .unwrap()
}

/// A withheld key (before the step, after Not now) is no summaries: the
/// pipeline gets no LLM pass, so a meeting completes without a summary
/// rather than fail at it; once a key is saved the reload has both
/// passes, and the summary can be run again. The Codex backend needs no
/// key, and without a key item there is nothing to withhold.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_withheld_key_builds_no_llm_pass_until_a_key_is_saved() {
    let (dir, store) = crate::testing::temp_store();
    let paths = steno_core::StenoPaths::new(dir.path().join("support"));
    let mut settings = store.settings().unwrap();
    settings.llm_provider = steno_core::LlmProvider::Endpoint;
    settings.llm_base_url = Some("http://127.0.0.1:9/v1".to_owned());
    settings.llm_model = Some("model".to_owned());
    store.save_settings(&settings).unwrap();
    let graph_for =
        |keychain: FakeKeychain| wired(dir.path(), keychain, Arc::new(InMemorySecretStore::new()));
    let passes = |built: &crate::pipeline::BuiltPipeline| {
        let dependencies = &built.dependencies;
        (
            dependencies.cleaner.is_some(),
            dependencies.summarizer.is_some(),
        )
    };

    let (graph, secrets) = graph_for(FakeKeychain::swift_app());
    assert_eq!(
        passes(&built_over(&store, &paths, &secrets)),
        (false, false)
    );
    let step = graph.step(tokio::runtime::Handle::current(), Box::new(|| {}));
    let step = Arc::new(step);
    let skipping = step.clone();
    tokio::task::spawn_blocking(move || skipping.skip())
        .await
        .unwrap();
    assert!(graph.gate.key_withheld());
    assert_eq!(
        passes(&built_over(&store, &paths, &secrets)),
        (false, false)
    );
    graph
        .secrets
        .set_secret(&SecretKey::llm_api_key(), Some("sk-saved"))
        .await
        .unwrap();
    assert_eq!(passes(&built_over(&store, &paths, &secrets)), (true, true));

    let (missing, missing_secrets) = graph_for(FakeKeychain {
        key_item: Ok(ApiKeyItem::Missing),
        ..FakeKeychain::swift_app()
    });
    assert!(!missing.gate.key_withheld());
    assert_eq!(
        passes(&built_over(&store, &paths, &missing_secrets)),
        (true, true)
    );

    settings.llm_provider = steno_core::LlmProvider::Codex;
    settings.codex_model = Some("gpt-5".to_owned());
    settings.codex_confirmed_at = Some(chrono::Utc::now());
    store.save_settings(&settings).unwrap();
    let (codex, codex_secrets) = graph_for(FakeKeychain::swift_app());
    assert!(codex.gate.key_withheld());
    assert!(!codex.gate.withholds_summaries(&store.settings().unwrap()));
    assert_eq!(
        passes(&built_over(&store, &paths, &codex_secrets)),
        (true, true)
    );
}

/// A meeting processed while the key is withheld runs neither LLM pass:
/// no request reaches the endpoint, the transcript stays raw (no cleanup)
/// and the meeting is ready without a summary, never failed. Once a key is
/// saved, the summary runs again from the stored transcript, with the
/// recording gone (retention may have removed it), and the endpoint gets
/// the saved key. The re-run summarises only, so the transcript stays raw.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_meeting_under_a_withheld_key_stays_raw_and_its_summary_runs_later_without_its_audio() {
    use steno_core::{AudioRetention, MeetingState};
    use steno_llm::testing::{Scripts, StubChatServer};

    let (dir, store) = crate::testing::temp_store();
    let paths = steno_core::StenoPaths::new(dir.path().join("support"));
    let server = StubChatServer::start().await.unwrap();
    let summary =
        std::fs::read_to_string(repository().join("Tests/Fixtures/llm/responses/e2e-summary.json"))
            .unwrap();
    let usage = steno_core::LlmUsage {
        prompt_tokens: 10,
        completion_tokens: 5,
        requests: 1,
    };
    let echo = Scripts.cleanup_echo(usage, |_, text| Some(text.to_uppercase()));
    server.respond(Arc::new(move |request| {
        if request.purpose.as_deref() == Some("summary") {
            Some(Scripts.text(&summary))
        } else {
            echo(request)
        }
    }));
    let mut settings = store.settings().unwrap();
    settings.llm_provider = steno_core::LlmProvider::Endpoint;
    settings.llm_base_url = Some(server.base_url().to_string());
    settings.llm_model = Some("stub-model".to_owned());
    store.save_settings(&settings).unwrap();
    let (graph, secrets) = wired(
        dir.path(),
        FakeKeychain::swift_app(),
        Arc::new(InMemorySecretStore::new()),
    );
    assert!(graph.gate.key_withheld());
    // The passes the graph builds now, on core's fakes for the rest.
    let pipeline_now = || {
        let built = built_over(&store, &paths, &secrets).dependencies;
        steno_pipeline::ProcessingPipeline::new(
            crate::testing::fake_dependencies(&store, "fake-engine")
                .with_llm(built.cleaner, built.summarizer),
        )
    };

    let pipeline = pipeline_now();
    let mut meeting = steno_core::testing::sample_data::meeting();
    meeting.state = MeetingState::Recording;
    meeting.summary = None;
    let audio = dir.path().join("recordings");
    let asset =
        steno_pipeline::fixtures::two_lane_call(&audio, meeting.id, AudioRetention::KeepForever)
            .unwrap();
    pipeline.enqueue(&meeting, &asset).unwrap();
    pipeline.wait_until_idle().await;
    let stored = store.meeting(meeting.id).unwrap().unwrap();
    assert_eq!(stored.state, MeetingState::Ready, "{:?}", stored.state);
    assert!(stored.summary.is_none());
    let segments = store.export(meeting.id).unwrap().segments;
    assert_ne!(segments.len(), 0);
    assert!(
        segments
            .iter()
            .all(|segment| segment.text == segment.raw_text),
        "the cleanup ran under a withheld key"
    );
    assert_eq!(
        server.request_count(),
        0,
        "a request went out without a key"
    );

    std::fs::remove_dir_all(&audio).unwrap();
    graph
        .secrets
        .set_secret(&SecretKey::llm_api_key(), Some("sk-saved"))
        .await
        .unwrap();
    pipeline_now()
        .rerun_summary(meeting.id, &stored.template_id)
        .await
        .unwrap();
    let stored = store.meeting(meeting.id).unwrap().unwrap();
    assert_eq!(stored.state, MeetingState::Ready);
    assert!(stored.summary.is_some(), "the summary ran again");
    assert_eq!(store.export(meeting.id).unwrap().segments, segments);
    let requests = server.requests();
    let purposes: Vec<_> = requests
        .iter()
        .map(|request| request.purpose.as_deref())
        .collect();
    assert_eq!(purposes, [Some("summary")]);
    assert_eq!(requests[0].authorization(), Some("Bearer sk-saved"));
    assert!(
        requests[0].body_text().contains(&segments[0].raw_text),
        "the summary read the stored transcript"
    );
    server.stop();
}

/// The pipeline the M1 test builds: core's fakes, with a cleaner that
/// fails as a server refuses a request without a key (a 401) whenever the
/// gated store answered none when it was built.
fn keyed_make(
    store: &Arc<steno_core::Store>,
    secrets: &Arc<dyn SecretStore>,
) -> crate::pipeline::MakeDependencies {
    let (store, secrets) = (store.clone(), secrets.clone());
    Arc::new(move || {
        let key = crate::block_on(
            &tokio::runtime::Handle::current(),
            secrets.secret(&SecretKey::llm_api_key()),
        )
        .unwrap();
        let cleaner = steno_core::testing::PassthroughCleaner {
            failure: key
                .is_none()
                .then(|| "401 Unauthorized: no API key".to_owned()),
            ..steno_core::testing::PassthroughCleaner::default()
        };
        Ok(crate::testing::built(
            crate::testing::fake_dependencies(&store, "fake-engine")
                .with_llm(Some(Arc::new(cleaner)), None),
        ))
    })
}

/// What the capture hands over for a recording that ended normally.
fn finished_result(
    meeting: &steno_core::Meeting,
    files: &steno_audio::writer::RecordingFiles,
    lanes: &[steno_core::AudioLane],
) -> steno_pipeline::RecordingResult {
    steno_pipeline::RecordingResult {
        asset: steno_core::AudioAsset {
            id: uuid::Uuid::new_v4(),
            meeting_id: meeting.id,
            url: steno_core::paths::file_url(&files.master, false),
            format: steno_core::AudioFormat::Caf48kFloat32,
            lanes: lanes.to_vec(),
            sidecars_16k: std::collections::BTreeMap::new(),
            mixdown_url: None,
            retention: steno_core::AudioRetention::KeepForever,
            expires_at: None,
        },
        duration: files.duration,
        end_reason: steno_core::RecordingEndReason::Manual,
    }
}

/// The graph over core's fakes behind a pending import whose store holds
/// the Swift key, on a pipeline built by [`keyed_make`], with its step.
async fn app_behind_a_pending_import(
    dir: &tempfile::TempDir,
    store: &Arc<steno_core::Store>,
) -> (crate::App, Arc<ImportStep>) {
    let mut app =
        crate::testing::app_over_fakes(dir.path(), store, crate::testing::synthetic_capture());
    app.live_recording_check = crate::testing::an_hour_later();
    let secrets = Arc::new(InMemorySecretStore::new());
    secrets
        .set_secret(&SecretKey::llm_api_key(), Some("sk-swift"))
        .await
        .unwrap();
    let support = dir.path().join("support");
    std::fs::create_dir_all(&support).unwrap();
    let graph = GraphImport::new(
        pending(launch_at_home(
            Arc::new(FilePreferences::in_support_directory(&support)),
            Arc::new(FakeKeychain::swift_app()),
        )),
        secrets,
        record_in(&support),
    );
    let make = keyed_make(store, &graph.secrets);
    app.pipeline = Arc::new(crate::pipeline::CurrentPipeline::new(
        make().unwrap(),
        make,
        tokio::runtime::Handle::current(),
    ));
    app.import_gate = Some(graph.gate.clone());
    let reloaded = app.pipeline.clone();
    let step = Arc::new(graph.step(
        tokio::runtime::Handle::current(),
        Box::new(move || reloaded.reload().unwrap()),
    ));

    (app, step)
}

/// What the Swift app left at the update, launched behind a pending
/// import: a queued meeting (`resume_unfinished`) and an interrupted
/// recording (the recovery's intake) wait until the step ran, then run on
/// the pipeline the step reloaded with the key. The retention sweep waits
/// with them, so it follows the resume: a ready meeting's expired audio
/// stays until the step ran. A meeting that failed for want of the key
/// before the step processes again with it, through the app's Process
/// again (`ProcessingPipeline::process_again`, #240).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn meetings_left_at_the_first_launch_wait_for_the_step_and_run_with_its_key() {
    use steno_audio::writer::RecordingWriting as _;
    use steno_core::{AudioLane, MeetingSource, MeetingState};

    let (dir, store) = crate::testing::temp_store();
    let (app, step) = app_behind_a_pending_import(&dir, &store).await;

    let intake = || {
        steno_pipeline::LocalRecordingIntake::over(store.clone(), app.pipeline.current(), app.zone)
    };
    let lanes = [AudioLane::Mic, AudioLane::System];
    let recording = |frames: usize, finish: bool| {
        let meeting = intake()
            .begin(
                uuid::Uuid::new_v4(),
                MeetingSource::MacCall,
                None,
                None,
                &[],
                chrono::Utc::now(),
            )
            .unwrap();
        let layout = steno_core::RecordingLayout::new(&dir.path().join("audio"), meeting.id);
        let mut writer = steno_audio::RecordingWriter::new(&layout, &lanes, false).unwrap();
        crate::testing::write_frames(&mut writer, frames);
        let files = finish.then(|| writer.finish().unwrap());
        (meeting, files)
    };
    // Left queued by the Swift app's last run.
    let (queued, files) = recording(100, true);
    let finished = finished_result(&queued, files.as_ref().unwrap(), &lanes);
    let mut left = queued.clone();
    left.state = MeetingState::Queued;
    left.duration = finished.duration;
    store
        .save_meeting_with_asset(&left, &finished.asset)
        .unwrap();
    // Interrupted: its writer died.
    let (interrupted, _) = recording(100, false);
    // Processed before the step, without the key: failed.
    let (keyless, files) = recording(100, true);
    intake()
        .complete(
            keyless.id,
            finished_result(&keyless, files.as_ref().unwrap(), &lanes),
            None,
        )
        .await
        .unwrap();
    app.pipeline.current().wait_until_idle().await;
    let state = |meeting: &steno_core::Meeting| store.meeting(meeting.id).unwrap().unwrap().state;
    assert!(matches!(state(&keyless), MeetingState::Failed { .. }));
    // Ready, its audio past its retention.
    let (expired, files) = recording(100, true);
    let mut asset = finished_result(&expired, files.as_ref().unwrap(), &lanes).asset;
    asset.retention = steno_core::AudioRetention::KeepDays(1);
    asset.expires_at = Some(chrono::Utc::now() - chrono::Duration::hours(1));
    let mut ready = expired.clone();
    ready.state = MeetingState::Ready;
    store.save_meeting_with_asset(&ready, &asset).unwrap();
    let expired_master = files.unwrap().master;

    let host = Arc::new(app.host().unwrap());
    app.launch(&host);
    assert!(expired_master.exists(), "swept before the step");
    assert_eq!(
        state(&queued),
        MeetingState::Queued,
        "resumed before the step"
    );
    assert_eq!(
        state(&interrupted),
        MeetingState::Recording,
        "recovered before the step"
    );

    let running = step.clone();
    let status = tokio::task::spawn_blocking(move || running.run())
        .await
        .unwrap();
    assert_eq!(status.stage, SwiftImportStage::Done);
    app.launch_finished().await;
    app.pipeline.current().wait_until_idle().await;
    assert_eq!(
        state(&queued),
        MeetingState::Ready,
        "the resumed job ran without the key"
    );
    assert_eq!(
        state(&interrupted),
        MeetingState::Ready,
        "the recovered job ran without the key"
    );
    assert!(
        !expired_master.exists(),
        "the sweep did not run after the step"
    );

    // Process again, with the key now.
    app.pipeline.current().process_again(keyless.id).unwrap();
    app.pipeline.current().wait_until_idle().await;
    assert_eq!(state(&keyless), MeetingState::Ready);
    app.shutdown();
}
