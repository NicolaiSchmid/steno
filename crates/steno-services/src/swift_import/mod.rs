//! The import of what the Swift app left behind, on the Mac's first launch
//! after the update: [`launch`] copies three preferences and leaves the
//! import pending while the keychain holds the Swift handover certificate;
//! [`ImportStep`], the onboarding step, reads the Swift API key and brings
//! the handover identity over. Skipped in smoke runs and under a scratch `HOME`.

mod identity;
mod sources;

use std::io::Cursor;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, TryLockError};

use steno_core::protocols::BoundaryResult;
use steno_core::{SecretKey, SecretPlace, SecretStore, Settings, Store, async_trait};
use steno_handover::{FingerprintRecord, HandoverIdentity};
use steno_host::onboarding::OnboardingViewModel;
use steno_host::services::{Preferences as _, SwiftImport, SwiftImportStage, SwiftImportStatus};
use tokio::sync::watch;

pub use identity::{ImportedIdentityError, decode_pkcs12, store_imported_identity};
pub use sources::{ApiKeyItem, ImportMarker, KeychainRefusal, SwiftDefaults, SwiftKeychain};
#[cfg(target_os = "macos")]
pub use sources::{DefaultsCommand, LoginKeychain};

use crate::block_on;
use crate::platform::FilePreferences;
use crate::updates::{AUTOMATIC_CHECKS_KEY, AUTOMATIC_DOWNLOAD_KEY};

/// The Swift app's bundle identifier, its `UserDefaults` domain.
pub const SWIFT_DEFAULTS_DOMAIN: &str = "uno.schmid.steno.mac";
/// The label of the Swift handover certificate and key.
pub const SWIFT_IDENTITY_LABEL: &str = "Steno handover identity";
/// The label the Swift app gives the API key item; the `keyring` crate
/// sets none.
pub const SWIFT_API_KEY_LABEL: &str = "Steno llm-api-key";
/// Set once the import is over: the identity came over, or there was none.
pub const IMPORT_RAN_KEY: &str = "steno.swiftImportRan";
/// The keychain entry that records the import outside `preferences.json`,
/// written right after the identity is stored; from then on the import never
/// touches `handover-identity` again. Pair again must keep it, and write it
/// before it stores a new identity, or a later launch would bring the Swift
/// identity back over the new one.
pub const IMPORT_DONE_ENTRY: &str = "swift-import-done";
/// Set once the step read the Swift API key, or was refused.
pub const KEY_READ_KEY: &str = "steno.swiftImportKeyRead";
/// Set when the step's read of the Swift API key was refused, cleared once
/// the user saves a key: until then every launch answers no key without
/// asking the keychain ([`key_denied_secrets`]).
pub const KEY_DENIED_KEY: &str = "steno.swiftImportKeyDenied";

/// The Swift domain's keys the launch half copies, and where to.
const COPIED: [(&str, &str); 3] = [
    (
        OnboardingViewModel::COMPLETED_KEY,
        OnboardingViewModel::COMPLETED_KEY,
    ),
    ("SUEnableAutomaticChecks", AUTOMATIC_CHECKS_KEY),
    ("SUAutomaticallyUpdate", AUTOMATIC_DOWNLOAD_KEY),
];

/// Why the import does not run at this launch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    /// A smoke run (`STENO_SMOKE_SECONDS`).
    Smoke,
    /// `HOME` is not the account's home directory, or one is unknown.
    ForeignHome,
}

/// What decides whether the import may run at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchContext {
    pub smoke: bool,
    /// `HOME`.
    pub home: Option<PathBuf>,
    /// The account's home directory from the user database (`getpwuid`).
    pub account_home: Option<PathBuf>,
}

impl LaunchContext {
    /// This process: a smoke run when `smoke_variable` is set, `HOME`, and
    /// the account's home.
    #[must_use]
    pub fn current(smoke_variable: &str) -> Self {
        LaunchContext {
            smoke: std::env::var_os(smoke_variable).is_some(),
            home: std::env::var_os("HOME").map(PathBuf::from),
            account_home: account_home(),
        }
    }

    #[must_use]
    pub fn skip_reason(&self) -> Option<SkipReason> {
        if self.smoke {
            return Some(SkipReason::Smoke);
        }
        match (&self.home, &self.account_home) {
            (Some(home), Some(account)) if home == account => None,
            _ => Some(SkipReason::ForeignHome),
        }
    }
}

/// The current account's home directory.
#[cfg(target_os = "macos")]
fn account_home() -> Option<PathBuf> {
    nix::unistd::User::from_uid(nix::unistd::getuid())
        .ok()
        .flatten()
        .map(|user| user.dir)
}

#[cfg(not(target_os = "macos"))]
fn account_home() -> Option<PathBuf> {
    None
}

/// What the launch half found.
pub enum Launch {
    Skipped(SkipReason),
    /// Nothing (more) to import.
    Done,
    /// The Swift identity waits for the onboarding step.
    Pending(PendingImport),
}

impl std::fmt::Debug for Launch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Launch::Skipped(reason) => f.debug_tuple("Skipped").field(reason).finish(),
            Launch::Done => f.write_str("Done"),
            Launch::Pending(pending) => f
                .debug_struct("Pending")
                .field("key", &pending.key)
                .finish_non_exhaustive(),
        }
    }
}

/// Where the API key stands at a launch whose import is pending.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchKey {
    /// No earlier step read or was refused the key: the graph reads none
    /// until the step ran.
    Unread(ApiKeyItem),
    /// An earlier step read it: the store answers.
    Read,
    /// An earlier step was refused it: no key until the user saves one.
    Denied,
}

/// The import the graph is built over while it waits for the step: no API
/// key is read and no listener bound under it.
pub struct PendingImport {
    /// The graph's `preferences.json`, shared with the step.
    pub preferences: Arc<FilePreferences>,
    pub keychain: Arc<dyn SwiftKeychain>,
    /// Where the API key stands.
    pub key: LaunchKey,
    /// The keychain already holds a `handover-identity` entry, or the query
    /// failed: replacing it may prompt once more.
    pub replaces_identity: bool,
    /// The [`IMPORT_DONE_ENTRY`] query failed: the run asks again before it
    /// stores, and replaces nothing while it fails.
    pub marker_unknown: bool,
}

/// The launch half. While `preferences.json` holds no onboarding flag it
/// copies `COPIED` from the Swift domain. Then, when the keychain holds a
/// Swift handover certificate, or the query failed, the import is pending.
/// A key alone does not make it pending, since a desktop-id build files its
/// key under the same service and account. A query that fails counts as an
/// item that may prompt. Once the import ran, or the [`IMPORT_DONE_ENTRY`]
/// marker is found, it does nothing; only a clean marker not-found counts as
/// a first run.
pub fn launch(
    context: &LaunchContext,
    preferences: Arc<FilePreferences>,
    defaults: &dyn SwiftDefaults,
    keychain: Arc<dyn SwiftKeychain>,
) -> Launch {
    if let Some(reason) = context.skip_reason() {
        return Launch::Skipped(reason);
    }
    if preferences.flag(IMPORT_RAN_KEY) {
        return Launch::Done;
    }
    if !preferences.contains(OnboardingViewModel::COMPLETED_KEY)
        && let Err(error) = copy_defaults(defaults, &preferences)
    {
        tracing::warn!(%error, "the Swift app's preferences could not be read");
    }
    let marker = keychain.import_done();
    if marker == Ok(true) {
        preferences.set_flag(IMPORT_RAN_KEY, true);
        return Launch::Done;
    }
    match keychain.swift_certificates() {
        Ok(certificates) if certificates.is_empty() => {
            preferences.set_flag(IMPORT_RAN_KEY, true);
            Launch::Done
        }
        found => {
            // A failed query may have hidden the Swift identity: wait rather
            // than let the listener mint over it.
            if let Err(error) = found {
                tracing::warn!(%error, "the Swift handover certificate could not be looked up");
            }
            // An item a query could not see may be there and prompt.
            let key = if preferences.flag(KEY_DENIED_KEY) {
                LaunchKey::Denied
            } else if preferences.flag(KEY_READ_KEY) {
                LaunchKey::Read
            } else {
                LaunchKey::Unread(keychain.api_key_item().unwrap_or_else(|error| {
                    tracing::warn!(%error, "the API key item could not be looked up");
                    ApiKeyItem::Other
                }))
            };
            let replaces_identity = keychain.has_stored_identity().unwrap_or_else(|error| {
                tracing::warn!(%error, "the stored handover identity could not be looked up");
                true
            });
            if let Err(error) = &marker {
                tracing::warn!(%error, "the import's marker could not be looked up");
            }
            Launch::Pending(PendingImport {
                preferences,
                keychain,
                key,
                replaces_identity,
                marker_unknown: marker.is_err(),
            })
        }
    }
}

/// The launch half on this Mac: the import the graph is to be built over,
/// or `None` when nothing is pending (and always off the Mac).
#[must_use]
pub fn launch_on_this_mac(
    preferences: Arc<FilePreferences>,
    smoke_variable: &str,
) -> Option<PendingImport> {
    #[cfg(target_os = "macos")]
    {
        match launch(
            &LaunchContext::current(smoke_variable),
            preferences,
            &DefaultsCommand,
            Arc::new(LoginKeychain::default()),
        ) {
            Launch::Pending(pending) => Some(pending),
            Launch::Skipped(reason) => {
                tracing::info!(?reason, "the import from the Swift app is skipped");
                None
            }
            Launch::Done => None,
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (preferences, smoke_variable);
        None
    }
}

/// Where Pair again writes the import's marker on this Mac: the login
/// keychain, unless the import is skipped here; `None` off the Mac.
#[must_use]
pub fn marker_on_this_mac(smoke_variable: &str) -> Option<Arc<dyn ImportMarker>> {
    #[cfg(target_os = "macos")]
    {
        LaunchContext::current(smoke_variable)
            .skip_reason()
            .is_none()
            .then(|| Arc::new(LoginKeychain::default()) as Arc<dyn ImportMarker>)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = smoke_variable;
        None
    }
}

/// Copies [`COPIED`] from the Swift domain where `preferences.json` has no
/// value of its own yet.
fn copy_defaults(
    defaults: &dyn SwiftDefaults,
    preferences: &FilePreferences,
) -> Result<(), String> {
    let exported = defaults.export()?;
    let domain = plist::Value::from_reader(Cursor::new(exported))
        .map_err(|error| format!("the Swift domain is not a property list: {error}"))?;
    let domain = domain
        .as_dictionary()
        .ok_or("the Swift domain is not a dictionary")?;
    for (swift, ours) in COPIED {
        let value = domain.get(swift).and_then(|value| {
            value
                .as_boolean()
                .or_else(|| value.as_signed_integer().map(|number| number != 0))
        });
        if let Some(value) = value
            && !preferences.contains(ours)
        {
            preferences.set_flag(ours, value);
        }
    }
    Ok(())
}

/// Whether the phone handover may run: the signal the listener follows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandoverGate {
    /// The onboarding step has not run yet.
    Pending,
    /// The identity is in place (or there was none to import).
    Ready,
    /// The listener stays off until the step runs again.
    Waiting(WaitReason),
}

impl HandoverGate {
    /// Whether the listener may be built: only once the identity is in place.
    #[must_use]
    pub fn opens_listener(self) -> bool {
        self == HandoverGate::Ready
    }
}

/// Why the handover waits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitReason {
    /// The export was denied, failed or skipped, or a certificate's start
    /// or the paired phones could not be read.
    ImportDenied,
    /// No paired phone knows any Swift certificate.
    Replaced,
    /// Some paired phones, but not all, know one Swift certificate.
    PartlyReplaced,
    /// A pairing or a certificate's start is after now on this Mac's clock.
    ClockBehind,
}

/// The iPhone section's reason for the gate's.
impl From<WaitReason> for steno_bridge::PhoneWaitingReason {
    fn from(reason: WaitReason) -> Self {
        match reason {
            WaitReason::ImportDenied => steno_bridge::PhoneWaitingReason::ImportDenied,
            WaitReason::Replaced => steno_bridge::PhoneWaitingReason::Replaced,
            WaitReason::PartlyReplaced => steno_bridge::PhoneWaitingReason::PartlyReplaced,
            WaitReason::ClockBehind => steno_bridge::PhoneWaitingReason::ClockBehind,
        }
    }
}

/// What the step chooses the Swift certificate by ([`choose_certificate`]).
pub struct CertificateChoice {
    /// The database, for the paired phones' pairing times.
    pub store: Arc<Store>,
    /// This Mac's clock, which the pairing-time rule judges by.
    pub now: fn() -> chrono::DateTime<chrono::Utc>,
}

/// Of the Swift certificates the keychain holds (DER, keychain order), the
/// one to import, or `None` when there is none, by the handover identity's
/// decision over every certificate and the paired phones; never by the
/// keychain's order. A start or phones that cannot be read wait as
/// [`FAILED_EXPORT`].
pub fn choose_certificate(
    certificates: Vec<Vec<u8>>,
    choice: &CertificateChoice,
) -> Result<Option<Vec<u8>>, NotImported> {
    use steno_handover::identity::decision::{
        self, Caller, Found, Identities, Observations, Outcome, Phones, Reason, Record,
    };
    if certificates.is_empty() {
        return Ok(None);
    }
    let fingerprint = |certificate: &[u8]| {
        steno_handover::identity::hex(&HandoverIdentity::fingerprint_of_der(certificate))
    };
    let found = certificates
        .iter()
        .map(|certificate| Found {
            fingerprint: fingerprint(certificate),
            start: HandoverIdentity::not_valid_before_of_der(certificate)
                .inspect_err(|error| {
                    tracing::warn!(%error, "a Swift handover certificate's start could not be read");
                })
                .ok(),
        })
        .collect();
    let phones = match choice.store.paired_devices() {
        Ok(devices) => Phones::Paired(devices.iter().map(|device| device.paired_at).collect()),
        Err(error) => {
            tracing::warn!(%error, "the paired phones could not be read");
            Phones::Unreadable
        }
    };
    let decided = decision::decide(&Observations {
        caller: Caller::Import,
        identities: Identities::Found(found),
        record: Record::EMPTY,
        phones,
        now: (choice.now)(),
    });
    let (line, reason) = match decided.outcome {
        // The decision serves one of the certificates it was given;
        // should that not hold, wait rather than import nothing.
        Outcome::Serve(chosen) => {
            return certificates
                .into_iter()
                .find(|certificate| fingerprint(certificate) == chosen)
                .map(Some)
                .ok_or_else(NotImported::failed);
        }
        Outcome::Wait(Reason::Replaced) => (REPLACED_PAIRING, WaitReason::Replaced),
        Outcome::Wait(Reason::PartlyReplaced) => {
            (PARTLY_REPLACED_PAIRING, WaitReason::PartlyReplaced)
        }
        Outcome::Wait(Reason::ClockBehind) => (CLOCK_BEHIND_PAIRING, WaitReason::ClockBehind),
        Outcome::Wait(_) | Outcome::Mint => return Err(NotImported::failed()),
    };
    Err(NotImported { line, reason })
}

/// Why a run brought no identity over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NotImported {
    /// What the step says ([`SwiftImportStatus::error`]).
    pub line: &'static str,
    /// What the handover waits with ([`HandoverGate::Waiting`]).
    pub reason: WaitReason,
}

impl NotImported {
    /// A lookup or an export that failed another way than a refusal.
    fn failed() -> Self {
        Self::export(false)
    }

    /// An export or store that macOS refused (`denied`) or that failed.
    fn export(denied: bool) -> Self {
        NotImported {
            line: if denied { DENIED_EXPORT } else { FAILED_EXPORT },
            reason: WaitReason::ImportDenied,
        }
    }
}

/// What the step lets through to the API key reads. Its `Debug` leaves
/// the key out.
#[derive(Clone, PartialEq, Eq)]
enum KeyGate {
    /// No key item until the step ran: every read answers no key.
    Closed,
    /// A key this launch does not read: every read answers no key, and the
    /// pipeline runs no LLM pass for an endpoint that needs it.
    Withheld,
    /// The value the step read, answered without asking the keychain again.
    Read(Option<String>),
    /// The secret store answers.
    Open,
}

impl std::fmt::Debug for KeyGate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            KeyGate::Closed => "Closed",
            KeyGate::Withheld => "Withheld",
            KeyGate::Read(None) => "Read(None)",
            KeyGate::Read(Some(_)) => "Read(Some(<redacted>))",
            KeyGate::Open => "Open",
        })
    }
}

/// The shared state of a pending import: the handover gate and the key
/// gate. Also the gate of a refused key read without a pending import
/// ([`key_denied_secrets`]), where the handover half stays `Pending` for
/// good, so nothing may follow [`Self::handover`] on that one.
#[derive(Debug)]
pub struct ImportGate {
    handover: watch::Sender<HandoverGate>,
    key: Mutex<KeyGate>,
}

impl ImportGate {
    fn new(key: KeyGate) -> Self {
        ImportGate {
            handover: watch::Sender::new(HandoverGate::Pending),
            key: Mutex::new(key),
        }
    }

    /// Where the handover stands now.
    #[must_use]
    pub fn handover(&self) -> HandoverGate {
        *self.handover.borrow()
    }

    /// Follows the handover gate.
    #[must_use]
    pub fn subscribe(&self) -> watch::Receiver<HandoverGate> {
        self.handover.subscribe()
    }

    /// Returns once the step ran or was skipped and the pipeline reloaded
    /// with what it read.
    pub async fn step_over(&self) {
        let mut gate = self.subscribe();
        // The sender lives in `self`, so the wait cannot fail.
        let _ = gate.wait_for(|gate| *gate != HandoverGate::Pending).await;
    }

    /// Whether the API key is withheld now, whatever the endpoint.
    #[must_use]
    pub fn key_withheld(&self) -> bool {
        *self.key() == KeyGate::Withheld
    }

    /// Whether the graph runs no summaries now: the key is withheld and the
    /// endpoint of `settings` needs it, so meetings complete without one.
    /// `ChatGPT` summaries need no key and keep running.
    #[must_use]
    pub fn withholds_summaries(&self, settings: &Settings) -> bool {
        self.key_withheld()
            && steno_llm::LlmEndpoint::from_settings(settings)
                .is_some_and(|endpoint| !endpoint.is_codex_backend())
    }

    fn set_handover(&self, gate: HandoverGate) {
        self.handover.send_replace(gate);
    }

    fn key(&self) -> MutexGuard<'_, KeyGate> {
        self.key.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl steno_host::services::WithheldApiKey for ImportGate {
    fn withheld(&self, settings: &Settings) -> bool {
        self.withholds_summaries(settings)
    }
}

/// The secret store the graph reads through while an import is pending:
/// the API key as the [`KeyGate`] allows, everything else from the store.
struct GatedSecrets {
    inner: Arc<dyn SecretStore>,
    gate: Arc<ImportGate>,
    /// Where a saved key clears [`KEY_DENIED_KEY`].
    preferences: Arc<FilePreferences>,
}

impl GatedSecrets {
    /// Whether a cleared API key goes to the store: only through an open
    /// gate; otherwise it is dropped for this launch.
    fn clear_gated_key(&self) -> bool {
        let mut gate = self.gate.key();
        match *gate {
            KeyGate::Open => true,
            KeyGate::Closed => false,
            KeyGate::Withheld | KeyGate::Read(_) => {
                *gate = KeyGate::Read(None);
                false
            }
        }
    }
}

#[async_trait]
impl SecretStore for GatedSecrets {
    async fn secret(&self, key: &SecretKey) -> BoundaryResult<Option<String>> {
        if key.as_str() == SecretKey::LLM_API_KEY {
            let gate = self.gate.key().clone();
            match gate {
                KeyGate::Closed | KeyGate::Withheld => return Ok(None),
                KeyGate::Read(value) => return Ok(value),
                KeyGate::Open => {}
            }
        }
        self.inner.secret(key).await
    }

    /// A saved key goes to the store, which answers from then on. While the
    /// gate is not open, a `None` or empty key never reaches the store: it
    /// would delete the item the Swift app shares, which a rollback still
    /// reads.
    async fn set_secret(&self, key: &SecretKey, value: Option<&str>) -> BoundaryResult<()> {
        if key.as_str() != SecretKey::LLM_API_KEY {
            return self.inner.set_secret(key, value).await;
        }
        let saving = value.is_some_and(|value| !value.is_empty());
        if !saving && !self.clear_gated_key() {
            return Ok(());
        }
        self.inner.set_secret(key, value).await?;
        if saving {
            // Under the gate's lock, so a refused read cannot cross it.
            let mut gate = self.gate.key();
            *gate = KeyGate::Open;
            if self.preferences.flag(KEY_DENIED_KEY) {
                self.preferences.set_flag(KEY_DENIED_KEY, false);
            }
        }
        Ok(())
    }
    fn place(&self) -> Option<SecretPlace> {
        self.inner.place()
    }
}

/// The secret store of a graph without a pending import: `secrets` itself,
/// or, while the key read stands refused ([`KEY_DENIED_KEY`]), `secrets`
/// behind a gate that withholds the key until the user saves one.
#[must_use]
pub fn key_denied_secrets(
    preferences: &Arc<FilePreferences>,
    secrets: Arc<dyn SecretStore>,
) -> (Arc<dyn SecretStore>, Option<Arc<ImportGate>>) {
    if !preferences.flag(KEY_DENIED_KEY) {
        return (secrets, None);
    }
    let gate = Arc::new(ImportGate::new(KeyGate::Withheld));
    let gated = Arc::new(GatedSecrets {
        inner: secrets,
        gate: gate.clone(),
        preferences: preferences.clone(),
    });
    (gated, Some(gate))
}

/// A pending import inside the graph: the gated secret store, the gate,
/// and the step once the graph built it.
pub struct GraphImport {
    preferences: Arc<FilePreferences>,
    pub gate: Arc<ImportGate>,
    /// The store every read in the graph goes through.
    pub secrets: Arc<dyn SecretStore>,
    raw_secrets: Arc<dyn SecretStore>,
    /// Where the imported identity's fingerprint is recorded.
    record: Arc<dyn FingerprintRecord>,
    keychain: Arc<dyn SwiftKeychain>,
    /// The step's items, from the launch half.
    items: StepItems,
    /// The launch half's marker query failed.
    marker_unknown: bool,
}

impl GraphImport {
    /// Wraps `secrets` for the graph.
    #[must_use]
    pub fn new(
        pending: PendingImport,
        secrets: Arc<dyn SecretStore>,
        record: Arc<dyn FingerprintRecord>,
    ) -> Self {
        let key = match pending.key {
            LaunchKey::Unread(ApiKeyItem::Missing) => KeyGate::Closed,
            LaunchKey::Unread(_) | LaunchKey::Denied => KeyGate::Withheld,
            LaunchKey::Read => KeyGate::Open,
        };
        let gate = Arc::new(ImportGate::new(key));
        GraphImport {
            secrets: Arc::new(GatedSecrets {
                inner: secrets.clone(),
                gate: gate.clone(),
                preferences: pending.preferences.clone(),
            }),
            preferences: pending.preferences,
            gate,
            raw_secrets: secrets,
            record,
            keychain: pending.keychain,
            items: StepItems {
                read_key: pending.key == LaunchKey::Unread(ApiKeyItem::Swift),
                other_key: pending.key == LaunchKey::Unread(ApiKeyItem::Other),
                replaces_identity: pending.replaces_identity,
            },
            marker_unknown: pending.marker_unknown,
        }
    }

    /// The step, which chooses the certificate by `choice`, with `reload`
    /// run whenever the key gate opened.
    #[must_use]
    pub fn step(
        &self,
        runtime: tokio::runtime::Handle,
        choice: CertificateChoice,
        reload: Box<dyn Fn() + Send + Sync>,
    ) -> ImportStep {
        ImportStep {
            keychain: self.keychain.clone(),
            choice,
            preferences: self.preferences.clone(),
            secrets: self.raw_secrets.clone(),
            record: self.record.clone(),
            gate: self.gate.clone(),
            reload,
            runtime,
            state: Mutex::new(StepState {
                stage: SwiftImportStage::Pending,
                items: self.items,
                marker_unknown: self.marker_unknown,
                bundle: None,
                error: None,
            }),
            running: Mutex::new(()),
        }
    }
}

/// The keychain items a run still reads, each behind a prompt.
#[derive(Debug, Clone, Copy)]
struct StepItems {
    /// The Swift API key, until the step read it or was refused.
    read_key: bool,
    /// A key that is not the Swift app's, until the step let the graph read it.
    other_key: bool,
    /// The stored `handover-identity` entry the identity replaces.
    replaces_identity: bool,
}

struct StepState {
    stage: SwiftImportStage,
    items: StepItems,
    /// The [`IMPORT_DONE_ENTRY`] query has not answered yet.
    marker_unknown: bool,
    /// The identity a failed store left, so Try again skips the export.
    bundle: Option<String>,
    error: Option<&'static str>,
}

/// The onboarding step's half of the import (`steno_host::services::SwiftImport`).
///
/// A run reads the Swift API key (one prompt; the item stays, as the Swift
/// app still reads it after a rollback) unless the user saved one first,
/// then exports the chosen handover identity, stores it as
/// `handover-identity` and records its fingerprint: the Swift identity is
/// the one the paired phones pin. A denied export or store leaves the
/// handover waiting. Not now brings up no prompt.
///
/// One run at a time: a run or skip during a run answers the status at once,
/// so the main thread never waits on a keychain prompt.
pub struct ImportStep {
    keychain: Arc<dyn SwiftKeychain>,
    /// What the run chooses the certificate by.
    choice: CertificateChoice,
    preferences: Arc<FilePreferences>,
    /// The store behind the gate: the identity is written here.
    secrets: Arc<dyn SecretStore>,
    /// Where the identity's fingerprint is recorded.
    record: Arc<dyn FingerprintRecord>,
    gate: Arc<ImportGate>,
    reload: Box<dyn Fn() + Send + Sync>,
    runtime: tokio::runtime::Handle,
    state: Mutex<StepState>,
    /// Held by the run under way.
    running: Mutex<()>,
}

/// The step's line when macOS refused the export.
pub const DENIED_EXPORT: &str =
    "macOS did not let Steno read this Mac's phone pairing. Choose Try again, then Always Allow.";
/// The step's line when the export failed another way.
pub const FAILED_EXPORT: &str = "Steno could not bring over this Mac's phone pairing.";
/// The step's line when no paired phone knows a Swift certificate
/// ([`WaitReason::Replaced`]).
pub const REPLACED_PAIRING: &str = "Steno found a different phone pairing on this Mac than the \
     one your phones know. Choose Try again if you put the old one back, or Pair again in \
     Settings > iPhone to start over.";
/// The step's line when the paired phones do not all know one Swift
/// certificate ([`WaitReason::PartlyReplaced`]).
pub const PARTLY_REPLACED_PAIRING: &str = "Some of your phones were paired before this Mac's \
     pairing changed and can't connect. To reconnect them, choose Pair again in Settings > \
     iPhone.";
/// The step's line when a pairing or a certificate's start is after now
/// ([`WaitReason::ClockBehind`]).
pub const CLOCK_BEHIND_PAIRING: &str = "This Mac's date and time are earlier than when its \
     phone pairing was made, so Steno can't tell which pairing your phones know. Check the date \
     and time in System Settings, then choose Try again.";

impl ImportStep {
    fn state(&self) -> MutexGuard<'_, StepState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// What the step read, or that it withholds the key, is what the graph
    /// reads from now on; a key the user saved meanwhile stays. The pipeline
    /// reloads on a change.
    fn key_read(&self, read: KeyGate) {
        let mut key = self.gate.key();
        if *key != KeyGate::Open && *key != read {
            *key = read;
            drop(key);
            (self.reload)();
        }
    }

    /// The user saved a key in Settings since the launch.
    fn key_saved(&self) -> bool {
        *self.gate.key() == KeyGate::Open
    }

    /// Held for a run, a skip or Pair again; why not when another one is
    /// under way or the step is over.
    fn one_at_a_time(&self) -> Result<MutexGuard<'_, ()>, NotNow> {
        let running = match self.running.try_lock() {
            Ok(guard) => guard,
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(TryLockError::WouldBlock) => return Err(NotNow::UnderWay),
        };
        if self.state().stage == SwiftImportStage::Done {
            return Err(NotNow::Over);
        }
        Ok(running)
    }

    /// The step had no Swift key to read: the store answers from now on,
    /// also over another app's key item when `other_key`.
    fn open_key(&self, other_key: bool) {
        let mut key = self.gate.key();
        if *key == KeyGate::Closed || (other_key && *key == KeyGate::Withheld) {
            *key = KeyGate::Open;
            drop(key);
            (self.reload)();
        }
    }

    /// Exports and decodes the chosen Swift identity; `None` when the
    /// certificate is gone by now.
    fn export(&self) -> Result<Option<String>, NotImported> {
        let certificates = self.keychain.swift_certificates().map_err(|error| {
            tracing::warn!(%error, "the Swift handover certificate could not be looked up");
            NotImported::failed()
        })?;
        let Some(certificate) = choose_certificate(certificates, &self.choice)? else {
            return Ok(None);
        };
        let passphrase = uuid::Uuid::new_v4().to_string();
        let pkcs12 = self
            .keychain
            .export_identity(&certificate, &passphrase)
            .map_err(|refusal| {
                tracing::warn!(%refusal, "the Swift handover identity was not exported");
                NotImported::export(refusal.denied)
            })?;
        let (_, bundle) = decode_pkcs12(&pkcs12, &passphrase, &certificate).map_err(|error| {
            tracing::warn!(%error, "the exported Swift handover identity was refused");
            NotImported::failed()
        })?;
        Ok(Some(bundle))
    }

    /// Stores the identity a failed store kept, or else a fresh export, with
    /// its fingerprint, then writes the [`IMPORT_DONE_ENTRY`] marker. A failed
    /// store keeps the bundle for Try again and writes no marker.
    fn import_identity(&self) -> Result<(), NotImported> {
        if self.state().marker_unknown {
            // Only a clean not-found is a first run: an unreadable marker may
            // stand for an identity stored since, which a replace would lose.
            match self.keychain.import_done() {
                Ok(true) => return Ok(()),
                Ok(false) => self.state().marker_unknown = false,
                Err(error) => {
                    tracing::warn!(%error, "the import's marker could not be looked up");
                    return Err(NotImported::failed());
                }
            }
        }
        let kept = self.state().bundle.take();
        let Some(bundle) = kept.map_or_else(|| self.export(), |bundle| Ok(Some(bundle)))? else {
            return Ok(());
        };
        let stored = block_on(
            &self.runtime,
            store_imported_identity(&*self.secrets, &*self.record, &bundle),
        );
        stored.map_err(|error| {
            tracing::warn!(%error, "the Swift handover identity was not stored");
            let mut state = self.state();
            state.bundle = Some(bundle);
            // A fingerprint not recorded is no keychain refusal.
            match error {
                ImportedIdentityError::Record(_) => NotImported::failed(),
                _ => NotImported::export(state.items.replaces_identity),
            }
        })?;
        // After the identity, so a crash in between reruns the same store.
        // A failed write is logged; `IMPORT_RAN_KEY` still ends the import.
        if let Err(error) = self.keychain.mark_import_done() {
            tracing::warn!(%error, "the import's marker was not written");
        }
        Ok(())
    }
}

impl SwiftImport for ImportStep {
    fn status(&self) -> SwiftImportStatus {
        let state = self.state();
        let items = state.items;
        SwiftImportStatus {
            stage: state.stage,
            prompts: u8::from(items.read_key)
                + u8::from(items.other_key)
                + u8::from(state.bundle.is_none())
                + u8::from(items.replaces_identity),
            error: state.error.map(str::to_owned),
        }
    }

    fn run(&self) -> SwiftImportStatus {
        // A second Continue or Try again brings up no second prompt.
        let Ok(_running) = self.one_at_a_time() else {
            return self.status();
        };
        self.run_held()
    }

    fn skip(&self) -> SwiftImportStatus {
        // A run under way sets the stage itself; the caller may be the
        // main thread, which must not wait on its prompts.
        let Ok(_running) = self.one_at_a_time() else {
            return self.status();
        };
        // Not now brings up no prompt.
        let items = self.state().items;
        if items.read_key || items.other_key {
            self.key_read(KeyGate::Withheld);
        } else {
            self.open_key(false);
        }
        self.state().stage = SwiftImportStage::Waiting;
        self.gate
            .set_handover(HandoverGate::Waiting(WaitReason::ImportDenied));
        self.status()
    }
}

impl ImportStep {
    /// The run, its caller holding the lock ([`Self::one_at_a_time`]).
    fn run_held(&self) -> SwiftImportStatus {
        if self.state().items.read_key {
            // A key saved in Settings since the launch wins.
            let key = if self.key_saved() {
                KeyGate::Open
            } else {
                match self.keychain.read_api_key() {
                    Ok(key) => KeyGate::Read(key),
                    Err(refusal) => {
                        tracing::warn!(%refusal, "the Swift API key was not read");
                        // A key saved while the prompt was up wins.
                        let gate = self.gate.key();
                        if *gate != KeyGate::Open {
                            self.preferences.set_flag(KEY_DENIED_KEY, true);
                        }
                        KeyGate::Withheld
                    }
                }
            };
            self.preferences.set_flag(KEY_READ_KEY, true);
            self.state().items.read_key = false;
            self.key_read(key);
        } else {
            let other_key = std::mem::take(&mut self.state().items.other_key);
            self.open_key(other_key);
        }
        let failure = self.import_identity().err();
        {
            let mut state = self.state();
            state.stage = if failure.is_none() {
                SwiftImportStage::Done
            } else {
                SwiftImportStage::Waiting
            };
            state.error = failure.map(|failure| failure.line);
        }
        match failure {
            None => {
                self.preferences.set_flag(IMPORT_RAN_KEY, true);
                self.gate.set_handover(HandoverGate::Ready);
            }
            Some(failure) => self
                .gate
                .set_handover(HandoverGate::Waiting(failure.reason)),
        }
        self.status()
    }
}

/// Settings' iPhone section drives the step while the handover waits.
impl crate::handover::WaitingImport for ImportStep {
    /// The run, as the step's Try again; `STEP_UNDER_WAY` while the
    /// onboarding window runs it.
    fn try_again(&self) -> Result<(), String> {
        let _running = match self.one_at_a_time() {
            Ok(running) => running,
            Err(NotNow::UnderWay) => return Err(STEP_UNDER_WAY.to_owned()),
            Err(NotNow::Over) => return Ok(()),
        };
        let status = self.run_held();
        if status.stage == SwiftImportStage::Done {
            return Ok(());
        }
        Err(status.error.unwrap_or_else(|| FAILED_EXPORT.to_owned()))
    }

    /// Ends a waiting import with a new identity, under the run's lock: the
    /// marker first (else a later launch brings the Swift identity back),
    /// then `mint`, then `IMPORT_RAN`, and the gate opens.
    fn pair_again(&self, mint: &dyn Fn() -> Result<(), String>) -> Result<(), String> {
        let _running = match self.one_at_a_time() {
            Ok(running) => running,
            Err(NotNow::UnderWay) => return Err(STEP_UNDER_WAY.to_owned()),
            Err(NotNow::Over) => return Err(crate::handover::NOT_WAITING.to_owned()),
        };
        self.keychain
            .mark_import_done()
            .map_err(|error| crate::handover::marker_not_written(&error))?;
        mint()?;
        {
            let mut state = self.state();
            state.stage = SwiftImportStage::Done;
            state.error = None;
            state.bundle = None;
        }
        self.preferences.set_flag(IMPORT_RAN_KEY, true);
        self.gate.set_handover(HandoverGate::Ready);
        Ok(())
    }
}

/// What Try again and Pair again answer while the step runs.
pub(crate) const STEP_UNDER_WAY: &str = "Steno is still bringing over this Mac's phone pairing.";

/// Why [`ImportStep::one_at_a_time`] did not take the lock.
enum NotNow {
    /// Another run, skip or Pair again holds it.
    UnderWay,
    /// The step is over.
    Over,
}

#[cfg(test)]
mod tests;
