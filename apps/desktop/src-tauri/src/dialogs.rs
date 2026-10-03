//! The native dialogs the pages cannot draw: the folder panels for the
//! recordings folder and the Obsidian vault, revealing a file, opening a
//! folder. The Swift hosts open `NSOpenPanel` themselves inside the view
//! model call; the Rust host is headless, so the shell shows the panel and
//! hands the host the choice.
//!
//! The three chooser methods keep their contract (no params in, a
//! `chosenPath` reply out) towards the page. Towards the host the shell
//! calls the same method with `{ "path": "<chosen>" }` when a folder was
//! chosen, so the host saves it as the Swift view models do
//! (`audio.setAudioFolder`, `obsidian.chooseVault`, `model.chooseVault`),
//! and does not call the host at all when the panel was cancelled. The
//! reply to the page is the shell's, built from the choice, never the
//! host's.
//!
//! Swift: `chooseFolder` in `SettingsBridge.swift` and
//! `OnboardingBridge.swift`.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use tauri::{AppHandle, WebviewWindow};
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_opener::OpenerExt;

use crate::bridge::{BridgeError, failed};

/// The bridge methods that open a folder panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FolderChooser {
    /// `settings.recording.chooseFolder`
    RecordingsFolder,
    /// `settings.export.chooseVault`
    ExportVault,
    /// `onboarding.chooseVault`
    OnboardingVault,
}

impl FolderChooser {
    pub fn for_method(method: &str) -> Option<Self> {
        match method {
            "settings.recording.chooseFolder" => Some(Self::RecordingsFolder),
            "settings.export.chooseVault" => Some(Self::ExportVault),
            "onboarding.chooseVault" => Some(Self::OnboardingVault),
            _ => None,
        }
    }

    /// The panel's title; the Swift panels have none, the GTK and Windows
    /// ones need one.
    pub const fn title(self) -> &'static str {
        match self {
            Self::RecordingsFolder => "Choose where recordings are kept",
            Self::ExportVault | Self::OnboardingVault => "Choose your Obsidian vault",
        }
    }
}

/// `reply.chosenPath`: `{ "path": … }`, or `{}` when the panel was
/// cancelled (`ChosenPathReply(path: nil)` encodes without the key). The
/// `Some` shape is also what the host is told.
pub fn chosen_path_reply(path: Option<&Path>) -> Value {
    match path {
        Some(path) => json!({ "path": path.to_string_lossy() }),
        None => json!({}),
    }
}

/// Shows the folder panel over `window` and waits for the choice off the
/// main thread. `None` when the user cancelled.
pub async fn choose_folder(
    window: &WebviewWindow,
    chooser: FolderChooser,
) -> Result<Option<PathBuf>, BridgeError> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    window
        .dialog()
        .file()
        .set_title(chooser.title())
        .set_can_create_directories(true)
        .set_parent(window)
        .pick_folder(move |chosen| {
            let _ = sender.send(chosen);
        });
    let chosen = receiver
        .await
        .map_err(|_| BridgeError::failed("The folder panel closed without an answer."))?;
    match chosen {
        None => Ok(None),
        Some(path) => path
            .into_path()
            .map(Some)
            .map_err(|error| BridgeError::failed(error.to_string())),
    }
}

/// Shows a file or folder in the file manager, selected where the
/// platform can. The reveal methods (`settings.recording.revealFolder`,
/// `meeting.reveal*`) need a path only the host knows, so the host calls
/// this (`WP6b`).
#[allow(dead_code)]
pub fn reveal(app: &AppHandle, path: &Path) -> Result<(), BridgeError> {
    app.opener().reveal_item_in_dir(path).map_err(failed)
}

/// Opens a URL the shell itself composed (a settings pane); the page's own
/// links go through `bridge::openable_url` first.
pub fn open_url(app: &AppHandle, url: &str) -> Result<(), BridgeError> {
    app.opener().open_url(url, None::<&str>).map_err(failed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_three_chooser_methods_and_nothing_else() {
        assert_eq!(
            FolderChooser::for_method("settings.recording.chooseFolder"),
            Some(FolderChooser::RecordingsFolder)
        );
        assert_eq!(
            FolderChooser::for_method("settings.export.chooseVault"),
            Some(FolderChooser::ExportVault)
        );
        assert_eq!(
            FolderChooser::for_method("onboarding.chooseVault"),
            Some(FolderChooser::OnboardingVault)
        );
        for method in [
            "settings.recording.revealFolder",
            "onboarding.saveVault",
            "ui.openPanel",
        ] {
            assert_eq!(FolderChooser::for_method(method), None, "{method}");
        }
        for chooser in [
            FolderChooser::RecordingsFolder,
            FolderChooser::ExportVault,
            FolderChooser::OnboardingVault,
        ] {
            assert_ne!(chooser.title(), "");
        }
    }

    #[test]
    fn the_reply_is_the_recorded_shape() {
        let recorded: Value = serde_json::from_str(include_str!(
            "../../../macos/web/fixtures/bridge/reply.chosenPath.json"
        ))
        .unwrap();
        let path = PathBuf::from(recorded["path"].as_str().unwrap());
        assert_eq!(chosen_path_reply(Some(&path)), recorded);
        assert_eq!(chosen_path_reply(None), json!({}));
    }
}
