//! The bridge host: what answers every method that is not the shell's own.
//! By default it is [`steno_host::Host`] over the services graph
//! (`steno_services::build`): the real store under the support directory,
//! the pipeline, the recorder and the handover listener (plan: `WP6b`).
//! Snapshots leave through [`crate::bridge::emit`] on every open window,
//! which also ends onboarding on a finished `onboarding` snapshot and moves
//! the tray and the panels on a `recording` one, so a host never closes a
//! window itself. They go out from the main thread (`WindowSink`): the
//! host emits under its `publishing` lock, the main thread can be waiting
//! for a thread that holds it (a Stop from the tray joins the recorder's
//! level thread, which publishes), and the tray's setters wait for the
//! main thread when called from another. With the opt-in `fixture-host`
//! feature every window gets the recorded snapshots on `page.ready` and
//! commands are answered as `apps/macos/web/src/bridge/mock-transport.ts`
//! answers them, so the UI runs without a database.
//!
//! The host reaches back into the shell through five seams, all wired here:
//! the `Opener` (`ShellOpener`, over `dialogs` and `windows`), the login
//! item (`autostart::ShellLoginItem`), the update source the services'
//! update schedule drives (`updater::ShellUpdates`), the destructive alert
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
    use std::collections::VecDeque;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex, PoisonError};

    use serde_json::Value;
    use steno_bridge::{BridgeEvent, BridgeMethod, BridgeWindow, Dispatcher, EventSink};
    use steno_host::Host;
    use tauri::{AppHandle, Manager, WebviewWindow};

    use crate::bridge::BridgeError;
    use crate::windows::RequestField;

    /// What [`WindowSink`] needs of the main thread: a task posted to it
    /// runs there, at once when posted from it and in posting order
    /// otherwise; `deliver` hands a snapshot to every open window, and
    /// runs only there.
    pub trait MainThread: Clone + Send + Sync + 'static {
        fn post(&self, task: Box<dyn FnOnce() + Send>);
        fn deliver(&self, event: BridgeEvent);
    }

    impl MainThread for AppHandle {
        fn post(&self, task: Box<dyn FnOnce() + Send>) {
            if let Err(error) = self.run_on_main_thread(task) {
                tracing::debug!(%error, "snapshot not delivered: the run loop has ended");
            }
        }

        fn deliver(&self, event: BridgeEvent) {
            for window in self.webview_windows().values() {
                if let Err(error) =
                    crate::bridge::emit(window, event.topic.as_str(), event.payload.clone())
                {
                    tracing::debug!(%error, window = window.label(), "snapshot not delivered");
                }
            }
        }
    }

    /// Snapshots go to every open window, from the main thread; each page
    /// keeps the topics it shows, and the tray and the panels follow the
    /// `recording` one (`bridge::emit`). `emit` queues the snapshot and
    /// returns without waiting: the host emits under its `publishing` lock
    /// (from a command, its flush thread or the recorder's level thread),
    /// the main thread can be waiting for a thread that holds it (a Stop
    /// from the tray joins the recorder's level thread, which publishes),
    /// and the tray's setters wait for the main thread when called from
    /// another. One queue keeps the snapshots in emit order whichever
    /// thread emits them, also when an emit on the main thread runs
    /// before a task posted earlier, and one drain at a time empties it, so
    /// every window gets them in that order.
    ///
    /// Swift: `WebBridge.emit`, `@MainActor`
    /// (`apps/macos/Steno/Web/WebBridge.swift`).
    pub struct WindowSink<M = AppHandle> {
        main: M,
        queue: Arc<Mutex<VecDeque<BridgeEvent>>>,
        /// Set while a drain delivers, on the main thread.
        draining: Arc<AtomicBool>,
    }

    impl<M: MainThread> WindowSink<M> {
        pub fn new(main: M) -> Self {
            WindowSink {
                main,
                queue: Arc::default(),
                draining: Arc::default(),
            }
        }
    }

    impl<M: MainThread> EventSink for WindowSink<M> {
        fn emit(&self, event: BridgeEvent) {
            lock_queue(&self.queue).push_back(event);
            let (main, pending, draining) =
                (self.main.clone(), self.queue.clone(), self.draining.clone());
            self.main.post(Box::new(move || {
                // An emit from inside a delivery leaves its snapshot to the
                // drain under way, which delivers it once every window has
                // the one before.
                let Some(_draining) = Draining::begin(&draining) else {
                    return;
                };
                // The lock is released before each delivery: one may emit
                // again on this thread.
                loop {
                    let next = lock_queue(&pending).pop_front();
                    let Some(event) = next else { break };
                    main.deliver(event);
                }
            }));
        }
    }

    /// The drain under way; it ends when this is dropped.
    struct Draining<'a>(&'a AtomicBool);

    impl<'a> Draining<'a> {
        /// None when a drain is already under way.
        fn begin(flag: &'a AtomicBool) -> Option<Self> {
            (!flag.swap(true, Ordering::Acquire)).then_some(Draining(flag))
        }
    }

    impl Drop for Draining<'_> {
        fn drop(&mut self) {
            self.0.store(false, Ordering::Release);
        }
    }

    fn lock_queue(
        queue: &Mutex<VecDeque<BridgeEvent>>,
    ) -> std::sync::MutexGuard<'_, VecDeque<BridgeEvent>> {
        queue.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The Finder, the browser and the shell's windows for the host:
    /// `dialogs::reveal`, `dialogs::open_url`, `windows::open` and
    /// `windows::close`. The page's own `system.openURL` and `window.*`
    /// calls are the shell's (`bridge.rs`) and never reach the host, so in
    /// practice the host reveals files (`meeting.revealRecording`,
    /// `meeting.revealExport`, `settings.recording.revealFolder`). A
    /// failure warns without its text, which can name the path or the URL
    /// (a folder named after a meeting); the text goes to debug.
    ///
    /// Swift: `NSWorkspace.activateFileViewerSelecting` and
    /// `NSWorkspace.open`.
    pub struct ShellOpener {
        pub app: AppHandle,
    }

    impl steno_host::services::Opener for ShellOpener {
        fn reveal(&self, path: &Path) {
            if let Err(error) = crate::dialogs::reveal(&self.app, path) {
                tracing::warn!("revealing a file failed");
                tracing::debug!(%error, "revealing a file failed");
            }
        }

        fn open_url(&self, url: &str) {
            if let Err(error) = crate::dialogs::open_url(&self.app, url) {
                tracing::warn!("opening a link failed");
                tracing::debug!(%error, "opening a link failed");
            }
        }

        fn open_window(&self, window: BridgeWindow) {
            crate::actions::open(&self.app, window);
        }

        fn close_window(&self, window: BridgeWindow) {
            if let Err(error) = crate::windows::close(&self.app, window) {
                tracing::warn!(%window, "closing a window failed");
                tracing::debug!(%error, %window, "closing a window failed");
            }
        }
    }

    /// The folder a chooser call carries to the host. The shell shows the
    /// panel first and calls the host only with a choice (`dialogs.rs`);
    /// the host's `choose_folder` callback takes it from here. One chooser
    /// call at a time holds `calls`, so two windows choosing at once cannot
    /// swap their folders.
    ///
    /// Swift: the `NSOpenPanel` inside `chooseFolder` (`SettingsBridge.swift`).
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

    /// The detection prompt's panel (`panels::set_prompt`): what the
    /// host's detection controller raises and clears.
    pub struct ShellPromptPanel {
        pub app: AppHandle,
    }

    impl steno_services::detection::PromptPanel for ShellPromptPanel {
        fn show(&self, prompt: Option<&steno_services::detection::DetectionPrompt>) {
            crate::panels::set_prompt(
                &self.app,
                prompt.map(|prompt| crate::panels::RaisedPrompt {
                    request: crate::panels::PromptRequest {
                        app_name: prompt.app_name.clone(),
                        seconds: prompt.seconds,
                    },
                    raised: prompt.number,
                }),
            );
        }
    }

    /// The name the prompt gives the app with `bundle_id`: on the Mac the
    /// running app's localized name, as Swift's `liveAppName` read the
    /// bundle's name; elsewhere, and for an app the Mac does not list,
    /// `steno_services::detection::fallback_app_name`.
    pub fn app_name(bundle_id: Option<&str>) -> String {
        #[cfg(target_os = "macos")]
        if let Some(name) = bundle_id.and_then(running_app_name) {
            return name;
        }
        steno_services::detection::fallback_app_name(bundle_id)
    }

    #[cfg(target_os = "macos")]
    fn running_app_name(bundle_id: &str) -> Option<String> {
        use objc2_app_kit::NSRunningApplication;
        use objc2_foundation::NSString;

        let running = NSRunningApplication::runningApplicationsWithBundleIdentifier(
            &NSString::from_str(bundle_id),
        );
        let name = running.firstObject()?.localizedName()?.to_string();
        (!name.trim().is_empty()).then_some(name)
    }

    /// The window a command came from, for `Host::for_window`: the
    /// Summaries form answers on the calling window's view model. A
    /// panel's label, or any other, answers on main.
    fn bridge_window(label: &str) -> BridgeWindow {
        label.parse().unwrap_or(BridgeWindow::Main)
    }

    /// The services graph and the host over it, with the chosen folder the
    /// folder commands answer through.
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

        type Task = Box<dyn FnOnce() + Send>;

        /// A main thread the test drives: a task posted from it runs at
        /// once, one posted from elsewhere waits its turn; `deliver` notes
        /// each topic and whether it ran on that thread.
        #[derive(Clone)]
        struct TestMain {
            id: std::thread::ThreadId,
            tasks: std::sync::mpsc::Sender<Task>,
            delivered: Arc<Mutex<Vec<(steno_bridge::BridgeTopic, bool)>>>,
        }

        impl MainThread for TestMain {
            fn post(&self, task: Task) {
                if std::thread::current().id() == self.id {
                    task();
                } else {
                    self.tasks.send(task).unwrap();
                }
            }

            fn deliver(&self, event: BridgeEvent) {
                let on_main = std::thread::current().id() == self.id;
                self.delivered.lock().unwrap().push((event.topic, on_main));
            }
        }

        /// The main thread, running what is posted to it until every
        /// handle is gone.
        fn test_main() -> (TestMain, std::thread::JoinHandle<()>) {
            let (tasks, posted) = std::sync::mpsc::channel::<Task>();
            let thread = std::thread::spawn(move || {
                while let Ok(task) = posted.recv() {
                    task();
                }
            });
            let main = TestMain {
                id: thread.thread().id(),
                tasks,
                delivered: Arc::default(),
            };
            (main, thread)
        }

        fn event(topic: steno_bridge::BridgeTopic) -> BridgeEvent {
            BridgeEvent::new(topic, Value::Null)
        }

        /// Runs a test's `body` on a thread of its own and fails the test
        /// when it has not ended within five seconds, so a drain that
        /// deadlocks (or a main thread that never ends) fails rather than
        /// hangs.
        fn bounded(body: impl FnOnce() + Send + 'static) {
            let (done, finished) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                body();
                let _ = done.send(());
            });
            match finished.recv_timeout(std::time::Duration::from_secs(5)) {
                Ok(()) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => panic!("the test hung"),
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    panic!("the test panicked")
                }
            }
        }

        /// The tray's Stop: the main thread waits for a thread that holds
        /// the `publishing` lock (it joins the level thread) while the flush
        /// thread emits under it. The emit returns without the main thread,
        /// and the snapshot is delivered there once the lock is free; an
        /// emit that delivered on its own thread would have waited for the
        /// main thread inside the tray's setters, under the lock.
        #[test]
        fn a_snapshot_is_delivered_on_the_main_thread_and_the_emit_never_waits_for_it() {
            bounded(|| {
                let (main, thread) = test_main();
                let sink = WindowSink::new(main.clone());
                let publishing = Arc::new(Mutex::new(()));
                let held = publishing.lock().unwrap();
                let (blocked, main_blocked) = std::sync::mpsc::channel();
                let waiting = publishing.clone();
                main.tasks
                    .send(Box::new(move || {
                        blocked.send(()).unwrap();
                        drop(waiting.lock().unwrap());
                    }))
                    .unwrap();
                main_blocked
                    .recv_timeout(std::time::Duration::from_secs(5))
                    .unwrap();
                sink.emit(event(steno_bridge::BridgeTopic::Recording));
                assert!(
                    main.delivered.lock().unwrap().is_empty(),
                    "nothing is delivered while the main thread is busy"
                );
                drop(held);
                let delivered = main.delivered.clone();
                drop((sink, main));
                thread.join().unwrap();
                assert_eq!(
                    *delivered.lock().unwrap(),
                    [(steno_bridge::BridgeTopic::Recording, true)]
                );
            });
        }

        /// A snapshot emitted on the main thread is delivered at once, but
        /// never ahead of one emitted earlier from another thread whose
        /// task has not run yet.
        #[test]
        fn snapshots_are_delivered_in_emit_order_from_any_thread() {
            use steno_bridge::BridgeTopic;
            bounded(|| {
                let (main, thread) = test_main();
                let sink = Arc::new(WindowSink::new(main.clone()));
                let (go, released) = std::sync::mpsc::channel::<()>();
                let on_main = sink.clone();
                main.tasks
                    .send(Box::new(move || {
                        released.recv().unwrap();
                        on_main.emit(event(BridgeTopic::App));
                    }))
                    .unwrap();
                sink.emit(event(BridgeTopic::Recording));
                go.send(()).unwrap();
                let delivered = main.delivered.clone();
                drop((sink, main));
                thread.join().unwrap();
                assert_eq!(
                    *delivered.lock().unwrap(),
                    [(BridgeTopic::Recording, true), (BridgeTopic::App, true)]
                );
            });
        }

        /// A main thread that runs every task where it is posted, the
        /// test's own, over two windows; delivering `App` to the first one
        /// emits `Recording` there, as a delivery that led to a publish
        /// would.
        #[derive(Clone, Default)]
        struct NestingMain {
            sink: Arc<std::sync::OnceLock<Arc<WindowSink<NestingMain>>>>,
            delivered: Arc<Mutex<Vec<(&'static str, steno_bridge::BridgeTopic)>>>,
        }

        impl MainThread for NestingMain {
            fn post(&self, task: Task) {
                task();
            }

            fn deliver(&self, event: BridgeEvent) {
                for window in ["main", "settings"] {
                    self.delivered.lock().unwrap().push((window, event.topic));
                    if window == "main" && event.topic == steno_bridge::BridgeTopic::App {
                        self.sink
                            .get()
                            .unwrap()
                            .emit(self::event(steno_bridge::BridgeTopic::Recording));
                    }
                }
            }
        }

        /// A snapshot emitted while one is being delivered reaches every
        /// window after that one: the inner drain leaves it to the outer.
        /// Once that drain ended, the next emit drains again.
        #[test]
        fn a_snapshot_emitted_during_a_delivery_reaches_every_window_after_it() {
            use steno_bridge::BridgeTopic;
            bounded(|| {
                let main = NestingMain::default();
                let sink = Arc::new(WindowSink::new(main.clone()));
                assert!(main.sink.set(sink.clone()).is_ok());
                sink.emit(event(BridgeTopic::App));
                sink.emit(event(BridgeTopic::Progress));
                assert_eq!(
                    *main.delivered.lock().unwrap(),
                    [
                        ("main", BridgeTopic::App),
                        ("settings", BridgeTopic::App),
                        ("main", BridgeTopic::Recording),
                        ("settings", BridgeTopic::Recording),
                        ("main", BridgeTopic::Progress),
                        ("settings", BridgeTopic::Progress),
                    ]
                );
            });
        }

        #[test]
        fn a_command_answers_for_its_window_and_one_from_a_panel_for_main() {
            for window in BridgeWindow::ALL {
                assert_eq!(bridge_window(window.as_str()), *window);
            }
            assert_eq!(bridge_window("bubble"), BridgeWindow::Main);
        }
    }
}

#[cfg(not(feature = "fixture-host"))]
pub use real::{RealHost, ShellOpener, ShellPromptPanel, WindowSink, app_name};

/// Why the real host could not be built.
#[cfg(not(feature = "fixture-host"))]
#[derive(Debug, thiserror::Error)]
pub enum ShellHostError {
    #[error("Could not create the support directory: {0}")]
    SupportDirectory(#[source] std::io::Error),
    #[error(transparent)]
    Build(#[from] steno_services::BuildError),
    #[error(transparent)]
    Host(#[from] steno_host::host::HostError),
}

#[cfg(not(feature = "fixture-host"))]
impl ShellHostError {
    /// Whether another process holds the database (`steno_core::DatabaseLock`):
    /// another Steno app, or a `steno` command.
    pub fn is_database_held(&self) -> bool {
        matches!(
            self,
            Self::Build(steno_services::BuildError::Lock(
                steno_core::DatabaseLockError::Held(_)
            ))
        )
    }
}

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
    pub fn real(
        app: &AppHandle,
        runtime: &tokio::runtime::Runtime,
    ) -> Result<Self, ShellHostError> {
        use std::sync::Arc;

        let mut options = steno_services::AppOptions::product(
            runtime.handle().clone(),
            Arc::new(ShellOpener { app: app.clone() }),
            app.package_info().version.to_string().as_str(),
        )
        .map_err(ShellHostError::SupportDirectory)?;
        options.login_item = Some(Arc::new(crate::autostart::ShellLoginItem {
            app: app.clone(),
        }));
        // A smoke run checks for no update, so it neither reaches the
        // network nor raises the update alert over the windows it shows,
        // and detects no meeting, so no prompt comes up over them either.
        if std::env::var_os(crate::smoke::SECONDS_VARIABLE).is_none() {
            options.update_source = Some(
                app.state::<Arc<crate::updater::ShellUpdates>>()
                    .inner()
                    .clone(),
            );
            options.detection = Some(steno_services::detection::DetectionOptions::live(
                Arc::new(ShellPromptPanel { app: app.clone() }),
                Arc::new(app_name),
            ));
        }
        let graph = runtime.block_on(async { steno_services::build(options) })?;
        for warning in &graph.startup_warnings {
            tracing::warn!("{warning}");
        }
        let chosen = real::ChosenFolder::default();
        let confirm_app = app.clone();
        let host = graph.host()?.with_dialogs(
            Box::new(move |prompt| crate::dialogs::confirm_destructive(&confirm_app, prompt)),
            chosen.callback(),
        );
        let host = Arc::new(host);
        host.attach(Arc::new(WindowSink::new(app.clone())));
        Ok(Host {
            inner: RealHost {
                host,
                app: Arc::new(graph),
                chosen,
            },
        })
    }

    /// The fixture answerer (`fixture-host`).
    #[cfg(feature = "fixture-host")]
    pub fn fixtures() -> Self {
        Host {}
    }

    /// The update schedule, which records every check; `None` for the
    /// fixtures and in a smoke run.
    pub fn updates(&self) -> Option<std::sync::Arc<steno_services::updates::UpdateSchedule>> {
        #[cfg(not(feature = "fixture-host"))]
        {
            self.inner.app.updates.clone()
        }
        #[cfg(feature = "fixture-host")]
        {
            None
        }
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

    /// The X of prompt `raised` took it down: the detection controller
    /// hears of it
    /// (`steno_services::detection::DetectionController::dismissed`); a
    /// no-op for the fixtures and without detection.
    #[cfg_attr(feature = "fixture-host", allow(unused_variables))]
    pub fn prompt_dismissed(&self, raised: Option<u64>) {
        #[cfg(not(feature = "fixture-host"))]
        if let Some(detection) = &self.inner.app.detection {
            detection.dismissed(raised);
        }
    }

    /// The Record of prompt `raised`, the prompt taken down: the detection
    /// controller starts a call recording for the app it named
    /// (`DetectionController::record`). Blocks for the start; a no-op for
    /// the fixtures and without detection.
    #[cfg_attr(feature = "fixture-host", allow(unused_variables))]
    pub fn record_from_prompt(&self, raised: Option<u64>) {
        #[cfg(not(feature = "fixture-host"))]
        if let Some(detection) = &self.inner.app.detection {
            detection.record(raised);
        }
    }

    /// The onboarding window is gone, closed by the user or by Finish:
    /// the host counts the pages as seen, as the Swift window's
    /// `onDisappear` did.
    pub fn onboarding_window_closed(&self) {
        #[cfg(not(feature = "fixture-host"))]
        self.inner.host.onboarding_window_closed();
    }

    /// What every exit runs first (`steno_services::App::shutdown`): the
    /// pipelines quit, a start or a stop settles, a recording in progress
    /// is stopped and saved, the handover listener stops; a no-op for the
    /// fixtures.
    pub fn shutdown_action(&self) -> impl FnOnce() + Send + 'static + use<> {
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

    /// Quits the pipelines (`steno_services::pipeline::CurrentPipeline::quit`)
    /// ahead of the shutdown, which quits them again: an exit signal calls
    /// it before its request waits for the main thread. A no-op for the
    /// fixtures. Rust only: Swift had no signal handler.
    #[cfg(unix)]
    pub fn quit_pipeline(&self) {
        #[cfg(not(feature = "fixture-host"))]
        self.inner.app.pipeline.quit();
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

/// Whether the host is managed: false before `setup` built it, and for good
/// in an app that refused to start (`refuse_to_start` in `main.rs`).
pub fn is_running(app: &AppHandle) -> bool {
    app.try_state::<Host>().is_some()
}

#[cfg(all(test, not(feature = "fixture-host")))]
mod refusal_tests {
    use super::ShellHostError;

    /// Only a database another process holds makes the shell refuse to
    /// start; any other build failure stays an error.
    #[test]
    fn only_a_held_database_refuses_the_start() {
        let held = ShellHostError::Build(steno_services::BuildError::Lock(
            steno_core::DatabaseLockError::Held("/support/steno.lock".into()),
        ));
        assert!(held.is_database_held());
        let unreadable = ShellHostError::Build(steno_services::BuildError::Lock(
            steno_core::DatabaseLockError::Io {
                path: "/support/steno.lock".into(),
                source: std::io::Error::other("read-only"),
            },
        ));
        assert!(!unreadable.is_database_held());
        assert!(
            !ShellHostError::SupportDirectory(std::io::Error::other("full")).is_database_held()
        );
    }
}
