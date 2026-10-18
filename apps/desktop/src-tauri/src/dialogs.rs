//! The native dialogs the pages cannot draw: folder panels, the
//! destructive alert, revealing a file, opening a folder. The host is
//! headless, so the shell shows a folder panel and calls the same method
//! on the host with `{ "path": "<chosen>" }`, or not at all when cancelled;
//! the page's reply is built from the choice, never the host's.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use steno_bridge::ConfirmDestructiveParams;
use tauri::{AppHandle, WebviewWindow};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
use tauri_plugin_opener::OpenerExt;

use crate::bridge::{BridgeError, failed};

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

    /// The panel's title, which GTK and Windows need.
    pub const fn title(self) -> &'static str {
        match self {
            Self::RecordingsFolder => "Choose where recordings are kept",
            Self::ExportVault | Self::OnboardingVault => "Choose your Obsidian vault",
        }
    }
}

/// `reply.chosenPath`: `{ "path": … }`, or `{}` when cancelled. The `Some`
/// shape is also what the host is told.
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
        .map_err(|_| BridgeError::failed("The folder dialog closed without an answer."))?;
    match chosen {
        None => Ok(None),
        Some(path) => path.into_path().map(Some).map_err(failed),
    }
}

/// The host's destructive confirmation: a warning alert, true when the user
/// confirmed. Blocks until answered, so never call it on the main thread.
pub fn confirm_destructive(app: &AppHandle, prompt: &ConfirmDestructiveParams) -> bool {
    app.dialog()
        .message(&prompt.message)
        .title(&prompt.title)
        .kind(MessageDialogKind::Warning)
        .buttons(confirm_buttons(prompt))
        .blocking_show()
}

fn confirm_buttons(prompt: &ConfirmDestructiveParams) -> MessageDialogButtons {
    MessageDialogButtons::OkCancelCustom(prompt.confirm_title.clone(), "Cancel".to_owned())
}

/// Shows a file or folder in the file manager, selected where the platform
/// can; the host calls it through its `Opener`.
pub fn reveal(app: &AppHandle, path: &Path) -> Result<(), BridgeError> {
    app.opener().reveal_item_in_dir(path).map_err(failed)
}

/// Opens a URL the shell itself composed; the page's links go through
/// `bridge::openable_url` first.
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
    fn the_alert_confirms_with_the_prompts_button() {
        let prompt: ConfirmDestructiveParams = serde_json::from_str(include_str!(
            "../../../macos/web/fixtures/bridge/params.ui.confirmDestructive.json"
        ))
        .unwrap();
        let MessageDialogButtons::OkCancelCustom(confirm, cancel) = confirm_buttons(&prompt) else {
            panic!("not a confirm and a cancel button");
        };
        assert_eq!(
            (confirm.as_str(), cancel.as_str()),
            (prompt.confirm_title.as_str(), "Cancel")
        );
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
