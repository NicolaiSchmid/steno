//! Audio sessions to process activity, the mapping under the Windows
//! `LiveProcessAudioActivity` (WP10), over synthetic sessions: which
//! process counts as holding the microphone, what its "bundle id" is, and
//! that the result drives `MeetingDetector` as the macOS HAL's does. Runs
//! on every OS. No Swift equivalent.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use steno_audio::detection::sessions::image_file_name;
use steno_audio::detection::{
    AudioSessionRecord, EndpointFlow, MeetingDetector, MeetingEvent, ProcessAudioActivity,
    SessionState, processes_from_sessions,
};
use steno_audio::testing::{FakeProcessAudioActivity, ManualClock};

fn session(pid: u32, flow: EndpointFlow, state: SessionState, image: &str) -> AudioSessionRecord {
    AudioSessionRecord {
        pid,
        flow,
        state,
        system_sounds: false,
        image_path: Some(image.into()),
    }
}

const TEAMS: &str = r"C:\Users\anna\AppData\Local\Microsoft\Teams\current\Teams.exe";
const ZOOM: &str = r"\Device\HarddiskVolume3\Program Files\Zoom\bin\Zoom.exe";
const SPOTIFY: &str = r"C:\Program Files\Spotify\Spotify.exe";

#[test]
fn an_active_capture_session_is_running_input_and_a_render_one_output() {
    let records = [
        session(7_100, EndpointFlow::Render, SessionState::Active, TEAMS),
        session(7_100, EndpointFlow::Capture, SessionState::Active, TEAMS),
        session(5_151, EndpointFlow::Capture, SessionState::Inactive, ZOOM),
        session(9_000, EndpointFlow::Render, SessionState::Active, SPOTIFY),
    ];
    let processes = processes_from_sessions(&records);
    assert_eq!(
        processes,
        vec![
            ProcessAudioActivity::new(5_151, Some("Zoom.exe"), false),
            ProcessAudioActivity {
                pid: 7_100,
                bundle_id: Some("Teams.exe".into()),
                is_running_input: true,
                is_running_output: true,
            },
            ProcessAudioActivity {
                pid: 9_000,
                bundle_id: Some("Spotify.exe".into()),
                is_running_input: false,
                is_running_output: true,
            },
        ],
        "one entry per process, in pid order"
    );
}

#[test]
fn system_sounds_pid_zero_and_expired_sessions_are_skipped() {
    let mut system_sounds = session(4, EndpointFlow::Render, SessionState::Active, "");
    system_sounds.system_sounds = true;
    let records = [
        system_sounds,
        session(0, EndpointFlow::Capture, SessionState::Active, "Idle"),
        session(3_000, EndpointFlow::Capture, SessionState::Expired, TEAMS),
    ];
    assert_eq!(processes_from_sessions(&records), Vec::new());
}

#[test]
fn the_first_known_image_names_the_process() {
    let mut unnamed = session(42, EndpointFlow::Capture, SessionState::Active, "");
    unnamed.image_path = None;
    let records = [
        unnamed,
        session(42, EndpointFlow::Render, SessionState::Inactive, TEAMS),
    ];
    let processes = processes_from_sessions(&records);
    assert_eq!(processes.len(), 1);
    assert_eq!(processes[0].bundle_id.as_deref(), Some("Teams.exe"));
    assert!(processes[0].is_running_input);
    assert!(!processes[0].is_running_output);
}

#[test]
fn image_file_names_from_both_path_forms() {
    assert_eq!(image_file_name(TEAMS), Some("Teams.exe"));
    assert_eq!(image_file_name(ZOOM), Some("Zoom.exe"));
    assert_eq!(image_file_name("Zoom.exe"), Some("Zoom.exe"));
    assert_eq!(image_file_name(r"C:\odd/mixed\App.exe"), Some("App.exe"));
    assert_eq!(image_file_name(r"C:\trailing\"), None);
    assert_eq!(image_file_name(""), None);
}

#[test]
fn sessions_drive_the_meeting_detector() {
    let clock = Arc::new(ManualClock::new());
    let idle = [session(
        7_100,
        EndpointFlow::Capture,
        SessionState::Inactive,
        TEAMS,
    )];
    let source = FakeProcessAudioActivity::new(processes_from_sessions(&idle));
    let detector = MeetingDetector::new(
        Arc::new(source.clone()),
        Arc::clone(&clock) as Arc<dyn steno_audio::Clock>,
        Some(BTreeSet::from([1])),
        MeetingDetector::DEFAULT_DEBOUNCE,
        MeetingDetector::DEFAULT_POLL_INTERVAL,
    );
    let events = detector.events();
    detector.start().unwrap();
    assert!(clock.wait_for_sleepers(1), "the poll timer is armed");

    let call = [
        session(7_100, EndpointFlow::Capture, SessionState::Active, TEAMS),
        session(7_100, EndpointFlow::Render, SessionState::Active, TEAMS),
    ];
    source.set(processes_from_sessions(&call));
    assert!(clock.wait_for_sleepers(2), "the open debounce is armed");
    clock.advance(Duration::from_secs(2));
    assert_eq!(
        events.recv_timeout(Duration::from_secs(5)).unwrap(),
        MeetingEvent::MicrophoneOpened {
            bundle_id: Some("Teams.exe".into()),
            pid: 7_100
        }
    );

    source.set(processes_from_sessions(&idle));
    assert!(clock.wait_for_sleepers(2), "the release debounce is armed");
    clock.advance(Duration::from_secs(2));
    assert_eq!(
        events.recv_timeout(Duration::from_secs(5)).unwrap(),
        MeetingEvent::MicrophoneReleased
    );
    detector.stop();
}
