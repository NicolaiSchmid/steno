//! The macOS live backend against the real HAL, ignored by default: these
//! need a Mac with audio devices and run with `cargo test -p steno-audio
//! --test live -- --ignored --nocapture` (without `--nocapture` the record
//! they print is swallowed). Over SSH the tap and the microphone deliver
//! silence (a session without a GUI gets no TCC grant), which is fine: what
//! these assert is that enumeration returns, that `start` and `stop` return
//! within a bound, that nothing hangs, and that the in-person `IOProc` runs.
//! Call mode's `IOProc` runs only while another client has the output device
//! open (see `capture::live::backend`), so a call capture with no callbacks
//! is reported as skipped rather than as silence. Each run happens on its
//! own thread joined with a deadline, so a hang fails instead of stalling
//! the suite.
//! Swift: `Tests/StenoAudioTests/LiveCaptureBackendTests.swift`.
#![cfg(target_os = "macos")]
// The docs name Core Audio's IOProc as the HAL spells it.
#![allow(clippy::doc_markdown)]

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

/// Starts the backend for `lanes`, captures half a second, stops, and
/// returns the IOProc callbacks counted in between (each completed callback
/// signals the sink's wake once), or `None` when the start failed. A start
/// that fails (no input device on a headless Mac, a tap refused without the
/// capture permission) is printed, not failed. Zero callbacks means the
/// IOProc never ran; callbacks with a zero peak mean it ran silent.
fn start_capture_stop(lanes: &'static [AudioLane]) -> Option<usize> {
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
                return None;
            }
        };
        println!("started {lanes:?} in {:?}: {stream:?}", started.elapsed());
        std::thread::sleep(Duration::from_millis(500));
        let mut callbacks = 0;
        while sink.wake().try_take() {
            callbacks += 1;
        }
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
            "{callbacks} callbacks, {available} samples per lane after 500 ms, dropped {:?}; {} (silence is expected without a GUI session)",
            sink.dropped_samples(),
            peaks.join(", ")
        );
        let stopping = Instant::now();
        backend.stop();
        println!("stopped in {:?}", stopping.elapsed());
        Some(callbacks)
    })
}

#[test]
#[ignore = "needs a Mac with audio devices; run with -- --ignored"]
fn in_person_capture_starts_and_stops_within_bounds() {
    if let Some(callbacks) = start_capture_stop(&[AudioLane::Mixed]) {
        assert!(callbacks > 0, "the microphone's IOProc never ran");
    }
}

/// Plays nothing itself: with no other client on the output device the
/// IOProc does not run, and the test says so instead of passing as if it
/// had captured silence. Play something during the run (`afplay`) to see
/// callbacks.
#[test]
#[ignore = "needs a Mac with audio devices; run with -- --ignored"]
fn call_capture_starts_and_stops_within_bounds() {
    if start_capture_stop(&[AudioLane::Mic, AudioLane::System]) == Some(0) {
        println!(
            "SKIPPED call capture: the IOProc never ran in 500 ms. The tap aggregate runs only \
             while another client has the output device open; play something during the test \
             to exercise it."
        );
    }
}
