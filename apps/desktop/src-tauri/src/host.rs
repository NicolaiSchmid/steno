//! The bridge host: what answers every method that is not the shell's own,
//! [`steno_host::Host`] over the services graph, or the recorded fixtures
//! with the `fixture-host` feature. Every call into the host can wait, so
//! none runs on the main thread: commands go to Tauri's blocking pool
//! ([`call_blocking`]), callers with no one to answer to one ordered lane
//! ([`call_in_order`]).
#![cfg_attr(
    feature = "fixture-host",
    allow(clippy::unused_self, clippy::unnecessary_wraps)
)]

use std::sync::mpsc;

use serde_json::Value;
use tauri::{AppHandle, Manager, WebviewWindow};

use crate::bridge::BridgeError;

/// A job for the [`Lane`].
pub type Job = Box<dyn FnOnce() + Send>;

/// The host as the shell reaches it off the main thread: the app's managed
/// [`Host`], or a test's fake. The calls can wait, so each runs inside
/// [`call_blocking`] or [`call_in_order`]; `refuse_starts`,
/// `note_quitting`, `note_login_item` and `queue` never wait.
pub trait ShellHost: Clone + Send + Sync + 'static {
    type Window: Clone + Send + Sync + 'static;

    /// The page has mounted ([`Host::page_ready`]).
    fn page_ready(&self, window: &Self::Window) -> Result<(), BridgeError>;

    fn call(
        &self,
        window: &Self::Window,
        method: &str,
        params: Value,
    ) -> Result<Value, BridgeError>;

    /// `window.open` names a meeting or a section ([`Host::publish_request`]).
    fn publish_request(
        &self,
        window: &Self::Window,
        field: &str,
        value: &str,
    ) -> Result<(), BridgeError>;

    fn onboarding_window_closed(&self);

    fn login_item_on(&self) -> bool;

    fn switch_login_item(&self, value: bool) -> Result<(), BridgeError>;

    /// The tray's check mark follows the login item, `on` as the caller
    /// read it; posted to the main thread, never waited for. Two switches
    /// on different threads may post out of order, leaving the mark stale
    /// until the next change.
    fn note_login_item(&self, on: bool);

    /// The menus' Quit, on the main thread: every recording start is
    /// refused from now on. Not on the lane, where it would wait behind the
    /// Records clicked before the Quit, which it must refuse.
    fn refuse_starts(&self);

    /// The menus' Quit, on the main thread: the tray reads quitting.
    fn note_quitting(&self);

    /// Queues `job` on the shell's ordered lane, as `what` in the log;
    /// returns at once.
    fn queue(&self, what: &'static str, job: Job);
}

impl ShellHost for AppHandle {
    type Window = WebviewWindow;

    fn page_ready(&self, window: &WebviewWindow) -> Result<(), BridgeError> {
        host(self).page_ready(window)?;
        crate::windows::publish_owed(
            self,
            &self.state::<crate::windows::Pages>(),
            window.label(),
            window.clone(),
        );
        Ok(())
    }

    fn call(
        &self,
        window: &WebviewWindow,
        method: &str,
        params: Value,
    ) -> Result<Value, BridgeError> {
        host(self).call(window, method, params)
    }

    fn publish_request(
        &self,
        window: &WebviewWindow,
        field: &str,
        value: &str,
    ) -> Result<(), BridgeError> {
        host(self).publish_request(window, field, value)
    }

    fn onboarding_window_closed(&self) {
        host(self).onboarding_window_closed();
    }

    fn login_item_on(&self) -> bool {
        crate::autostart::status(self).is_on()
    }

    fn switch_login_item(&self, value: bool) -> Result<(), BridgeError> {
        crate::autostart::set_enabled(self, value)
    }

    fn note_login_item(&self, on: bool) {
        let app = self.clone();
        if let Err(error) = self.run_on_main_thread(move || crate::tray::note_login_item(&app, on))
        {
            tracing::debug!(%error, "the tray's check mark not moved: the run loop has ended");
        }
    }

    fn refuse_starts(&self) {
        if let Some(host) = self.try_state::<Host>() {
            host.refuse_starts();
        }
    }

    fn note_quitting(&self) {
        crate::tray::note_quitting(self);
    }

    fn queue(&self, what: &'static str, job: Job) {
        self.state::<Lane>().queue(what, job);
    }
}

/// Runs `body` with `host` on Tauri's blocking pool and resolves to what it
/// returned, so the caller's thread never waits on the host. A panic
/// resumes in the awaiting task; a pool already gone answers `failed`.
pub async fn call_blocking<H, T>(
    host: H,
    body: impl FnOnce(&H) -> Result<T, BridgeError> + Send + 'static,
) -> Result<T, BridgeError>
where
    H: Send + 'static,
    T: Send + 'static,
{
    match tauri::async_runtime::spawn_blocking(move || body(&host)).await {
        Ok(outcome) => outcome,
        Err(tauri::Error::JoinError(error)) if error.is_panic() => {
            std::panic::resume_unwind(error.into_panic())
        }
        Err(error) => Err(crate::bridge::failed(error)),
    }
}

/// Queues `body` with `host` on the shell's ordered lane and returns at
/// once, for a caller with no one to answer: the tray's actions, a window
/// request and the onboarding window's close. They reach the host in the
/// order queued, so a Record clicked during a Stop's save runs after it. A
/// failure is logged as `what`.
pub fn call_in_order<H: ShellHost>(
    host: H,
    what: &'static str,
    body: impl FnOnce(&H) -> Result<(), BridgeError> + Send + 'static,
) {
    let lane = host.clone();
    lane.queue(
        what,
        Box::new(move || {
            if let Err(error) = body(&host) {
                stderr_line!("[steno-desktop] {what}: {error}");
            }
        }),
    );
}

/// One thread of the shell's own that runs its jobs in the order queued,
/// which Tauri's pools cannot guarantee. A job that panics is logged at
/// error as its `what` alone (the payload can name a path), and the next
/// job runs. The thread ends when the lane is dropped; the exit never
/// joins it.
pub struct Lane {
    jobs: mpsc::Sender<(&'static str, Job)>,
}

impl Lane {
    pub fn start() -> Self {
        let (jobs, queued) = mpsc::channel::<(&'static str, Job)>();
        std::thread::Builder::new()
            .name("steno-shell-lane".to_owned())
            .spawn(move || {
                for (what, job) in queued {
                    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(job)).is_err() {
                        tracing::error!(what, "a call the shell queued panicked");
                    }
                }
            })
            .expect("the shell's ordered lane");
        Lane { jobs }
    }

    /// Runs `job` after every job queued before it; returns at once.
    pub fn queue(&self, what: &'static str, job: Job) {
        if self.jobs.send((what, job)).is_err() {
            tracing::debug!(what, "not run: the shell's lane has ended");
        }
    }
}

#[cfg(not(feature = "fixture-host"))]
pub(crate) mod real {
    use std::collections::VecDeque;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex, PoisonError};

    use serde_json::Value;
    use steno_bridge::{BridgeEvent, BridgeMethod, BridgeWindow, Dispatcher, EventSink};
    use steno_host::Host;
    use tauri::{AppHandle, Manager};

    use crate::bridge::BridgeError;
    use crate::windows::RequestField;

    /// What [`WindowSink`] needs of the main thread: a task posted runs
    /// there, at once when posted from it and in posting order otherwise;
    /// `deliver` runs only there.
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

    /// Snapshots go to every open window, from the main thread. `emit`
    /// queues and returns without waiting: the host emits under its
    /// `publishing` lock and the tray's setters wait for the main thread, so
    /// delivering on the emitting thread would hold the lock until the main
    /// thread is free. One queue keeps emit order across threads, and one
    /// drain at a time empties it.
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
                // drain under way.
                let Some(_draining) = Draining::begin(&draining) else {
                    return;
                };
                // Released before each delivery: one may emit again.
                loop {
                    let next = lock_queue(&pending).pop_front();
                    let Some(event) = next else { break };
                    main.deliver(event);
                }
            }));
        }
    }

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

    /// The Finder, the browser and the shell's windows for the host. A
    /// failure warns without its text, which can name a path or a URL; the
    /// text goes to debug.
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

    /// The folder a chooser call carries to the host: the shell shows the
    /// panel first and calls the host only with a choice. One chooser call
    /// at a time holds `calls`, so two windows cannot swap their folders.
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

    /// The detection prompt's panel, which the detection controller raises
    /// and clears.
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

    /// The name the prompt gives the app with `bundle_id`: the running
    /// app's localized name on the Mac, else the services' fallback.
    pub fn app_name(bundle_id: Option<&str>) -> String {
        #[cfg(target_os = "macos")]
        if let Some(name) = bundle_id.and_then(running_app_name) {
            return name;
        }
        steno_services::detection::fallback_app_name(bundle_id)
    }

    /// In a pool of its own: the detector's events thread has none, so what
    /// `AppKit` autoreleases would stay until the thread ends.
    #[cfg(target_os = "macos")]
    fn running_app_name(bundle_id: &str) -> Option<String> {
        use objc2_app_kit::NSRunningApplication;
        use objc2_foundation::NSString;

        objc2::rc::autoreleasepool(|_| {
            let running = NSRunningApplication::runningApplicationsWithBundleIdentifier(
                &NSString::from_str(bundle_id),
            );
            let name = running.firstObject()?.localizedName()?.to_string();
            (!name.trim().is_empty()).then_some(name)
        })
    }

    /// The window a command came from, for `Host::for_window`; a panel's
    /// label, or any other, answers on main.
    fn bridge_window(label: &str) -> BridgeWindow {
        label.parse().unwrap_or(BridgeWindow::Main)
    }

    pub struct RealHost {
        pub host: Arc<Host>,
        pub app: Arc<steno_services::App>,
        pub chosen: ChosenFolder,
    }

    impl RealHost {
        /// The onboarding window's ready page starts its vault check.
        pub fn page_ready(&self, label: &str) -> Result<(), BridgeError> {
            use steno_bridge::BridgeHost as _;
            self.host.for_window(bridge_window(label)).page_ready()
        }

        pub fn call(&self, label: &str, method: &str, params: Value) -> Result<Value, BridgeError> {
            let parsed: BridgeMethod = method.parse().map_err(|_| {
                BridgeError::unknown_method(format!("The host does not answer {method}."))
            })?;
            let dispatcher = Dispatcher::new(self.host.for_window(bridge_window(label)));
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
        /// once, one posted from elsewhere waits its turn.
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

        /// Runs `body` on a thread of its own and fails the test after five
        /// seconds, so a deadlock fails rather than hangs.
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

        /// The tray's Stop: an emit while the main thread is blocked returns
        /// at once and is delivered once the main thread is free.
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

        /// A snapshot emitted on the main thread never overtakes one emitted
        /// earlier from another thread.
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

        /// A main thread that runs every task where it is posted; delivering
        /// `App` to the first window emits `Recording` there.
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

        /// A snapshot emitted during a delivery reaches every window after
        /// that one; the next emit drains again.
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
    /// Whether another process (Steno or a `steno` command) holds the
    /// database.
    pub fn is_database_held(&self) -> bool {
        matches!(
            self,
            Self::Build(steno_services::BuildError::Lock(
                steno_core::DatabaseLockError::Held(_)
            ))
        )
    }

    /// The stored settings the app cannot read: the store's error, which
    /// never names a value, and the database.
    pub fn unreadable_settings(&self) -> Option<(&steno_core::StoreError, &std::path::Path)> {
        match self {
            Self::Build(error) => error.unreadable_settings(),
            _ => None,
        }
    }
}

/// The window a [`Host`] call answers for: a [`WebviewWindow`], or a test's
/// stand-in.
pub trait HostWindow {
    #[cfg(not(feature = "fixture-host"))]
    fn label(&self) -> &str;

    #[cfg(feature = "fixture-host")]
    fn emit(&self, topic: &str, payload: Value) -> Result<(), BridgeError>;
}

impl HostWindow for WebviewWindow {
    #[cfg(not(feature = "fixture-host"))]
    fn label(&self) -> &str {
        WebviewWindow::label(self)
    }

    #[cfg(feature = "fixture-host")]
    fn emit(&self, topic: &str, payload: Value) -> Result<(), BridgeError> {
        crate::bridge::emit(self, topic, payload)
    }
}

/// What the shell manages: the real host, or the fixture answerer.
pub struct Host {
    #[cfg(not(feature = "fixture-host"))]
    inner: RealHost,
}

impl Host {
    /// Builds the services graph on `runtime` and the host over it, then
    /// attaches the window sink. The launch sequence runs later, in
    /// [`Host::launch`].
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
        options.installed_bundle = crate::autostart::installed_bundle();
        // The system's permissions and, on the Mac, the calendar that names
        // a recording.
        options.permissions = Some(Arc::new(crate::permissions::ShellPermissions {
            app: app.clone(),
        }));
        options.calendar = Some(steno_services::calendar::system_calendar());
        // A smoke run checks for no update and detects no meeting, so
        // nothing reaches the network or covers its windows.
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
        options.import_marker =
            steno_services::swift_import::marker_on_this_mac(crate::smoke::SECONDS_VARIABLE);
        // The Swift import's launch half (macOS), under the database's lock
        // and before the graph reads a preference or a secret.
        let graph = runtime.block_on(async {
            steno_services::build_with_import(options, |preferences| {
                steno_services::swift_import::launch_on_this_mac(
                    preferences,
                    crate::smoke::SECONDS_VARIABLE,
                )
            })
        })?;
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

    #[cfg(feature = "fixture-host")]
    pub fn fixtures() -> Self {
        Host {}
    }

    /// The update schedule; `None` for the fixtures and in a smoke run.
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
    /// hears of it.
    #[cfg_attr(feature = "fixture-host", allow(unused_variables))]
    pub fn prompt_dismissed(&self, raised: Option<u64>) {
        #[cfg(not(feature = "fixture-host"))]
        if let Some(detection) = &self.inner.app.detection {
            detection.dismissed(raised);
        }
    }

    /// The Record of prompt `raised`: the detection controller starts a call
    /// recording for the app it named. Blocks for the start.
    #[cfg_attr(feature = "fixture-host", allow(unused_variables))]
    pub fn record_from_prompt(&self, raised: Option<u64>) {
        #[cfg(not(feature = "fixture-host"))]
        if let Some(detection) = &self.inner.app.detection {
            detection.record(raised);
        }
    }

    /// Every recording start is refused from now on: the menus' Quit, before
    /// its exit waits for the actions clicked before it. Only sets a flag.
    pub fn refuse_starts(&self) {
        #[cfg(not(feature = "fixture-host"))]
        {
            use steno_host::services::Recorder as _;
            self.inner.app.recorder.refuse_starts();
        }
    }

    /// The onboarding window is gone: the host counts the pages as seen.
    /// It reads the store, so not on the main thread.
    pub fn onboarding_window_closed(&self) {
        steno_host::assert_off_ui_thread("the onboarding window's close");
        #[cfg(not(feature = "fixture-host"))]
        self.inner.host.onboarding_window_closed();
    }

    /// What every exit runs first: the pipelines quit, a recording in
    /// progress is stopped and saved, the handover listener stops.
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

    /// Quits the pipelines ahead of the shutdown: an exit signal calls it
    /// before its request waits for the main thread.
    #[cfg(unix)]
    pub fn quit_pipeline(&self) {
        #[cfg(not(feature = "fixture-host"))]
        self.inner.app.pipeline.quit();
    }

    /// The launch sequence on the runtime, the tray told the meeting rows
    /// from here on.
    pub fn launch(&self, app: &AppHandle, runtime: &tokio::runtime::Runtime) {
        #[cfg(not(feature = "fixture-host"))]
        {
            let app = app.clone();
            self.inner.host.on_tray(Box::new(move |meetings| {
                let tray = app.clone();
                let _ =
                    app.run_on_main_thread(move || crate::tray::note_meetings(&tray, &meetings));
            }));
            let _guard = runtime.enter();
            self.inner.app.launch(&self.inner.host);
        }
        #[cfg(feature = "fixture-host")]
        {
            let _ = (app, runtime);
        }
    }

    /// The page has mounted: publish every topic it may show.
    pub fn page_ready(&self, window: &impl HostWindow) -> Result<(), BridgeError> {
        steno_host::assert_off_ui_thread("page.ready");
        #[cfg(not(feature = "fixture-host"))]
        {
            self.inner.page_ready(window.label())
        }
        #[cfg(feature = "fixture-host")]
        {
            for (topic, snapshot) in crate::fixtures::snapshots() {
                window.emit(topic, snapshot)?;
            }
            Ok(())
        }
    }

    pub fn call(
        &self,
        window: &impl HostWindow,
        method: &str,
        params: Value,
    ) -> Result<Value, BridgeError> {
        steno_host::assert_off_ui_thread(method);
        #[cfg(not(feature = "fixture-host"))]
        {
            self.inner.call(window.label(), method, params)
        }
        #[cfg(feature = "fixture-host")]
        {
            let _ = (window, params);
            Ok(crate::fixtures::reply(method))
        }
    }

    /// `window.open` names a meeting or a section for an open window: the
    /// request rides on the `app` snapshot, and the clean snapshot goes out
    /// right after. The fixture host only publishes the request.
    pub fn publish_request(
        &self,
        window: &impl HostWindow,
        field: &str,
        value: &str,
    ) -> Result<(), BridgeError> {
        steno_host::assert_off_ui_thread("a window request");
        #[cfg(not(feature = "fixture-host"))]
        {
            let _ = window;
            self.inner.publish_request(field, value)
        }
        #[cfg(feature = "fixture-host")]
        {
            for app in crate::fixtures::app_snapshots_requesting(field, value) {
                window.emit("app", app)?;
            }
            Ok(())
        }
    }
}

pub fn host(app: &AppHandle) -> tauri::State<'_, Host> {
    app.state::<Host>()
}

/// Whether the host is managed: false before `setup` built it, and in an
/// app that refused to start.
pub fn is_running(app: &AppHandle) -> bool {
    app.try_state::<Host>().is_some()
}

#[cfg(all(test, not(feature = "fixture-host")))]
mod refusal_tests {
    use super::ShellHostError;

    /// Only a database another process holds makes the shell refuse to
    /// start.
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

#[cfg(test)]
mod lane_tests {
    use std::time::Duration;

    use super::Lane;

    /// The lane runs jobs in order, and a panicking job leaves it running.
    #[test]
    fn the_lane_runs_its_jobs_in_order_and_past_a_panic() {
        let lane = Lane::start();
        let (ran, order) = std::sync::mpsc::channel();
        for job in 0..20 {
            let ran = ran.clone();
            lane.queue("a job", Box::new(move || ran.send(job).unwrap()));
        }
        lane.queue(
            "a job that panics",
            Box::new(|| panic!("the host panicked")),
        );
        lane.queue("the next job", Box::new(move || ran.send(20).unwrap()));
        let seen: Vec<_> = (0..21)
            .map(|_| order.recv_timeout(Duration::from_secs(5)).unwrap())
            .collect();
        assert_eq!(seen, (0..21).collect::<Vec<_>>());
    }
}

/// Each of the shell's host entry points fails a debug build on the
/// marked UI thread.
#[cfg(all(test, feature = "fixture-host", debug_assertions))]
mod ui_thread_tests {
    use serde_json::Value;

    use super::{Host, HostWindow};
    use crate::bridge::BridgeError;

    struct StandInWindow;

    impl HostWindow for StandInWindow {
        fn emit(&self, _topic: &str, _payload: Value) -> Result<(), BridgeError> {
            Ok(())
        }
    }

    /// Runs `call` on a thread marked as the UI thread and requires it to
    /// fail with the assert's message for `what`.
    fn asserts_on_the_ui_thread(what: &'static str, call: impl FnOnce(&Host) + Send + 'static) {
        std::thread::spawn(move || {
            steno_host::mark_ui_thread();
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                call(&Host::fixtures());
            }));
            let payload = outcome.expect_err("the entry point asserted");
            let message = payload
                .downcast_ref::<String>()
                .map(String::as_str)
                .unwrap_or_default();
            assert!(
                message.starts_with(&format!("{what} ran on the UI thread")),
                "{message}"
            );
        })
        .join()
        .unwrap();
    }

    #[test]
    fn every_entry_point_asserts_on_the_ui_thread() {
        asserts_on_the_ui_thread("recording.toggle", |host| {
            let _ = host.call(&StandInWindow, "recording.toggle", Value::Null);
        });
        asserts_on_the_ui_thread("page.ready", |host| {
            let _ = host.page_ready(&StandInWindow);
        });
        asserts_on_the_ui_thread("a window request", |host| {
            let _ = host.publish_request(&StandInWindow, "requestedMeetingID", "m-1");
        });
        asserts_on_the_ui_thread(
            "the onboarding window's close",
            Host::onboarding_window_closed,
        );
    }
}

/// A [`ShellHost`] for the tests, whose calls of one method wait at a gate.
#[cfg(test)]
pub mod gated {
    use std::sync::{Arc, Condvar, Mutex, MutexGuard};
    use std::time::Duration;

    use serde_json::Value;

    use super::{Job, Lane, ShellHost};
    use crate::bridge::BridgeError;

    /// A host whose calls of `held` wait at a gate until it opens; other
    /// calls answer at once or after their delay. It notes each answered
    /// call, login item switch, check mark and Quit step. `before_hold` runs
    /// inside a held call before it waits.
    #[derive(Clone)]
    pub struct GatedHost {
        held: &'static str,
        before_hold: Option<Arc<dyn Fn() + Send + Sync>>,
        delayed: Option<(&'static str, Duration)>,
        /// Every switch of the login item fails.
        switch_fails: bool,
        state: Arc<(Mutex<State>, Condvar)>,
        lane: Arc<Lane>,
    }

    #[derive(Default)]
    struct State {
        open: bool,
        waiting: usize,
        answered: Vec<String>,
        switched: Vec<bool>,
        login_item_on: bool,
        /// The check marks, as each was asked to show.
        checked: Vec<bool>,
        starts_refused: bool,
        /// `refuse starts` and `quitting`, in the order asked.
        quit_steps: Vec<&'static str>,
    }

    /// Opens the gate when dropped, so a failed test never leaves a call
    /// waiting.
    pub struct Opens(GatedHost);

    impl Drop for Opens {
        fn drop(&mut self) {
            self.0.open();
        }
    }

    /// Runs `call` on a thread of its own and returns whether it returned
    /// within a second.
    pub fn returns_at_once(call: impl FnOnce() + Send + 'static) -> bool {
        let (returned, done) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            call();
            let _ = returned.send(());
        });
        done.recv_timeout(Duration::from_secs(1)).is_ok()
    }

    impl GatedHost {
        pub fn holding(held: &'static str) -> Self {
            GatedHost {
                held,
                before_hold: None,
                delayed: None,
                switch_fails: false,
                state: Arc::default(),
                lane: Arc::new(Lane::start()),
            }
        }

        pub fn before_hold(mut self, hook: impl Fn() + Send + Sync + 'static) -> Self {
            self.before_hold = Some(Arc::new(hook));
            self
        }

        /// Every call of `method` takes `delay` before it answers.
        pub fn delaying(mut self, method: &'static str, delay: Duration) -> Self {
            self.delayed = Some((method, delay));
            self
        }

        /// Every switch of the login item fails.
        pub fn failing_switch(mut self) -> Self {
            self.switch_fails = true;
            self
        }

        fn state(&self) -> MutexGuard<'_, State> {
            self.state.0.lock().unwrap()
        }

        pub fn opens_on_drop(&self) -> Opens {
            Opens(self.clone())
        }

        pub fn open(&self) {
            self.state().open = true;
            self.state.1.notify_all();
        }

        fn wait_for(&self, patience: Duration, reached: impl Fn(&State) -> bool) -> bool {
            let (state, _) = self
                .state
                .1
                .wait_timeout_while(self.state(), patience, |state| !reached(state))
                .unwrap();
            reached(&state)
        }

        pub fn wait_held(&self, patience: Duration) -> bool {
            self.wait_for(patience, |state| state.waiting > 0)
        }

        pub fn wait_answered(&self, method: &str, patience: Duration) -> bool {
            self.wait_for(patience, |state| state.answered.iter().any(|m| m == method))
        }

        pub fn wait_answers(&self, count: usize, patience: Duration) -> bool {
            self.wait_for(patience, |state| state.answered.len() >= count)
        }

        pub fn wait_noted(&self, times: usize, patience: Duration) -> bool {
            self.wait_for(patience, |state| state.checked.len() == times)
        }

        pub fn answered(&self) -> Vec<String> {
            self.state().answered.clone()
        }

        pub fn switched(&self) -> Vec<bool> {
            self.state().switched.clone()
        }

        pub fn notes(&self) -> usize {
            self.state().checked.len()
        }

        pub fn checked(&self) -> Vec<bool> {
            self.state().checked.clone()
        }

        pub fn quit_steps(&self) -> Vec<&'static str> {
            self.state().quit_steps.clone()
        }
    }

    impl ShellHost for GatedHost {
        type Window = &'static str;

        fn page_ready(&self, window: &&'static str) -> Result<(), BridgeError> {
            self.call(window, "page.ready", Value::Null).map(drop)
        }

        fn call(
            &self,
            _window: &&'static str,
            method: &str,
            _params: Value,
        ) -> Result<Value, BridgeError> {
            if method == self.held {
                if let Some(hook) = &self.before_hold {
                    hook();
                }
                let mut state = self.state();
                state.waiting += 1;
                self.state.1.notify_all();
                while !state.open {
                    state = self.state.1.wait(state).unwrap();
                }
                state.waiting -= 1;
            }
            if let Some((delayed, delay)) = self.delayed
                && delayed == method
            {
                std::thread::sleep(delay);
            }
            let mut state = self.state();
            let answer = if method == "recording.start" && state.starts_refused {
                format!("{method} refused")
            } else {
                method.to_owned()
            };
            state.answered.push(answer.clone());
            drop(state);
            self.state.1.notify_all();
            Ok(Value::String(answer))
        }

        fn publish_request(
            &self,
            window: &&'static str,
            field: &str,
            value: &str,
        ) -> Result<(), BridgeError> {
            self.call(window, &format!("request {field}={value}"), Value::Null)
                .map(drop)
        }

        fn onboarding_window_closed(&self) {
            let _ = self.call(&"onboarding", "onboarding.closed", Value::Null);
        }

        fn login_item_on(&self) -> bool {
            self.state().login_item_on
        }

        fn switch_login_item(&self, value: bool) -> Result<(), BridgeError> {
            let mut state = self.state();
            state.switched.push(value);
            if self.switch_fails {
                return Err(crate::bridge::failed("the system refused the login item"));
            }
            state.login_item_on = value;
            Ok(())
        }

        fn note_login_item(&self, on: bool) {
            self.state().checked.push(on);
            self.state.1.notify_all();
        }

        fn refuse_starts(&self) {
            let mut state = self.state();
            state.starts_refused = true;
            state.quit_steps.push("refuse starts");
        }

        fn note_quitting(&self) {
            self.state().quit_steps.push("quitting");
        }

        fn queue(&self, what: &'static str, job: Job) {
            self.lane.queue(what, job);
        }
    }
}
