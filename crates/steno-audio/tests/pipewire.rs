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
//! links playback only into sinks). Each `start` and `stop()` runs on its
//! own thread with a deadline, so a hang fails the test instead of
//! stalling the suite. The tests share one daemon, so they run one at a
//! time and put back what they move.
//!
//! The tests cover: each lane carries its own tone in the layout `start`
//! answers; a refused second start and a start after a failed one; one
//! report per default move or burst of moves, not before the coalescing
//! delay after the last, and the rebuild's restart on the new default; a
//! report while other apps' streams keep coming and going; no report for a
//! default that comes back or for an unrelated node; a lost microphone or
//! its link reported as the input gone (in person or during a call), a lost
//! monitor link as the output gone, both links lost (the monitor's first)
//! as the output gone, and the capture's connection closed from outside as
//! the output gone (the input in person); a report stuck in its handler
//! not holding `stop()` past its 2 s bound, with no frame after it and the
//! capture torn down once the handler returns.
//!
//! A round the machine stretched past the coalescing delay is not held to
//! the one-burst checks (the timing of its one report, or no report for a
//! default that came back), and the test prints that. A missing report
//! says whether the session manager never moved the default or the
//! capture missed the move. The backend's logs go to the test output, at
//! `info` unless a non-empty `RUST_LOG` says otherwise.
//!
//! The real-time promise is counted on the real thread here: libpipewire
//! runs the stream's `process` on its data-loop thread, which this file
//! finds in `/proc/self/task` and counts with
//! `CountingAllocator::allocations_on` for a second of cycles. After
//! `stop()` that thread and the capture's own are gone from the process,
//! and the capture's node from the daemon.
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
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use steno_audio::capture::{ChannelRef, DeviceChangeReason};
use steno_audio::testing::rt::CountingAllocator;
use steno_audio::writer::WavStreamWriter;
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
/// The capture stream's `node.name`.
const CAPTURE_NODE: &str = "steno-capture";
/// A call's lanes.
const CALL: [AudioLane; 2] = [AudioLane::Mic, AudioLane::System];
const COALESCE_DELAY: Duration = LiveCaptureBackend::COALESCE_DELAY;
/// The backend's private `COALESCE_LIMIT`: a burst that never settles is
/// judged this long after its first change.
const COALESCE_LIMIT: Duration = Duration::from_secs(2);
/// The backend's private `STOP_TIMEOUT`: how long `stop()` waits for a
/// report in its handler and for the capture thread.
const STOP_TIMEOUT: Duration = Duration::from_secs(2);
/// How long a test listens for a report that must not come: three
/// coalescing delays, fixed so that a shorter delay cannot shorten it.
const QUIET: Duration = Duration::from_millis(1_500);
/// How long the daemon and WirePlumber get to settle after a change.
const SETTLE: Duration = Duration::from_secs(2);
/// How long a tone's stream gets to be linked: the first test after the
/// daemon came up can wait on WirePlumber still starting.
const TONE_LINKED: Duration = Duration::from_secs(15);

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

/// Starts `backend` on its own thread with a deadline.
fn start(
    backend: &Arc<LiveCaptureBackend>,
    lanes: &[AudioLane],
    uid: Option<&str>,
    sink: &Arc<LaneFrameSink>,
) -> Result<CaptureStream, CaptureError> {
    show_logs();
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
    let mut bytes = WavStreamWriter::header(48_000, frames);
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
        write_tone(&path, frequency, 60.0);
        (dir, path)
    }

    /// Into a sink, linked by WirePlumber; returns once the link is there.
    fn into_sink(sink: &str, frequency: f64) -> Self {
        let (dir, path) = Self::file(frequency);
        let name = format!("steno-test-tone-{frequency}");
        let child = Command::new("pw-play")
            .args([
                "--target",
                sink,
                "--properties",
                &format!("{{ node.name = {name} }}"),
            ])
            .arg(&path)
            .spawn()
            .expect("pw-play");
        let tone = Self { child, _dir: dir };
        assert!(
            eventually(TONE_LINKED, || linked(&name, sink)),
            "WirePlumber did not link {name} to {sink}"
        );
        tone
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
        assert!(
            eventually(TONE_LINKED, || tool("pw-link", &[&output, &input])),
            "could not link {output} to {input}"
        );
        tone
    }
}

impl Drop for Tone {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// This process's threads: kernel id and name.
fn threads() -> Vec<(usize, String)> {
    std::fs::read_dir("/proc/self/task")
        .expect("/proc/self/task")
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let id = entry.file_name().to_string_lossy().parse().ok()?;
            let name = std::fs::read_to_string(entry.path().join("comm")).ok()?;
            Some((id, name.trim().to_owned()))
        })
        .collect()
}

/// Whether a thread of this process has `part` in its name: PipeWire's
/// data loop (`data-loop.0`) or the capture's own `steno-pipewire`.
fn thread_named(part: &str) -> Option<usize> {
    threads()
        .into_iter()
        .find_map(|(id, name)| name.contains(part).then_some(id))
}

/// The kernel id of PipeWire's data-loop thread in this process, where the
/// stream's `process` runs.
fn data_loop_thread() -> usize {
    thread_named("data-loop")
        .unwrap_or_else(|| panic!("no PipeWire data-loop thread among {:?}", threads()))
}

/// The daemon's objects, from `pw-dump`; none when it fails.
fn dump() -> Vec<serde_json::Value> {
    Command::new("pw-dump")
        .output()
        .ok()
        .and_then(|output| serde_json::from_slice(&output.stdout).ok())
        .and_then(|objects: serde_json::Value| objects.as_array().cloned())
        .unwrap_or_default()
}

/// The daemon's global id of the node named `name`, from `pw-dump`.
fn node_id(name: &str) -> Option<u64> {
    node_in(&dump(), name)
}

/// The global id of the node named `name` among `objects`.
fn node_in(objects: &[serde_json::Value], name: &str) -> Option<u64> {
    objects.iter().find_map(|object| {
        let found = object.pointer("/info/props/node.name")?.as_str()?;
        (found == name).then(|| object.get("id")?.as_u64())?
    })
}

/// The links among `objects`: each one's global id and the ids of the
/// nodes it connects, from and to.
fn links_in(objects: &[serde_json::Value]) -> impl Iterator<Item = (u64, u64, u64)> + '_ {
    let id = |object: &serde_json::Value, pointer: &str| {
        object.pointer(pointer).and_then(serde_json::Value::as_u64)
    };
    objects
        .iter()
        .filter(|object| {
            object.get("type").and_then(serde_json::Value::as_str)
                == Some("PipeWire:Interface:Link")
        })
        .filter_map(move |object| {
            Some((
                id(object, "/id")?,
                id(object, "/info/output-node-id")?,
                id(object, "/info/input-node-id")?,
            ))
        })
}

/// Removes, as a patchbay or a policy would, one of the capture's links
/// from each node named in `from`, in that order.
fn destroy_capture_links_from(from: &[&str]) {
    let objects = dump();
    let node = |name| node_in(&objects, name).unwrap_or_else(|| panic!("no node {name}"));
    let capture = node(CAPTURE_NODE);
    let links: Vec<_> = links_in(&objects)
        .filter(|&(.., to)| to == capture)
        .collect();
    assert_eq!(links.len(), 3, "one link per channel: {links:?}");
    for name in from {
        let source = node(name);
        let (link, ..) = links
            .iter()
            .find(|&&(_, from, _)| from == source)
            .unwrap_or_else(|| panic!("no capture link from {name}: {links:?}"));
        assert!(tool("pw-cli", &["destroy", &link.to_string()]));
    }
}

/// Whether the daemon holds a link from the node named `from` to the node
/// named `to`, from `pw-dump`.
fn linked(from: &str, to: &str) -> bool {
    let objects = dump();
    let (Some(from), Some(to)) = (node_in(&objects, from), node_in(&objects, to)) else {
        return false;
    };
    links_in(&objects).any(|(_, f, t)| (f, t) == (from, to))
}

/// Polls `done` for up to `limit`; whether it held.
fn eventually(limit: Duration, mut done: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + limit;
    while !done() {
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    true
}

/// Stops `backend` and checks the teardown: the capture thread joined
/// (it held the only other references to `sink`), no frame after `stop()`
/// returns, both capture threads gone from the process, the capture's
/// node gone from the daemon.
fn stop_and_check_teardown(backend: &Arc<LiveCaptureBackend>, sink: &Arc<LaneFrameSink>) {
    stop(backend);
    let after_stop = sink.available_to_read();
    assert_eq!(
        Arc::strong_count(sink),
        1,
        "stop() returned before the capture thread let go of the sink"
    );
    let threads_gone =
        || thread_named("data-loop").is_none() && thread_named("steno-pipewire").is_none();
    assert!(
        eventually(SETTLE, threads_gone),
        "the capture's threads outlive stop(): {:?}",
        threads()
    );
    assert!(
        eventually(SETTLE, || node_id(CAPTURE_NODE).is_none()),
        "{CAPTURE_NODE} is still in the graph after stop()"
    );
    assert_eq!(
        sink.available_to_read(),
        after_stop,
        "no frame arrives after stop() returns"
    );
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

/// Empties the rings; the frames the first lane held.
fn drain(sink: &LaneFrameSink) -> usize {
    let held: Vec<usize> = (0..sink.lanes().len())
        .map(|lane| sink.ring(lane).drain_all().len())
        .collect();
    held.first().copied().unwrap_or(0)
}

/// Empties the rings, then waits until every lane holds `frames`.
fn collect(sink: &LaneFrameSink, frames: usize) -> Vec<Vec<f32>> {
    drain(sink);
    let filled = eventually(Duration::from_secs(5), || {
        sink.available_to_read() >= frames
    });
    assert!(filled, "the capture stalled");
    (0..sink.lanes().len())
        .map(|lane| {
            let mut samples = vec![0.0f32; frames];
            assert!(sink.ring(lane).read(&mut samples));
            samples
        })
        .collect()
}

/// A device-change report and when the handler got it.
type Report = (DeviceChangeReason, Instant);

/// The next device-change report, which must come within 3 s after the
/// coalescing delay.
fn next_report(reasons: &Receiver<Report>) -> DeviceChangeReason {
    reasons
        .recv_timeout(COALESCE_DELAY + Duration::from_secs(3))
        .expect("a device-change report")
        .0
}

/// A sink whose device-change reports arrive on the returned channel.
fn reporting_sink(lanes: &[AudioLane]) -> (Arc<LaneFrameSink>, Receiver<Report>) {
    let (sender, receiver) = channel();
    let sender = Mutex::new(sender);
    let sink = LaneFrameSink::with_handler(
        lanes,
        SAMPLE_RATE,
        2.0,
        Box::new(move |reason| {
            let _ = sender
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .send((reason, Instant::now()));
        }),
    );
    (Arc::new(sink), receiver)
}

#[test]
#[ignore = "needs a PipeWire daemon; run under scripts/pipewire-headless.sh with -- --ignored"]
fn a_call_records_the_microphone_and_the_monitor_aligned_without_allocating() {
    let _sink_tone = Tone::into_sink(SINK, SINK_TONE);
    let _mic_tone = Tone::into_source(MIC, MIC_TONE);
    let lanes = CALL;
    let sink = Arc::new(LaneFrameSink::new(&lanes));
    let backend = Arc::new(LiveCaptureBackend::new());
    let stream = start(&backend, &lanes, None, &sink).expect("start");
    println!("{stream:?}");
    assert!(
        sink.available_to_read() > 0,
        "start returns once the first cycle arrived"
    );
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
        (24_000..60_000).contains(&delivered),
        "a second should deliver about 48 000 frames, got {delivered}"
    );
    assert_eq!(
        allocations, 0,
        "{allocations} allocations on PipeWire's data-loop thread"
    );

    let lanes_audio = collect(&sink, 24_000);
    assert_tone(&lanes_audio[0], MIC_TONE, SINK_TONE, "mic");
    assert_tone(&lanes_audio[1], SINK_TONE, MIC_TONE, "system");

    stop_and_check_teardown(&backend, &sink);
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
    stop_and_check_teardown(&backend, &sink);
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
    assert!(
        thread_named("steno-pipewire").is_none(),
        "the failed start joined its thread: {:?}",
        threads()
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
    let lanes = CALL;
    let (sink, reasons) = reporting_sink(&lanes);
    let backend = Arc::new(LiveCaptureBackend::new());
    start(&backend, &lanes, None, &sink).expect("start");
    let _restore = DefaultSink;
    let since = Instant::now();
    DefaultSink::set(SECOND_SINK);
    assert_output_moved_once(&reasons, SECOND_SINK, since, true, "");
    stop(&backend);
}

/// Expects one `DefaultOutputChanged` report and no second, after
/// WirePlumber moved the default to `to`. `since` is taken right before
/// the last switch; when `timed`, the report must come no sooner than the
/// coalescing delay after it.
///
/// WirePlumber moves `default.audio.sink` after the configured one, so
/// the report gets time, and a failure says which side missed:
/// WirePlumber, which did not move the default, or the capture, which did
/// not report the move. The metadata is read only once the report is
/// late: a client binding it while WirePlumber moves the default can keep
/// that move from every client already bound (seen with WirePlumber
/// 0.5.14 and PipeWire 1.6.5), so polling it here would make the miss it
/// looks for. `context` prefixes the failure messages.
fn assert_output_moved_once(
    reasons: &Receiver<Report>,
    to: &str,
    since: Instant,
    timed: bool,
    context: &str,
) {
    let wait = COALESCE_DELAY + Duration::from_secs(8);
    let (reason, at) = reasons.recv_timeout(wait).unwrap_or_else(|_| {
        let who = if default_sink().as_deref() == Some(to) {
            "the capture missed the move"
        } else {
            "WirePlumber did not move the default"
        };
        panic!(
            "{context}no device-change report within {wait:?}: {who} to {to}; {}",
            default_metadata()
        )
    });
    let after = at.duration_since(since);
    // From before the last switch: WirePlumber moves the default before
    // `pw-metadata` has exited, never before it started.
    assert!(
        !timed || after >= COALESCE_DELAY,
        "{context}reported {after:?} after the last switch, inside the coalescing delay"
    );
    assert_eq!(reason, DeviceChangeReason::DefaultOutputChanged);
    assert!(
        reasons.recv_timeout(QUIET).is_err(),
        "{context}one report per change"
    );
}

/// Whether the switches reach the daemon as one burst, judged the
/// coalescing delay after its last switch: each of `gaps` (from the start
/// of one `DefaultSink::set` to the return of the next) at least 50 ms
/// short of the delay, and `whole`, the first call to the last, short
/// enough that the 2 s cap cannot judge it sooner. Prints why not, after
/// `context`.
fn one_burst(gaps: &[Duration], whole: Duration, context: &str) -> bool {
    let tight = gaps
        .iter()
        .all(|gap| *gap < COALESCE_DELAY.saturating_sub(Duration::from_millis(50)))
        && whole < COALESCE_LIMIT.saturating_sub(COALESCE_DELAY);
    if !tight {
        println!(
            "{context}not judged as one burst: the machine stretched the \
             switches (gaps {gaps:?}, {whole:?} in all)"
        );
    }
    tight
}

/// What `pw-metadata` prints of the `default` metadata on subject 0:
/// `key`, or every key; nothing when it fails.
fn read_default_metadata(key: Option<&str>) -> String {
    Command::new("pw-metadata")
        .args(["-n", "default", "0"])
        .args(key)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

/// What the `default` metadata holds, for a failure message.
fn default_metadata() -> String {
    format!(
        "the default metadata holds:\n{}",
        read_default_metadata(None)
    )
}

/// The default sink as WirePlumber resolved it, from `pw-metadata`.
fn default_sink() -> Option<String> {
    let text = read_default_metadata(Some("default.audio.sink"));
    [SECOND_SINK, SINK]
        .into_iter()
        .find(|name| text.contains(&format!("\"{name}\"")))
        .map(str::to_owned)
}

#[test]
#[ignore = "needs a PipeWire daemon; run under scripts/pipewire-headless.sh with -- --ignored"]
fn a_burst_of_switches_is_reported_once_and_the_rebuild_restarts() {
    let lanes = CALL;
    let (sink, reasons) = reporting_sink(&lanes);
    let backend = Arc::new(LiveCaptureBackend::new());
    let _restore = DefaultSink;
    start(&backend, &lanes, None, &sink).expect("start");
    for round in 0..6 {
        let (to, from) = if round % 2 == 0 {
            (SECOND_SINK, SINK)
        } else {
            (SINK, SECOND_SINK)
        };
        assert!(
            eventually(SETTLE, || default_sink().as_deref() == Some(from)),
            "round {round}: the default is not {from}; {}",
            default_metadata()
        );
        // Within the coalescing delay of each other: one report, timed
        // from the last. WirePlumber usually moves the default while `set`
        // runs, so two moves are never further apart than the start of one
        // call and the return of the next; when it moves after `set`
        // returned, `one_burst`'s 50 ms spare covers its lag.
        let context = format!("round {round}: ");
        let first = Instant::now();
        DefaultSink::set(to);
        std::thread::sleep(COALESCE_DELAY / 2);
        let second = Instant::now();
        DefaultSink::set(from);
        let first_gap = first.elapsed();
        std::thread::sleep(COALESCE_DELAY * 2 / 5);
        let since = Instant::now();
        DefaultSink::set(to);
        let timed = one_burst(&[first_gap, second.elapsed()], first.elapsed(), &context);
        assert_output_moved_once(&reasons, to, since, timed, &context);
        // The session's rebuild: stop, open the latch, start again.
        stop_and_check_teardown(&backend, &sink);
        sink.rearm_device_change();
        assert!(
            eventually(SETTLE, || default_sink().as_deref() == Some(to)),
            "round {round}: WirePlumber did not settle on {to}"
        );
        start(&backend, &lanes, None, &sink)
            .unwrap_or_else(|e| panic!("round {round}: the rebuild's start failed: {e}"));
        assert!(
            reasons.recv_timeout(QUIET).is_err(),
            "round {round}: the restarted capture is on the new default"
        );
    }
    stop_and_check_teardown(&backend, &sink);
}

#[test]
#[ignore = "needs a PipeWire daemon; run under scripts/pipewire-headless.sh with -- --ignored"]
fn changes_that_settle_back_or_touch_other_nodes_are_not_reported() {
    let lanes = CALL;
    let (sink, reasons) = reporting_sink(&lanes);
    let backend = Arc::new(LiveCaptureBackend::new());
    let _restore = DefaultSink;
    start(&backend, &lanes, None, &sink).expect("start");
    // There and back within the coalescing delay: back as soon as
    // WirePlumber moved it (about 15 ms). Polling the metadata while
    // WirePlumber moves the default can keep the move from the capture
    // (see `assert_output_moved_once`), and a missed move also ends in no
    // report, so this round can pass without the capture having seen the
    // move; a fixed pause instead would not know when WirePlumber moved.
    let there = Instant::now();
    DefaultSink::set(SECOND_SINK);
    assert!(
        eventually(SETTLE, || default_sink().as_deref() == Some(SECOND_SINK)),
        "WirePlumber did not move the default; {}",
        default_metadata()
    );
    DefaultSink::set(SINK);
    let away = there.elapsed();
    if one_burst(&[away], away, "there and back: ") {
        assert!(
            reasons.recv_timeout(QUIET).is_err(),
            "a default that came back is no change"
        );
    } else if reasons.recv_timeout(COALESCE_LIMIT + QUIET).is_ok() {
        // Away long enough to be judged moved: restart on the default that
        // came back, as the session's rebuild would.
        stop(&backend);
        sink.rearm_device_change();
        assert!(
            eventually(SETTLE, || default_sink().as_deref() == Some(SINK)),
            "WirePlumber did not move the default back; {}",
            default_metadata()
        );
        start(&backend, &lanes, None, &sink).expect("the restart");
    }
    // A device Steno does not record comes and goes.
    let other = TemporaryMic::create("steno-test-unrelated");
    other.destroy();
    assert!(
        reasons.recv_timeout(QUIET).is_err(),
        "an unrelated node is no change"
    );
    stop(&backend);
}

/// Short playback streams into the default sink, one after another on a
/// thread, as other apps' notification sounds come and go; stopped when
/// dropped.
struct Churn {
    done: Arc<AtomicBool>,
    streams: Arc<AtomicUsize>,
    thread: Option<JoinHandle<()>>,
}

impl Churn {
    fn start() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("blip.wav");
        write_tone(&path, SINK_TONE, 0.1);
        let done = Arc::new(AtomicBool::new(false));
        let streams = Arc::new(AtomicUsize::new(0));
        let thread = std::thread::spawn({
            let (done, streams) = (Arc::clone(&done), Arc::clone(&streams));
            move || {
                let _dir = dir;
                while !done.load(Ordering::Relaxed) {
                    let _ = Command::new("pw-play")
                        .arg(&path)
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .status();
                    streams.fetch_add(1, Ordering::Relaxed);
                    std::thread::sleep(Duration::from_millis(150));
                }
            }
        });
        Self {
            done,
            streams,
            thread: Some(thread),
        }
    }

    /// Streams played to the end so far.
    fn streams(&self) -> usize {
        self.streams.load(Ordering::Relaxed)
    }
}

impl Drop for Churn {
    fn drop(&mut self) {
        self.done.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[test]
#[ignore = "needs a PipeWire daemon; run under scripts/pipewire-headless.sh with -- --ignored"]
fn other_apps_streams_coming_and_going_do_not_hold_back_a_report() {
    let lanes = CALL;
    let (sink, reasons) = reporting_sink(&lanes);
    let backend = Arc::new(LiveCaptureBackend::new());
    let _restore = DefaultSink;
    start(&backend, &lanes, None, &sink).expect("start");
    let churn = Churn::start();
    assert!(
        eventually(Duration::from_secs(5), || churn.streams() >= 2),
        "the other streams did not play"
    );
    let before = churn.streams();
    let since = Instant::now();
    DefaultSink::set(SECOND_SINK);
    assert_output_moved_once(&reasons, SECOND_SINK, since, true, "under churn: ");
    assert!(
        churn.streams() > before,
        "the other streams kept coming while the report was due"
    );
    drop(churn);
    stop_and_check_teardown(&backend, &sink);
}

/// Starts a capture of `lanes` from the default devices, does `outside`
/// to it, and expects one report of `gone`.
fn assert_reported_after(lanes: &[AudioLane], outside: impl FnOnce(), gone: DeviceChangeReason) {
    let (sink, reasons) = reporting_sink(lanes);
    let backend = Arc::new(LiveCaptureBackend::new());
    start(&backend, lanes, None, &sink).expect("start");
    outside();
    assert_eq!(next_report(&reasons), gone);
    stop_and_check_teardown(&backend, &sink);
}

/// Destroys the capture's client in the daemon (the `client.id` of its
/// node), so the server drops the capture's connection.
fn destroy_own_client() {
    let client = dump()
        .iter()
        .find_map(|object| {
            let props = object.pointer("/info/props")?;
            let ours = props.get("node.name")?.as_str()? == CAPTURE_NODE;
            ours.then(|| props.get("client.id")?.as_u64())?
        })
        .expect("the capture's client in the daemon");
    assert!(tool("pw-cli", &["destroy", &client.to_string()]));
}

#[test]
#[ignore = "needs a PipeWire daemon; run under scripts/pipewire-headless.sh with -- --ignored"]
fn a_monitor_link_removed_from_outside_is_reported_as_the_output_gone() {
    assert_reported_after(
        &CALL,
        || destroy_capture_links_from(&[SINK]),
        DeviceChangeReason::OutputDeviceGone,
    );
}

#[test]
#[ignore = "needs a PipeWire daemon; run under scripts/pipewire-headless.sh with -- --ignored"]
fn a_microphone_link_removed_from_outside_is_reported_as_the_input_gone() {
    assert_reported_after(
        &CALL,
        || destroy_capture_links_from(&[MIC]),
        DeviceChangeReason::InputDeviceGone,
    );
}

#[test]
#[ignore = "needs a PipeWire daemon; run under scripts/pipewire-headless.sh with -- --ignored"]
fn a_monitor_then_a_microphone_link_removed_are_reported_as_the_output_gone() {
    // Both within one coalescing delay: the monitor's loss must stay.
    assert_reported_after(
        &CALL,
        || destroy_capture_links_from(&[SINK, MIC]),
        DeviceChangeReason::OutputDeviceGone,
    );
}

#[test]
#[ignore = "needs a PipeWire daemon; run under scripts/pipewire-headless.sh with -- --ignored"]
fn a_connection_closed_from_outside_during_a_call_is_reported_as_the_output_gone() {
    assert_reported_after(
        &CALL,
        destroy_own_client,
        DeviceChangeReason::OutputDeviceGone,
    );
}

#[test]
#[ignore = "needs a PipeWire daemon; run under scripts/pipewire-headless.sh with -- --ignored"]
fn a_connection_closed_from_outside_in_person_is_reported_as_the_input_gone() {
    assert_reported_after(
        &[AudioLane::Mixed],
        destroy_own_client,
        DeviceChangeReason::InputDeviceGone,
    );
}

/// The `steno-capture` nodes in the graph.
fn capture_nodes() -> usize {
    dump()
        .iter()
        .filter(|object| {
            object
                .pointer("/info/props/node.name")
                .and_then(serde_json::Value::as_str)
                == Some(CAPTURE_NODE)
        })
        .count()
}

/// The threads of this process with `part` in their name.
fn threads_named(part: &str) -> usize {
    threads()
        .iter()
        .filter(|(_, name)| name.contains(part))
        .count()
}

/// A sink whose handler sends each report on the returned receiver, and
/// is stuck in the first, as a handler waiting for a lock, until the
/// returned sender sends.
fn sink_stuck_in_its_first_report(
    lanes: &[AudioLane],
) -> (Arc<LaneFrameSink>, Receiver<DeviceChangeReason>, Sender<()>) {
    let (entered, handler_entered) = channel();
    let (release, released) = channel::<()>();
    let released = Mutex::new(released);
    let stuck_once = AtomicBool::new(true);
    let sink = LaneFrameSink::with_handler(
        lanes,
        SAMPLE_RATE,
        2.0,
        Box::new(move |reason| {
            let _ = entered.send(reason);
            if stuck_once.swap(false, Ordering::SeqCst) {
                let _ = released
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .recv();
            }
        }),
    );
    (Arc::new(sink), handler_entered, release)
}

/// A report stuck in the handler holds `stop()` only to its bound, and the
/// session's rebuild then starts the same backend on the same sink while
/// the old capture's thread and node are still there: the rebuilt capture
/// records both lanes beside it and through its teardown, and that
/// teardown reads as no change.
#[test]
#[ignore = "needs a PipeWire daemon; run under scripts/pipewire-headless.sh with -- --ignored"]
fn a_report_stuck_in_its_handler_holds_neither_stop_nor_the_rebuild() {
    let _sink_tone = Tone::into_sink(SINK, SINK_TONE);
    let _mic_tone = Tone::into_source(MIC, MIC_TONE);
    let lanes = CALL;
    let (sink, handler_entered, release) = sink_stuck_in_its_first_report(&lanes);
    let backend = Arc::new(LiveCaptureBackend::new());
    start(&backend, &lanes, None, &sink).expect("start");
    destroy_capture_links_from(&[SINK]);
    assert_eq!(
        handler_entered
            .recv_timeout(COALESCE_DELAY + Duration::from_secs(3))
            .expect("a device-change report"),
        DeviceChangeReason::OutputDeviceGone
    );
    let stopping = Instant::now();
    stop(&backend);
    let took = stopping.elapsed();
    println!("stop() returned after {took:?} with a report in its handler");
    assert!(
        took >= STOP_TIMEOUT.saturating_sub(Duration::from_millis(100)),
        "stop() waits for the report up to its bound, returned after {took:?}"
    );
    assert!(
        took < STOP_TIMEOUT + Duration::from_secs(1),
        "stop() returns at its bound, returned after {took:?}"
    );
    // The stream runs on while the handler holds the PipeWire thread, so
    // cycles keep coming: wait for several, short of the ring's headroom.
    let after_stop = sink.available_to_read();
    std::thread::sleep(Duration::from_millis(150));
    assert_eq!(
        sink.available_to_read(),
        after_stop,
        "a frame arrived after stop() returned"
    );
    assert!(
        sink.dropped_samples().is_empty(),
        "samples dropped after stop() returned: {:?}",
        sink.dropped_samples()
    );

    // The session's rebuild: the latch re-armed, then the same backend
    // started on the same sink beside the old capture.
    sink.rearm_device_change();
    start(&backend, &lanes, None, &sink).expect("a start beside the stuck capture");
    assert_eq!(threads_named("steno-pipewire"), 2, "{:?}", threads());
    assert_eq!(
        capture_nodes(),
        2,
        "the old capture's node stays while stuck"
    );
    let audio = collect(&sink, 12_000);
    assert_tone(
        &audio[0],
        MIC_TONE,
        SINK_TONE,
        "mic beside the stuck capture",
    );
    assert_tone(
        &audio[1],
        SINK_TONE,
        MIC_TONE,
        "system beside the stuck capture",
    );

    release.send(()).expect("the handler is still waiting");
    // Through the old capture's teardown the rebuilt one keeps delivering.
    let mut delivered = 0;
    let listened = Instant::now();
    while listened.elapsed() < QUIET {
        std::thread::sleep(Duration::from_millis(50));
        delivered += drain(&sink);
    }
    assert!(
        delivered > 48_000,
        "only {delivered} frames in {QUIET:?} across the old capture's teardown"
    );
    assert!(
        sink.dropped_samples().is_empty(),
        "samples dropped across the old capture's teardown: {:?}",
        sink.dropped_samples()
    );
    assert!(
        handler_entered.try_recv().is_err(),
        "the old capture's teardown was reported as a change"
    );
    assert!(
        eventually(SETTLE, || threads_named("steno-pipewire") == 1
            && capture_nodes() == 1),
        "the old capture outlives its late report: {:?}",
        threads()
    );
    let audio = collect(&sink, 12_000);
    assert_tone(&audio[0], MIC_TONE, SINK_TONE, "mic after the old teardown");
    assert_tone(
        &audio[1],
        SINK_TONE,
        MIC_TONE,
        "system after the old teardown",
    );
    stop_and_check_teardown(&backend, &sink);
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
        assert!(
            eventually(Duration::from_secs(5), || node_id(name).is_some()),
            "{name} did not appear"
        );
        mic
    }

    fn destroy(&self) {
        if let Some(id) = node_id(self.name) {
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
fn a_microphone_that_goes_away_in_person_is_reported_as_the_input_gone() {
    let mic = TemporaryMic::create("steno-test-mic-gone");
    let lanes = [AudioLane::Mixed];
    let (sink, reasons) = reporting_sink(&lanes);
    let backend = Arc::new(LiveCaptureBackend::new());
    start(&backend, &lanes, Some(mic.name), &sink).expect("start");
    mic.destroy();
    let reason = next_report(&reasons);
    assert_eq!(reason, DeviceChangeReason::InputDeviceGone);
    stop(&backend);
    assert_eq!(
        start(&backend, &lanes, Some(mic.name), &sink),
        Err(CaptureError::InputDeviceUnavailable),
        "the rebuild's restart finds it gone"
    );
}

#[test]
#[ignore = "needs a PipeWire daemon; run under scripts/pipewire-headless.sh with -- --ignored"]
fn a_microphone_that_goes_away_during_a_call_is_reported_as_the_input_gone() {
    let mic = TemporaryMic::create("steno-test-mic-call");
    let lanes = CALL;
    let (sink, reasons) = reporting_sink(&lanes);
    let backend = Arc::new(LiveCaptureBackend::new());
    start(&backend, &lanes, Some(mic.name), &sink).expect("start");
    // The server removes Steno's link to it too; the output stays.
    mic.destroy();
    let reason = next_report(&reasons);
    assert_eq!(
        reason,
        DeviceChangeReason::InputDeviceGone,
        "not the output gone"
    );
    stop_and_check_teardown(&backend, &sink);
}
