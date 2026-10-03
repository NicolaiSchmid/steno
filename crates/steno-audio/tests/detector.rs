//! `MeetingDetector` on `FakeProcessAudioActivity` and `ManualClock`: no
//! real sleeping in the code under test. `wait_for_sleepers` synchronises
//! with the detector's timers (the poll is one sleeper, an armed debounce a
//! second one).
//! Swift: `Tests/StenoAudioTests/MeetingDetectorTests.swift`.

// Test arithmetic: sample counts and dB values cast freely, and sample
// rates compare exactly on purpose.
#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::float_cmp,
    clippy::too_many_lines,
    clippy::doc_markdown,
    clippy::cast_lossless
)]

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::mpsc::Receiver;
use std::time::Duration;

use steno_audio::detection::{MeetingDetector, MeetingEvent, ProcessAudioActivity};
use steno_audio::testing::{FakeProcessAudioActivity, ManualClock};

fn face_time() -> ProcessAudioActivity {
    ProcessAudioActivity::new(4_242, Some("com.apple.FaceTime"), true)
}
fn zoom() -> ProcessAudioActivity {
    ProcessAudioActivity::new(5_151, Some("us.zoom.xos"), true)
}
fn idle_zoom() -> ProcessAudioActivity {
    ProcessAudioActivity::new(5_151, Some("us.zoom.xos"), false)
}

fn make_detector(source: &FakeProcessAudioActivity, clock: &Arc<ManualClock>) -> MeetingDetector {
    MeetingDetector::new(
        Arc::new(source.clone()),
        Arc::clone(clock) as Arc<dyn steno_audio::Clock>,
        Some(BTreeSet::from([1])),
        MeetingDetector::DEFAULT_DEBOUNCE,
        MeetingDetector::DEFAULT_POLL_INTERVAL,
    )
}

fn next(events: &Receiver<MeetingEvent>) -> MeetingEvent {
    events
        .recv_timeout(Duration::from_secs(5))
        .expect("an event")
}

#[test]
fn a_flapping_input_yields_one_opened_and_one_released() {
    let clock = Arc::new(ManualClock::new());
    let source = FakeProcessAudioActivity::new(vec![idle_zoom()]);
    let detector = make_detector(&source, &clock);
    let events = detector.events();
    detector.start().unwrap();
    assert!(clock.wait_for_sleepers(1), "the poll timer is armed");

    source.set(vec![face_time()]);
    assert!(clock.wait_for_sleepers(2), "the open debounce is armed");
    source.set(vec![]);
    assert!(
        clock.wait_for_sleepers(1),
        "released within the debounce: disarmed"
    );
    source.set(vec![face_time(), idle_zoom()]);
    assert!(clock.wait_for_sleepers(2));

    clock.advance(Duration::from_secs(2));
    assert_eq!(
        next(&events),
        MeetingEvent::MicrophoneOpened {
            bundle_id: Some("com.apple.FaceTime".into()),
            pid: 4_242
        }
    );
    assert_eq!(detector.holder(), Some(face_time()));

    // Still open two polls later: nothing new.
    assert!(clock.wait_for_sleepers(1));
    clock.advance(Duration::from_secs(2));
    assert!(clock.wait_for_sleepers(1));
    clock.advance(Duration::from_secs(2));
    assert!(clock.wait_for_sleepers(1));

    source.set(vec![idle_zoom()]);
    assert!(clock.wait_for_sleepers(2), "the release debounce is armed");
    source.set(vec![face_time()]);
    assert!(
        clock.wait_for_sleepers(1),
        "a flap back cancels the release"
    );
    source.set(vec![]);
    assert!(clock.wait_for_sleepers(2));
    clock.advance(Duration::from_secs(2));
    assert_eq!(next(&events), MeetingEvent::MicrophoneReleased);
    assert_eq!(detector.holder(), None);

    detector.stop();
    let trailing: Vec<MeetingEvent> = events.try_iter().collect();
    assert!(
        trailing.is_empty(),
        "exactly one opened and one released: {trailing:?}"
    );
    assert!(events.recv().is_err(), "the channel closes on stop");
}

#[test]
fn own_process_is_ignored() {
    let clock = Arc::new(ManualClock::new());
    let own = ProcessAudioActivity::new(1, Some("uno.schmid.steno.mac"), true);
    let source = FakeProcessAudioActivity::new(vec![own.clone()]);
    let detector = make_detector(&source, &clock);
    detector.start().unwrap();
    assert!(clock.wait_for_sleepers(1));
    assert_eq!(
        clock.pending_sleepers(),
        1,
        "no debounce for our own microphone use"
    );
    source.set(vec![own, idle_zoom()]);
    assert!(clock.wait_for_sleepers(1));
    clock.advance(Duration::from_secs(4));
    assert!(clock.wait_for_sleepers(1));
    assert_eq!(detector.holder(), None);
    detector.stop();
}

#[test]
fn the_poll_notices_what_the_listener_missed_within_three_seconds() {
    let clock = Arc::new(ManualClock::new());
    let source = FakeProcessAudioActivity::new(vec![]);
    let detector = make_detector(&source, &clock);
    let events = detector.events();
    detector.start().unwrap();
    assert!(clock.wait_for_sleepers(1));
    assert_eq!(detector.poll_interval(), Duration::from_secs(1));
    let before = source.snapshot_count();

    source.set_silently(vec![zoom()]);
    clock.advance(Duration::from_secs(1));
    assert!(
        clock.wait_for_sleepers(2),
        "the poll fired and armed the debounce"
    );
    assert!(
        source.snapshot_count() > before,
        "one second after opening, the poll has seen it"
    );
    clock.advance(Duration::from_secs(2));
    assert_eq!(
        next(&events),
        MeetingEvent::MicrophoneOpened {
            bundle_id: Some("us.zoom.xos".into()),
            pid: 5_151
        }
    );
    assert_eq!(steno_audio::Clock::now(&*clock), Duration::from_secs(3));
    detector.stop();
}

#[test]
fn an_already_open_microphone_is_reported_after_the_debounce() {
    let clock = Arc::new(ManualClock::new());
    let source = FakeProcessAudioActivity::new(vec![zoom()]);
    let detector = make_detector(&source, &clock);
    let events = detector.events();
    detector.start().unwrap();
    assert!(clock.wait_for_sleepers(2), "poll plus debounce");
    clock.advance(Duration::from_secs(2));
    assert_eq!(
        next(&events),
        MeetingEvent::MicrophoneOpened {
            bundle_id: Some("us.zoom.xos".into()),
            pid: 5_151
        }
    );
    detector.stop();
    assert!(!detector.is_running());
}

#[test]
fn a_failing_snapshot_fails_start_and_is_tolerated_later() {
    let clock = Arc::new(ManualClock::new());
    let source = FakeProcessAudioActivity::new(vec![zoom()]);
    source.set_failure(Some("broken"));
    let detector = make_detector(&source, &clock);
    assert!(detector.start().is_err());
    assert!(!detector.is_running());

    source.set_failure(None);
    let events = detector.events();
    detector.start().unwrap();
    assert!(clock.wait_for_sleepers(2));
    source.set_failure(Some("broken"));
    source.set(vec![]);
    clock.advance(Duration::from_secs(2));
    // The failed re-read changed nothing; the debounce still fires on what
    // was last seen.
    assert_eq!(
        next(&events),
        MeetingEvent::MicrophoneOpened {
            bundle_id: Some("us.zoom.xos".into()),
            pid: 5_151
        }
    );
    assert_eq!(detector.holder(), Some(zoom()));
    detector.stop();
}
