//! Synthetic backend to sink rings to processing thread to relay, without
//! a session or files, and the synthetic backend on a bare sink.
//! Swift: `Tests/StenoAudioTests/ProcessingThreadTests.swift`,
//! `SyntheticCaptureBackendTests.swift`.

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

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use steno_audio::capture::{CaptureBackend, CaptureError, CaptureStream};
use steno_audio::realtime::{
    FrameRelay, LaneFrameSink, LevelMeter, ProcessingConfiguration, ProcessingThread,
};
use steno_audio::testing::synthetic::SyntheticOptions;
use steno_audio::testing::{SyntheticCaptureBackend, SyntheticLane};
use steno_audio::{PassthroughEchoCanceller, SAMPLE_RATE};
use steno_core::{AudioLane, EchoCanceller};

/// Drains the relay on the test thread and returns every frame per channel.
fn drain(
    relay: &FrameRelay,
    channels: usize,
    frame_size: usize,
    expected_frames: usize,
) -> Vec<Vec<f32>> {
    let mut output = vec![Vec::new(); channels];
    let mut scratch = vec![0.0f32; frame_size];
    let mut frames = 0;
    let mut idle = 0;
    while frames < expected_frames && idle < 2_000 {
        if relay.available_frames() > 0 {
            for (channel, out) in output.iter_mut().enumerate() {
                relay.read(channel, &mut scratch);
                out.extend_from_slice(&scratch);
            }
            frames += 1;
            idle = 0;
        } else {
            idle += 1;
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    output
}

fn rms_decibels(samples: &[f32]) -> f32 {
    let sum: f64 = samples.iter().map(|s| f64::from(*s) * f64::from(*s)).sum();
    LevelMeter::decibels((sum / samples.len().max(1) as f64).sqrt() as f32)
}

fn upward_crossings(samples: &[f32]) -> usize {
    samples
        .windows(2)
        .filter(|w| w[0] < 0.0 && w[1] >= 0.0)
        .count()
}

fn signals(entries: &[(AudioLane, SyntheticLane)]) -> BTreeMap<AudioLane, SyntheticLane> {
    entries.iter().copied().collect()
}

#[test]
fn tones_flow_through_to_the_relay_without_drops() {
    let lanes = [AudioLane::Mic, AudioLane::System];
    let backend = SyntheticCaptureBackend::tones(
        &lanes,
        &[(AudioLane::Mic, 440.0), (AudioLane::System, 1_000.0)],
        1.0,
    );
    let sink = Arc::new(LaneFrameSink::new(&lanes));
    let relay = Arc::new(FrameRelay::new(2, 480, 400));
    let mut thread = ProcessingThread::new(
        Arc::clone(&sink),
        Arc::clone(&relay),
        ProcessingConfiguration::new(&lanes, None),
        None,
    );
    thread.start();
    backend.start(&lanes, None, Arc::clone(&sink)).unwrap();

    let output = drain(&relay, 2, 480, 100);
    backend.stop();
    let frames_processed = thread.frames_processed();
    let levels = Arc::clone(thread.levels());
    let system_peak = thread.system_peak();
    thread.stop();

    assert_eq!(output[0].len(), 48_000);
    assert_eq!(output[1].len(), 48_000);
    assert_eq!(backend.frames_delivered(), 48_000);
    assert_eq!(frames_processed, 100);
    assert!(sink.dropped_samples().is_empty());
    assert_eq!(relay.dropped_frames(), vec![0, 0]);
    // Amplitude 0.5 sines: -9.03 dBFS rms on both lanes.
    assert!((rms_decibels(&output[0]) - -9.03).abs() < 0.1);
    assert!((rms_decibels(&output[1]) - -9.03).abs() < 0.1);
    assert!(upward_crossings(&output[0]).abs_diff(440) <= 1);
    assert!(upward_crossings(&output[1]).abs_diff(1_000) <= 1);
    let current = levels.levels();
    assert!((current.mic.rms - -9.03).abs() < 0.2);
    assert!((current.system.unwrap().rms - -9.03).abs() < 0.2);
    assert_eq!(levels.current_generation(), 10, "10 Hz over one second");
    assert!((system_peak - 0.5).abs() < 0.01);
}

/// The session replaces the processing thread after a device change but
/// keeps the writer thread, which reads the slot it was given at start: a
/// replacement built with the first thread's slot publishes into that same
/// slot, generation counting on from where the first left off.
#[test]
fn a_replacement_thread_publishes_into_the_shared_level_slot() {
    let lanes = [AudioLane::Mic, AudioLane::System];
    let sink = Arc::new(LaneFrameSink::new(&lanes));
    let relay = Arc::new(FrameRelay::new(2, 480, 400));
    let mut first = ProcessingThread::new(
        Arc::clone(&sink),
        Arc::clone(&relay),
        ProcessingConfiguration::new(&lanes, None),
        None,
    );
    let fresh = ProcessingThread::new(
        Arc::clone(&sink),
        Arc::clone(&relay),
        ProcessingConfiguration::new(&lanes, None),
        None,
    );
    assert!(
        !Arc::ptr_eq(fresh.levels(), first.levels()),
        "without a slot every thread allocates its own"
    );
    drop(fresh);

    let backend = SyntheticCaptureBackend::tones(
        &lanes,
        &[(AudioLane::Mic, 440.0), (AudioLane::System, 1_000.0)],
        0.5,
    );
    first.start();
    backend.start(&lanes, None, Arc::clone(&sink)).unwrap();
    drain(&relay, 2, 480, 50);
    backend.stop();
    let slot = Arc::clone(first.levels());
    first.stop();
    assert_eq!(slot.current_generation(), 5, "10 Hz over half a second");

    let mut configuration = ProcessingConfiguration::new(&lanes, None);
    configuration.far_end_delay_frames = 960;
    let mut replacement = ProcessingThread::new(
        Arc::clone(&sink),
        Arc::clone(&relay),
        configuration,
        Some(Arc::clone(&slot)),
    );
    assert!(Arc::ptr_eq(replacement.levels(), &slot));
    let again = SyntheticCaptureBackend::tones(
        &lanes,
        &[(AudioLane::Mic, 440.0), (AudioLane::System, 1_000.0)],
        0.5,
    );
    replacement.start();
    again.start(&lanes, None, Arc::clone(&sink)).unwrap();
    drain(&relay, 2, 480, 50);
    again.stop();
    let frames = replacement.frames_processed();
    replacement.stop();
    assert_eq!(slot.current_generation(), 10, "the shared slot counts on");
    assert!((slot.levels().mic.rms - -9.03).abs() < 0.2);
    assert_eq!(frames, 50);
    assert!(sink.dropped_samples().is_empty());
    assert_eq!(relay.dropped_frames(), vec![0, 0]);
}

#[test]
fn passthrough_canceller_and_raw_mic_channel() {
    let lanes = [AudioLane::Mic, AudioLane::System];
    let backend = SyntheticCaptureBackend::new(SyntheticOptions::signals(
        signals(&[
            (AudioLane::Mic, SyntheticLane::new(300.0, 0.25)),
            (AudioLane::System, SyntheticLane::new(2_000.0, 0.5)),
        ]),
        0.5,
    ));
    let sink = Arc::new(LaneFrameSink::new(&lanes));
    let relay = Arc::new(FrameRelay::new(3, 480, 100));
    let mut configuration = ProcessingConfiguration::new(
        &lanes,
        Some(Box::new(PassthroughEchoCanceller::new(48_000.0, 480))),
    );
    configuration.far_end_delay_frames = 2_880;
    configuration.keep_raw_mic = true;
    let mut thread =
        ProcessingThread::new(Arc::clone(&sink), Arc::clone(&relay), configuration, None);
    thread.start();
    backend.start(&lanes, None, Arc::clone(&sink)).unwrap();
    let output = drain(&relay, 3, 480, 50);
    backend.stop();
    let levels = thread.levels().levels();
    thread.stop();
    assert_eq!(
        output[0], output[2],
        "passthrough leaves the mic identical to the raw copy"
    );
    assert!((rms_decibels(&output[0]) - -15.05).abs() < 0.1);
    assert!((rms_decibels(&output[1]) - -9.03).abs() < 0.1);
    assert!(levels.system.is_some());
}

#[test]
fn in_person_single_lane_meters_as_mic() {
    let lanes = [AudioLane::Mixed];
    let backend = SyntheticCaptureBackend::tones(&lanes, &[(AudioLane::Mixed, 500.0)], 0.3);
    let sink = Arc::new(LaneFrameSink::new(&lanes));
    let relay = Arc::new(FrameRelay::new(1, 480, 100));
    let mut thread = ProcessingThread::new(
        Arc::clone(&sink),
        Arc::clone(&relay),
        ProcessingConfiguration::new(&lanes, None),
        None,
    );
    thread.start();
    backend.start(&lanes, None, Arc::clone(&sink)).unwrap();
    let output = drain(&relay, 1, 480, 30);
    backend.stop();
    let levels = thread.levels().levels();
    let peak = thread.system_peak();
    thread.stop();
    assert_eq!(output[0].len(), 14_400);
    assert_eq!(levels.system, None);
    assert!((levels.mic.rms - -9.03).abs() < 0.2);
    assert_eq!(peak, 0.0);
}

#[test]
fn a_stalled_writer_is_counted_not_waited_for() {
    let lanes = [AudioLane::Mixed];
    let backend = SyntheticCaptureBackend::tones(&lanes, &[(AudioLane::Mixed, 500.0)], 1.0);
    let sink = Arc::new(LaneFrameSink::new(&lanes));
    // Room for 20 frames only and nobody draining: 80 of 100 frames are refused.
    let relay = Arc::new(FrameRelay::new(1, 480, 20));
    let mut thread = ProcessingThread::new(
        Arc::clone(&sink),
        Arc::clone(&relay),
        ProcessingConfiguration::new(&lanes, None),
        None,
    );
    thread.start();
    backend.start(&lanes, None, Arc::clone(&sink)).unwrap();
    backend.wait_until_finished();
    backend.stop();
    // `stop` drains the whole frames still in the rings before it returns.
    thread.stop();
    // The ring rounds up to a power of two, so the exact headroom is derived.
    let headroom = relay.ring_capacity_frames();
    assert!(headroom >= 20);
    assert_eq!(thread.frames_processed(), 100);
    assert_eq!(relay.dropped_frames(), vec![100 - headroom]);
    assert_eq!(relay.available_frames(), headroom);
}

/// Records every near-end and far-end frame it is given and negates the
/// near-end into the output, so the relay shows which channel the
/// canceller wrote and which it left alone.
#[derive(Default)]
struct SpyLog {
    near_end: Vec<f32>,
    far_end: Vec<f32>,
    calls: usize,
}

struct SpyEchoCanceller {
    log: Arc<Mutex<SpyLog>>,
}

impl EchoCanceller for SpyEchoCanceller {
    fn process(&mut self, near_end: &[f32], far_end: &[f32], out: &mut [f32]) {
        let mut log = self.log.lock().unwrap();
        log.near_end.extend_from_slice(near_end);
        log.far_end.extend_from_slice(far_end);
        log.calls += 1;
        for (o, n) in out.iter_mut().zip(near_end) {
            *o = -n;
        }
    }
}

/// AEC routing: the canceller's near-end is the mic lane, its far-end the
/// system lane of the same frame, its output the written mic channel; the
/// system lane and the raw mic copy are untouched.
#[test]
fn the_canceller_sees_the_mic_as_near_end_and_the_same_frames_system_as_far_end() {
    let lanes = [AudioLane::Mic, AudioLane::System];
    let backend = SyntheticCaptureBackend::new(
        SyntheticOptions::signals(
            signals(&[
                (AudioLane::Mic, SyntheticLane::new(300.0, 0.25)),
                (AudioLane::System, SyntheticLane::new(2_000.0, 0.5)),
            ]),
            0.5,
        )
        .callback_frames(333),
    );
    let sink = Arc::new(LaneFrameSink::new(&lanes));
    let relay = Arc::new(FrameRelay::new(3, 480, 100));
    let log = Arc::new(Mutex::new(SpyLog::default()));
    let mut configuration = ProcessingConfiguration::new(
        &lanes,
        Some(Box::new(SpyEchoCanceller {
            log: Arc::clone(&log),
        })),
    );
    configuration.keep_raw_mic = true;
    let mut thread =
        ProcessingThread::new(Arc::clone(&sink), Arc::clone(&relay), configuration, None);
    thread.start();
    backend.start(&lanes, None, Arc::clone(&sink)).unwrap();
    let output = drain(&relay, 3, 480, 50);
    backend.stop();
    thread.stop();

    let log = log.lock().unwrap();
    assert_eq!(log.calls, 50, "one call per 10 ms frame");
    assert_eq!(
        log.near_end, output[2],
        "near-end is the raw mic lane, frame for frame"
    );
    assert_eq!(
        log.far_end, output[1],
        "far-end is the system lane of the same frame"
    );
    assert_eq!(output[0], output[2].iter().map(|s| -s).collect::<Vec<_>>());
    assert!(
        (rms_decibels(&output[1]) - -9.03).abs() < 0.1,
        "the system lane is what arrived"
    );
    assert!(
        (rms_decibels(&output[2]) - -15.05).abs() < 0.1,
        "the raw mic is what arrived"
    );
    assert_ne!(output[1], output[2]);
}

/// With a device latency above 100 ms the far-end is the system lane
/// delayed by that many samples: zeros first, then the system lane shifted.
#[test]
fn the_far_end_delay_line_shifts_the_system_lane_by_samples() {
    let lanes = [AudioLane::Mic, AudioLane::System];
    let delay = 1_000;
    let backend = SyntheticCaptureBackend::new(SyntheticOptions::signals(
        signals(&[
            (AudioLane::Mic, SyntheticLane::new(300.0, 0.25)),
            (AudioLane::System, SyntheticLane::new(2_000.0, 0.5)),
        ]),
        0.5,
    ));
    let sink = Arc::new(LaneFrameSink::new(&lanes));
    let relay = Arc::new(FrameRelay::new(2, 480, 100));
    let log = Arc::new(Mutex::new(SpyLog::default()));
    let mut configuration = ProcessingConfiguration::new(
        &lanes,
        Some(Box::new(SpyEchoCanceller {
            log: Arc::clone(&log),
        })),
    );
    configuration.far_end_delay_frames = delay;
    let mut thread =
        ProcessingThread::new(Arc::clone(&sink), Arc::clone(&relay), configuration, None);
    thread.start();
    backend.start(&lanes, None, Arc::clone(&sink)).unwrap();
    let output = drain(&relay, 2, 480, 50);
    backend.stop();
    thread.stop();

    let log = log.lock().unwrap();
    assert_eq!(log.far_end.len(), 24_000);
    assert!(
        log.far_end[..delay].iter().all(|s| *s == 0.0),
        "primed with zeros"
    );
    assert_eq!(log.far_end[delay..], output[1][..24_000 - delay]);
    assert_eq!(log.near_end.len(), 24_000);
    assert_eq!(
        output[0],
        log.near_end.iter().map(|s| -s).collect::<Vec<_>>()
    );
}

#[test]
fn synthetic_echo_is_a_delayed_copy() {
    let lanes = [AudioLane::Mic, AudioLane::System];
    let backend = SyntheticCaptureBackend::new(
        SyntheticOptions::signals(
            signals(&[
                (
                    AudioLane::Mic,
                    SyntheticLane::new(0.0, 0.0).with_echo(AudioLane::System, 0.060, 0.5),
                ),
                (AudioLane::System, SyntheticLane::new(1_000.0, 0.5)),
            ]),
            0.5,
        )
        .callback_frames(333),
    );
    let sink = Arc::new(LaneFrameSink::new(&lanes));
    let relay = Arc::new(FrameRelay::new(2, 480, 100));
    let mut thread = ProcessingThread::new(
        Arc::clone(&sink),
        Arc::clone(&relay),
        ProcessingConfiguration::new(&lanes, None),
        None,
    );
    thread.start();
    backend.start(&lanes, None, Arc::clone(&sink)).unwrap();
    let output = drain(&relay, 2, 480, 50);
    backend.stop();
    thread.stop();
    let delay = 2_880;
    assert!(
        output[0][..delay].iter().all(|s| *s == 0.0),
        "silent until the delay elapsed"
    );
    let mut max_error = 0.0f32;
    for index in delay..24_000 {
        max_error = max_error.max((output[0][index] - 0.5 * output[1][index - delay]).abs());
    }
    assert!(max_error < 1e-4);
}

/// The synthetic backend on a bare sink: the device change fires once per
/// instance, every `start` delivers its own `seconds`, and a restart
/// reports `stream_after_restart`.
#[test]
fn a_change_fires_once_per_instance_and_every_start_delivers_its_seconds() {
    let reports = Arc::new(AtomicUsize::new(0));
    let sink = {
        let reports = Arc::clone(&reports);
        Arc::new(LaneFrameSink::with_handler(
            &[AudioLane::Mixed],
            SAMPLE_RATE,
            2.0,
            Box::new(move |_| {
                reports.fetch_add(1, Ordering::Relaxed);
            }),
        ))
    };
    let restarted = CaptureStream {
        sample_rate: SAMPLE_RATE,
        input_latency_frames: 480,
        output_latency_frames: 9_600,
        layout: None,
    };
    let backend = SyntheticCaptureBackend::new(
        SyntheticOptions::tones(&[AudioLane::Mixed], &[(AudioLane::Mixed, 440.0)], 0.2)
            .change_device_after(0.1)
            .restarts_that_fail(1)
            .stream_after_restart(restarted.clone()),
    );
    assert_eq!(
        backend
            .start(&[AudioLane::Mixed], None, Arc::clone(&sink))
            .unwrap(),
        CaptureStream::SYNTHETIC
    );
    backend.wait_until_finished();
    assert_eq!(reports.load(Ordering::Relaxed), 1);
    assert_eq!(backend.frames_delivered(), 4_800, "clipped to the change");
    backend.stop();
    sink.clear();

    assert_eq!(
        backend
            .start(&[AudioLane::Mixed], None, Arc::clone(&sink))
            .unwrap_err(),
        CaptureError::InputDeviceUnavailable
    );
    assert_eq!(backend.starts(), 2);

    // The session rearms the latch before a restart; a change that fired
    // again would be counted here.
    sink.rearm_device_change();
    assert_eq!(
        backend
            .start(&[AudioLane::Mixed], None, Arc::clone(&sink))
            .unwrap(),
        restarted
    );
    backend.wait_until_finished();
    assert_eq!(
        reports.load(Ordering::Relaxed),
        1,
        "the one change is spent"
    );
    assert_eq!(
        backend.frames_delivered(),
        4_800 + 9_600,
        "the restart delivers its full 0.2 s"
    );
    assert_eq!(backend.starts(), 3);
    backend.stop();
}
