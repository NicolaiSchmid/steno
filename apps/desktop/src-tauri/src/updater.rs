//! Updates over `tauri-plugin-updater`: a signed manifest per lane on the
//! GitHub release, checked on request from the tray or from Settings
//! (`updates.check`). The lane follows the installed version, as it does
//! with Sparkle: a pre-release build ("0.11.0-rc.1") reads the `beta`
//! manifest first and falls back to the stable one; a stable build reads
//! the stable manifest only. No setting.
//!
//! The manifests are written by the release job once WP9 signs the
//! bundles; `tauri.conf.json` carries the public key
//! (`plugins.updater.pubkey`) and the stable endpoint. The matching private
//! key is the `TAURI_SIGNING_PRIVATE_KEY` secret and lives nowhere in the
//! repository.
//!
//! Swift: `UpdaterController.swift`, `UpdateChannels.swift`.

use std::sync::Mutex;

use tauri::{AppHandle, Manager, Url};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
use tauri_plugin_updater::UpdaterExt;

/// The stable lane: the latest release's manifest.
pub const STABLE_ENDPOINT: &str =
    "https://github.com/NicolaiSchmid/steno/releases/latest/download/latest.json";
/// The pre-release lane: a rolling `beta` release that the release job
/// moves to the newest pre-release.
pub const BETA_ENDPOINT: &str =
    "https://github.com/NicolaiSchmid/steno/releases/download/beta/latest.json";

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

/// The plugin with the shell's lane order; `tauri.conf.json` supplies the
/// public key.
pub fn plugin() -> tauri::plugin::TauriPlugin<tauri::Wry, tauri_plugin_updater::Config> {
    tauri_plugin_updater::Builder::new().build()
}

/// Checks the lanes and records the outcome. The update itself, when there
/// is one, is returned for the caller to offer.
pub async fn check(app: &AppHandle) -> Result<Option<tauri_plugin_updater::Update>, String> {
    let version = app.package_info().version.to_string();
    let outcome = async {
        let updater = app
            .updater_builder()
            .endpoints(endpoints(&version))
            .map_err(|error| error.to_string())?
            .build()
            .map_err(|error| error.to_string())?;
        updater.check().await.map_err(|error| error.to_string())
    }
    .await;
    let updates = app.state::<Updates>();
    match &outcome {
        Ok(Some(update)) => updates.record(UpdateOutcome::Available(update.version.clone())),
        Ok(None) => updates.record(UpdateOutcome::UpToDate),
        Err(message) => updates.record(UpdateOutcome::Failed(message.clone())),
    }
    outcome
}

/// The tray's "Check for Updates…": checks, then asks before installing,
/// as Sparkle's standard driver does, and relaunches when the user agrees.
pub async fn check_and_offer(app: &AppHandle) {
    let update = match check(app).await {
        Ok(Some(update)) => update,
        Ok(None) => {
            app.dialog()
                .message("Steno is up to date.")
                .title("Steno")
                .kind(MessageDialogKind::Info)
                .show(|_| {});
            return;
        }
        Err(message) => {
            app.dialog()
                .message(format!("The update check failed: {message}"))
                .title("Steno")
                .kind(MessageDialogKind::Error)
                .show(|_| {});
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
            app.dialog()
                .message(format!("The update could not be installed: {error}"))
                .title("Steno")
                .kind(MessageDialogKind::Error)
                .show(|_| {});
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
