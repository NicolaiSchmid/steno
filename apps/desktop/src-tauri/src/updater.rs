//! Updates over `tauri-plugin-updater`: a signed manifest per lane on the
//! GitHub release, checked on request from the tray or from Settings
//! (`updates.check`). The lane follows the installed version, as Sparkle's
//! channel does in the Swift app: a pre-release build ("0.11.0-rc.1") reads
//! the beta lane's manifest first and falls back to the stable one; a
//! stable build reads the stable manifest only. No setting.
//!
//! Each lane is a rolling GitHub release that holds only its `latest.json`,
//! which `.github/workflows/desktop-release.yml` replaces when it publishes
//! a `desktop-v*` tag. The stable lane takes releases, the beta lane every
//! version, and neither moves backwards (`apps/desktop/scripts/updater-lanes.sh`),
//! so a beta build is offered the stable release that follows it. The
//! manifest points at the installers on the versioned release. Neither
//! lane is GitHub's "latest" release, which stays the Swift app's until the
//! Mac cutover (`.plans/2026-10-04-mac-cutover.md`). `tauri.conf.json`
//! carries the public key (`plugins.updater.pubkey`) and the stable
//! endpoint. The matching private key is the `TAURI_SIGNING_PRIVATE_KEY`
//! secret and lives nowhere in the repository.
//!
//! Swift: `UpdaterController.swift`, `UpdateChannels.swift`.

use std::sync::Mutex;

use tauri::{AppHandle, Manager, Url};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
use tauri_plugin_updater::UpdaterExt;

use crate::bridge::{BridgeError, failed};

/// The stable lane: the rolling `desktop-stable` release, which carries the
/// newest release's manifest.
pub const STABLE_ENDPOINT: &str =
    "https://github.com/NicolaiSchmid/steno/releases/download/desktop-stable/latest.json";
/// The pre-release lane: the rolling `desktop-beta` release, which carries
/// the manifest of the newest pre-release or release.
pub const BETA_ENDPOINT: &str =
    "https://github.com/NicolaiSchmid/steno/releases/download/desktop-beta/latest.json";

/// Whether a marketing version is a pre-release (`UpdateChannels.allowed`:
/// a hyphen means the beta lane).
pub fn is_prerelease(version: &str) -> bool {
    version.contains('-')
}

/// The manifests a build reads, in order.
pub fn endpoints(version: &str) -> Vec<Url> {
    let mut urls = Vec::with_capacity(2);
    if is_prerelease(version) {
        urls.push(Url::parse(BETA_ENDPOINT).expect("a valid beta endpoint"));
    }
    urls.push(Url::parse(STABLE_ENDPOINT).expect("a valid stable endpoint"));
    urls
}

/// What the last check found; the General section shows it.
/// `steno_host::services::UpdateOutcome` is the same; `WP6b` keeps that one
/// when it implements the host's `Updater` over this module.
///
/// Swift: `UpdateCheckOutcome`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum UpdateOutcome {
    #[default]
    NotChecked,
    UpToDate,
    Available(String),
    Failed(String),
}

impl UpdateOutcome {
    /// The raw value on the wire (`GeneralSettingsSnapshot.Updates.Outcome`);
    /// the host's, for its General snapshot.
    #[allow(dead_code)]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::NotChecked => "notChecked",
            Self::UpToDate => "upToDate",
            Self::Available(_) => "available",
            Self::Failed(_) => "failed",
        }
    }
}

/// The last outcome, managed state the host reads for its snapshot.
#[derive(Debug, Default)]
pub struct Updates {
    last: Mutex<UpdateOutcome>,
}

impl Updates {
    /// The host reads this for the General section (`WP6b`).
    #[allow(dead_code)]
    pub fn last(&self) -> UpdateOutcome {
        self.last
            .lock()
            .map(|last| last.clone())
            .unwrap_or_default()
    }

    fn record(&self, outcome: UpdateOutcome) {
        if let Ok(mut last) = self.last.lock() {
            *last = outcome;
        }
    }
}

/// The plugin; `check` supplies the lanes per build and `tauri.conf.json`
/// the public key.
pub fn plugin() -> tauri::plugin::TauriPlugin<tauri::Wry, tauri_plugin_updater::Config> {
    tauri_plugin_updater::Builder::new().build()
}

/// Checks the lanes and records the outcome. The update itself, when there
/// is one, is returned for the caller to offer; a check that could not run
/// is `failed` with the updater's words.
pub async fn check(app: &AppHandle) -> Result<Option<tauri_plugin_updater::Update>, BridgeError> {
    let version = app.package_info().version.to_string();
    let outcome = async {
        let updater = app
            .updater_builder()
            .endpoints(endpoints(&version))
            .map_err(failed)?
            .build()
            .map_err(failed)?;
        updater.check().await.map_err(failed)
    }
    .await;
    let updates = app.state::<Updates>();
    match &outcome {
        Ok(Some(update)) => updates.record(UpdateOutcome::Available(update.version.clone())),
        Ok(None) => updates.record(UpdateOutcome::UpToDate),
        Err(error) => updates.record(UpdateOutcome::Failed(error.message.clone())),
    }
    outcome
}

/// A one-button message from the updater.
fn notify(app: &AppHandle, kind: MessageDialogKind, message: impl Into<String>) {
    app.dialog()
        .message(message)
        .title("Steno")
        .kind(kind)
        .show(|_| {});
}

/// The tray's "Check for Updates…": checks, then asks before installing,
/// as Sparkle's standard driver does, and relaunches when the user agrees.
pub async fn check_and_offer(app: &AppHandle) {
    let update = match check(app).await {
        Ok(Some(update)) => update,
        Ok(None) => {
            notify(app, MessageDialogKind::Info, "Steno is up to date.");
            return;
        }
        Err(error) => {
            notify(
                app,
                MessageDialogKind::Error,
                format!("The update check failed: {}", error.message),
            );
            return;
        }
    };
    let (sender, receiver) = tokio::sync::oneshot::channel();
    app.dialog()
        .message(format!(
            "Steno {} is available. Install it and relaunch?",
            update.version
        ))
        .title("Steno")
        .kind(MessageDialogKind::Info)
        .buttons(MessageDialogButtons::OkCancelCustom(
            "Install and Relaunch".into(),
            "Later".into(),
        ))
        .show(move |agreed| {
            let _ = sender.send(agreed);
        });
    if receiver.await != Ok(true) {
        return;
    }
    match update.download_and_install(|_, _| {}, || {}).await {
        Ok(()) => app.restart(),
        Err(error) => {
            app.state::<Updates>()
                .record(UpdateOutcome::Failed(error.to_string()));
            notify(
                app,
                MessageDialogKind::Error,
                format!("The update could not be installed: {error}"),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hyphen_means_the_beta_lane() {
        assert!(is_prerelease("0.11.0-rc.1"));
        assert!(is_prerelease("1.0.0-beta"));
        assert!(!is_prerelease("0.11.0"));
        assert!(!is_prerelease("1.0.0"));
    }

    #[test]
    fn a_prerelease_reads_beta_then_stable_and_a_release_stable_only() {
        let beta: Vec<String> = endpoints("0.11.0-rc.1")
            .into_iter()
            .map(String::from)
            .collect();
        assert_eq!(beta, [BETA_ENDPOINT, STABLE_ENDPOINT]);
        let stable: Vec<String> = endpoints("0.11.0").into_iter().map(String::from).collect();
        assert_eq!(stable, [STABLE_ENDPOINT]);
    }

    #[test]
    fn the_endpoints_are_https_on_github() {
        for url in endpoints("0.11.0-rc.1") {
            assert_eq!(url.scheme(), "https");
            assert_eq!(url.host_str(), Some("github.com"));
            assert!(url.path().ends_with("/latest.json"));
        }
    }

    #[test]
    fn the_configured_endpoint_is_the_stable_lane() {
        let config: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).expect("tauri.conf.json");
        assert_eq!(
            config["plugins"]["updater"]["endpoints"],
            serde_json::json!([STABLE_ENDPOINT])
        );
    }

    #[test]
    fn the_outcome_spells_as_the_contract_does() {
        assert_eq!(UpdateOutcome::NotChecked.as_str(), "notChecked");
        assert_eq!(UpdateOutcome::UpToDate.as_str(), "upToDate");
        assert_eq!(UpdateOutcome::Available("1.0".into()).as_str(), "available");
        assert_eq!(UpdateOutcome::Failed("x".into()).as_str(), "failed");
        let updates = Updates::default();
        assert_eq!(updates.last(), UpdateOutcome::NotChecked);
        updates.record(UpdateOutcome::UpToDate);
        assert_eq!(updates.last(), UpdateOutcome::UpToDate);
    }
}
