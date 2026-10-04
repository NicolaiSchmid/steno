//! The Windows live backend against the real WASAPI (WP10a).
//!
//! **Nobody has run the ignored tests on hardware yet.** The
//! `windows-latest` CI runner has no audio endpoint, so it records no
//! microphone; process loopback runs there all the same and delivers
//! silence. The `#[ignore]`d tests here are the live check a Windows
//! machine with a microphone and speakers must run before the backend
//! ships (the plan's parity list):
//!
//! ```text
//! cargo test -p steno-audio --test live_windows -- --ignored --nocapture
//! ```
//!
//! These tests print a record of each run and the backend's `info` logs
//! (which loopback runs, the follower's underrun, slip and trim counts at
//! stop); without `--nocapture` the harness swallows both. A non-empty
//! `RUST_LOG` replaces their `steno_audio=info` filter. Play audio during
//! `call_capture_records_both_lanes` (any media player) so the loopback has
//! something to deliver.
//!
//! The tests that are not ignored run on every Windows host, the CI runner
//! included: they check that the COM paths return within a bound with an
//! answer (a capture that fails with "no input device" is a pass) instead
//! of hanging or crashing. Each run happens on its own thread joined with
//! a deadline, so a hang fails instead of stalling the suite. On the CI
//! runner (`GITHUB_ACTIONS` set) the system-lane capture must also start,
//! deliver whole periods, stop dead and restart. CI runs them with
//! `--nocapture`, so its log shows what COM answered.
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

/// The backend's logs into the test output, at `info` unless a non-empty
/// `RUST_LOG` says otherwise.
fn show_logs() {
    let filter = std::env::var("RUST_LOG")
        .ok()
        .filter(|filter| !filter.is_empty())
        .unwrap_or_else(|| "steno_audio=info".into());
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new(filter))
        .with_test_writer()
        .try_init();
}

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
    /// Frames per lane delivered before `stop()` returned.
    available: usize,
    /// Frames per lane that arrived in the 300 ms after `stop()`.
    after_stop: usize,
    peaks: Vec<f32>,
}

/// Starts the backend for `lanes`, captures `duration`, stops, and returns
/// the callbacks counted in between (each completed callback signals the
/// sink's wake once), what arrived after the stop and each lane's peak, or
/// `None` when the start failed, which is printed. The same backend first
/// starts and stops `restarts` times, as the session's rebuild does, and
/// the run is the last one's.
fn start_capture_stop(lanes: Vec<AudioLane>, duration: Duration, restarts: usize) -> Option<Run> {
    show_logs();
    within(Duration::from_secs(60), "start, capture, stop", move || {
        let backend = LiveCaptureBackend::new();
        // Room for the whole capture: nothing overflows undrained.
        let seconds = duration.as_secs_f64() + 1.0;
        let sink = Arc::new(LaneFrameSink::with_handler(
            &lanes,
            steno_audio::SAMPLE_RATE,
            seconds,
            Box::new(|_| {}),
        ));
        for _ in 0..restarts {
            backend
                .start(&lanes, None, Arc::clone(&sink))
                .inspect_err(|error| println!("first start for {lanes:?} failed: {error}"))
                .ok()?;
            std::thread::sleep(duration);
            backend.stop();
            while sink.wake().try_take() {}
            sink.clear();
        }
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
        std::thread::sleep(Duration::from_millis(300));
        let after_stop = sink.available_to_read() - available;
        let peaks: Vec<f32> = (0..lanes.len())
            .map(|index| peak(&sink.ring(index).drain_all()))
            .collect();
        println!(
            "{callbacks} callbacks, {available} samples per lane, {after_stop} after stop, \
             dropped {:?}, peaks {peaks:?}",
            sink.dropped_samples()
        );
        Some(Run {
            callbacks,
            available,
            after_stop,
            peaks,
        })
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
        if start_capture_stop(lanes.clone(), Duration::from_millis(200), 0).is_none() {
            println!(
                "start for {lanes:?} failed (expected for the microphone lanes on the CI \
                 runner); run the --ignored tests on a Windows machine"
            );
        }
    }
    // A stop without a start, and twice, is a no-op.
    let backend = LiveCaptureBackend::new();
    backend.stop();
    backend.stop();
}

/// The CI runner has no endpoint, but process loopback runs there and
/// delivers a silent 10 ms period per callback, so the system-lane path is
/// checked for real: it starts, delivers whole periods, delivers nothing
/// once `stop()` has returned, and the same backend starts again, three
/// times over. Only where `GITHUB_ACTIONS` is set: elsewhere a machine may
/// have no audio service at all.
#[test]
fn on_the_ci_runner_a_system_capture_delivers_stops_and_restarts() {
    // One engine period, 10 ms at 48 kHz.
    const PERIOD: usize = 480;
    if std::env::var_os("GITHUB_ACTIONS").is_none() {
        println!("SKIPPED: the system-lane contract is checked on the CI runner only");
        return;
    }
    let run = start_capture_stop(vec![AudioLane::System], Duration::from_millis(300), 3)
        .expect("the system capture starts and restarts on the CI runner");
    assert!(run.callbacks > 0, "the system stream never delivered");
    assert_eq!(
        run.available,
        run.callbacks * PERIOD,
        "every callback carries one 10 ms period"
    );
    assert_eq!(run.after_stop, 0, "nothing arrives after stop()");
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
    let run = start_capture_stop(CaptureMode::InPerson.lanes(), Duration::from_secs(1), 1)
        .expect("the in-person capture starts and restarts");
    assert!(run.callbacks > 0, "the microphone stream never delivered");
    assert_eq!(run.after_stop, 0, "nothing arrives after stop()");
}

#[test]
#[ignore = "needs a Windows machine with a microphone and speakers, audio playing; run with -- --ignored --nocapture"]
fn call_capture_records_both_lanes() {
    let run = start_capture_stop(CaptureMode::Call.lanes(), Duration::from_secs(3), 0)
        .expect("the call capture starts");
    assert!(run.callbacks > 0, "the microphone stream never delivered");
    assert_eq!(run.after_stop, 0, "nothing arrives after stop()");
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
