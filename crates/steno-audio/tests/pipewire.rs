//! The Linux live backend against a real `pipewire` daemon, ignored by
//! default. Run it inside the headless harness, which starts a private
//! daemon and `wireplumber` with the test devices these tests name:
//!
//! ```text
//! scripts/pipewire-headless.sh cargo test -p steno-audio --test pipewire \
//!   -- --ignored --test-threads=1 --nocapture
//! ```
//!
//! Tones go in with `pw-play`: into `steno-test-sink` through the session
//! manager's own linking, into the virtual microphone `steno-test-mic` by a
//! stream the session manager leaves alone and `pw-link` (`wireplumber`
//! links playback only into sinks). Each start runs on its own thread
//! joined with a deadline, so a hang fails instead of stalling the suite.
//! The tests share one daemon, so they run one at a time and put back what
//! they move.
//!
//! The real-time promise is counted on the real thread here: libpipewire
//! runs the stream's `process` on its data-loop thread, which this file
//! finds in `/proc/self/task` and counts with
//! `CountingAllocator::allocations_on` for a second of cycles.
#![cfg(target_os = "linux")]
// Test arithmetic: sample counts and frequencies cast freely.
#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::doc_markdown
)]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use steno_audio::capture::live::pipewire::COALESCE_DELAY;
use steno_audio::capture::{ChannelRef, DeviceChangeReason};
use steno_audio::testing::rt::CountingAllocator;
use steno_audio::{
    CaptureBackend, CaptureError, CaptureStream, LaneFrameSink, LiveCaptureBackend, SAMPLE_RATE,
};
use steno_core::AudioLane;

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

const SINK: &str = "steno-test-sink";
const SECOND_SINK: &str = "steno-test-sink-2";
const MIC: &str = "steno-test-mic";
const SINK_TONE: f64 = 440.0;
const MIC_TONE: f64 = 1_000.0;

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

/// Starts `backend` on its own thread with a deadline.
fn start(
    backend: &Arc<LiveCaptureBackend>,
    lanes: &[AudioLane],
    uid: Option<&str>,
    sink: &Arc<LaneFrameSink>,
) -> Result<CaptureStream, CaptureError> {
    let (backend, lanes, uid, sink) = (
        Arc::clone(backend),
        lanes.to_vec(),
        uid.map(str::to_owned),
        Arc::clone(sink),
    );
    within(Duration::from_secs(10), "start", move || {
        backend.start(&lanes, uid.as_deref(), sink)
    })
}

/// Stops `backend` on its own thread with a deadline.
fn stop(backend: &Arc<LiveCaptureBackend>) {
    let backend = Arc::clone(backend);
    within(Duration::from_secs(5), "stop", move || backend.stop());
}

/// Runs a PipeWire tool to completion; whether it succeeded.
fn tool(program: &str, args: &[&str]) -> bool {
    Command::new(program)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// A 16-bit mono 48 kHz sine of `seconds` as a WAV file.
fn write_tone(path: &Path, frequency: f64, seconds: f64) {
    let frames = (seconds * SAMPLE_RATE) as usize;
    let mut bytes = Vec::with_capacity(44 + frames * 2);
    let data_size = (frames * 2) as u32;
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_size).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    for field in [16u32.to_le_bytes(), [1, 0, 1, 0]] {
        bytes.extend_from_slice(&field);
    }
    bytes.extend_from_slice(&48_000u32.to_le_bytes());
    bytes.extend_from_slice(&96_000u32.to_le_bytes());
    bytes.extend_from_slice(&[2, 0, 16, 0]);
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_size.to_le_bytes());
    for index in 0..frames {
        let phase = std::f64::consts::TAU * frequency * index as f64 / SAMPLE_RATE;
        bytes.extend_from_slice(&((phase.sin() * 12_000.0) as i16).to_le_bytes());
    }
    std::fs::write(path, bytes).expect("write the tone");
}

/// A `pw-play` of a tone, killed when dropped.
struct Tone {
    child: Child,
    _dir: tempfile::TempDir,
}

impl Tone {
    fn file(frequency: f64) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("tone.wav");
        write_tone(&path, frequency, 30.0);
        (dir, path)
    }

    /// Into a sink, linked by WirePlumber.
    fn into_sink(sink: &str, frequency: f64) -> Self {
        let (dir, path) = Self::file(frequency);
        let child = Command::new("pw-play")
            .args(["--target", sink])
            .arg(&path)
            .spawn()
            .expect("pw-play");
        Self { child, _dir: dir }
    }

    /// Into a virtual source's input, linked here: WirePlumber links
    /// playback only into sinks.
    fn into_source(source: &str, frequency: f64) -> Self {
        let (dir, path) = Self::file(frequency);
        let name = format!("steno-test-tone-{frequency}");
        let child = Command::new("pw-play")
            .args([
                "--properties",
                &format!("{{ node.name = {name} node.autoconnect = false }}"),
            ])
            .arg(&path)
            .spawn()
            .expect("pw-play");
        let tone = Self { child, _dir: dir };
        let output = format!("{name}:output_MONO");
        let input = format!("{source}:input_MONO");
        let deadline = Instant::now() + Duration::from_secs(5);
        while !tool("pw-link", &[&output, &input]) {
            assert!(
                Instant::now() < deadline,
                "could not link {output} to {input}"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        tone
    }
}

impl Drop for Tone {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The kernel id of PipeWire's data-loop thread in this process, where the
/// stream's `process` runs (`data-loop.0` since PipeWire 0.3.x).
fn data_loop_thread() -> usize {
    let mut names = Vec::new();
    for entry in std::fs::read_dir("/proc/self/task").expect("/proc/self/task") {
        let entry = entry.expect("task entry");
        let name = std::fs::read_to_string(entry.path().join("comm")).unwrap_or_default();
        let name = name.trim().to_owned();
        if name.contains("data-loop") {
            return entry
                .file_name()
                .to_string_lossy()
                .parse()
                .expect("a thread id");
        }
        names.push(name);
    }
    panic!("no PipeWire data-loop thread among {names:?}");
}

/// Goertzel power of `frequency` in `samples` at 48 kHz.
fn power(samples: &[f32], frequency: f64) -> f64 {
    let coefficient = 2.0 * (std::f64::consts::TAU * frequency / SAMPLE_RATE).cos();
    let (mut previous, mut before) = (0.0f64, 0.0f64);
    for &sample in samples {
        let current = f64::from(sample) + coefficient * previous - before;
        before = previous;
        previous = current;
    }
    previous * previous + before * before - coefficient * previous * before
}

/// `samples` carry `wanted` and not `other`: at least a hundred times the
/// power, and audible.
fn assert_tone(samples: &[f32], wanted: f64, other: f64, lane: &str) {
    let peak = samples.iter().fold(0.0f32, |peak, s| peak.max(s.abs()));
    let (wanted_power, other_power) = (power(samples, wanted), power(samples, other));
    println!(
        "{lane}: peak {peak:.3}, {wanted} Hz {wanted_power:.3e}, {other} Hz {other_power:.3e}"
    );
    assert!(peak > 0.01, "{lane} is silent (peak {peak})");
    assert!(
        wanted_power > 100.0 * other_power,
        "{lane} should carry {wanted} Hz, not {other} Hz"
    );
}

/// Empties the rings, then waits until every lane holds `frames`.
fn collect(sink: &LaneFrameSink, frames: usize) -> Vec<Vec<f32>> {
    for lane in 0..sink.lanes().len() {
        drop(sink.ring(lane).drain_all());
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while sink.available_to_read() < frames {
        assert!(Instant::now() < deadline, "the capture stalled");
        std::thread::sleep(Duration::from_millis(20));
    }
    (0..sink.lanes().len())
        .map(|lane| {
            let mut samples = vec![0.0f32; frames];
            assert!(sink.ring(lane).read(&mut samples));
            samples
        })
        .collect()
}

/// A sink whose device-change reports arrive on the returned channel.
fn reporting_sink(lanes: &[AudioLane]) -> (Arc<LaneFrameSink>, Receiver<DeviceChangeReason>) {
    let (sender, receiver) = channel();
    let sender = Mutex::new(sender);
    let sink = LaneFrameSink::with_handler(
        lanes,
        SAMPLE_RATE,
        2.0,
        Box::new(move |reason| {
            let _ = sender
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .send(reason);
        }),
    );
    (Arc::new(sink), receiver)
}

#[test]
#[ignore = "needs a PipeWire daemon; run under scripts/pipewire-headless.sh with -- --ignored"]
fn a_call_records_the_microphone_and_the_monitor_aligned_without_allocating() {
    let _sink_tone = Tone::into_sink(SINK, SINK_TONE);
    let _mic_tone = Tone::into_source(MIC, MIC_TONE);
    let lanes = [AudioLane::Mic, AudioLane::System];
    let sink = Arc::new(LaneFrameSink::new(&lanes));
    let backend = Arc::new(LiveCaptureBackend::new());
    let stream = start(&backend, &lanes, None, &sink).expect("start");
    println!("{stream:?}");
    assert_eq!(stream.sample_rate, SAMPLE_RATE);
    let layout = stream.layout.expect("a layout");
    assert_eq!(layout.sources[0].left, ChannelRef::new(0, 0, 3));
    assert_eq!(layout.sources[1].left, ChannelRef::new(0, 1, 3));
    assert_eq!(layout.sources[1].right, Some(ChannelRef::new(0, 2, 3)));

    let thread = data_loop_thread();
    let before = sink.available_to_read();
    let allocations =
        CountingAllocator::allocations_on(thread, || std::thread::sleep(Duration::from_secs(1)));
    let delivered = sink.available_to_read().saturating_sub(before);
    println!("data-loop thread {thread}: {allocations} allocations over {delivered} frames");
    assert!(
        delivered > 24_000,
        "a second should deliver most of 48 000 frames, got {delivered}"
    );
    assert_eq!(
        allocations, 0,
        "{allocations} allocations on PipeWire's data-loop thread"
    );

    let lanes_audio = collect(&sink, 24_000);
    assert_tone(&lanes_audio[0], MIC_TONE, SINK_TONE, "mic");
    assert_tone(&lanes_audio[1], SINK_TONE, MIC_TONE, "system");

    stop(&backend);
    let after_stop = sink.available_to_read();
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(
        sink.available_to_read(),
        after_stop,
        "no frame arrives after stop() returns"
    );
}

#[test]
#[ignore = "needs a PipeWire daemon; run under scripts/pipewire-headless.sh with -- --ignored"]
fn in_person_records_only_the_microphone_by_its_uid() {
    let _mic_tone = Tone::into_source(MIC, MIC_TONE);
    let _sink_tone = Tone::into_sink(SINK, SINK_TONE);
    let lanes = [AudioLane::Mixed];
    let sink = Arc::new(LaneFrameSink::new(&lanes));
    let backend = Arc::new(LiveCaptureBackend::new());
    let stream = start(&backend, &lanes, Some(MIC), &sink).expect("start");
    assert_eq!(
        stream.layout.expect("a layout").sources[0].left,
        ChannelRef::new(0, 0, 1)
    );
    assert_eq!(stream.output_latency_frames, 0, "no output in person");
    let audio = collect(&sink, 24_000);
    assert_tone(&audio[0], MIC_TONE, SINK_TONE, "mixed");
    stop(&backend);
}

#[test]
#[ignore = "needs a PipeWire daemon; run under scripts/pipewire-headless.sh with -- --ignored"]
fn an_unknown_microphone_fails_and_the_backend_starts_again() {
    let lanes = [AudioLane::Mixed];
    let sink = Arc::new(LaneFrameSink::new(&lanes));
    let backend = Arc::new(LiveCaptureBackend::new());
    assert_eq!(
        start(&backend, &lanes, Some("no-such-device"), &sink),
        Err(CaptureError::InputDeviceUnavailable)
    );
    for round in 0..2 {
        start(&backend, &lanes, None, &sink)
            .unwrap_or_else(|e| panic!("start {round} after a failure: {e}"));
        assert!(
            matches!(
                start(&backend, &lanes, None, &sink),
                Err(CaptureError::InvalidState(_))
            ),
            "a second start while running is refused"
        );
        stop(&backend);
        stop(&backend);
    }
}

/// Puts the configured default sink back when dropped.
struct DefaultSink;

impl DefaultSink {
    fn set(name: &str) {
        assert!(tool(
            "pw-metadata",
            &[
                "-n",
                "default",
                "0",
                "default.configured.audio.sink",
                &format!("{{ \"name\": \"{name}\" }}"),
                "Spa:String:JSON",
            ]
        ));
    }
}

impl Drop for DefaultSink {
    fn drop(&mut self) {
        Self::set(SINK);
        // Let WirePlumber move the default back before the next test.
        std::thread::sleep(Duration::from_millis(500));
    }
}

#[test]
#[ignore = "needs a PipeWire daemon; run under scripts/pipewire-headless.sh with -- --ignored"]
fn moving_the_default_output_is_reported_once() {
    let lanes = [AudioLane::Mic, AudioLane::System];
    let (sink, reasons) = reporting_sink(&lanes);
    let backend = Arc::new(LiveCaptureBackend::new());
    start(&backend, &lanes, None, &sink).expect("start");
    let _restore = DefaultSink;
    DefaultSink::set(SECOND_SINK);
    // WirePlumber moves `default.audio.sink` after the configured one;
    // give it time, and say what the metadata held if it never did.
    let reason = reasons
        .recv_timeout(COALESCE_DELAY + Duration::from_secs(8))
        .unwrap_or_else(|_| {
            let metadata = Command::new("pw-metadata")
                .args(["-n", "default", "0"])
                .output()
                .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
                .unwrap_or_default();
            panic!("no device-change report; the default metadata holds:\n{metadata}")
        });
    assert_eq!(reason, DeviceChangeReason::DefaultOutputChanged);
    assert!(
        reasons.recv_timeout(COALESCE_DELAY * 2).is_err(),
        "one report per change"
    );
    stop(&backend);
}

/// A lingering virtual microphone, created by `pw-cli`; destroyed when
/// dropped if the test did not.
struct TemporaryMic {
    name: &'static str,
}

impl TemporaryMic {
    fn create(name: &'static str) -> Self {
        assert!(tool(
            "pw-cli",
            &[
                "create-node",
                "adapter",
                &format!(
                    "{{ factory.name = support.null-audio-sink node.name = {name} \
                     media.class = Audio/Source/Virtual audio.position = [ MONO ] \
                     object.linger = true }}"
                ),
            ]
        ));
        let mic = Self { name };
        let deadline = Instant::now() + Duration::from_secs(5);
        while mic.id().is_none() {
            assert!(Instant::now() < deadline, "{name} did not appear");
            std::thread::sleep(Duration::from_millis(50));
        }
        mic
    }

    /// Its global id, from `pw-dump`.
    fn id(&self) -> Option<u64> {
        let output = Command::new("pw-dump").output().ok()?;
        let objects: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;
        objects.as_array()?.iter().find_map(|object| {
            let name = object.pointer("/info/props/node.name")?.as_str()?;
            (name == self.name).then(|| object.get("id")?.as_u64())?
        })
    }

    fn destroy(&self) {
        if let Some(id) = self.id() {
            assert!(tool("pw-cli", &["destroy", &id.to_string()]));
        }
    }
}

impl Drop for TemporaryMic {
    fn drop(&mut self) {
        self.destroy();
    }
}

#[test]
#[ignore = "needs a PipeWire daemon; run under scripts/pipewire-headless.sh with -- --ignored"]
fn a_microphone_that_goes_away_is_reported_gone() {
    let mic = TemporaryMic::create("steno-test-mic-gone");
    let lanes = [AudioLane::Mixed];
    let (sink, reasons) = reporting_sink(&lanes);
    let backend = Arc::new(LiveCaptureBackend::new());
    start(&backend, &lanes, Some(mic.name), &sink).expect("start");
    mic.destroy();
    let reason = reasons
        .recv_timeout(COALESCE_DELAY + Duration::from_secs(3))
        .expect("a device-change report");
    assert_eq!(reason, DeviceChangeReason::InputDeviceGone);
    stop(&backend);
    assert_eq!(
        start(&backend, &lanes, Some(mic.name), &sink),
        Err(CaptureError::InputDeviceUnavailable),
        "the rebuild's restart finds it gone"
    );
}
