//! Meeting detection against the real Core Audio HAL, ignored by default:
//! it needs a Mac with a microphone and runs with `cargo test -p
//! steno-services --test live_detection -- --ignored --nocapture`. The
//! test opens the microphone in its own process, through the live capture
//! backend, with a detector that does not ignore its own pid: the
//! controller over the live process list raises the prompt once the
//! microphone has been held for the debounce, and takes it down once the
//! capture stops. Over SSH the microphone delivers silence (no TCC grant),
//! which does not matter: the HAL still reports the input running.
//! Swift had no live detection test.
#![cfg(target_os = "macos")]

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use steno_audio::detection::LiveProcessAudioActivity;
use steno_audio::{
    CaptureBackend, LaneFrameSink, LiveCaptureBackend, MeetingDetector, SystemClock,
};
use steno_core::AudioLane;
use steno_services::auto_stop::MicrophoneActivity;
use steno_services::detection::{
    CallRecorder, DetectionController, DetectionParts, DetectionPrompt, PromptPanel,
    fallback_app_name,
};

#[derive(Default)]
struct Panel(Mutex<Vec<Option<DetectionPrompt>>>);

impl PromptPanel for Panel {
    fn show(&self, prompt: Option<&DetectionPrompt>) {
        println!("panel: {prompt:?}");
        self.0.lock().unwrap().push(prompt.cloned());
    }
}

struct Idle;

impl CallRecorder for Idle {
    fn is_busy(&self) -> bool {
        false
    }
    fn start_call(&self, _call_app: Option<&str>) {}
    fn microphone_activity(&self, _activity: MicrophoneActivity) {}
}

fn wait_for(what: &str, limit: Duration, done: impl Fn() -> bool) {
    let started = Instant::now();
    while !done() {
        assert!(started.elapsed() < limit, "{what} within {limit:?}");
        std::thread::sleep(Duration::from_millis(50));
    }
    println!("{what} after {:?}", started.elapsed());
}

#[test]
#[ignore = "needs a Mac with a microphone; run with -- --ignored --nocapture"]
fn the_microphone_held_in_another_capture_raises_the_prompt_and_its_release_takes_it_down() {
    let dir = tempfile::tempdir().unwrap();
    let store = steno_services::open_store(&dir.path().join("steno.sqlite")).unwrap();
    let panel = Arc::new(Panel::default());
    let clock = Arc::new(SystemClock::new());
    let controller = DetectionController::new(DetectionParts {
        detector: MeetingDetector::new(
            Arc::new(LiveProcessAudioActivity::new()),
            clock.clone(),
            // This process holds the microphone below.
            Some(std::collections::BTreeSet::new()),
            MeetingDetector::DEFAULT_DEBOUNCE,
            MeetingDetector::DEFAULT_POLL_INTERVAL,
        ),
        recorder: Arc::new(Idle),
        panel: panel.clone(),
        store,
        clock,
        app_names: Arc::new(fallback_app_name),
    });
    controller.follow_settings();
    assert!(controller.is_detecting(), "the detector started on the HAL");
    std::thread::sleep(Duration::from_secs(3));
    if controller.has_prompt() {
        println!("another app holds the microphone already; nothing to measure");
        controller.stop();
        return;
    }

    let backend = LiveCaptureBackend::new();
    let lanes = &[AudioLane::Mixed];
    let sink = Arc::new(LaneFrameSink::new(lanes));
    let started = Instant::now();
    backend
        .start(lanes, None, sink)
        .expect("the in-person capture starts");
    println!("capture started in {:?}", started.elapsed());
    wait_for("the prompt", Duration::from_secs(10), || {
        controller.has_prompt()
    });
    backend.stop();
    wait_for("the release took it down", Duration::from_secs(10), || {
        !controller.has_prompt()
    });
    controller.stop();
    assert!(!controller.is_detecting());
}
