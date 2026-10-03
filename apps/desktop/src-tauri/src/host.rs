//! The bridge host: what answers every method that is not the shell's own.
//! With the `fixture-host` feature (the default until WP6) every window gets
//! the recorded snapshots on `page.ready` and commands are answered as
//! `apps/macos/web/src/bridge/mock-transport.ts` answers them. Without it
//! there is no host yet and every command fails, loudly. Snapshots leave
//! through `bridge::emit`, which also ends onboarding on a finished
//! `onboarding` snapshot, so a host never closes a window itself.
//!
//! The method signatures are the host interface WP6 fills in, not what this
//! implementation happens to need, so the lints about them are off here.
#![allow(clippy::unused_self, clippy::unnecessary_wraps)]

use serde_json::Value;
use tauri::WebviewWindow;

use crate::bridge::BridgeError;

pub struct Host;

#[cfg(feature = "fixture-host")]
impl Host {
    /// The page has mounted: publish every topic it may show.
    pub fn page_ready(&self, window: &WebviewWindow) -> Result<(), BridgeError> {
        for (topic, snapshot) in crate::fixtures::snapshots() {
            crate::bridge::emit(window, topic, snapshot)?;
        }
        Ok(())
    }

    /// A command that is not one of the shell's own.
    pub fn call(
        &self,
        _window: &WebviewWindow,
        method: &str,
        _params: Value,
    ) -> Result<Value, BridgeError> {
        Ok(crate::fixtures::reply(method))
    }

    /// `window.open` names a meeting or a section for a window that is
    /// already open: the request rides on the `app` snapshot
    /// (`requestedMeetingID`, `requestedSettingsSection`), and the clean
    /// snapshot goes out right after, as the publish that carried the
    /// request consumes it. The Settings page follows its section; the
    /// main page ignores the meeting, which the host selects itself
    /// (`consumeMeetingRequest`), so with the fixture host a meeting link
    /// changes nothing on screen until `WP6b`'s host does the selecting.
    ///
    /// Swift: `didPublish` in `MainWindowBridge.swift` and
    /// `SettingsBridge.swift`, `MainWindowBridge.consumeMeetingRequest`.
    pub fn publish_request(
        &self,
        window: &WebviewWindow,
        field: &str,
        value: &str,
    ) -> Result<(), BridgeError> {
        for app in crate::fixtures::app_snapshots_requesting(field, value) {
            crate::bridge::emit(window, "app", app)?;
        }
        Ok(())
    }
}

#[cfg(not(feature = "fixture-host"))]
impl Host {
    pub fn page_ready(&self, _window: &WebviewWindow) -> Result<(), BridgeError> {
        Ok(())
    }

    pub fn call(
        &self,
        _window: &WebviewWindow,
        method: &str,
        _params: Value,
    ) -> Result<Value, BridgeError> {
        Err(BridgeError::failed(format!(
            "{method}: no bridge host is wired yet"
        )))
    }

    pub fn publish_request(
        &self,
        _window: &WebviewWindow,
        _field: &str,
        _value: &str,
    ) -> Result<(), BridgeError> {
        Ok(())
    }
}
