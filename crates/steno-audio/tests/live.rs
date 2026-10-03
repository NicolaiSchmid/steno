//! The macOS live backend against the real HAL, ignored by default: these
//! need a Mac with audio devices and run with `cargo test -p steno-audio
//! -- --ignored`. Over SSH the tap and the microphone deliver silence (a
//! session without a GUI gets no TCC grant), which is fine: what these
//! assert is that enumeration returns, that `start` and `stop` return
//! within a bound, and that nothing hangs. Each run happens on its own
//! thread joined with a deadline, so a hang fails instead of stalling the
//! suite. What came back is printed for the record.
//! Swift: `Tests/StenoAudioTests/LiveCaptureBackendTests.swift`.
#![cfg(target_os = "macos")]

use std::sync::Arc;
use std::sync::mpsc::channel;
use std::time::{Duration, Instant};

use steno_audio::capture::live::AudioDevices;
use steno_audio::{CaptureBackend, LaneFrameSink, LiveCaptureBackend};
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

#[test]
#[ignore = "needs a Mac with audio devices; run with -- --ignored"]
fn device_enumeration_returns() {
    let devices = within(
        Duration::from_secs(10),
        "AudioDevices::all",
        AudioDevices::all,
    )
    .expect("device enumeration");
    for device in &devices {
        println!(
            "{} in / {} out, {} Hz, {}: {} ({})",
            device.input_channels,
            device.output_channels,
            device.nominal_sample_rate,
            device.transport_type,
            device.name,
            device.uid
        );
    }
    println!(
        "default input: {:?}",
        AudioDevices::default_input().map(|d| d.name)
    );
    println!(
        "default system output: {:?}",
        AudioDevices::default_system_output().map(|d| d.name)
    );
}

/// Starts the backend for `lanes`, captures half a second, stops. A start
/// that fails (no input device on a headless Mac, a tap refused without
/// the capture permission) is printed, not failed: the test's claim is
/// that every call returns within the bound.
fn start_capture_stop(lanes: &'static [AudioLane]) {
    within(Duration::from_secs(30), "start, capture, stop", move || {
        let backend = LiveCaptureBackend::new();
        let sink = Arc::new(LaneFrameSink::new(lanes));
        let started = Instant::now();
        let stream = match backend.start(lanes, None, Arc::clone(&sink)) {
            Ok(stream) => stream,
            Err(error) => {
                println!(
                    "start for {lanes:?} failed after {:?}: {error}",
                    started.elapsed()
                );
                return;
            }
        };
        println!("started {lanes:?} in {:?}: {stream:?}", started.elapsed());
        std::thread::sleep(Duration::from_millis(500));
        let available = sink.available_to_read();
        let peaks: Vec<String> = lanes
            .iter()
            .enumerate()
            .map(|(index, lane)| {
                format!(
                    "{} peak {:.4}",
                    lane.as_str(),
                    peak(&sink.ring(index).drain_all())
                )
            })
            .collect();
        println!(
            "{available} samples per lane after 500 ms, dropped {:?}; {} (silence is expected without a GUI session)",
            sink.dropped_samples(),
            peaks.join(", ")
        );
        let stopping = Instant::now();
        backend.stop();
        println!("stopped in {:?}", stopping.elapsed());
    });
}

#[test]
#[ignore = "needs a Mac with audio devices; run with -- --ignored"]
fn in_person_capture_starts_and_stops_within_bounds() {
    start_capture_stop(&[AudioLane::Mixed]);
}

#[test]
#[ignore = "needs a Mac with audio devices; run with -- --ignored"]
fn call_capture_starts_and_stops_within_bounds() {
    start_capture_stop(&[AudioLane::Mic, AudioLane::System]);
}
