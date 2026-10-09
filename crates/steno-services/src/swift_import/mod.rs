//! The import of what the Swift app left behind, on the Mac's first launch
//! after the update to this app (plan `.plans/2026-10-07-stable-promotion.md`,
//! S6 and "First launch after the handoff"). The new app has its own
//! identifier, so it reads the Swift app's defaults domain by name and asks
//! macOS, once per keychain item, for what the Swift app stored there.
//!
//! Two halves, both macOS only in the product (the logic is
//! platform-independent and tested everywhere over fakes):
//!
//! - **[`launch`]**, first in the shell's `setup`, before the graph is
//!   built. While `preferences.json` holds no onboarding flag it copies
//!   three values from the Swift domain (`/usr/bin/defaults export
//!   uno.schmid.steno.mac -`, [`SwiftDefaults`]):
//!   `steno.onboardingCompleted` as it is, and Sparkle's
//!   `SUEnableAutomaticChecks` and `SUAutomaticallyUpdate` as
//!   [`AUTOMATIC_CHECKS_KEY`] and [`AUTOMATIC_DOWNLOAD_KEY`], the two
//!   flags the updater's schedule (S4) reads. `steno.loginItemRegistered`
//!   stays behind, so the new identifier registers its own login item; the
//!   floating panel's anchor stays behind, so the panel opens at its
//!   default place. Then, when the keychain holds the Swift handover
//!   certificate ([`SWIFT_IDENTITY_LABEL`], found by a certificate query
//!   that asks nothing, [`SwiftKeychain`]), the import is pending: the graph
//!   is built over [`PendingImport`], which reads no API key and starts no
//!   handover listener until the second half ends. A key alone does not
//!   make the import pending, since a desktop-id build files its key under
//!   the same service and account; the step reads the key only when it is
//!   the Swift one ([`SWIFT_API_KEY_LABEL`]).
//! - **[`ImportStep`]**, the onboarding step's half, shown first while the
//!   import is pending (`steno_host::services::SwiftImport`). It reads the
//!   API key through `keyring` (one prompt; the item stays as it is,
//!   shared with the Swift app, which still reads it after a rollback),
//!   then exports the handover identity (the certificate by label,
//!   `SecIdentityCreateWithCertificate`, `SecItemExport` as PKCS#12 through
//!   `steno-macos`, one prompt), decodes it ([`decode_pkcs12`]) and stores
//!   it as the PEM entry `handover-identity` with its fingerprint
//!   ([`store_imported_identity`]), replacing a desktop-id identity (D5):
//!   the Swift one is the identity the paired phones pin. A denied key read
//!   leaves the key empty, and Settings asks for it. A denied or failed
//!   export never mints an identity and never replaces one: the gate
//!   ([`HandoverGate`]) says the handover waits, the step offers Try again,
//!   and the step comes back at the next launch. Skipping the step counts
//!   as a denied read and a denied export.
//!
//! `steno.swiftImportRan` ([`IMPORT_RAN_KEY`]) is set once the identity is
//! in place, or at launch when there is no Swift certificate to import,
//! and the import never runs again. The import is skipped altogether
//! under `STENO_SMOKE_SECONDS` and whenever `HOME` is not the account's
//! home directory ([`LaunchContext`]), so a smoke run or a test with a
//! scratch `HOME` never touches the user's keychain or preferences.

mod identity;
mod sources;

use std::io::Cursor;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use steno_core::protocols::BoundaryResult;
use steno_core::{SecretKey, SecretStore, async_trait};
use steno_host::onboarding::OnboardingViewModel;
use steno_host::services::{Preferences as _, SwiftImport, SwiftImportStage, SwiftImportStatus};
use tokio::sync::watch;

pub use identity::{
    IDENTITY_FINGERPRINT_KEY, ImportedIdentityError, decode_pkcs12, store_imported_identity,
};
#[cfg(target_os = "macos")]
pub use sources::{DefaultsCommand, LoginKeychain};
pub use sources::{KeychainRefusal, SwiftDefaults, SwiftKeychain};

use crate::block_on;
use crate::platform::FilePreferences;

/// The Swift app's bundle identifier, its `UserDefaults` domain.
pub const SWIFT_DEFAULTS_DOMAIN: &str = "uno.schmid.steno.mac";
/// The label of the Swift handover certificate and key
/// (`IdentityKeychain.defaultLabel`).
pub const SWIFT_IDENTITY_LABEL: &str = "Steno handover identity";
/// The label the Swift app gives the API key item (`Steno <key>`,
/// `KeychainSecretStore`); the `keyring` crate sets none.
pub const SWIFT_API_KEY_LABEL: &str = "Steno llm-api-key";
/// Set once the import is over: the identity came over, or there was none.
pub const IMPORT_RAN_KEY: &str = "steno.swiftImportRan";
/// Set once the step read the Swift API key, or was refused: a later
/// launch whose import still waits for the identity reads the key as it
/// always does, and the step asks for the identity alone.
pub const KEY_READ_KEY: &str = "steno.swiftImportKeyRead";
/// Sparkle's `SUEnableAutomaticChecks`, for the updater's schedule (S4).
pub const AUTOMATIC_CHECKS_KEY: &str = "steno.updates.automaticChecks";
/// Sparkle's `SUAutomaticallyUpdate`, for the updater's schedule (S4).
pub const AUTOMATIC_DOWNLOAD_KEY: &str = "steno.updates.automaticDownload";

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
                .field("read_key", &pending.read_key)
                .finish_non_exhaustive(),
        }
    }
}

/// The import the graph is built over while it waits for the step:
/// `steno_services::build_with_import` reads no API key and binds no
/// listener under it.
pub struct PendingImport {
    /// The graph's `preferences.json`, shared with the step so neither
    /// overwrites the other's keys.
    pub preferences: Arc<FilePreferences>,
    pub keychain: Arc<dyn SwiftKeychain>,
    /// The graph reads no API key until the step ran: no earlier launch's
    /// step read or was refused the key ([`KEY_READ_KEY`]).
    pub gate_key: bool,
    /// The step reads the Swift API key: the key is gated and the item is
    /// the Swift app's.
    pub read_key: bool,
}

/// The launch half: copies the Swift preferences while `preferences.json`
/// has no onboarding flag, then decides whether the import is pending.
/// A second launch after the import ran does nothing.
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
            let gate_key = !preferences.flag(KEY_READ_KEY);
            let read_key = gate_key
                && keychain.has_swift_api_key().unwrap_or_else(|error| {
                    tracing::warn!(%error, "the Swift API key could not be looked up");
                    false
                });
            Launch::Pending(PendingImport {
                preferences,
                keychain,
                gate_key,
                read_key,
            })
        }
    }
}

/// The launch half on this Mac, over the real defaults command and the
/// login keychain: the import the graph is to be built over
/// ([`crate::build_with_import`]), or `None` when nothing is pending (and
/// always off the Mac).
#[must_use]
pub fn launch_on_this_mac(
    support_directory: &std::path::Path,
    smoke_variable: &str,
) -> Option<PendingImport> {
    #[cfg(target_os = "macos")]
    {
        let preferences = Arc::new(FilePreferences::in_support_directory(support_directory));
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
        let _ = (support_directory, smoke_variable);
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
            preferences
                .set_bool(ours, value)
                .map_err(|error| format!("{ours} could not be saved: {error}"))?;
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

/// Why the handover waits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitReason {
    /// The export of the Swift identity was denied, failed or skipped.
    ImportDenied,
}

/// What the step lets through to the API key reads.
#[derive(Debug, Clone, PartialEq, Eq)]
enum KeyGate {
    /// Nothing: every read answers no key.
    Closed,
    /// The value the step read (`None` when it was refused or skipped),
    /// answered without asking the keychain again.
    Read(Option<String>),
    /// The secret store answers.
    Open,
}

/// The shared state of a pending import: the handover gate and the key
/// gate.
#[derive(Debug)]
pub struct ImportGate {
    handover: watch::Sender<HandoverGate>,
    key: Mutex<KeyGate>,
}

impl ImportGate {
    fn new(gate_key: bool) -> Self {
        ImportGate {
            handover: watch::Sender::new(HandoverGate::Pending),
            key: Mutex::new(if gate_key {
                KeyGate::Closed
            } else {
                KeyGate::Open
            }),
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
}

#[async_trait]
impl SecretStore for GatedSecrets {
    async fn secret(&self, key: &SecretKey) -> BoundaryResult<Option<String>> {
        if key.as_str() == SecretKey::LLM_API_KEY {
            let gate = self.gate.key().clone();
            match gate {
                KeyGate::Closed => return Ok(None),
                KeyGate::Read(value) => return Ok(value),
                KeyGate::Open => {}
            }
        }
        self.inner.secret(key).await
    }

    /// A key the user saves goes to the store, which answers from then on.
    async fn set_secret(&self, key: &SecretKey, value: Option<&str>) -> BoundaryResult<()> {
        self.inner.set_secret(key, value).await?;
        if key.as_str() == SecretKey::LLM_API_KEY {
            *self.gate.key() = KeyGate::Open;
        }
        Ok(())
    }
}

/// A pending import inside the graph: the gated secret store, the gate,
/// and the step once the graph built it.
pub struct GraphImport {
    pub preferences: Arc<FilePreferences>,
    pub gate: Arc<ImportGate>,
    /// The store every read in the graph goes through.
    pub secrets: Arc<dyn SecretStore>,
    raw_secrets: Arc<dyn SecretStore>,
    keychain: Arc<dyn SwiftKeychain>,
    read_key: bool,
}

impl GraphImport {
    /// Wraps `secrets` for the graph.
    #[must_use]
    pub fn new(pending: PendingImport, secrets: Arc<dyn SecretStore>) -> Self {
        let gate = Arc::new(ImportGate::new(pending.gate_key));
        GraphImport {
            preferences: pending.preferences,
            secrets: Arc::new(GatedSecrets {
                inner: secrets.clone(),
                gate: gate.clone(),
            }),
            gate,
            raw_secrets: secrets,
            keychain: pending.keychain,
            read_key: pending.read_key,
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
            gate: self.gate.clone(),
            reload,
            runtime,
            state: Mutex::new(StepState {
                stage: SwiftImportStage::Pending,
                read_key: self.read_key,
                failure: None,
            }),
            running: Mutex::new(()),
        }
    }
}

#[derive(Debug)]
struct StepState {
    stage: SwiftImportStage,
    read_key: bool,
    failure: Option<&'static str>,
}

/// The onboarding step's half of the import (module doc).
pub struct ImportStep {
    keychain: Arc<dyn SwiftKeychain>,
    preferences: Arc<FilePreferences>,
    /// The store behind the gate: the identity is written here.
    secrets: Arc<dyn SecretStore>,
    gate: Arc<ImportGate>,
    reload: Box<dyn Fn() + Send + Sync>,
    runtime: tokio::runtime::Handle,
    state: Mutex<StepState>,
    /// One run at a time; a second Try again waits for the first.
    running: Mutex<()>,
}

/// The step's line when macOS refused the export.
pub const DENIED_EXPORT: &str =
    "macOS did not let Steno read this Mac's phone pairing. Choose Try again, then Always Allow.";
/// The step's line when the export failed another way.
pub const FAILED_EXPORT: &str = "Steno could not bring over this Mac's phone pairing.";

impl ImportStep {
    fn state(&self) -> MutexGuard<'_, StepState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The key as the step leaves it: read (or refused) when it was the
    /// Swift one, else the store answers.
    fn settle_key(&self, read: Option<Option<String>>) {
        let mut key = self.gate.key();
        match read {
            Some(value) => *key = KeyGate::Read(value),
            None if *key == KeyGate::Closed => *key = KeyGate::Open,
            None => return,
        }
        drop(key);
        (self.reload)();
    }

    /// Exports, decodes and stores the Swift identity; `Ok` also when the
    /// certificate is gone by now (nothing is left to import).
    fn export(&self) -> Result<(), &'static str> {
        let certificate = match self.keychain.swift_certificate() {
            Ok(Some(certificate)) => certificate,
            Ok(None) => return Ok(()),
            Err(error) => {
                tracing::warn!(%error, "the Swift handover certificate could not be looked up");
                return Err(FAILED_EXPORT);
            }
        };
        let passphrase = uuid::Uuid::new_v4().to_string();
        let pkcs12 = self
            .keychain
            .export_identity(&passphrase)
            .map_err(|refusal| {
                tracing::warn!(%refusal, "the Swift handover identity was not exported");
                if refusal.denied {
                    DENIED_EXPORT
                } else {
                    FAILED_EXPORT
                }
            })?;
        let stored =
            decode_pkcs12(&pkcs12, &passphrase, &certificate).and_then(|(identity, bundle)| {
                block_on(
                    &self.runtime,
                    store_imported_identity(&*self.secrets, &self.preferences, &identity, &bundle),
                )
            });
        stored.map_err(|error| {
            tracing::warn!(%error, "the Swift handover identity was not stored");
            FAILED_EXPORT
        })
    }
}

impl SwiftImport for ImportStep {
    fn status(&self) -> SwiftImportStatus {
        let state = self.state();
        SwiftImportStatus {
            stage: state.stage,
            prompts: u8::from(state.read_key) + 1,
            failure: state.failure.map(str::to_owned),
        }
    }

    fn run(&self) -> SwiftImportStatus {
        let _one_at_a_time = self.running.lock().unwrap_or_else(PoisonError::into_inner);
        if self.state().stage == SwiftImportStage::Done {
            return self.status();
        }
        if self.state().read_key {
            let key = self.keychain.read_api_key().unwrap_or_else(|refusal| {
                tracing::warn!(%refusal, "the Swift API key was not read");
                None
            });
            self.preferences.set_flag(KEY_READ_KEY, true);
            self.state().read_key = false;
            self.settle_key(Some(key));
        } else {
            self.settle_key(None);
        }
        let outcome = self.export();
        {
            let mut state = self.state();
            match outcome {
                Ok(()) => {
                    state.stage = SwiftImportStage::Done;
                    state.failure = None;
                }
                Err(failure) => {
                    state.stage = SwiftImportStage::Waiting;
                    state.failure = Some(failure);
                }
            }
        }
        if outcome.is_ok() {
            self.preferences.set_flag(IMPORT_RAN_KEY, true);
            self.gate.set_handover(HandoverGate::Ready);
        } else {
            self.gate
                .set_handover(HandoverGate::Waiting(WaitReason::ImportDenied));
        }
        self.status()
    }

    fn skip(&self) -> SwiftImportStatus {
        let _one_at_a_time = self.running.lock().unwrap_or_else(PoisonError::into_inner);
        if self.state().stage == SwiftImportStage::Done {
            return self.status();
        }
        let read_key = self.state().read_key;
        self.settle_key(read_key.then_some(None));
        self.state().stage = SwiftImportStage::Waiting;
        self.gate
            .set_handover(HandoverGate::Waiting(WaitReason::ImportDenied));
        self.status()
    }
}

#[cfg(test)]
mod tests;
