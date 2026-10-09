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
    /// The certificate query's failure, which wins over `certificate`.
    certificate_error: Mutex<Option<String>>,
    key_item: ApiKeyItem,
    /// A `handover-identity` entry is filed (a desktop-id build's).
    stored_identity: bool,
    key: Result<Option<String>, KeychainRefusal>,
    /// The export's refusal; `None` exports the test identity.
    export_refusal: Mutex<Option<KeychainRefusal>>,
    /// The certificate each export was asked for.
    exported: Mutex<Vec<Vec<u8>>>,
    calls: Mutex<Vec<&'static str>>,
}

impl FakeKeychain {
    fn swift_app() -> Self {
        FakeKeychain {
            certificate: Mutex::new(Some(identity_der())),
            certificate_error: Mutex::new(None),
            key_item: ApiKeyItem::Swift,
            stored_identity: false,
            key: Ok(Some("sk-swift".to_owned())),
            export_refusal: Mutex::new(None),
            exported: Mutex::new(Vec::new()),
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
        if let Some(error) = self.certificate_error.lock().unwrap().clone() {
            return Err(error);
        }
        Ok(self.certificate.lock().unwrap().clone())
    }

    fn api_key_item(&self) -> Result<ApiKeyItem, String> {
        self.calls.lock().unwrap().push("key item");
        Ok(self.key_item)
    }

    fn has_stored_identity(&self) -> Result<bool, String> {
        self.calls.lock().unwrap().push("identity entry");
        Ok(self.stored_identity)
    }

    fn read_api_key(&self) -> Result<Option<String>, KeychainRefusal> {
        self.calls.lock().unwrap().push("read key");
        self.key.clone()
    }

    fn export_identity(
        &self,
        certificate_der: &[u8],
        passphrase: &str,
    ) -> Result<Vec<u8>, KeychainRefusal> {
        self.calls.lock().unwrap().push("export");
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
    assert_eq!(
        keychain.calls(),
        ["certificate", "key item", "identity entry"],
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
        key_item: ApiKeyItem::Other,
        ..FakeKeychain::swift_app()
    });
    let pending = pending(launch(&at_home(), preferences.clone(), &defaults, keychain));
    assert!(
        pending.gate_key,
        "the key is gated while pending all the same"
    );
    assert!(!pending.read_key && pending.other_key);
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
/// identity's writes while `refuse_identity` is set.
#[derive(Default)]
struct CountingSecrets {
    inner: InMemorySecretStore,
    reads: Mutex<Vec<String>>,
    refuse_identity: std::sync::atomic::AtomicBool,
}

#[async_trait]
impl SecretStore for CountingSecrets {
    async fn secret(&self, key: &SecretKey) -> BoundaryResult<Option<String>> {
        self.reads.lock().unwrap().push(key.as_str().to_owned());
        self.inner.secret(key).await
    }

    async fn set_secret(&self, key: &SecretKey, value: Option<&str>) -> BoundaryResult<()> {
        if key.as_str() == HandoverIdentity::SECRET_KEY
            && self.refuse_identity.load(Ordering::SeqCst)
        {
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
    graph: GraphImport,
    import: ImportStep,
    reloads: Arc<AtomicUsize>,
}

fn step(keychain: FakeKeychain, existing_identity: Option<&HandoverIdentity>) -> Step {
    let dir = tempfile::tempdir().unwrap();
    let preferences = preferences(&dir);
    let keychain = Arc::new(FakeKeychain {
        stored_identity: existing_identity.is_some(),
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
        dir,
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
            "certificate",
            "key item",
            "identity entry",
            "read key",
            "certificate",
            "export"
        ]
    );
    let stored = read(&*step.raw, &HandoverIdentity::secret_key()).unwrap();
    let imported = HandoverIdentity::from_pem(&stored).unwrap();
    assert_eq!(hex(&imported.fingerprint()), FIXTURE_FINGERPRINT);
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
        key_item: ApiKeyItem::Other,
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
        let exports = step
            .keychain
            .calls()
            .iter()
            .filter(|call| **call == "export")
            .count();
        assert_eq!(exports, 1, "Try again exported again");
        let stored = read(&*step.raw, &HandoverIdentity::secret_key()).unwrap();
        assert_eq!(
            hex(&HandoverIdentity::from_pem(&stored).unwrap().fingerprint()),
            FIXTURE_FINGERPRINT
        );
    }
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
    let launched = launch(
        &at_home(),
        preferences.clone(),
        &FakeDefaults::new(fixture("swift-domain.plist")),
        keychain,
    );
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
    let write = |value: Option<&str>| {
        RUNTIME
            .block_on(step.graph.secrets.set_secret(&key, value))
            .unwrap();
    };
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

/// A refused key read is remembered: the next launch, whose import still
/// waits for the identity, answers no key without asking the keychain
/// (so nothing prompts before the step), and so does every launch after
/// the import is over, until the user saves a key.
#[test]
fn a_refused_key_read_is_never_asked_again_until_a_key_is_saved() {
    let denied_key = || FakeKeychain {
        key: Err(KeychainRefusal {
            denied: true,
            detail: "userCanceledErr".to_owned(),
        }),
        ..FakeKeychain::swift_app()
    };
    let first = step(denied_key(), None);
    first.keychain.deny_export();
    let status = first.import.run();
    assert_eq!(status.stage, SwiftImportStage::Waiting);
    assert!(first.preferences.flag(KEY_DENIED_KEY));

    // The next launch: still pending, the key stays unread.
    let keychain = Arc::new(denied_key());
    let again = pending(launch(
        &at_home(),
        first.preferences.clone(),
        &FakeDefaults::new(fixture("swift-domain.plist")),
        keychain.clone(),
    ));
    assert!(!again.gate_key && again.key_denied);
    let graph = GraphImport::new(again, first.raw.clone());
    first.raw.reads.lock().unwrap().clear();
    let key = SecretKey::llm_api_key();
    assert_eq!(read(&*graph.secrets, &key), None);
    let step = graph.step(RUNTIME.handle().clone(), Box::new(|| {}));
    assert_eq!(step.status().prompts, 1, "the identity alone");
    assert_eq!(step.run().stage, SwiftImportStage::Done);
    assert_eq!(read(&*graph.secrets, &key), None);
    assert!(
        first
            .raw
            .reads
            .lock()
            .unwrap()
            .iter()
            .all(|read| read != SecretKey::LLM_API_KEY),
        "the keychain was asked for the key"
    );
    assert!(!keychain.calls().contains(&"read key"));

    // The import is over: the graph's store still answers no key...
    let over = key_denied_secrets(&first.preferences, first.raw.clone());
    assert_eq!(read(&*over, &key), None);
    assert!(
        first
            .raw
            .reads
            .lock()
            .unwrap()
            .iter()
            .all(|read| read != SecretKey::LLM_API_KEY),
        "the keychain was asked for the key"
    );
    // ...until the user saves one, which clears the flag for good.
    RUNTIME
        .block_on(over.set_secret(&key, Some("sk-new")))
        .unwrap();
    assert_eq!(read(&*over, &key).as_deref(), Some("sk-new"));
    assert!(!first.preferences.flag(KEY_DENIED_KEY));
    assert_flags_only(first.dir.path());
    let later = key_denied_secrets(&first.preferences, first.raw.clone());
    assert_eq!(read(&*later, &key).as_deref(), Some("sk-new"));
}

/// The graph of a launch after the import, with the key read refused,
/// answers no key from its store though the store holds one.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_graph_after_a_refused_key_read_reads_no_key() {
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
    assert_eq!(app.secrets.secret(&key).await.unwrap(), None);
    assert_eq!(
        file.secret(&key).await.unwrap().as_deref(),
        Some("sk-swift")
    );
    app.shutdown();
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
        ["certificate", "key item", "identity entry"],
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
    assert!(follow.as_mut().poll(&mut context) == Poll::Pending);
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
        app.store
            .save_paired_device(
                &steno_core::PairedDevice {
                    id: uuid::Uuid::new_v4(),
                    name: "Phone".to_owned(),
                    paired_at: chrono::Utc::now(),
                    last_seen_at: None,
                },
                &[1; 32],
            )
            .unwrap();
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
        let follow = tokio::spawn(gated.clone().follow(|| panic!("no listener opens")));
        failure.wait_for(Option::is_some).await.unwrap();
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
    }
}

/// The graph under `paths` over the launch half against the Swift domain
/// fixture and `keychain`, which must leave the import pending.
fn build_over(paths: steno_core::StenoPaths, keychain: Arc<FakeKeychain>) -> crate::App {
    crate::build_with_import(test_options(paths), |preferences| {
        Some(pending(launch(
            &at_home(),
            preferences,
            &FakeDefaults::new(fixture("swift-domain.plist")),
            keychain,
        )))
    })
    .unwrap()
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
