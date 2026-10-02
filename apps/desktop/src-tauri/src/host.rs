//! What answers the bridge behind the shell's own methods. With the
//! `fixture-host` feature (the default until WP6) every window gets the
//! recorded snapshots on `page.ready` and commands are answered as
//! `apps/macos/web/src/bridge/mock-transport.ts` answers them. Without it
//! there is no host yet and every command fails, loudly.
//!
//! The method signatures are the host interface WP6 fills in, not what this
//! implementation happens to need, so the lints about them are off here.
#![allow(clippy::unused_self, clippy::unnecessary_wraps)]

use serde_json::Value;
use tauri::WebviewWindow;

use crate::bridge::BridgeFailure;

#[cfg(not(feature = "fixture-host"))]
use crate::bridge::ErrorCode;

pub struct Host {
    _private: (),
}

impl Host {
    pub const fn new() -> Self {
        Self { _private: () }
    }

    /// The page has mounted: publish every topic it may show.
    pub fn page_ready(&self, window: &WebviewWindow) -> Result<(), BridgeFailure> {
        #[cfg(feature = "fixture-host")]
        for (topic, snapshot) in crate::fixtures::snapshots() {
            crate::bridge::emit(window, topic, snapshot)?;
        }
        #[cfg(not(feature = "fixture-host"))]
        let _ = window;
        Ok(())
    }

    /// A command that is not one of the shell's own.
    pub fn call(
        &self,
        _window: &WebviewWindow,
        method: &str,
        _params: Value,
    ) -> Result<Value, BridgeFailure> {
        #[cfg(feature = "fixture-host")]
        {
            Ok(crate::fixtures::reply(method))
        }
        #[cfg(not(feature = "fixture-host"))]
        {
            Err(BridgeFailure::new(
                ErrorCode::Failed,
                format!("{method}: no bridge host is wired yet"),
            ))
        }
    }

    /// `window.open` names a meeting or a section for a window that is
    /// already open: the Swift host carries the request in the `app`
    /// snapshot (`requestedMeetingID`, `requestedSettingsSection`) and the
    /// page follows it (plan `2026-09-29-macos-webview-ui.md`, WP3).
    pub fn publish_request(
        &self,
        window: &WebviewWindow,
        field: &str,
        value: &str,
    ) -> Result<(), BridgeFailure> {
        #[cfg(feature = "fixture-host")]
        if let Some(app) = crate::fixtures::app_snapshot_requesting(field, value) {
            crate::bridge::emit(window, "app", app)?;
        }
        #[cfg(not(feature = "fixture-host"))]
        let _ = (window, field, value);
        Ok(())
    }
}
