//! Meeting detection: the prompt that offers to record when another app
//! opens the microphone, and the events that feed the auto-stop after a
//! call ([`crate::auto_stop`]).
//!
//! The [`DetectionController`] consumes the [`MeetingDetector`]'s events on
//! every platform (the Mac's Core Audio processes, Linux's `PipeWire`
//! streams, Windows' audio sessions) and shows one prompt at a time
//! through the shell's [`PromptPanel`], for [`PROMPT_SECONDS`]:
//!
//! - an opened microphone raises the prompt only while detection is on in
//!   Settings, nothing records and no prompt is up;
//! - a released microphone, a recording starting, detection turned off and
//!   the countdown running out take it down, and so do the panel's own X
//!   ([`DetectionController::dismissed`]) and Record
//!   ([`DetectionController::record`]), which starts a call recording
//!   attributed to the app the prompt named;
//! - while a recording starts, runs or stops, both events go to the
//!   recorder's auto-stop instead ([`CallRecorder::microphone_activity`]).
//!
//! The detector keeps running through Steno's own recordings (it ignores
//! Steno's own capture), so a Stop during a call that still holds the
//! microphone never prompts again: a restarted detector would report that
//! microphone as newly opened. A detector that cannot start (`PipeWire`
//! not up yet at login) is started again by the next
//! [`DetectionController::follow_settings`], which [`DetectionController::run`]
//! calls every two seconds along with the setting.
//!
//! Lives here rather than in `steno-host`: it drives the detector of
//! `steno-audio` and the capture recorder, which this crate assembles, as
//! the Swift `DetectionController` lived in the app beside
//! `RecordingController`; the host only renders the recorder's status.
//!
//! Swift: `apps/macos/Steno/Detection/DetectionController.swift` and
//! `DetectionPromptViewModel.swift`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::thread::JoinHandle;
use std::time::Duration;

use steno_audio::clock::{Cancel, Clock, SystemClock};
use steno_audio::{MeetingDetector, MeetingEvent, ProcessAudioActivitySource};
use steno_bridge::CaptureMode;
use steno_core::Store;
use steno_host::services::Recorder as _;

use crate::auto_stop::MicrophoneActivity;
use crate::recorder::CaptureRecorder;

/// How long a prompt stays up unanswered. Swift:
/// `DetectionPromptViewModel`'s `timeout`.
pub const PROMPT_SECONDS: u64 = 60;

/// How often [`DetectionController::run`] reads the setting again and
/// retries a detector that did not start.
pub const SETTINGS_INTERVAL: Duration = Duration::from_secs(2);

/// What the prompt asks: which app opened the microphone, and for how
/// many seconds the prompt stays.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectionPrompt {
    /// The app's name; "Another app" when it could not be named.
    pub app_name: String,
    pub seconds: u64,
}

/// Where the prompt shows: the shell's floating panel (`panels::set_prompt`
/// in `apps/desktop`).
pub trait PromptPanel: Send + Sync {
    /// Raises `prompt`, or takes the prompt down for `None`. Called with
    /// the controller's lock held, so the panel sees raises and clears in
    /// the order the controller made them; it must return without calling
    /// the controller or waiting on a thread that may.
    fn show(&self, prompt: Option<&DetectionPrompt>);
}

/// What the controller needs of the recorder. [`CaptureRecorder`] in the
/// app; a fake in tests.
pub trait CallRecorder: Send + Sync {
    /// Whether a recording is starting, running or stopping.
    fn is_busy(&self) -> bool;
    /// The prompt's Record: a call recording attributed to `call_app`. A
    /// recording already in progress makes it do nothing.
    fn start_call(&self, call_app: Option<&str>);
    /// Another app's microphone activity while a recording is busy.
    fn microphone_activity(&self, activity: MicrophoneActivity);
}

impl CallRecorder for CaptureRecorder {
    fn is_busy(&self) -> bool {
        CaptureRecorder::is_busy(self)
    }

    fn start_call(&self, call_app: Option<&str>) {
        self.start(CaptureMode::Call, call_app);
    }

    fn microphone_activity(&self, activity: MicrophoneActivity) {
        CaptureRecorder::microphone_activity(self, activity);
    }
}

/// Names the app with a bundle id (`None` when the detector knows none).
/// The shell's resolves Mac bundle ids through `NSRunningApplication`;
/// [`fallback_app_name`] elsewhere.
pub type AppNames = Arc<dyn Fn(Option<&str>) -> String + Send + Sync>;

/// "Another app" without a bundle id, else the id itself, as Swift's
/// `liveAppName` falls back: on Linux the holder's binary name, on Windows
/// its executable's file name.
#[must_use]
pub fn fallback_app_name(bundle_id: Option<&str>) -> String {
    bundle_id.unwrap_or("Another app").to_owned()
}

/// What meeting detection needs from the shell
/// ([`AppOptions::detection`](crate::AppOptions::detection)).
pub struct DetectionOptions {
    /// Where the prompt shows.
    pub panel: Arc<dyn PromptPanel>,
    pub app_names: AppNames,
    /// Which processes hold the microphone: `LiveProcessAudioActivity` in
    /// the app.
    pub activity: Arc<dyn ProcessAudioActivitySource>,
}

impl DetectionOptions {
    /// The live process list of this platform
    /// ([`LiveProcessAudioActivity`](steno_audio::detection::LiveProcessAudioActivity)):
    /// the Mac's Core Audio processes, Linux's `PipeWire` streams, Windows'
    /// audio sessions.
    #[must_use]
    pub fn live(panel: Arc<dyn PromptPanel>, app_names: AppNames) -> Self {
        Self {
            panel,
            app_names,
            activity: Arc::new(steno_audio::detection::LiveProcessAudioActivity::new()),
        }
    }

    /// The controller over `recorder` and the setting in `store`, its
    /// detector and countdown on the system clock with the detector's
    /// default debounce and poll.
    #[must_use]
    pub fn controller(
        self,
        recorder: Arc<CaptureRecorder>,
        store: Arc<Store>,
    ) -> Arc<DetectionController> {
        let clock: Arc<dyn Clock> = Arc::new(SystemClock::new());
        DetectionController::new(DetectionParts {
            detector: MeetingDetector::new(
                self.activity,
                clock.clone(),
                None,
                MeetingDetector::DEFAULT_DEBOUNCE,
                MeetingDetector::DEFAULT_POLL_INTERVAL,
            ),
            recorder,
            panel: self.panel,
            store,
            clock,
            app_names: self.app_names,
        })
    }
}

/// What the controller is built from.
pub struct DetectionParts {
    pub detector: MeetingDetector,
    pub recorder: Arc<dyn CallRecorder>,
    pub panel: Arc<dyn PromptPanel>,
    /// Where `meeting_detection_enabled` is read.
    pub store: Arc<Store>,
    /// The prompt's countdown runs on it.
    pub clock: Arc<dyn Clock>,
    pub app_names: AppNames,
}

/// One prompt up.
struct OpenPrompt {
    number: u64,
    /// The app a recording from it is attributed to: its name when the
    /// detector knew a bundle id, else `None`.
    call_app: Option<String>,
    /// Raised when the prompt closes, which ends its countdown.
    countdown: Cancel,
}

#[derive(Default)]
struct State {
    enabled: bool,
    prompt: Option<OpenPrompt>,
    raised: u64,
    /// Raised to stop [`DetectionController::run`]'s thread.
    running: Option<Cancel>,
}

/// The meeting detection controller; see the module doc.
pub struct DetectionController {
    detector: MeetingDetector,
    recorder: Arc<dyn CallRecorder>,
    panel: Arc<dyn PromptPanel>,
    store: Arc<Store>,
    clock: Arc<dyn Clock>,
    app_names: AppNames,
    state: Mutex<State>,
    /// Serialises the detector's start and stop and owns the thread that
    /// carries its events, apart from `state`: a start can wait seconds
    /// for `PipeWire`, and `state` is taken by every recorder change.
    detecting: Mutex<Option<JoinHandle<()>>>,
    /// The detector's last start failed, so the next failure is logged
    /// quietly and a success says it recovered.
    start_failed: AtomicBool,
    this: Weak<Self>,
}

impl DetectionController {
    /// Detection off until [`Self::follow_settings`] or
    /// [`Self::set_enabled`] turns it on.
    #[must_use]
    pub fn new(parts: DetectionParts) -> Arc<Self> {
        Arc::new_cyclic(|this| Self {
            detector: parts.detector,
            recorder: parts.recorder,
            panel: parts.panel,
            store: parts.store,
            clock: parts.clock,
            app_names: parts.app_names,
            state: Mutex::default(),
            detecting: Mutex::default(),
            start_failed: AtomicBool::new(false),
            this: this.clone(),
        })
    }

    /// Follows the setting from now on, every [`SETTINGS_INTERVAL`] on a
    /// thread of its own, until [`Self::stop`].
    pub fn run(&self) {
        let running = Cancel::new();
        {
            let mut state = self.state();
            if state.running.is_some() {
                return;
            }
            state.running = Some(running.clone());
        }
        let this = self.this.clone();
        let spawned = std::thread::Builder::new()
            .name("steno-detection".into())
            .spawn(move || {
                loop {
                    let Some(controller) = this.upgrade() else {
                        return;
                    };
                    if running.is_cancelled() {
                        return;
                    }
                    controller.follow_settings();
                    drop(controller);
                    if !running.sleep(SETTINGS_INTERVAL) {
                        return;
                    }
                }
            });
        if let Err(error) = spawned {
            tracing::error!(%error, "meeting detection could not start");
        }
    }

    /// Detection off for good: the setting is no longer followed, the
    /// detector stops and the prompt goes. For the app's shutdown.
    pub fn stop(&self) {
        if let Some(running) = self.state().running.take() {
            running.cancel();
        }
        self.set_enabled(false);
    }

    /// Reads the setting and applies it; an unreadable setting leaves
    /// detection as it is. While detection is on, a detector that did not
    /// start is started again.
    pub fn follow_settings(&self) {
        match self.store.settings() {
            Ok(settings) => self.set_enabled(settings.meeting_detection_enabled),
            Err(error) => tracing::debug!(%error, "the detection setting could not be read"),
        }
    }

    /// Whether detection is on.
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        self.state().enabled
    }

    /// Whether the detector runs.
    #[must_use]
    pub fn is_detecting(&self) -> bool {
        self.detector.is_running()
    }

    /// Whether a prompt is up.
    #[must_use]
    pub fn has_prompt(&self) -> bool {
        self.state().prompt.is_some()
    }

    /// Turns detection on (the detector starts, or is started again when
    /// it did not) or off (the detector stops and the prompt goes).
    pub fn set_enabled(&self, enabled: bool) {
        let mut detecting = self
            .detecting
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        {
            let mut state = self.state();
            state.enabled = enabled;
            if !enabled {
                self.close(&mut state);
            }
        }
        if enabled {
            if !self.detector.is_running() {
                *detecting = self.start_detector(detecting.take());
            }
        } else if self.detector.is_running() || detecting.is_some() {
            // The events' channel closes with the detector, which ends
            // the thread that carries them.
            self.detector.stop();
            if let Some(thread) = detecting.take() {
                let _ = thread.join();
            }
        }
    }

    /// Starts the detector and the thread that hands its events to
    /// [`Self::handle`]; `old`, the thread of an earlier start, is joined
    /// first. `None` when the detector did not start.
    fn start_detector(&self, old: Option<JoinHandle<()>>) -> Option<JoinHandle<()>> {
        if let Some(old) = old {
            let _ = old.join();
        }
        if let Err(error) = self.detector.start() {
            if self.start_failed.swap(true, Ordering::Relaxed) {
                tracing::debug!(%error, "the meeting detector did not start again");
            } else {
                tracing::warn!(%error, "the meeting detector did not start; it is tried again every two seconds");
            }
            return None;
        }
        if self.start_failed.swap(false, Ordering::Relaxed) {
            tracing::info!("the meeting detector started after all");
        }
        let events = self.detector.events();
        let this = self.this.clone();
        std::thread::Builder::new()
            .name("steno-detection-events".into())
            .spawn(move || {
                for event in events {
                    let Some(controller) = this.upgrade() else {
                        return;
                    };
                    controller.handle(event);
                }
            })
            .inspect_err(|error| {
                tracing::error!(%error, "the meeting detector's events cannot be read");
            })
            .ok()
    }

    /// One of the detector's events; see the module doc. The decision the
    /// tests pin. Swift: `DetectionController.handle`.
    pub fn handle(&self, event: MeetingEvent) {
        let forward = {
            let mut state = self.state();
            // Read under the lock: a recording that starts meanwhile takes
            // the lock in `recorder_changed` after this, and closes what
            // this raised.
            let busy = self.recorder.is_busy();
            match event {
                MeetingEvent::MicrophoneOpened { bundle_id, .. } => {
                    let app_name = (self.app_names)(bundle_id.as_deref());
                    let call_app = bundle_id.is_some().then(|| app_name.clone());
                    if busy {
                        Some(MicrophoneActivity::Opened { app_name: call_app })
                    } else {
                        if state.enabled && state.prompt.is_none() {
                            self.raise(&mut state, app_name, call_app);
                        }
                        None
                    }
                }
                MeetingEvent::MicrophoneReleased => {
                    self.close(&mut state);
                    busy.then_some(MicrophoneActivity::Released)
                }
            }
        };
        // With the lock released: the recorder reports its change through
        // `recorder_changed`, which takes it.
        if let Some(activity) = forward {
            self.recorder.microphone_activity(activity);
        }
    }

    /// The recorder changed (its change hook): a recording starting takes
    /// the prompt down. Swift: `DetectionController.recordingDidChange`.
    pub fn recorder_changed(&self) {
        let mut state = self.state();
        if state.prompt.is_some() && self.recorder.is_busy() {
            self.close(&mut state);
        }
    }

    /// The prompt's X: the panel has already taken it down.
    pub fn dismissed(&self) {
        if let Some(prompt) = self.state().prompt.take() {
            prompt.countdown.cancel();
        }
    }

    /// The prompt's Record, the panel having taken it down: a call
    /// recording attributed to the app it named. Nothing when no prompt is
    /// up (it closed meanwhile); a recording already in progress makes the
    /// recorder refuse it, so a prompt never starts a second one. Blocks
    /// for the start, as the recorder's start does.
    pub fn record(&self) {
        let Some(prompt) = self.state().prompt.take() else {
            return;
        };
        prompt.countdown.cancel();
        self.recorder.start_call(prompt.call_app.as_deref());
    }

    fn raise(&self, state: &mut State, app_name: String, call_app: Option<String>) {
        state.raised += 1;
        let number = state.raised;
        let countdown = Cancel::new();
        self.panel.show(Some(&DetectionPrompt {
            app_name,
            seconds: PROMPT_SECONDS,
        }));
        state.prompt = Some(OpenPrompt {
            number,
            call_app,
            countdown: countdown.clone(),
        });
        let (clock, this) = (self.clock.clone(), self.this.clone());
        let spawned = std::thread::Builder::new()
            .name("steno-prompt".into())
            .spawn(move || {
                if clock.sleep(Duration::from_secs(PROMPT_SECONDS), &countdown)
                    && let Some(controller) = this.upgrade()
                {
                    controller.timed_out(number);
                }
            });
        if let Err(error) = spawned {
            tracing::error!(%error, "the prompt cannot count down; it is taken down");
            self.close(state);
        }
    }

    /// The countdown of prompt `number` ran out.
    fn timed_out(&self, number: u64) {
        let mut state = self.state();
        if state
            .prompt
            .as_ref()
            .is_some_and(|prompt| prompt.number == number)
        {
            self.close(&mut state);
        }
    }

    /// Takes the prompt down, if one is up.
    fn close(&self, state: &mut State) {
        if let Some(prompt) = state.prompt.take() {
            prompt.countdown.cancel();
            self.panel.show(None);
        }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Drop for DetectionController {
    fn drop(&mut self) {
        if let Some(running) = self.state().running.take() {
            running.cancel();
        }
        if let Some(prompt) = self.state().prompt.take() {
            prompt.countdown.cancel();
        }
    }
}

#[cfg(test)]
mod tests;
