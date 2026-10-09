//! The import of what the Swift app left behind, on the Mac's first launch
//! after the update to this app (plan `.plans/2026-10-07-stable-promotion.md`,
//! S6 and "First launch after the handoff"). The new app has its own
//! identifier, so it reads the Swift app's defaults domain by name and asks
//! macOS, once per keychain item, for what the Swift app stored there.
//!
//! A *desktop-id build* below is a build of this app under the beta's
//! identifier, `uno.schmid.steno.desktop` (D5 of
//! `.plans/2026-10-07-stable-promotion.md`), that ran on this Mac before
//! the update: it filed its keychain entries under the same service and
//! accounts as the Swift app, and macOS lists that build, not this one, on
//! their access lists.
//!
//! Two halves, both macOS only in the product (the logic is
//! platform-independent and tested everywhere over fakes):
//!
//! - **[`launch`]** runs inside [`crate::build_with_import`] once the
//!   database's lock is held, before the graph reads a preference or a
//!   secret. It copies three Swift preferences and, while the keychain holds
//!   the Swift handover certificate, leaves the import pending
//!   ([`PendingImport`]): the graph reads no API key and builds no handover
//!   listener until the step ends.
//! - **[`ImportStep`]**, the onboarding step's half, reads the Swift API key,
//!   exports the handover identity and stores it as the `handover-identity`
//!   entry, replacing a desktop-id build's, with its fingerprint recorded
//!   (`handover-identity.json`, [`crate::handover::FingerprintFile`]) so
//!   the handover's guard accepts it. A denied or failed export or store
//!   never mints an identity and never replaces one: the handover waits
//!   ([`HandoverGate`]) and the step offers Try again.
//!
//! | Part | Items |
//! |------|-------|
//! | Launch half | [`launch`], [`launch_on_this_mac`], [`LaunchContext`] with [`SkipReason`], [`Launch`], [`LaunchKey`], [`PendingImport`] |
//! | The graph | [`GraphImport`] (the gated secret store and the step), [`ImportGate`], [`HandoverGate`] with [`WaitReason`], [`key_denied_secrets`] |
//! | The step | [`ImportStep`], [`DENIED_EXPORT`], [`FAILED_EXPORT`] |
//! | Sources (`sources.rs`) | [`SwiftDefaults`], [`SwiftKeychain`], [`ApiKeyItem`], [`KeychainRefusal`]; on the Mac `DefaultsCommand` and `LoginKeychain` |
//! | Identity (`identity.rs`) | [`decode_pkcs12`], [`store_imported_identity`], [`ImportedIdentityError`] |
//! | Flags in `preferences.json` | [`IMPORT_RAN_KEY`], [`KEY_READ_KEY`], [`KEY_DENIED_KEY`], [`AUTOMATIC_CHECKS_KEY`], [`AUTOMATIC_DOWNLOAD_KEY`] |
//! | In the keychain | [`IMPORT_DONE_ENTRY`], the marker Pair again keeps |
//! | The Swift app's names | [`SWIFT_DEFAULTS_DOMAIN`], [`SWIFT_IDENTITY_LABEL`], [`SWIFT_API_KEY_LABEL`] |
//!
//! The whole state machine, one row per event. The key gate is what the
//! graph's API key reads answer ([`KeyGate`]): *withheld* answers no key
//! and the pipeline runs no LLM pass (the meeting completes without a
//! summary, which can be run again once a key is saved); *read* is the
//! value the step read; *open* is the secret store. The handover gate
//! ([`HandoverGate`]) is what the listener follows.
//!
//! | Event | Key gate | Handover gate | Written |
//! |-------|----------|---------------|---------|
//! | Launch, `IMPORT_RAN` set, or the [`IMPORT_DONE_ENTRY`] marker found | none, or withheld while `KEY_DENIED` | none (listener at launch) | `IMPORT_RAN` when the marker found it |
//! | Launch, no Swift certificate | as above | none | `IMPORT_RAN` |
//! | Launch, certificate found or its query failed, key unread | withheld (open when no key item) | Pending | nothing |
//! | Launch, as above, `KEY_READ` set | open | Pending | nothing |
//! | Launch, as above, `KEY_DENIED` set | withheld | Pending | nothing |
//! | Launch, the marker query failed | as above | Pending; the run replaces nothing until a query answers | nothing |
//! | Launch while Pending | as above | Pending | resume, re-export, sweep and recovery wait until the gate leaves Pending |
//! | Run: the key read | read | | `KEY_READ` |
//! | Run: the key refused | withheld | | `KEY_READ`, `KEY_DENIED` (unless a key was saved meanwhile) |
//! | Run: identity stored | | Ready | the identity and its fingerprint, then the marker, then `IMPORT_RAN` |
//! | Run: export or store denied or failed | | Waiting (Try again) | nothing |
//! | Run: identity stored, its fingerprint not recorded | | Waiting (Try again repeats the store) | the identity; no marker, no `IMPORT_RAN` |
//! | Not now, or the window closed | withheld (open when no key item) | Waiting | nothing: the step returns at the next launch |
//! | A key saved in Settings | open | | the key; `KEY_DENIED` cleared |
//! | Later launch with `KEY_DENIED` | withheld until a key is saved | | nothing |
//!
//! The pipeline reloads whenever the key gate changes, before the
//! handover gate is published, so a launch's held work runs on the
//! reloaded pipeline. Once the marker or `IMPORT_RAN` exists the import
//! never touches `handover-identity` again: a `preferences.json` set aside
//! cannot bring the Swift identity back over one stored since (Pair
//! again, in Settings, must keep the marker). The import is skipped
//! altogether under `STENO_SMOKE_SECONDS` and whenever `HOME` is not the
//! account's home directory ([`LaunchContext`]), so a smoke run or a test
//! with a scratch `HOME` never touches the user's keychain or preferences.

mod identity;
mod sources;

use std::io::Cursor;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, TryLockError};

use steno_core::protocols::BoundaryResult;
use steno_core::{SecretKey, SecretStore, async_trait};
use steno_handover::FingerprintRecord;
use steno_host::onboarding::OnboardingViewModel;
use steno_host::services::{Preferences as _, SwiftImport, SwiftImportStage, SwiftImportStatus};
use tokio::sync::watch;

pub use identity::{ImportedIdentityError, decode_pkcs12, store_imported_identity};
pub use sources::{ApiKeyItem, KeychainRefusal, SwiftDefaults, SwiftKeychain};
#[cfg(target_os = "macos")]
pub use sources::{DefaultsCommand, LoginKeychain};

use crate::block_on;
use crate::platform::FilePreferences;

/// The Swift app's bundle identifier, its `UserDefaults` domain. Swift:
/// `PRODUCT_BUNDLE_IDENTIFIER` in `apps/macos/project.yml`.
pub const SWIFT_DEFAULTS_DOMAIN: &str = "uno.schmid.steno.mac";
/// The label of the Swift handover certificate and key. Swift:
/// `IdentityKeychain.defaultLabel` in
/// `Sources/StenoHandover/Identity/IdentityKeychain.swift`.
pub const SWIFT_IDENTITY_LABEL: &str = "Steno handover identity";
/// The label the Swift app gives the API key item, `Steno <key>`; the
/// `keyring` crate sets none. Swift: `KeychainSecretStore` in
/// `apps/macos/Steno/Services/KeychainSecretStore.swift`.
pub const SWIFT_API_KEY_LABEL: &str = "Steno llm-api-key";
/// Set once the import is over: the identity came over, or there was none.
pub const IMPORT_RAN_KEY: &str = "steno.swiftImportRan";
/// The keychain entry, filed under
/// [`KEYRING_SERVICE`](crate::secrets::KEYRING_SERVICE) like the secrets,
/// that records the import outside `preferences.json`: written right after
/// the identity is stored, and from then on the import never touches
/// `handover-identity` again, though `preferences.json` was set aside. An
/// item of this app's own, found by an attribute query, so neither its
/// write nor its lookup prompts. Pair again (Settings) must keep it, or a
/// later launch would bring the Swift identity back over the new one.
pub const IMPORT_DONE_ENTRY: &str = "swift-import-done";
/// Set once the step read the Swift API key, or was refused: a later
/// launch whose import still waits for the identity reads the key as it
/// always does, unless [`KEY_DENIED_KEY`] is set too, and the step asks
/// for the identity alone.
pub const KEY_READ_KEY: &str = "steno.swiftImportKeyRead";
/// Set when the step's read of the Swift API key was refused, cleared once
/// the user saves a key: until then every launch answers no key without
/// asking the keychain ([`key_denied_secrets`]), so the key brings up no
/// prompt at any launch, during the import or after it.
pub const KEY_DENIED_KEY: &str = "steno.swiftImportKeyDenied";
/// Sparkle's `SUEnableAutomaticChecks`, for the updater's schedule (S4).
pub const AUTOMATIC_CHECKS_KEY: &str = "steno.updates.automaticChecks";
/// Sparkle's `SUAutomaticallyUpdate`, for the updater's schedule (S4).
pub const AUTOMATIC_DOWNLOAD_KEY: &str = "steno.updates.automaticDownload";

/// The Swift domain's keys the launch half copies, and where to. Swift:
/// `OnboardingViewModel.onboardingCompletedKey` in
/// `apps/macos/Steno/Onboarding/OnboardingViewModel.swift`; Sparkle writes
/// its two keys itself (`apps/macos/Steno/Services/UpdaterController.swift`).
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
    /// `HOME` is not the account's home directory, or one of them is
    /// unknown (off the Mac there is no account lookup).
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
    /// This process: a smoke run when `smoke_variable` is set (the shell's
    /// `STENO_SMOKE_SECONDS`), `HOME`, and the account's home.
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
    /// No earlier launch's step read or was refused the key
    /// ([`KEY_READ_KEY`]): the graph reads none until the step ran. The
    /// step reads the item when it is the Swift app's, and lets the graph
    /// read another app's (a desktop-id build's), which may prompt once.
    Unread(ApiKeyItem),
    /// An earlier step read it: the store answers.
    Read,
    /// An earlier step was refused it ([`KEY_DENIED_KEY`]): the graph
    /// answers no key until the user saves one.
    Denied,
}

/// The import the graph is built over while it waits for the step:
/// `steno_services::build_with_import` reads no API key and binds no
/// listener under it.
pub struct PendingImport {
    /// The graph's `preferences.json`, shared with the step so neither
    /// overwrites the other's keys.
    pub preferences: Arc<FilePreferences>,
    pub keychain: Arc<dyn SwiftKeychain>,
    /// Where the API key stands.
    pub key: LaunchKey,
    /// The keychain already holds a `handover-identity` entry (a
    /// desktop-id build's), or the query for one failed: replacing it may
    /// prompt once more, since `keyring` reads an item before it
    /// overwrites it.
    pub replaces_identity: bool,
    /// The [`IMPORT_DONE_ENTRY`] query failed: the run asks again before
    /// it stores, and replaces nothing while the query fails.
    pub marker_unknown: bool,
}

/// The launch half. While `preferences.json` holds no onboarding flag it
/// copies three values from the Swift domain (`/usr/bin/defaults export
/// uno.schmid.steno.mac -`, [`SwiftDefaults`]): `steno.onboardingCompleted`
/// as it is, and Sparkle's `SUEnableAutomaticChecks` and
/// `SUAutomaticallyUpdate` as [`AUTOMATIC_CHECKS_KEY`] and
/// [`AUTOMATIC_DOWNLOAD_KEY`], the two flags the updater's schedule (S4)
/// reads. `steno.loginItemRegistered` stays behind, so the new identifier
/// registers its own login item; the floating panel's anchor stays behind,
/// so the panel opens at its default place.
///
/// Then, when the keychain holds the Swift handover certificate
/// ([`SWIFT_IDENTITY_LABEL`], found by a certificate query that asks
/// nothing, [`SwiftKeychain`]), or the query failed, the import is pending.
/// A key alone does not make it pending, since a desktop-id build files its
/// key under the same service and account. Attribute queries, which ask
/// nothing either, tell the Swift key ([`SWIFT_API_KEY_LABEL`]) from a
/// desktop-id build's and find a `handover-identity` entry a desktop-id
/// build stored; the step counts a prompt for each. A query that fails
/// counts as an item that may prompt. A second launch after the import ran
/// does nothing; so does one that finds the [`IMPORT_DONE_ENTRY`] marker,
/// which writes [`IMPORT_RAN_KEY`] again. A marker query that fails leaves
/// the import pending with nothing to replace until a query answers: only
/// a clean not-found counts as a first run.
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
    match keychain.swift_certificate() {
        Ok(None) => {
            preferences.set_flag(IMPORT_RAN_KEY, true);
            Launch::Done
        }
        found => {
            // A query that failed may have hidden the Swift identity: the
            // import waits rather than let the listener mint over it.
            if let Err(error) = found {
                tracing::warn!(%error, "the Swift handover certificate could not be looked up");
            }
            // An item a query could not see may be there and prompt: a key
            // counts as one that is not the Swift app's, an identity entry
            // as one the import replaces.
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

/// The launch half on this Mac over the graph's `preferences.json`, the
/// real defaults command and the login keychain: the import the graph is
/// to be built over, or `None` when nothing is pending (and always off the
/// Mac). [`crate::build_with_import`] calls it under the database's lock.
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

/// Copies [`COPIED`] from the Swift domain, each only where
/// `preferences.json` has no value of its own yet.
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
    /// Whether the listener may be built: only once the identity is in
    /// place, never while the step is pending or waits.
    #[must_use]
    pub fn opens_listener(self) -> bool {
        self == HandoverGate::Ready
    }
}

/// Why the handover waits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitReason {
    /// The export of the Swift identity was denied, failed or skipped.
    ImportDenied,
}

/// What the step lets through to the API key reads. Its `Debug` leaves
/// the key out.
#[derive(Clone, PartialEq, Eq)]
enum KeyGate {
    /// No key item to read (`ApiKeyItem::Missing`) until the step ran:
    /// every read answers no key, and an LLM pass runs without one.
    Closed,
    /// A key is in the keychain that this launch does not read (before the
    /// step, after Not now or a refused read): every read answers no key,
    /// and the pipeline runs no LLM pass ([`ImportGate::key_withheld`]).
    Withheld,
    /// The value the step read (`None` when the item was gone, or the
    /// user cleared it), answered without asking the keychain again.
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
/// gate.
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

    /// Returns once the step ran or was skipped (the handover gate left
    /// `Pending`), when the pipeline has already reloaded with what the
    /// step read: the launch's resume, re-export and recovery wait here.
    pub async fn step_over(&self) {
        let mut gate = self.subscribe();
        // The sender lives in `self`, so the wait cannot fail.
        let _ = gate.wait_for(|gate| *gate != HandoverGate::Pending).await;
    }

    /// Whether the API key is withheld now: the graph builds no LLM pass
    /// for an endpoint that needs the key, so a meeting completes without
    /// a summary rather than fail at the summary.
    #[must_use]
    pub fn key_withheld(&self) -> bool {
        *self.key() == KeyGate::Withheld
    }

    fn set_handover(&self, gate: HandoverGate) {
        self.handover.send_replace(gate);
    }

    fn key(&self) -> MutexGuard<'_, KeyGate> {
        self.key.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The secret store the graph reads through while an import is pending:
/// the API key as the [`KeyGate`] allows, everything else as the store
/// behind it answers.
struct GatedSecrets {
    inner: Arc<dyn SecretStore>,
    gate: Arc<ImportGate>,
    /// Where a saved key clears [`KEY_DENIED_KEY`].
    preferences: Arc<FilePreferences>,
}

impl GatedSecrets {
    /// Whether a cleared API key goes to the store: only through an open
    /// gate. Otherwise a key the step read, or one withheld, is dropped for
    /// this launch: the user chose no key, so the LLM passes run without
    /// one.
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

    /// A key the user saves goes to the store, which answers from then on.
    /// While the gate is not open, a `None` or empty API key never reaches
    /// the store: the form showed no key because the gate hid it (or showed
    /// the one the step read), so a cleared field or a keyless preset would
    /// otherwise delete the item the Swift app shares, which a rollback
    /// still reads. The gate answers no key from then on at this launch; the
    /// item, and with it the key, is still there at the next launch.
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
            // Under the gate's lock, as a refused read sets the flag, so
            // the two cannot cross.
            let mut gate = self.gate.key();
            *gate = KeyGate::Open;
            if self.preferences.flag(KEY_DENIED_KEY) {
                self.preferences.set_flag(KEY_DENIED_KEY, false);
            }
        }
        Ok(())
    }
}

/// The secret store of a graph without a pending import: `secrets` itself,
/// or, while the step's read of the Swift API key stands refused
/// ([`KEY_DENIED_KEY`]), `secrets` behind a gate that withholds the key
/// without asking the keychain, until the user saves one; with that gate,
/// which the pipeline asks whether the key is withheld.
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
    pub preferences: Arc<FilePreferences>,
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
    /// Wraps `secrets` for the graph; the step stores the identity in
    /// `secrets` and its fingerprint in `record`.
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

    /// The step, with `reload` run whenever the key gate opened (the
    /// pipeline rebuilds its passes with the key).
    #[must_use]
    pub fn step(
        &self,
        runtime: tokio::runtime::Handle,
        reload: Box<dyn Fn() + Send + Sync>,
    ) -> ImportStep {
        ImportStep {
            keychain: self.keychain.clone(),
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
    /// A key that is not the Swift app's, until the step let the graph
    /// read it.
    other_key: bool,
    /// The stored `handover-identity` entry the identity replaces.
    replaces_identity: bool,
}

struct StepState {
    stage: SwiftImportStage,
    items: StepItems,
    /// The [`IMPORT_DONE_ENTRY`] query has not answered yet: the run asks
    /// before it stores.
    marker_unknown: bool,
    /// The decoded identity a failed store left, so Try again repeats the
    /// store alone, without a second export prompt.
    bundle: Option<String>,
    error: Option<&'static str>,
}

/// The onboarding step's half of the import, shown first while the import
/// is pending (`steno_host::services::SwiftImport`).
///
/// A run reads the Swift API key through `keyring` (one prompt; the item
/// stays as it is, shared with the Swift app, which still reads it after a
/// rollback), or lets the graph read a desktop-id build's key (one prompt at
/// most). A key the user saved in Settings before the run wins: the Swift
/// key is then not read. The run then exports the handover identity (the
/// certificate by label, `SecIdentityCreateWithCertificate`, `SecItemExport`
/// as PKCS#12 through `steno-macos`, one prompt), decodes it
/// ([`decode_pkcs12`]) and stores it as the PEM entry `handover-identity`
/// ([`store_imported_identity`]), replacing a desktop-id build's entry (one
/// more prompt, as `keyring` reads an item before it overwrites it), and
/// records its fingerprint: the Swift identity is the one the paired
/// phones pin.
///
/// A denied key read leaves the key empty, and Settings asks for it
/// ([`KEY_DENIED_KEY`]). A denied or failed export or store leaves the
/// handover waiting; Try again repeats only the store when the export got
/// through, and the step comes back at the next launch. Not now, or closing
/// the window over the step, brings up no prompt: what the step has not
/// read yet stays unread for this launch, and the step comes back with the
/// same prompts at the next launch.
///
/// One run at a time: a run or a skip while a run is under way answers the
/// status at once and changes nothing, since that run sets the stage
/// itself. So the main thread, which skips when the window closes, never
/// waits on a keychain prompt.
pub struct ImportStep {
    keychain: Arc<dyn SwiftKeychain>,
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

/// The step's line for an export or store that macOS refused (`denied`)
/// or that failed another way.
fn export_error(denied: bool) -> &'static str {
    if denied { DENIED_EXPORT } else { FAILED_EXPORT }
}

impl ImportStep {
    fn state(&self) -> MutexGuard<'_, StepState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// What the step read (`Read`), or that it withholds the key
    /// (`Withheld`, refused or skipped), is what the graph reads from now
    /// on, without asking again; a key the user saved meanwhile (an open
    /// gate) stays. The pipeline reloads on a change.
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

    /// Held for a run or a skip; `None` when another run is under way or
    /// the step is over, and the caller answers the status.
    fn one_at_a_time(&self) -> Option<MutexGuard<'_, ()>> {
        let running = match self.running.try_lock() {
            Ok(guard) => guard,
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(TryLockError::WouldBlock) => return None,
        };
        (self.state().stage != SwiftImportStage::Done).then_some(running)
    }

    /// The step had no Swift key to read: the store answers from now on.
    /// `other_key` opens the gate also over a key item that is not the
    /// Swift app's, which stays withheld until then.
    fn open_key(&self, other_key: bool) {
        let mut key = self.gate.key();
        if *key == KeyGate::Closed || (other_key && *key == KeyGate::Withheld) {
            *key = KeyGate::Open;
            drop(key);
            (self.reload)();
        }
    }

    /// Exports and decodes the Swift identity into its PEM bundle; `None`
    /// when the certificate is gone by now (nothing is left to import).
    /// The export is of the certificate this lookup found.
    fn export(&self) -> Result<Option<String>, &'static str> {
        let certificate = match self.keychain.swift_certificate() {
            Ok(Some(certificate)) => certificate,
            Ok(None) => return Ok(None),
            Err(error) => {
                tracing::warn!(%error, "the Swift handover certificate could not be looked up");
                return Err(FAILED_EXPORT);
            }
        };
        let passphrase = uuid::Uuid::new_v4().to_string();
        let pkcs12 = self
            .keychain
            .export_identity(&certificate, &passphrase)
            .map_err(|refusal| {
                tracing::warn!(%refusal, "the Swift handover identity was not exported");
                export_error(refusal.denied)
            })?;
        let (_, bundle) = decode_pkcs12(&pkcs12, &passphrase, &certificate).map_err(|error| {
            tracing::warn!(%error, "the exported Swift handover identity was refused");
            FAILED_EXPORT
        })?;
        Ok(Some(bundle))
    }

    /// Stores the identity a failed store kept, or else the one an export
    /// brings now, with its fingerprint, then writes the
    /// [`IMPORT_DONE_ENTRY`] marker. A store that fails keeps the bundle for
    /// Try again, and writes no marker, also when only the fingerprint was
    /// not recorded; while a stored entry is being replaced a failed write
    /// counts as the prompt for that entry denied (the `keyring` crate
    /// keeps the status in its text only), which Always Allow fixes.
    fn import_identity(&self) -> Result<(), &'static str> {
        if self.state().marker_unknown {
            // Only a clean not-found is a first run: a marker that cannot
            // be read may stand for an identity stored since, which a
            // replace would lose. Nothing is replaced, as after a denied
            // export.
            match self.keychain.import_done() {
                Ok(true) => return Ok(()),
                Ok(false) => self.state().marker_unknown = false,
                Err(error) => {
                    tracing::warn!(%error, "the import's marker could not be looked up");
                    return Err(FAILED_EXPORT);
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
            // A fingerprint that was not recorded is no keychain refusal:
            // the handover refuses the stored identity until Try again
            // stores it with its fingerprint.
            match error {
                ImportedIdentityError::Record(_) => FAILED_EXPORT,
                _ => export_error(state.items.replaces_identity),
            }
        })?;
        // Second, after the identity: a crash between the two leaves a
        // rerun that stores the same Swift identity again. A marker that
        // cannot be written is logged; `IMPORT_RAN_KEY` still ends the
        // import.
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
        let Some(_running) = self.one_at_a_time() else {
            return self.status();
        };
        if self.state().items.read_key {
            // A key saved in Settings since the launch wins: the Swift
            // key is not read over it.
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
        let error = self.import_identity().err();
        {
            let mut state = self.state();
            state.stage = if error.is_none() {
                SwiftImportStage::Done
            } else {
                SwiftImportStage::Waiting
            };
            state.error = error;
        }
        if error.is_none() {
            self.preferences.set_flag(IMPORT_RAN_KEY, true);
            self.gate.set_handover(HandoverGate::Ready);
        } else {
            self.gate
                .set_handover(HandoverGate::Waiting(WaitReason::ImportDenied));
        }
        self.status()
    }

    fn skip(&self) -> SwiftImportStatus {
        // A run under way sets the stage itself; the caller may be the
        // main thread, which must not wait on its prompts.
        let Some(_running) = self.one_at_a_time() else {
            return self.status();
        };
        // Not now brings up no prompt: a key item is left unread for this
        // launch, whoever stored it.
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

#[cfg(test)]
mod tests;
