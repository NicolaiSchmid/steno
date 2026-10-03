//! The bridge host: what answers every method that is not the shell's own.
//! By default it is [`steno_host::Host`] over the services graph
//! (`steno_services::build`): the real store under the support directory,
//! the pipeline, the recorder and the handover listener (plan: `WP6b`).
//! Snapshots leave through [`crate::bridge::emit`] on every open window,
//! which also ends onboarding on a finished `onboarding` snapshot and moves
//! the tray and the panels on a `recording` one, so a host never closes a
//! window itself. With the opt-in `fixture-host` feature every window gets
//! the recorded snapshots on `page.ready` and commands are answered as
//! `apps/macos/web/src/bridge/mock-transport.ts` answers them, so the UI
//! runs without a database.
//!
//! The host reaches back into the shell through four seams, all wired here:
//! the `Opener` (`ShellOpener`, over `dialogs` and `windows`), the login
//! item (`autostart::ShellLoginItem`), the destructive alert
//! (`dialogs::confirm_destructive`) and the folder chooser, whose panel the
//! shell shows itself before it calls the host (`dialogs.rs`), so the
//! host's `choose_folder` callback answers with the folder the call
//! carries (`ChosenFolder`).
#![cfg_attr(
    feature = "fixture-host",
    allow(clippy::unused_self, clippy::unnecessary_wraps)
)]

use serde_json::Value;
use tauri::{AppHandle, Manager, WebviewWindow};

use crate::bridge::BridgeError;

#[cfg(not(feature = "fixture-host"))]
mod real {
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex, PoisonError};

    use serde_json::Value;
    use steno_bridge::{BridgeEvent, BridgeMethod, BridgeWindow, Dispatcher, EventSink};
    use steno_host::Host;
    use tauri::{AppHandle, Manager, WebviewWindow};

    use crate::bridge::BridgeError;
    use crate::windows::RequestField;

    /// Snapshots go to every open window; each page keeps the topics it
    /// shows, and the panels read the `recording` one.
    pub struct WindowSink {
        pub app: AppHandle,
    }

    impl EventSink for WindowSink {
        fn emit(&self, event: BridgeEvent) {
            for window in self.app.webview_windows().values() {
                if let Err(error) =
                    crate::bridge::emit(window, event.topic.as_str(), event.payload.clone())
                {
                    tracing::debug!(%error, window = window.label(), "snapshot not delivered");
                }
            }
        }
    }

    /// The Finder, the browser and the shell's windows for the host:
    /// `dialogs::reveal`, `dialogs::open_url`, `windows::open` and
    /// `windows::close`. The page's own `system.openURL` and `window.*`
    /// calls are the shell's (`bridge.rs`) and never reach the host, so in
    /// practice the host reveals files (`meeting.revealRecording`,
    /// `meeting.revealExport`, `settings.recording.revealFolder`).
    pub struct ShellOpener {
        pub app: AppHandle,
    }

    impl steno_host::services::Opener for ShellOpener {
        fn reveal(&self, path: &Path) {
            if let Err(error) = crate::dialogs::reveal(&self.app, path) {
                tracing::warn!(%error, "revealing a file failed");
            }
        }

        fn open_url(&self, url: &str) {
            if let Err(error) = crate::dialogs::open_url(&self.app, url) {
                tracing::warn!(%error, "opening a link failed");
            }
        }

        fn open_window(&self, window: BridgeWindow) {
            crate::actions::open(&self.app, window);
        }

        fn close_window(&self, window: BridgeWindow) {
            if let Err(error) = crate::windows::close(&self.app, window) {
                tracing::warn!(%error, %window, "closing a window failed");
            }
        }
    }

    /// The folder a chooser call carries to the host. The shell shows the
    /// panel first and calls the host only with a choice (`dialogs.rs`);
    /// the host's `choose_folder` callback takes it from here. One chooser
    /// call at a time holds `calls`, so two windows choosing at once cannot
    /// swap their folders.
    #[derive(Default)]
    pub struct ChosenFolder {
        calls: Mutex<()>,
        folder: Arc<Mutex<Option<PathBuf>>>,
    }

    impl ChosenFolder {
        /// The host's `choose_folder` callback: the folder of the call in
        /// progress, taken once; none (cancelled) outside a call.
        pub fn callback(&self) -> steno_host::host::ChooseFolder {
            let folder = self.folder.clone();
            Box::new(move |_current| folder.lock().unwrap_or_else(PoisonError::into_inner).take())
        }

        /// Runs `call` with `folder` as the chooser's answer.
        pub fn answering<T>(&self, folder: PathBuf, call: impl FnOnce() -> T) -> T {
            let _one_at_a_time = self.calls.lock().unwrap_or_else(PoisonError::into_inner);
            *self.folder.lock().unwrap_or_else(PoisonError::into_inner) = Some(folder);
            let outcome = call();
            self.folder
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take();
            outcome
        }
    }

    /// The window a command came from, for `Host::for_window`: the
    /// Summaries form answers on the calling window's view model.
    fn bridge_window(label: &str) -> BridgeWindow {
        label.parse().unwrap_or(BridgeWindow::Main)
    }

    pub struct RealHost {
        pub host: Arc<Host>,
        pub app: Arc<steno_services::App>,
        pub chosen: ChosenFolder,
    }

    impl RealHost {
        pub fn page_ready(&self) -> Result<(), BridgeError> {
            use steno_bridge::BridgeHost as _;
            self.host.page_ready()
        }

        pub fn call(
            &self,
            window: &WebviewWindow,
            method: &str,
            params: Value,
        ) -> Result<Value, BridgeError> {
            let parsed: BridgeMethod = method.parse().map_err(|_| {
                BridgeError::unknown_method(format!("The host does not answer {method}."))
            })?;
            let dispatcher = Dispatcher::new(self.host.for_window(bridge_window(window.label())));
            if crate::dialogs::FolderChooser::for_method(method).is_some() {
                let folder = params
                    .get("path")
                    .and_then(Value::as_str)
                    .map(PathBuf::from)
                    .ok_or_else(|| {
                        BridgeError::invalid_params(format!("{method}: no chosen path"))
                    })?;
                return self
                    .chosen
                    .answering(folder, || dispatcher.call(parsed, None))
                    .map(|reply| reply.unwrap_or(Value::Null));
            }
            let params = if params.is_null() { None } else { Some(params) };
            dispatcher
                .call(parsed, params)
                .map(|reply| reply.unwrap_or(Value::Null))
        }

        pub fn publish_request(&self, field: &str, value: &str) -> Result<(), BridgeError> {
            match field.parse::<RequestField>() {
                Ok(RequestField::MeetingId) => {
                    let id = steno_core::json::parse_uuid(value).ok_or_else(|| {
                        BridgeError::invalid_params(format!("Not a UUID: {value}"))
                    })?;
                    self.host.request_meeting(id);
                }
                Ok(RequestField::SettingsSection) => {
                    let section = value.parse().map_err(|_| {
                        BridgeError::invalid_params(format!("Not a settings section: {value}"))
                    })?;
                    self.host.open_settings(section);
                }
                Err(_) => {}
            }
            Ok(())
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn a_chooser_call_answers_with_its_folder_once() {
            let chosen = ChosenFolder::default();
            let choose = chosen.callback();
            assert_eq!(choose(None), None, "no call in progress");
            let seen = chosen.answering(PathBuf::from("/vault"), || {
                (choose(Some(Path::new("/old"))), choose(None))
            });
            assert_eq!(seen, (Some(PathBuf::from("/vault")), None));
            assert_eq!(choose(None), None, "nothing left after the call");
        }

        #[test]
        fn a_command_answers_for_the_window_that_sent_it() {
            for window in BridgeWindow::ALL {
                assert_eq!(bridge_window(window.as_str()), *window);
            }
            assert_eq!(bridge_window("bubble"), BridgeWindow::Main);
        }
    }
}

#[cfg(not(feature = "fixture-host"))]
pub use real::{RealHost, ShellOpener, WindowSink};

/// What the shell manages: the real host, or the fixture answerer.
pub struct Host {
    #[cfg(not(feature = "fixture-host"))]
    inner: RealHost,
}

impl Host {
    /// Builds the services graph on `runtime` and the host over it, with
    /// the shell's opener, login item and dialogs, then attaches the window
    /// sink. The launch sequence (recovery, sweep, handover) runs once the
    /// main window exists, see [`Host::launch`].
    #[cfg(not(feature = "fixture-host"))]
    pub fn real(app: &AppHandle, runtime: &tokio::runtime::Runtime) -> Result<Self, String> {
        use std::sync::Arc;

        let mut options = steno_services::AppOptions::product(
            runtime.handle().clone(),
            Arc::new(ShellOpener { app: app.clone() }),
            app.package_info().version.to_string().as_str(),
        )
        .map_err(|error| error.to_string())?;
        options.login_item = Some(Arc::new(crate::autostart::ShellLoginItem {
            app: app.clone(),
        }));
        let graph = runtime
            .block_on(async { steno_services::build(options) })
            .map_err(|error| error.to_string())?;
        for warning in &graph.startup_warnings {
            tracing::warn!("{warning}");
        }
        let chosen = real::ChosenFolder::default();
        let confirm_app = app.clone();
        let host = graph
            .host()
            .map_err(|error| error.to_string())?
            .with_dialogs(
                Box::new(move |prompt| crate::dialogs::confirm_destructive(&confirm_app, prompt)),
                chosen.callback(),
            );
        let host = Arc::new(host);
        host.attach(Arc::new(WindowSink { app: app.clone() }));
        Ok(Host {
            inner: RealHost {
                host,
                app: Arc::new(graph),
                chosen,
            },
        })
    }

    #[cfg(feature = "fixture-host")]
    pub fn fixtures() -> Self {
        Host {}
    }

    /// Whether onboarding should open at launch.
    pub fn should_open_onboarding(&self) -> bool {
        #[cfg(not(feature = "fixture-host"))]
        {
            self.inner.host.should_open_onboarding()
        }
        #[cfg(feature = "fixture-host")]
        {
            false
        }
    }

    /// The onboarding window is gone, closed by the user or by Finish:
    /// the host counts the pages as seen, as the Swift window's
    /// `onDisappear` did.
    pub fn onboarding_window_closed(&self) {
        #[cfg(not(feature = "fixture-host"))]
        self.inner.host.onboarding_window_closed();
    }

    /// The meeting being recorded, which an exit has to save first; never
    /// one for the fixtures.
    pub fn recording(&self) -> Option<uuid::Uuid> {
        #[cfg(not(feature = "fixture-host"))]
        {
            self.inner.app.recording()
        }
        #[cfg(feature = "fixture-host")]
        {
            None
        }
    }

    /// Stops and saves a recording in progress before the process exits
    /// (`steno_services::App::shutdown`); a no-op for the fixtures.
    pub fn shutdown_action(&self) -> impl FnOnce() + Send + 'static {
        #[cfg(not(feature = "fixture-host"))]
        {
            let app = self.inner.app.clone();
            move || app.shutdown()
        }
        #[cfg(feature = "fixture-host")]
        {
            || {}
        }
    }

    /// The launch sequence on the runtime; a no-op for the fixtures.
    pub fn launch(&self, runtime: &tokio::runtime::Runtime) {
        #[cfg(not(feature = "fixture-host"))]
        {
            let _guard = runtime.enter();
            self.inner.app.launch(&self.inner.host);
        }
        #[cfg(feature = "fixture-host")]
        {
            let _ = runtime;
        }
    }

    /// The page has mounted: publish every topic it may show.
    pub fn page_ready(&self, window: &WebviewWindow) -> Result<(), BridgeError> {
        #[cfg(not(feature = "fixture-host"))]
        {
            let _ = window;
            self.inner.page_ready()
        }
        #[cfg(feature = "fixture-host")]
        {
            for (topic, snapshot) in crate::fixtures::snapshots() {
                crate::bridge::emit(window, topic, snapshot)?;
            }
            Ok(())
        }
    }

    /// A command that is not one of the shell's own.
    pub fn call(
        &self,
        window: &WebviewWindow,
        method: &str,
        params: Value,
    ) -> Result<Value, BridgeError> {
        #[cfg(not(feature = "fixture-host"))]
        {
            self.inner.call(window, method, params)
        }
        #[cfg(feature = "fixture-host")]
        {
            let _ = (window, params);
            Ok(crate::fixtures::reply(method))
        }
    }

    /// `window.open` names a meeting or a section for a window that is
    /// already open: the request rides on the `app` snapshot
    /// (`requestedMeetingID`, `requestedSettingsSection`), and the clean
    /// snapshot goes out right after, as the publish that carried the
    /// request consumes it. The real host also selects the meeting
    /// (`Host::request_meeting`); the fixture host only publishes the
    /// request, so a meeting link changes nothing on screen there.
    ///
    /// Swift: `didPublish` in `MainWindowBridge.swift` and
    /// `SettingsBridge.swift`, `MainWindowBridge.consumeMeetingRequest`.
    pub fn publish_request(
        &self,
        window: &WebviewWindow,
        field: &str,
        value: &str,
    ) -> Result<(), BridgeError> {
        #[cfg(not(feature = "fixture-host"))]
        {
            let _ = window;
            self.inner.publish_request(field, value)
        }
        #[cfg(feature = "fixture-host")]
        {
            for app in crate::fixtures::app_snapshots_requesting(field, value) {
                crate::bridge::emit(window, "app", app)?;
            }
            Ok(())
        }
    }
}

/// The host the shell manages, from any handle.
pub fn host(app: &AppHandle) -> tauri::State<'_, Host> {
    app.state::<Host>()
}
