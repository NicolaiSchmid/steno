//! The macOS live backend against the real HAL, ignored by default: these
//! need a Mac with audio devices and run with `cargo test -p steno-audio
//! --test live -- --ignored --nocapture` (without `--nocapture` the record
//! they print is swallowed). Over SSH the tap and the microphone deliver
//! silence (a session without a GUI gets no TCC grant), which is fine: what
//! these assert is that enumeration returns, that `start` and `stop` return
//! within a bound, that nothing hangs, that the in-person `IOProc` runs, and
//! that a UID naming no device records the default input.
//! Call mode's tap aggregate runs only while a process the tap includes
//! drives the output, so the capture starts a silent output `IOProc` of its
//! own first (A10, see `capture::live::backend`): with nothing playing, the
//! call tests assert the first callback within 100 ms of the start and
//! about one second of frames per second; without the silent output
//! they fail with no callback at all. Nothing may play during those runs.
//! Each run happens on its own thread joined with a deadline, so a hang
//! fails instead of stalling the suite.
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

/// What one call capture measured.
struct CallRun {
    /// The first callback after `start` returned.
    first: Option<Duration>,
    /// Frames on every lane, in seconds at the stream's rate.
    seconds: f64,
    elapsed: Duration,
    /// The system lane's loudest sample.
    system_peak: f32,
}

/// Starts a call capture on `backend`, records `length` (drained every
/// 10 ms, so the rings never fill), stops, and prints what it measured.
/// `None` when the start failed (printed, not failed).
#[allow(clippy::cast_precision_loss)]
fn call_run(backend: &LiveCaptureBackend, what: &str, length: Duration) -> Option<CallRun> {
    let lanes = [AudioLane::Mic, AudioLane::System];
    let sink = Arc::new(LaneFrameSink::new(&lanes));
    let starting = Instant::now();
    let stream = match backend.start(&lanes, None, Arc::clone(&sink)) {
        Ok(stream) => stream,
        Err(error) => {
            println!(
                "SKIPPED {what}: start failed after {:?}: {error}",
                starting.elapsed()
            );
            return None;
        }
    };
    let started = Instant::now();
    println!("{what}: started in {:?}: {stream:?}", started - starting);
    let mut first = None;
    let mut callbacks = 0usize;
    let mut frames = 0usize;
    let mut system_peak = 0.0f32;
    let mut scratch = vec![0.0f32; 4_800];
    while started.elapsed() < length {
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
            for (index, lane) in lanes.iter().enumerate() {
                sink.ring(index).read(&mut scratch[..available]);
                if *lane == AudioLane::System {
                    system_peak = system_peak.max(peak(&scratch[..available]));
                }
            }
            frames += available;
        }
    }
    let elapsed = started.elapsed();
    let stopping = Instant::now();
    backend.stop();
    let seconds = frames as f64 / stream.sample_rate;
    println!(
        "{what}: first callback {first:?} after start returned ({:?} after it was called), \
         {callbacks} callbacks, {frames} frames ({seconds:.2} s at {} Hz) in {elapsed:?}, \
         system peak {system_peak}, dropped {:?}; stopped in {:?}",
        first.map(|first| first + (started - starting)),
        stream.sample_rate,
        sink.dropped_samples(),
        stopping.elapsed()
    );
    Some(CallRun {
        first,
        seconds,
        elapsed,
        system_peak,
    })
}

/// What a call capture with nothing playing must show: its first callback
/// within [`FIRST_CALLBACK`] of the start and about as many seconds of
/// frames as it ran.
fn assert_ran_from_the_start(run: &CallRun) {
    let first = run
        .first
        .expect("call mode's IOProc never ran with nothing playing");
    assert!(
        first <= FIRST_CALLBACK,
        "the first callback came {first:?} after the start, more than {FIRST_CALLBACK:?}"
    );
    assert!(
        run.seconds >= run.elapsed.as_secs_f64() - 0.2,
        "{:.2} s of frames in {:?}",
        run.seconds,
        run.elapsed
    );
}

/// Plays nothing itself, and nothing else may play during the run: the
/// call capture's own silent output keeps its tap aggregate running, so
/// the first callback comes within [`FIRST_CALLBACK`] of `start` returning
/// and four seconds hold about four seconds of frames on every lane. The
/// system lane is all zeros too, which over SSH proves nothing about its
/// content (a session without the capture grant gets a silent tap anyway);
/// it does show the silent output itself reaches it as zeros.
#[test]
#[ignore = "needs a Mac with audio devices; run with -- --ignored --nocapture"]
fn call_capture_runs_from_its_start_with_nothing_playing() {
    let run = within(Duration::from_secs(30), "start, capture, stop", || {
        call_run(
            &LiveCaptureBackend::new(),
            "call capture",
            Duration::from_secs(4),
        )
    });
    let Some(run) = run else {
        return;
    };
    assert_ran_from_the_start(&run);
    assert_eq!(run.system_peak, 0.0, "the system lane is digital silence");
}

/// A rebuild as the session runs it after a device change, `stop()` and
/// `start` again on the same backend: the rebuilt capture starts its silent
/// output again and runs from its start too. Forge cannot change its
/// default output (built-in speakers only), so the device change itself is
/// not exercised here.
#[test]
#[ignore = "needs a Mac with audio devices; run with -- --ignored --nocapture"]
fn a_rebuilt_call_capture_keeps_its_silent_output() {
    let runs = within(
        Duration::from_secs(30),
        "two starts, captures, stops",
        || {
            let backend = LiveCaptureBackend::new();
            let first = call_run(&backend, "before the rebuild", Duration::from_secs(1))?;
            let second = call_run(&backend, "after the rebuild", Duration::from_secs(2))?;
            Some((first, second))
        },
    );
    let Some((first, second)) = runs else {
        return;
    };
    assert_ran_from_the_start(&first);
    assert_ran_from_the_start(&second);
}
