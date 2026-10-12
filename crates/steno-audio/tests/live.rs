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
//! own on the aggregate's clock master before the aggregate's `IOProc`
//! (A10, see `capture::live::backend`): with nothing playing, the call
//! tests assert the first callback within 100 ms of `start` returning,
//! within 200 ms of the call to `start` while the machine is quiet, and
//! about one second of frames per second; without the silent output they
//! fail with no callback at all, and a `start` that fails fails them too.
//! Nothing may play during those runs.
//! Each run happens on its own thread joined with a deadline, so a hang
//! fails instead of stalling the suite.
//! Swift: `Tests/StenoAudioTests/LiveCaptureBackendTests.swift`.
#![cfg(target_os = "macos")]
// The docs name Core Audio's IOProc as the HAL spells it.
#![allow(clippy::doc_markdown)]

use std::sync::Arc;
use std::sync::mpsc::{RecvTimeoutError, channel};
use std::time::{Duration, Instant};

use steno_audio::capture::live::AudioDevices;
use steno_audio::{CaptureBackend, CaptureInput, LaneFrameSink, LiveCaptureBackend};
use steno_core::AudioLane;

/// Runs `work` on a thread and waits at most `limit` for its result; a
/// panic in `work` fails the test too.
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
        .unwrap_or_else(|error| match error {
            RecvTimeoutError::Timeout => panic!("{what} did not return within {limit:?}"),
            RecvTimeoutError::Disconnected => panic!("{what} panicked (its message is above)"),
        })
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
/// capture with nothing playing: what A10 changes.
const FIRST_CALLBACK: Duration = Duration::from_millis(100);

/// The bound on the first callback after the call to `start`, so that a
/// slower setup cannot hide behind [`FIRST_CALLBACK`]. Asserted only below
/// [`QUIET_LOAD`]: `start` itself took 54 to 100 ms on a quiet Mac and up to
/// 844 ms on a busy one.
const END_TO_END: Duration = Duration::from_millis(200);

/// The one-minute load average below which [`END_TO_END`] is asserted.
const QUIET_LOAD: f64 = 4.0;

/// The one-minute load average (`sysctl vm.loadavg`), `None` when it does
/// not read.
fn load_average() -> Option<f64> {
    let output = std::process::Command::new("sysctl")
        .args(["-n", "vm.loadavg"])
        .output()
        .ok()?;
    String::from_utf8(output.stdout)
        .ok()?
        .split_whitespace()
        .find_map(|field| field.parse().ok())
}

/// What one call capture measured.
struct CallRun {
    /// The first callback after `start` returned.
    first: Option<Duration>,
    /// How long `start` itself took.
    setup: Duration,
    /// Frames on every lane, in seconds at the stream's rate.
    seconds: f64,
    elapsed: Duration,
    /// The system lane's loudest sample.
    system_peak: f32,
}

/// Starts a call capture on `backend`, records `length` (drained every
/// 10 ms, so the rings never fill), stops, and prints what it measured.
/// A `start` that fails (the tap or the aggregate refused) fails the test.
#[allow(clippy::cast_precision_loss)]
fn call_run(backend: &LiveCaptureBackend, what: &str, length: Duration) -> CallRun {
    let lanes = [AudioLane::Mic, AudioLane::System];
    let sink = Arc::new(LaneFrameSink::new(&lanes));
    let starting = Instant::now();
    let stream = backend
        .start(&lanes, None, Arc::clone(&sink))
        .unwrap_or_else(|error| {
            panic!(
                "{what}: start failed after {:?}: {error}",
                starting.elapsed()
            )
        });
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
    CallRun {
        first,
        setup: started - starting,
        seconds,
        elapsed,
        system_peak,
    }
}

/// What a call capture with nothing playing must show: its first callback
/// within [`FIRST_CALLBACK`] of `start` returning, within [`END_TO_END`] of
/// the call to `start` while the load is under [`QUIET_LOAD`] (always
/// printed), and about as many seconds of frames as it ran.
fn assert_ran_from_the_start(run: &CallRun) {
    let first = run
        .first
        .expect("call mode's IOProc never ran with nothing playing");
    assert!(
        first <= FIRST_CALLBACK,
        "the first callback came {first:?} after start returned, more than {FIRST_CALLBACK:?}"
    );
    let end_to_end = run.setup + first;
    println!(
        "first callback {first:?} after start returned (bound {FIRST_CALLBACK:?}), \
         {end_to_end:?} after start was called (bound {END_TO_END:?})"
    );
    match load_average() {
        Some(load) if load < QUIET_LOAD => assert!(
            end_to_end <= END_TO_END,
            "the first callback came {end_to_end:?} after start was called, more than \
             {END_TO_END:?} at load {load}"
        ),
        Some(load) => println!("end-to-end bound not checked: load {load}"),
        None => println!("end-to-end bound not checked: load unknown"),
    }
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
/// (and [`END_TO_END`] of the call) and four seconds hold about four
/// seconds of frames on every lane. The system lane is all zeros too,
/// which over SSH proves nothing about its content (a session without the
/// capture grant gets a silent tap anyway).
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
    assert_ran_from_the_start(&run);
    assert_eq!(run.system_peak, 0.0, "the system lane is digital silence");
}

/// A rebuild as the session runs it after a device change, `stop()` and
/// `start` again on the same backend: the rebuilt capture starts its silent
/// output again and runs from its start too. A Mac with one output cannot
/// change it, so the device change itself is not exercised here.
#[test]
#[ignore = "needs a Mac with audio devices; run with -- --ignored --nocapture"]
fn a_rebuilt_call_capture_keeps_its_silent_output() {
    let (first, second) = within(
        Duration::from_secs(30),
        "two starts, captures, stops",
        || {
            let backend = LiveCaptureBackend::new();
            let first = call_run(&backend, "before the rebuild", Duration::from_secs(1));
            let second = call_run(&backend, "after the rebuild", Duration::from_secs(2));
            (first, second)
        },
    );
    assert_ran_from_the_start(&first);
    assert_ran_from_the_start(&second);
}

/// The session over the live backend, a call with nothing playing: a
/// stall's rebuild restarts the capture, the restarted stream delivers
/// within the session's wait for a first frame (`STALL_TIMEOUT`), so the
/// restart counts as run, and no further restart follows while it records.
/// Nothing may play during the run.
#[test]
#[ignore = "needs a Mac with audio devices; run with -- --ignored --nocapture"]
fn a_silent_call_restart_counts_as_run() {
    use steno_audio::{
        CaptureConfiguration, CaptureMode, CaptureNotice, CaptureSession, DeviceChangeReason,
    };

    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().to_owned();
    let (resumed_after, rest, result) =
        within(Duration::from_secs(40), "a session's rebuild", move || {
            let session =
                CaptureSession::new(CaptureConfiguration::new(CaptureMode::Call, &path)).unwrap();
            let notices = session.notices();
            session.start(uuid::Uuid::new_v4()).unwrap();
            std::thread::sleep(Duration::from_secs(2));
            let changed = Instant::now();
            session.device_changed(DeviceChangeReason::DeliveryStalled);
            let mut resumed_after = None;
            let mut rest = Vec::new();
            while let Ok(notice) = notices.recv_timeout(Duration::from_secs(10)) {
                match notice {
                    CaptureNotice::DeviceChanged(DeviceChangeReason::DeliveryStalled)
                        if resumed_after.is_none() => {}
                    CaptureNotice::DeviceResumed {
                        attempt: 1,
                        gap_seconds,
                    } if resumed_after.is_none() => {
                        resumed_after = Some((changed.elapsed(), gap_seconds));
                    }
                    other => rest.push(other),
                }
            }
            (resumed_after, rest, session.stop().unwrap())
        });
    let (after, gap) = resumed_after.expect("the restart resumed");
    println!(
        "restart resumed {:.3} s after the change, gap {gap:.3} s; then {rest:?}; duration {:.2} s, {} changes",
        after.as_secs_f64(),
        result.statistics.duration,
        result.statistics.device_changes
    );
    assert!(
        after < CaptureSession::STALL_TIMEOUT,
        "within the wait for a first frame"
    );
    assert!(rest.is_empty(), "no further restart: {rest:?}");
    assert_eq!(result.statistics.device_changes, 1);
}
