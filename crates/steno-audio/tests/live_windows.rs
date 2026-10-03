//! The Windows live backend against the real WASAPI (WP10a).
//!
//! **Nobody has run the ignored tests yet.** No Windows machine has run
//! them; the backend is compile-tested on the `windows-latest` CI runner,
//! which has no audio device. The `#[ignore]`d tests here are
//! the live check a Windows machine with a microphone and speakers must
//! run before the backend ships (the plan's parity list):
//!
//! ```text
//! cargo test -p steno-audio --test live_windows -- --ignored --nocapture
//! ```
//!
//! Without `--nocapture` the record they print is swallowed. Play audio
//! during `call_capture_records_both_lanes` (any media player) so the
//! loopback has something to deliver.
//!
//! The tests that are not ignored run on every Windows host, the CI runner
//! included: they need no device and check only that the COM paths return
//! within a bound with an answer (a capture that fails with "no input
//! device" is a pass) instead of hanging or crashing. Each run happens on
//! its own thread joined with a deadline, so a hang fails instead of
//! stalling the suite. CI runs them with `--nocapture`, so its log shows
//! what COM answered.
// Plan package names (WP10a) are not code.
#![allow(clippy::doc_markdown)]
#![cfg(windows)]

use std::sync::Arc;
use std::sync::mpsc::channel;
use std::time::{Duration, Instant};

use steno_audio::capture::live::AudioDevices;
use steno_audio::detection::LiveProcessAudioActivity;
use steno_audio::{
    CaptureBackend, CaptureMode, LaneFrameSink, LiveCaptureBackend, ProcessAudioActivitySource,
};
use steno_core::AudioLane;

/// Runs `work` on a thread and waits at most `limit` for its result.
fn within<T: Send + 'static>(
    limit: Duration,
    what: &str,
    work: impl FnOnce() -> T + Send + 'static,
) -> T {
    let (sender, receiver) = channel();
    std::thread::spawn(move || {
        let _ = sender.send(work());
    });
    receiver
        .recv_timeout(limit)
        .unwrap_or_else(|_| panic!("{what} did not return within {limit:?}"))
}

fn peak(samples: &[f32]) -> f32 {
    samples.iter().fold(0.0f32, |peak, s| peak.max(s.abs()))
}

/// What one start, capture, stop run saw.
struct Run {
    callbacks: usize,
    peaks: Vec<f32>,
}

/// Starts the backend for `lanes`, captures `duration`, stops, and returns
/// the callbacks counted in between (each completed callback signals the
/// sink's wake once) and each lane's peak, or `None` when the start
/// failed, which is printed.
fn start_capture_stop(lanes: Vec<AudioLane>, duration: Duration) -> Option<Run> {
    within(Duration::from_secs(60), "start, capture, stop", move || {
        let backend = LiveCaptureBackend::new();
        let sink = Arc::new(LaneFrameSink::new(&lanes));
        let started = Instant::now();
        let stream = match backend.start(&lanes, None, Arc::clone(&sink)) {
            Ok(stream) => stream,
            Err(error) => {
                println!(
                    "start for {lanes:?} failed after {:?}: {error}",
                    started.elapsed()
                );
                return None;
            }
        };
        println!("started {lanes:?} in {:?}: {stream:?}", started.elapsed());
        std::thread::sleep(duration);
        let stopping = Instant::now();
        backend.stop();
        println!("stopped in {:?}", stopping.elapsed());
        let mut callbacks = 0;
        while sink.wake().try_take() {
            callbacks += 1;
        }
        let available = sink.available_to_read();
        let peaks: Vec<f32> = (0..lanes.len())
            .map(|index| peak(&sink.ring(index).drain_all()))
            .collect();
        println!(
            "{callbacks} callbacks, {available} samples per lane, dropped {:?}, peaks {peaks:?}",
            sink.dropped_samples()
        );
        Some(Run { callbacks, peaks })
    })
}

#[test]
fn device_enumeration_returns_an_answer() {
    match within(
        Duration::from_secs(20),
        "AudioDevices::all",
        AudioDevices::all,
    ) {
        Ok(devices) => {
            for device in &devices {
                println!(
                    "{} in / {} out, {} Hz: {} ({})",
                    device.input_channels,
                    device.output_channels,
                    device.nominal_sample_rate,
                    device.name,
                    device.uid
                );
            }
            println!("{} active endpoints", devices.len());
        }
        Err(error) => println!("enumeration failed (no audio service?): {error}"),
    }
}

#[test]
fn a_capture_without_a_device_answers_and_stops() {
    // The system lane alone goes through the process-loopback activation
    // (and its fallback) without needing a microphone.
    for lanes in [
        CaptureMode::InPerson.lanes(),
        CaptureMode::Call.lanes(),
        vec![AudioLane::System],
    ] {
        if start_capture_stop(lanes.clone(), Duration::from_millis(200)).is_none() {
            println!(
                "SKIPPED live capture for {lanes:?}: no usable audio device here, which is \
                 expected on the CI runner; run the --ignored tests on a Windows machine"
            );
        }
    }
    // A stop without a start, and twice, is a no-op.
    let backend = LiveCaptureBackend::new();
    backend.stop();
    backend.stop();
}

#[test]
fn a_session_snapshot_answers() {
    let result = within(Duration::from_secs(20), "session snapshot", || {
        LiveProcessAudioActivity::new().snapshot()
    });
    match result {
        Ok(processes) => println!("{} processes with audio sessions", processes.len()),
        Err(error) => println!("session snapshot failed (no audio service?): {error}"),
    }
}

#[test]
fn session_notifications_start_and_stop() {
    within(Duration::from_secs(20), "session notifications", || {
        let source = LiveProcessAudioActivity::new();
        let changes = source.changes();
        // The registration thread sends once when it is registered.
        let first = changes.recv_timeout(Duration::from_secs(10));
        println!("first change notice: {first:?}");
        drop(source);
    });
}

#[test]
#[ignore = "needs a Windows machine with a microphone; run with -- --ignored --nocapture"]
fn in_person_capture_records_the_microphone() {
    let run = start_capture_stop(CaptureMode::InPerson.lanes(), Duration::from_secs(1))
        .expect("the in-person capture starts");
    assert!(run.callbacks > 0, "the microphone stream never delivered");
}

#[test]
#[ignore = "needs a Windows machine with a microphone and speakers, audio playing; run with -- --ignored --nocapture"]
fn call_capture_records_both_lanes() {
    let run = start_capture_stop(CaptureMode::Call.lanes(), Duration::from_secs(3))
        .expect("the call capture starts");
    assert!(run.callbacks > 0, "the microphone stream never delivered");
    println!(
        "system lane peak {:.4}: above zero means process loopback delivered what was playing",
        run.peaks[1]
    );
}

#[test]
#[ignore = "needs a Windows machine with a microphone; run with -- --ignored --nocapture"]
fn our_own_capture_shows_up_as_a_running_input_session() {
    let backend = LiveCaptureBackend::new();
    let lanes = CaptureMode::InPerson.lanes();
    let sink = Arc::new(LaneFrameSink::new(&lanes));
    backend
        .start(&lanes, None, sink)
        .expect("the in-person capture starts");
    std::thread::sleep(Duration::from_millis(500));
    let processes = LiveProcessAudioActivity::new()
        .snapshot()
        .expect("session snapshot");
    backend.stop();
    let own = i32::try_from(std::process::id()).unwrap();
    let ours = processes.iter().find(|p| p.pid == own);
    println!("our session: {ours:?}");
    assert!(
        ours.is_some_and(|p| p.is_running_input),
        "this process records, so its capture session is active: {processes:?}"
    );
}
