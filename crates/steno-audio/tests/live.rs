//! The macOS live backend against the real HAL, ignored by default: these
//! need a Mac with audio devices and run with `cargo test -p steno-audio
//! --test live -- --ignored --nocapture` (without `--nocapture` the record
//! they print is swallowed). Over SSH the tap and the microphone deliver
//! silence (a session without a GUI gets no TCC grant), which is fine: what
//! these assert is that enumeration returns, that `start` and `stop` return
//! within a bound, that nothing hangs, that the in-person `IOProc` runs, and
//! that a UID naming no device records the default input.
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
use steno_audio::{CaptureBackend, CaptureInput, LaneFrameSink, LiveCaptureBackend};
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
#[ignore = "needs a Mac with audio devices; run with -- --ignored --nocapture"]
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

/// A UID that names no device (a PipeWire node name from a settings file
/// synced from Linux) records the default input and says so; the default's
/// own UID records it as chosen.
#[test]
#[ignore = "needs a Mac with audio devices; run with -- --ignored --nocapture"]
fn an_unknown_microphone_records_the_default_input() {
    within(Duration::from_secs(30), "start, capture, stop", || {
        let Ok(default) = AudioDevices::default_input() else {
            println!("SKIPPED: no default input");
            return;
        };
        let lanes = [AudioLane::Mixed];
        let backend = LiveCaptureBackend::new();
        let sink = Arc::new(LaneFrameSink::new(&lanes));
        let stream = backend
            .start(
                &lanes,
                Some("alsa_input.pci-0000_00_1f.3.analog-stereo"),
                Arc::clone(&sink),
            )
            .expect("the default input as the fallback");
        println!("{stream:?}");
        assert_eq!(
            stream.input,
            Some(CaptureInput {
                uid: default.uid.clone(),
                name: Some(default.name.clone()),
                is_fallback: true,
            })
        );
        std::thread::sleep(Duration::from_millis(500));
        assert!(sink.available_to_read() > 0, "the default input runs");
        backend.stop();
        let stream = backend
            .start(&lanes, Some(&default.uid), sink)
            .expect("the default by its UID");
        assert_eq!(
            stream.input.map(|input| input.is_fallback),
            Some(false),
            "chosen, not the fallback"
        );
        backend.stop();
    });
}

#[test]
#[ignore = "needs a Mac with audio devices; run with -- --ignored --nocapture"]
fn in_person_capture_starts_and_stops_within_bounds() {
    if let Some(callbacks) = start_capture_stop(&[AudioLane::Mixed]) {
        assert!(callbacks > 0, "the microphone's IOProc never ran");
    }
}

/// The bound on the first callback after `start` returns, for a call
/// capture with nothing playing.
const FIRST_CALLBACK: Duration = Duration::from_millis(100);

/// How long the call capture records.
const CALL_SECONDS: u64 = 4;

/// Plays nothing itself, and nothing else may play during the run: call
/// mode's IOProc zero-fills the aggregate's output, so the capture is the
/// output device's client and runs from its start. The first callback
/// comes within [`FIRST_CALLBACK`] of `start` returning, and four seconds
/// of capture hold about four seconds of frames on every lane (drained
/// every 10 ms, so the rings never fill).
#[test]
#[ignore = "needs a Mac with audio devices; run with -- --ignored --nocapture"]
fn call_capture_runs_from_its_start_with_nothing_playing() {
    let lanes: &'static [AudioLane] = &[AudioLane::Mic, AudioLane::System];
    let measured = within(Duration::from_secs(30), "start, capture, stop", move || {
        let backend = LiveCaptureBackend::new();
        let sink = Arc::new(LaneFrameSink::new(lanes));
        let starting = Instant::now();
        let stream = match backend.start(lanes, None, Arc::clone(&sink)) {
            Ok(stream) => stream,
            Err(error) => {
                println!("SKIPPED call capture: start failed after {:?}: {error}", starting.elapsed());
                return None;
            }
        };
        let started = Instant::now();
        println!("started {lanes:?} in {:?}: {stream:?}", started - starting);
        let mut first = None;
        let mut callbacks = 0usize;
        let mut frames = 0usize;
        let mut scratch = vec![0.0f32; 4_800];
        while started.elapsed() < Duration::from_secs(CALL_SECONDS) {
            if sink.wake().wait(Duration::from_millis(10)) {
                callbacks += 1;
                first.get_or_insert_with(|| started.elapsed());
            }
            // The lanes move together, so the first ring counts the frames.
            loop {
                let available = sink.available_to_read().min(scratch.len());
                if available == 0 {
                    break;
                }
                for lane in 0..lanes.len() {
                    sink.ring(lane).read(&mut scratch[..available]);
                }
                frames += available;
            }
        }
        let elapsed = started.elapsed();
        let stopping = Instant::now();
        backend.stop();
        println!(
            "first callback {first:?} after start returned, {callbacks} callbacks, {frames} frames \
             ({:.2} s at {} Hz) in {elapsed:?}, dropped {:?}; stopped in {:?}",
            frames as f64 / stream.sample_rate,
            stream.sample_rate,
            sink.dropped_samples(),
            stopping.elapsed()
        );
        Some((first, frames as f64 / stream.sample_rate, elapsed))
    });
    let Some((first, seconds, elapsed)) = measured else {
        return;
    };
    let first = first.expect("call mode's IOProc never ran with nothing playing");
    assert!(
        first <= FIRST_CALLBACK,
        "the first callback came {first:?} after the start, more than {FIRST_CALLBACK:?}"
    );
    assert!(
        seconds >= elapsed.as_secs_f64() - 0.2,
        "{seconds:.2} s of frames in {elapsed:?}"
    );
}
