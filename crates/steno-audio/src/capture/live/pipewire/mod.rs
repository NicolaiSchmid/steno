//! The live backend on Linux: one PipeWire capture stream that Steno links
//! itself to the microphone and to the default sink's monitor, so every
//! graph cycle delivers all lanes in one interleaved buffer on the graph
//! clock. Each capture owns a `steno-pipewire` thread; the stream's
//! `process` callback runs on the real-time data-loop thread.

mod devices;
mod graph;

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, sync_channel};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use pipewire as pw;
use pw::proxy::ProxyT;
use pw::spa;
use pw::types::ObjectType;
use steno_core::AudioLane;

pub use self::devices::AudioDevices;
pub(crate) use self::graph::is_source_class;
use self::graph::{Graph, Latency, Lost, Targets};
use crate::SAMPLE_RATE;
use crate::capture::start_log::{self, start_log};
use crate::capture::{
    CaptureBackend, CaptureError, CaptureInput, CaptureStream, DeviceChangeReason, DeviceSnapshot,
    LaneSource,
};
use crate::realtime::{
    CycleClock, LaneFrameSink, Missed, SliceView, deliver_slices, interleaved_view,
};

/// How long `start` lets PipeWire answer, link and run the first cycle.
const START_TIMEOUT: Duration = Duration::from_secs(3);
/// How long `start` waits for the device latencies after the first cycle.
const LATENCY_TIMEOUT: Duration = Duration::from_millis(500);
/// The longest single wait on the loop while starting.
pub(crate) const PUMP_SLICE: Duration = Duration::from_millis(20);
/// The wait on the loop while nothing is pending.
const IDLE_WAIT: Duration = Duration::from_secs(1);

/// The longest a stream of changes defers its judgement: changes less than
/// [`LiveCaptureBackend::COALESCE_DELAY`] apart are judged this long after
/// the first of them.
const COALESCE_LIMIT: Duration = Duration::from_secs(2);

/// How long `stop()` waits for a report to leave the gate and the PipeWire
/// thread to tear down; past it they are left behind.
const STOP_TIMEOUT: Duration = Duration::from_secs(2);

/// The capture stream's `node.name`.
pub(crate) const STREAM_NODE_NAME: &str = "steno-capture";

/// The capture's way into the sink, closed for good by `stop()`, or by a
/// failed `start`, before it waits for the thread. Two kinds of pass:
///
/// - A cycle's [`deliver_slices`], on the data-loop thread. Lock-free and
///   `SeqCst`, so either the pass sees the gate closed or [`Gate::close`]
///   sees it inside. `close` waits for it without a bound: a delivery still
///   writing after `close` would be a second producer on single-producer
///   rings beside the next backend's. The body takes microseconds.
/// - A device-change report, on the PipeWire thread. Counted under a mutex
///   and waited for until `close`'s deadline; a late one costs at most a
///   spurious rebuild, which the session ignores or defers.
#[derive(Debug)]
struct Gate {
    open: AtomicBool,
    delivering: AtomicUsize,
    /// The last cycle delivered: its `pw_time` (`CLOCK_MONOTONIC`
    /// nanoseconds, 0 before the first) and its frames.
    last_cycle_at: AtomicU64,
    last_cycle_frames: AtomicUsize,
    reporting: Mutex<usize>,
    report_left: Condvar,
}

impl Gate {
    fn new() -> Self {
        Self {
            open: AtomicBool::new(true),
            delivering: AtomicUsize::new(0),
            last_cycle_at: AtomicU64::new(0),
            last_cycle_frames: AtomicUsize::new(0),
            reporting: Mutex::new(0),
            report_left: Condvar::new(),
        }
    }

    /// Runs a cycle's `work` if the gate is open; whether it ran. Real-time
    /// safe.
    #[inline(always)]
    fn deliver(&self, work: impl FnOnce()) -> bool {
        self.delivering.fetch_add(1, Ordering::SeqCst);
        let open = self.open.load(Ordering::SeqCst);
        if open {
            work();
        }
        self.delivering.fetch_sub(1, Ordering::SeqCst);
        open
    }

    /// Inside a delivery: the cycle at `at` delivered `frames`. Real-time
    /// safe.
    #[inline(always)]
    fn mark_cycle(&self, at: i64, frames: usize) {
        self.last_cycle_at
            .store(u64::try_from(at).unwrap_or(0), Ordering::Relaxed);
        self.last_cycle_frames.store(frames, Ordering::Relaxed);
    }

    /// The last cycle [`Self::mark_cycle`] saw; `None` before the first.
    fn last_cycle(&self) -> Option<(u64, usize)> {
        let at = self.last_cycle_at.load(Ordering::SeqCst);
        (at > 0).then(|| (at, self.last_cycle_frames.load(Ordering::SeqCst)))
    }

    /// Runs a report's `work` if the gate is open; whether it ran. Locks:
    /// not for the data-loop thread.
    fn report(&self, work: impl FnOnce()) -> bool {
        {
            let mut inside = self.reporting();
            // Under the lock, so a `close` that took it first is seen.
            if !self.open.load(Ordering::SeqCst) {
                return false;
            }
            *inside += 1;
        }
        let _leaves = ReportLeaves(self);
        work();
        true
    }

    fn reporting(&self) -> std::sync::MutexGuard<'_, usize> {
        self.reporting
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Closes the gate, waits for the deliveries inside to leave, then for
    /// the reports inside until `deadline`; whether none is left inside.
    /// No report can begin before `start` handed over its stream, so the
    /// session's handler never waits on a failing `start`'s `close`.
    fn close(&self, deadline: Instant) -> bool {
        self.open.store(false, Ordering::SeqCst);
        while self.delivering.load(Ordering::SeqCst) != 0 {
            std::thread::yield_now();
        }
        let wait = deadline.saturating_duration_since(Instant::now());
        let (inside, _) = self
            .report_left
            .wait_timeout_while(self.reporting(), wait, |inside| *inside != 0)
            .unwrap_or_else(PoisonError::into_inner);
        *inside == 0
    }
}

/// Counts a report out of its [`Gate`] when dropped, so a panicking handler
/// leaves too.
struct ReportLeaves<'a>(&'a Gate);

impl Drop for ReportLeaves<'_> {
    fn drop(&mut self) {
        *self.0.reporting() -= 1;
        self.0.report_left.notify_all();
    }
}

/// What the data-loop thread owns, read only by [`process`]. It crosses to
/// PipeWire's data-loop thread, which `pipewire` does not check, so it must
/// be `Send` (asserted below).
struct RealTime {
    sink: Arc<LaneFrameSink>,
    gate: Arc<Gate>,
    sources: Vec<LaneSource>,
    channels: usize,
    cycle_frames: Arc<AtomicUsize>,
    clock: CycleClock,
}

const _: () = {
    const fn sendable<T: Send>() {}
    sendable::<RealTime>();
};

/// The stream's `process` callback, on PipeWire's data-loop thread: the
/// cycles missed since the last one, then the buffer through
/// [`deliver_cycle`] while the gate is open. Nothing allocates, locks, logs
/// or can panic.
fn process(stream: &pw::stream::Stream, rt: &mut RealTime) {
    let Some(mut buffer) = stream.dequeue_buffer() else {
        return;
    };
    let Some(data) = buffer.datas_mut().first_mut() else {
        return;
    };
    let chunk = data.chunk();
    let (offset, size, stride) = (chunk.offset(), chunk.size(), chunk.stride());
    // Mapped by `MAP_BUFFERS`, valid while the buffer stays dequeued; the
    // view borrows it.
    let memory = data.data();
    let view = interleaved_view(memory.as_deref(), offset, size, stride, rt.channels);
    let time = stream.time().ok();
    let reading = time.as_ref().map(|time| (time.ticks(), time.rate()));
    let missed = missed_before(&mut rt.clock, reading, view.frames);
    rt.gate.deliver(|| {
        deliver_cycle(view, missed, &rt.sources, &rt.sink);
        if let Some(time) = &time {
            rt.gate.mark_cycle(time.now(), view.frames);
        }
    });
    // Release: `start` returns on seeing it, the frames already in the rings.
    rt.cycle_frames.store(view.frames, Ordering::Release);
}

/// One cycle into the sink: what was missed before it, then its own frames.
/// Real-time safe.
#[inline(always)]
fn deliver_cycle(
    view: SliceView<'_>,
    missed: Missed,
    sources: &[LaneSource],
    sink: &LaneFrameSink,
) {
    sink.write_missed(missed);
    deliver_slices(&[view], sources, sink);
}

/// The frames missed before a cycle of `frames` with stream time `time`
/// (none when `pw_stream_get_time` failed). A missing rate, a changed graph
/// rate or a changed quantum restart the clock and report nothing. At a
/// driver change `pw_stream` repeats the last ticks, so the next real jump
/// is counted one cycle short. Real-time safe.
#[inline(always)]
fn missed_before(
    clock: &mut CycleClock,
    time: Option<(u64, spa::utils::Fraction)>,
    frames: usize,
) -> Missed {
    let Some((ticks, rate)) = time.filter(|(_, rate)| rate.num > 0 && rate.denom > 0) else {
        clock.reset();
        return Missed::default();
    };
    // A tick is `num / denom` seconds, `SAMPLE_RATE * num / denom` frames.
    clock.rescale(
        (SAMPLE_RATE as u64) * u64::from(rate.num),
        u64::from(rate.denom),
    );
    if clock.resized(frames) {
        clock.reset();
    }
    clock.advance(ticks, frames)
}

/// What a stream missed after its last cycle, once it stops at `stopped`
/// (`CLOCK_MONOTONIC` nanoseconds).
fn missed_at_stop(last_cycle: Option<(u64, usize)>, stopped: u64) -> Missed {
    let Some((at, frames)) = last_cycle else {
        return Missed::default();
    };
    let mut clock = CycleClock::new(SAMPLE_RATE);
    clock.rescale(SAMPLE_RATE as u64, 1_000_000_000);
    clock.advance(at, frames);
    clock.missed_until(stopped)
}

/// `CLOCK_MONOTONIC` in nanoseconds, the clock of `pw_time`'s `now`.
fn monotonic_nanos() -> u64 {
    let now = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);
    // Seconds since boot and nanoseconds within one: both small and
    // positive.
    (now.tv_sec as u64)
        .saturating_mul(1_000_000_000)
        .saturating_add(now.tv_nsec as u64)
}

/// For tests: `work` through a new open [`Gate`], as [`process`] does, for
/// the allocation count in `tests/realtime.rs`.
#[doc(hidden)]
#[must_use]
pub fn through_an_open_gate(work: impl FnOnce()) -> bool {
    Gate::new().deliver(work)
}

/// The capture and playback latencies in frames at [`SAMPLE_RATE`], for a
/// first cycle of `cycle` frames on a graph clock of `clock` (none or a
/// zero denominator reads as 48 kHz).
fn latency_frames(
    (input, output): (Latency, Latency),
    cycle: usize,
    clock: Option<spa::utils::Fraction>,
) -> (usize, usize) {
    let rate = clock.map_or(0, |clock| clock.denom);
    (input.frames(cycle, rate), output.frames(cycle, rate))
}

pub(crate) fn failure(what: &'static str) -> impl FnOnce(pw::Error) -> String {
    move |error| format!("{what}: {error}")
}

fn failed(what: &'static str) -> impl FnOnce(pw::Error) -> CaptureError {
    move |error| CaptureError::BackendFailed(failure(what)(error))
}

/// A new connection to the daemon: loop, context, core and registry; the
/// error names the step that failed.
pub(crate) fn connect() -> Result<
    (
        pw::main_loop::MainLoopRc,
        pw::context::ContextRc,
        pw::core::CoreRc,
        pw::registry::RegistryRc,
    ),
    String,
> {
    pw::init();
    let main_loop =
        pw::main_loop::MainLoopRc::new(None).map_err(failure("the PipeWire main loop"))?;
    let context =
        pw::context::ContextRc::new(&main_loop, None).map_err(failure("the PipeWire context"))?;
    let core = context
        .connect_rc(None)
        .map_err(failure("connecting to PipeWire (is it running?)"))?;
    let registry = core
        .get_registry_rc()
        .map_err(failure("the PipeWire registry"))?;
    Ok((main_loop, context, core, registry))
}

#[derive(Default)]
struct Shared {
    graph: RefCell<Graph>,
    /// Whether the connection binds the `default` metadata.
    reads_defaults: bool,
    /// The `default` metadata once bound: its id, its listener, then the
    /// proxy, so the listener drops first.
    metadata: RefCell<Option<(u32, pw::metadata::MetadataListener, pw::metadata::Metadata)>>,
    done: Cell<Option<spa::utils::result::AsyncSeq>>,
    lost: Cell<Lost>,
    /// The connection to the daemon failed: judged as
    /// [`DeviceChangeReason::AudioServiceRestarted`].
    disconnected: Cell<bool>,
    /// The first and the last change since the graph was last judged.
    pending: Cell<Option<(Instant, Instant)>>,
}

impl Shared {
    fn changed(&self) {
        let now = Instant::now();
        let first = self.pending.get().map_or(now, |(first, _)| first);
        self.pending.set(Some((first, now)));
    }

    /// When the pending changes are judged; `None` while nothing is pending.
    fn due(&self) -> Option<Instant> {
        self.pending
            .get()
            .map(|(first, last)| judged_at(first, last))
    }

    /// `what` failed for good: the capture lost `lost`.
    fn fail(&self, what: &str, message: &str, lost: Lost) {
        tracing::warn!("{what} failed: {message}");
        self.lost.set(self.lost.get().union(lost));
        self.changed();
    }

    /// The connection to the daemon failed for good: both lanes are lost,
    /// and the change is the daemon's.
    fn lost_the_connection(&self, message: &str) {
        self.disconnected.set(true);
        self.fail("the connection to PipeWire", message, Lost::ALL);
    }

    /// The judgement of the pending changes: the audio service restarting
    /// after a lost connection, else the first difference from `baseline`.
    fn judgement(
        &self,
        snapshot: impl FnOnce() -> DeviceSnapshot,
        baseline: &DeviceSnapshot,
    ) -> Option<DeviceChangeReason> {
        if self.disconnected.get() {
            Some(DeviceChangeReason::AudioServiceRestarted)
        } else {
            snapshot().difference(baseline)
        }
    }

    /// The stream's new state; its error state loses both lanes.
    fn stream_state(&self, state: &pw::stream::StreamState) {
        if let pw::stream::StreamState::Error(message) = state {
            self.fail("the PipeWire capture stream", message, Lost::ALL);
        }
    }

    /// A registry global: nodes and ports into the graph, the `default`
    /// metadata bound once.
    fn announce(
        self: &Rc<Self>,
        registry: &pw::registry::RegistryWeak,
        global: &pw::registry::GlobalObject<&spa::utils::dict::DictRef>,
    ) {
        let Some(props) = global.props else {
            return;
        };
        match global.type_ {
            ObjectType::Node | ObjectType::Port => {
                self.add_object(&global.type_, global.id, |k| props.get(k));
            }
            ObjectType::Metadata
                if self.reads_defaults
                    && props.get("metadata.name") == Some("default")
                    && self.metadata.borrow().is_none() =>
            {
                let Some(registry) = registry.upgrade() else {
                    return;
                };
                let metadata: pw::metadata::Metadata = match registry.bind(global) {
                    Ok(metadata) => metadata,
                    Err(error) => {
                        tracing::warn!("binding the PipeWire default metadata failed: {error}");
                        return;
                    }
                };
                // Weak: the listener lives in `self.metadata`.
                let shared = Rc::downgrade(self);
                let listener = metadata
                    .add_listener_local()
                    .property(move |subject, key, _type, value| {
                        if subject == pw::core::PW_ID_CORE
                            && let Some(shared) = shared.upgrade()
                        {
                            tracing::debug!("PipeWire default metadata: {key:?} = {value:?}");
                            if shared.graph.borrow_mut().set_default(key, value) {
                                shared.changed();
                            }
                        }
                        0
                    })
                    .register();
                *self.metadata.borrow_mut() = Some((global.id, listener, metadata));
            }
            _ => {}
        }
    }

    /// A node or port global into the graph. The chosen source, or one of
    /// its ports, announced again is a change, so a capture on the fallback
    /// returns to it.
    fn add_object<'a>(&self, kind: &ObjectType, id: u32, props: impl Fn(&str) -> Option<&'a str>) {
        let chosen = match kind {
            ObjectType::Node => self.graph.borrow_mut().add_node(id, props),
            ObjectType::Port => self.graph.borrow_mut().add_port(id, props),
            _ => false,
        };
        if chosen {
            tracing::info!("the chosen source's {kind:?} {id} was announced");
            self.changed();
        }
    }

    /// From now on the graph serves the capture that resolved `targets` for
    /// `uid`, the UID asked for; returns the baseline later snapshots are
    /// compared with.
    fn track(&self, targets: &Targets, uid: Option<&str>) -> DeviceSnapshot {
        let mut graph = self.graph.borrow_mut();
        graph.track(targets, uid);
        graph.snapshot(targets, uid, Lost::NONE)
    }

    /// A registry global went away: a node, a port, or the `default`
    /// metadata, whose defaults go with it until it is announced again.
    fn forget(&self, id: u32) {
        let metadata = self
            .metadata
            .borrow()
            .as_ref()
            .is_some_and(|(bound, ..)| *bound == id);
        let changed = if metadata {
            drop(self.metadata.borrow_mut().take());
            self.graph.borrow_mut().set_default(None, None)
        } else {
            self.graph.borrow_mut().remove(id)
        };
        if changed {
            self.changed();
        }
    }
}

/// One connection to the PipeWire daemon and the registry view over it.
/// Fields drop in declaration order: listeners before the proxies they
/// listen on, `shared` after the listeners whose closures hold it, proxies
/// before the core, the core before the context and the loop.
struct Connection {
    _registry_listener: pw::registry::Listener,
    _core_listener: pw::core::Listener,
    shared: Rc<Shared>,
    registry: pw::registry::RegistryRc,
    core: pw::core::CoreRc,
    _context: pw::context::ContextRc,
    main_loop: pw::main_loop::MainLoopRc,
}

impl Drop for Connection {
    fn drop(&mut self) {
        // Runs after a capture's stream and links are destroyed.
        tracing::debug!("closing the PipeWire connection");
    }
}

impl Connection {
    /// A connection; `reads_defaults` binds the `default` metadata too.
    fn open(reads_defaults: bool) -> Result<Self, CaptureError> {
        let (main_loop, context, core, registry) =
            connect().map_err(CaptureError::BackendFailed)?;
        let shared = Rc::new(Shared {
            reads_defaults,
            ..Shared::default()
        });
        let core_listener = core
            .add_listener_local()
            .done({
                let shared = Rc::clone(&shared);
                move |id, seq| {
                    if id == pw::core::PW_ID_CORE {
                        shared.done.set(Some(seq));
                    }
                }
            })
            .error({
                let shared = Rc::clone(&shared);
                move |id, _seq, res, message| {
                    if id == pw::core::PW_ID_CORE {
                        shared.lost_the_connection(message);
                    } else {
                        tracing::warn!("PipeWire error on object {id}: {message} ({res})");
                    }
                }
            })
            .register();
        let registry_listener = registry
            .add_listener_local()
            .global({
                let shared = Rc::clone(&shared);
                let registry = registry.downgrade();
                move |global| shared.announce(&registry, global)
            })
            .global_remove({
                let shared = Rc::clone(&shared);
                move |id| shared.forget(id)
            })
            .register();
        Ok(Self {
            _registry_listener: registry_listener,
            _core_listener: core_listener,
            shared,
            registry,
            core,
            _context: context,
            main_loop,
        })
    }

    /// A connection that binds the `default` metadata, once the daemon has
    /// answered with the globals and the metadata's properties.
    fn read(deadline: Instant) -> Result<Self, CaptureError> {
        let connection = Self::open(true)?;
        connection.roundtrip(deadline)?;
        connection.roundtrip(deadline)?;
        Ok(connection)
    }

    /// Runs the loop until `ready` holds, the deadline passes or the
    /// connection is lost; whether `ready` held.
    fn pump_until(&self, deadline: Instant, mut ready: impl FnMut() -> bool) -> bool {
        loop {
            if ready() {
                return true;
            }
            let now = Instant::now();
            if now >= deadline || self.shared.lost.get().any() {
                return false;
            }
            self.main_loop
                .loop_()
                .iterate(pw::loop_::Timeout::Finite((deadline - now).min(PUMP_SLICE)));
        }
    }

    /// Waits until the daemon has answered everything sent before. No
    /// answer by the deadline, the connection intact, is
    /// [`CaptureError::DidNotRun`]: the daemon waits on something in the
    /// graph.
    fn roundtrip(&self, deadline: Instant) -> Result<(), CaptureError> {
        let pending = self.core.sync(0).map_err(failed("a PipeWire roundtrip"))?;
        if self.pump_until(deadline, || self.shared.done.get() == Some(pending)) {
            Ok(())
        } else {
            Err(self.stalled_as("answer", CaptureError::DidNotRun))
        }
    }

    fn stalled(&self, step: &str) -> CaptureError {
        self.stalled_as(step, CaptureError::BackendFailed)
    }

    /// [`Self::stalled`] with the deadline's error made by `timed_out`.
    fn stalled_as(&self, step: &str, timed_out: fn(String) -> CaptureError) -> CaptureError {
        if self.shared.lost.get().any() {
            CaptureError::BackendFailed(
                "the connection to PipeWire, the capture stream or a link failed".into(),
            )
        } else {
            timed_out(format!(
                "PipeWire did not {step} within {} s",
                START_TIMEOUT.as_secs()
            ))
        }
    }
}

/// The capture stream's format: 48 kHz native-endian `f32`, `channels`
/// interleaved at `AUX0`, `AUX1`, …, so the adapter neither remixes nor
/// reorders.
fn format_pod(channels: usize) -> Result<Vec<u8>, CaptureError> {
    let mut info = spa::param::audio::AudioInfoRaw::new();
    info.set_format(if cfg!(target_endian = "little") {
        spa::param::audio::AudioFormat::F32LE
    } else {
        spa::param::audio::AudioFormat::F32BE
    });
    // Whole hertz.
    info.set_rate(SAMPLE_RATE as u32);
    info.set_channels(channels as u32);
    let mut position = [0u32; spa::sys::SPA_AUDIO_MAX_CHANNELS as usize];
    for (channel, slot) in position.iter_mut().take(channels).enumerate() {
        *slot = spa::sys::SPA_AUDIO_CHANNEL_AUX0 + channel as u32;
    }
    info.set_position(position);
    let object = spa::pod::Object {
        type_: spa::utils::SpaTypes::ObjectParamFormat.as_raw(),
        id: spa::param::ParamType::EnumFormat.as_raw(),
        properties: info.into(),
    };
    spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &spa::pod::Value::Object(object),
    )
    .map(|(cursor, _)| cursor.into_inner())
    .map_err(|error| CaptureError::BackendFailed(format!("the capture format: {error:?}")))
}

/// The lower bounds of a `SPA_PARAM_Latency` object for `direction`;
/// `None` for the other direction or another pod.
fn parse_latency(pod: &spa::pod::Pod, direction: u32) -> Option<Latency> {
    let object = pod.as_object().ok()?;
    let prop = |key: u32| {
        object
            .find_prop(spa::utils::Id(key))
            .map(spa::pod::PodProp::value)
    };
    let found = prop(spa::sys::SPA_PARAM_LATENCY_direction)?.get_id().ok()?;
    if found.0 != direction {
        return None;
    }
    Some(Latency {
        quantum: prop(spa::sys::SPA_PARAM_LATENCY_minQuantum)
            .and_then(|v| v.get_float().ok())
            .unwrap_or(0.0),
        rate: prop(spa::sys::SPA_PARAM_LATENCY_minRate)
            .and_then(|v| v.get_int().ok())
            .unwrap_or(0),
        ns: prop(spa::sys::SPA_PARAM_LATENCY_minNs)
            .and_then(|v| v.get_long().ok())
            .unwrap_or(0),
    })
}

/// One started capture on its PipeWire thread. `Drop` disconnects the
/// stream (no `process` runs after that returns); the fields then drop in
/// declaration order: listeners while the stream exists, the stream, the
/// links, then the connection. Another order would be a use after free.
struct Capture {
    _rt_listener: pw::stream::StreamListener<RealTime>,
    _state_listener: pw::stream::StreamListener<()>,
    stream: pw::stream::StreamRc,
    links: Vec<WatchedLink>,
    connection: Connection,
    targets: Targets,
    input_device_uid: Option<String>,
    sink: Arc<LaneFrameSink>,
    gate: Arc<Gate>,
    cycle_frames: Arc<AtomicUsize>,
    info: CaptureStream,
    /// The devices the targets resolved to, taken before the loop runs
    /// again.
    baseline: DeviceSnapshot,
}

/// One of Steno's links and the listeners that mark its lane lost when it
/// fails or is removed. The listeners drop first.
struct WatchedLink {
    _info: pw::link::LinkListener,
    _error: pw::proxy::ProxyListener,
    _link: pw::link::Link,
}

impl Drop for Capture {
    fn drop(&mut self) {
        // No `process` runs once this returned. `pw_stream_disconnect`
        // returns 0 whatever happens; the log is for a version that does not.
        if let Err(error) = self.stream.disconnect() {
            tracing::warn!("disconnecting the PipeWire capture stream failed: {error}");
        }
        tracing::debug!("the PipeWire capture stream is disconnected");
    }
}

impl Capture {
    /// Everything `start` does, on the PipeWire thread; once `new`
    /// returned, an early return tears down through `Drop`. `answered` is
    /// raised once the daemon answered the first roundtrips.
    fn open(
        lanes: &[AudioLane],
        input_device_uid: Option<&str>,
        sink: Arc<LaneFrameSink>,
        gate: Arc<Gate>,
        answered: &AtomicBool,
    ) -> Result<Self, CaptureError> {
        let started = Instant::now();
        let deadline = started + START_TIMEOUT;
        let connection = Connection::read(deadline)?;
        answered.store(true, Ordering::Release);
        let mut capture =
            Self::linked_on(connection, lanes, input_device_uid, sink, gate, deadline)?;
        capture.measure(deadline)?;
        start_log!(
            info,
            "the PipeWire capture runs {} ms after start: input latency {} frames, output \
             latency {} frames",
            started.elapsed().as_millis(),
            capture.info.input_latency_frames,
            capture.info.output_latency_frames
        );
        Ok(capture)
    }

    /// Whether the source `uid` names runs now, asked on a connection and a
    /// stream of its own (its lane linked into a sink nobody reads) so the
    /// recording is not touched. `false` when `uid` names no recordable
    /// source, anything fails, or no cycle came by `START_TIMEOUT`. A
    /// Bluetooth source answers `false` unlinked: linking its input switches
    /// the headset to the hands-free profile, moving the system lane.
    ///
    /// A source that negotiated and then hung could hold up the driver the
    /// recording runs on; the watchdog then rebuilds as for any stall.
    fn probe(uid: &str) -> bool {
        let lanes = [AudioLane::Mixed];
        let deadline = Instant::now() + START_TIMEOUT;
        let sink = Arc::new(LaneFrameSink::new(&lanes));
        let probed = Connection::read(deadline)
            .and_then(|connection| {
                let graph = connection.shared.graph.borrow();
                if graph.known_source(Some(uid)).is_none() {
                    return Err(CaptureError::InputDeviceUnavailable);
                }
                if graph.is_bluetooth_source(uid) {
                    tracing::info!("the input device {uid} is a Bluetooth source; not probed");
                    return Err(CaptureError::InputDeviceUnavailable);
                }
                drop(graph);
                Self::linked_on(
                    connection,
                    &lanes,
                    Some(uid),
                    sink,
                    Arc::new(Gate::new()),
                    deadline,
                )
            })
            .map(|capture| {
                let chosen = capture
                    .info
                    .input
                    .as_ref()
                    .is_some_and(|input| !input.is_fallback);
                chosen
                    && capture.connection.pump_until(deadline, || {
                        capture.cycle_frames.load(Ordering::Acquire) > 0
                    })
            });
        match probed {
            Ok(runs) => runs,
            Err(error) => {
                tracing::info!("the input device {uid} did not answer the probe: {error}");
                false
            }
        }
    }

    /// [`Self::open`] up to the links, on a connection that has read the
    /// graph, before any cycle ran.
    fn linked_on(
        connection: Connection,
        lanes: &[AudioLane],
        input_device_uid: Option<&str>,
        sink: Arc<LaneFrameSink>,
        gate: Arc<Gate>,
        deadline: Instant,
    ) -> Result<Self, CaptureError> {
        let (targets, input) = {
            let graph = connection.shared.graph.borrow();
            let targets = graph.resolve(lanes, graph.known_source(input_device_uid))?;
            let input = graph.input(&targets, input_device_uid);
            if let (Some(uid), Some(input)) = (input_device_uid, &input)
                && input.is_fallback
            {
                start_log!(
                    warn,
                    "the input device {uid} is not connected; recording from the default \
                     source {}",
                    input.uid
                );
            }
            (targets, input)
        };
        let mut capture = Self::new(connection, targets, input_device_uid, input, sink, gate)?;
        capture.link(deadline)?;
        Ok(capture)
    }

    /// The capture stream for `targets` with its two listeners, not yet
    /// connected. `input_device_uid` is the UID asked for, which the
    /// snapshot keeps following; `input` the microphone it resolved to.
    fn new(
        connection: Connection,
        targets: Targets,
        input_device_uid: Option<&str>,
        input: Option<CaptureInput>,
        sink: Arc<LaneFrameSink>,
        gate: Arc<Gate>,
    ) -> Result<Self, CaptureError> {
        let mut props = pw::properties::PropertiesBox::new();
        props.insert(*pw::keys::MEDIA_TYPE, "Audio");
        props.insert(*pw::keys::MEDIA_CATEGORY, "Capture");
        props.insert(*pw::keys::NODE_NAME, STREAM_NODE_NAME);
        props.insert(*pw::keys::NODE_DESCRIPTION, "Steno recording");
        props.insert(*pw::keys::APP_NAME, "Steno");
        // Steno links the stream itself; the session manager must not.
        props.insert(*pw::keys::NODE_AUTOCONNECT, "false");
        props.insert(*pw::keys::NODE_DONT_RECONNECT, "true");
        props.insert(*pw::keys::STREAM_DONT_REMIX, "true");
        let stream = pw::stream::StreamRc::new(connection.core.clone(), STREAM_NODE_NAME, props)
            .map_err(failed("the PipeWire capture stream"))?;
        let cycle_frames = Arc::new(AtomicUsize::new(0));
        // Two listeners: `process` on the data-loop thread with its own
        // user data, `state_changed` on this one. One listener would share
        // `&mut` user data across two threads.
        let rt_listener = stream
            .add_local_listener_with_user_data(RealTime {
                sink: Arc::clone(&sink),
                gate: Arc::clone(&gate),
                sources: targets.layout.sources.clone(),
                channels: targets.channels(),
                cycle_frames: Arc::clone(&cycle_frames),
                clock: CycleClock::new(SAMPLE_RATE),
            })
            .process(process)
            .register()
            .map_err(failed("the capture stream's process callback"))?;
        let state_listener = stream
            .add_local_listener_with_user_data(())
            .state_changed({
                let shared = Rc::clone(&connection.shared);
                move |_, (), _old, new| shared.stream_state(&new)
            })
            .register()
            .map_err(failed("the capture stream's state callback"))?;
        let baseline = connection.shared.track(&targets, input_device_uid);
        Ok(Capture {
            _rt_listener: rt_listener,
            _state_listener: state_listener,
            stream,
            links: Vec::new(),
            connection,
            info: CaptureStream {
                sample_rate: SAMPLE_RATE,
                input_latency_frames: 0,
                output_latency_frames: 0,
                layout: Some(targets.layout.clone()),
                input,
            },
            targets,
            input_device_uid: input_device_uid.map(str::to_owned),
            sink,
            gate,
            cycle_frames,
            baseline,
        })
    }

    /// Connects the stream, waits for its ports, and links each.
    fn link(&mut self, deadline: Instant) -> Result<(), CaptureError> {
        let channels = self.targets.channels();
        let format = format_pod(channels)?;
        let pod = spa::pod::Pod::from_bytes(&format)
            .ok_or_else(|| CaptureError::BackendFailed("the capture format is not a pod".into()))?;
        let stream = &self.stream;
        stream
            .connect(
                spa::utils::Direction::Input,
                None,
                pw::stream::StreamFlags::MAP_BUFFERS | pw::stream::StreamFlags::RT_PROCESS,
                &mut [pod],
            )
            .map_err(failed("connecting the capture stream"))?;

        let connection = &self.connection;
        let mut stream_ports = None;
        if !connection.pump_until(deadline, || {
            let node = stream.node_id();
            stream_ports = (node != pw::constants::ID_ANY)
                .then(|| {
                    connection
                        .shared
                        .graph
                        .borrow()
                        .stream_ports(node, channels)
                })
                .flatten();
            stream_ports.is_some()
        }) {
            return Err(
                connection.stalled_as("create the capture stream's ports", CaptureError::DidNotRun)
            );
        }
        let stream_node = stream.node_id();
        let mut links = Vec::with_capacity(channels);
        for (&(node, port), input) in self
            .targets
            .feeds
            .iter()
            .zip(stream_ports.unwrap_or_default())
        {
            let mut props = pw::properties::PropertiesBox::new();
            props.insert(*pw::keys::LINK_OUTPUT_NODE, node.to_string());
            props.insert(*pw::keys::LINK_OUTPUT_PORT, port.to_string());
            props.insert(*pw::keys::LINK_INPUT_NODE, stream_node.to_string());
            props.insert(*pw::keys::LINK_INPUT_PORT, input.to_string());
            props.insert(*pw::keys::OBJECT_LINGER, "false");
            let link = connection
                .core
                .create_object::<pw::link::Link>("link-factory", &props)
                .map_err(failed("linking the capture stream"))?;
            let lost = self.targets.lost_with(node, port);
            links.push(Self::watch_link(&connection.shared, link, lost));
        }
        self.links = links;
        Ok(())
    }

    /// `link` with listeners that mark the lane `lost` when it fails or is
    /// removed. Steno's own teardown never fires them: they drop first.
    fn watch_link(shared: &Rc<Shared>, link: pw::link::Link, lost: Lost) -> WatchedLink {
        let fail = {
            let shared = Rc::downgrade(shared);
            move |message: &str| {
                if let Some(shared) = shared.upgrade() {
                    shared.fail("a capture link", message, lost);
                }
            }
        };
        let info = link
            .add_listener_local()
            .info({
                let fail = fail.clone();
                move |info| {
                    if let pw::link::LinkState::Error(message) = info.state() {
                        fail(message);
                    }
                }
            })
            .register();
        let error = link
            .upcast_ref()
            .add_listener_local()
            .error({
                let fail = fail.clone();
                move |_seq, _res, message| fail(message)
            })
            .removed(move || fail("the server removed it"))
            .register();
        WatchedLink {
            _info: info,
            _error: error,
            _link: link,
        }
    }

    /// Waits for the first cycle, then reads the latencies. A failed link
    /// fails the start.
    fn measure(&mut self, deadline: Instant) -> Result<(), CaptureError> {
        let connection = &self.connection;
        let linked = Instant::now();
        if !connection.pump_until(deadline, || self.cycle_frames.load(Ordering::Acquire) > 0) {
            return Err(connection.stalled_as("run the capture", CaptureError::DidNotRun));
        }
        let cycle = self.cycle_frames.load(Ordering::Relaxed);
        tracing::debug!(
            "the first cycle, {cycle} frames, came {} ms after linking",
            linked.elapsed().as_millis()
        );
        warn_without_realtime();
        let clock = self.stream.time().ok().map(|time| time.rate());
        // Its own deadline, so a late first cycle cannot leave the far-end
        // delay at zero.
        let latencies = self.latencies(Instant::now() + LATENCY_TIMEOUT);
        if connection.shared.lost.get().any() {
            return Err(connection.stalled("run the capture"));
        }
        (
            self.info.input_latency_frames,
            self.info.output_latency_frames,
        ) = latency_frames(latencies, cycle, clock);
        Ok(())
    }

    /// The microphone port's capture latency and the sink's playback
    /// latency; zero where missing (logged; the Speex tail absorbs it).
    fn latencies(&self, deadline: Instant) -> (Latency, Latency) {
        let connection = &self.connection;
        let requests = [
            (
                self.targets.mic.as_ref().and_then(|m| m.latency_port),
                spa::sys::SPA_DIRECTION_OUTPUT,
            ),
            (
                self.targets.output.as_ref().and_then(|o| o.latency_port),
                spa::sys::SPA_DIRECTION_INPUT,
            ),
        ];
        let found = [Rc::new(Cell::new(None)), Rc::new(Cell::new(None))];
        let mut bound = Vec::new();
        for ((port, direction), slot) in requests.into_iter().zip(&found) {
            let Some(id) = port else { continue };
            let global = pw::registry::GlobalObject {
                id,
                permissions: pw::permissions::PermissionFlags::empty(),
                type_: ObjectType::Port,
                version: 0,
                props: None::<pw::properties::PropertiesBox>,
            };
            let proxy: pw::port::Port = match connection.registry.bind(&global) {
                Ok(proxy) => proxy,
                Err(error) => {
                    tracing::warn!("binding port {id} for its latency failed: {error}");
                    continue;
                }
            };
            let slot = Rc::clone(slot);
            let listener = proxy
                .add_listener_local()
                .param(move |_seq, id, _index, _next, pod| {
                    if id == spa::param::ParamType::Latency
                        && let Some(latency) = pod.and_then(|pod| parse_latency(pod, direction))
                    {
                        slot.set(Some(latency));
                    }
                })
                .register();
            proxy.enum_params(0, Some(spa::param::ParamType::Latency), 0, u32::MAX);
            bound.push((listener, proxy));
        }
        if !bound.is_empty()
            && let Err(error) = connection.roundtrip(deadline)
        {
            tracing::warn!("reading the device latencies: {error}");
        }
        drop(bound);
        let [input, output] = found;
        (
            input.get().unwrap_or_default(),
            output.get().unwrap_or_default(),
        )
    }

    /// The loop after `start` answered: changes are coalesced and judged
    /// until the quit.
    fn watch(&self, quit: pw::channel::Receiver<()>) {
        let quitting = Rc::new(Cell::new(false));
        let main_loop = self.connection.main_loop.loop_();
        let _attached = quit.attach(main_loop, {
            let quitting = Rc::clone(&quitting);
            move |()| quitting.set(true)
        });
        let shared = &self.connection.shared;
        while !quitting.get() {
            let wait = match shared.due() {
                None => IDLE_WAIT,
                Some(due) => {
                    let now = Instant::now();
                    if now >= due {
                        shared.pending.set(None);
                        self.judge();
                        continue;
                    }
                    due - now
                }
            };
            if main_loop.iterate(pw::loop_::Timeout::Finite(wait)) < 0 {
                // A failing loop returns at once; do not spin.
                std::thread::sleep(PUMP_SLICE);
            }
        }
    }

    /// Reports the first difference from the baseline to the sink while the
    /// gate is open; a lost connection reads as the audio service
    /// restarting.
    fn judge(&self) {
        let shared = &self.connection.shared;
        let reason = shared.judgement(
            || {
                shared.graph.borrow().snapshot(
                    &self.targets,
                    self.input_device_uid.as_deref(),
                    shared.lost.get(),
                )
            },
            &self.baseline,
        );
        match reason {
            None => tracing::info!("ignored a PipeWire graph change"),
            Some(reason) => report(&self.gate, &self.sink, reason),
        }
    }
}

/// When changes from `first` to `last` are judged:
/// [`LiveCaptureBackend::COALESCE_DELAY`] after the last, at most
/// [`COALESCE_LIMIT`] after the first.
fn judged_at(first: Instant, last: Instant) -> Instant {
    (last + LiveCaptureBackend::COALESCE_DELAY).min(first + COALESCE_LIMIT)
}

/// Reports `reason` to `sink` through `gate`. The log comes after the pass,
/// so a blocking log write never holds [`Gate::close`].
fn report(gate: &Gate, sink: &LaneFrameSink, reason: DeviceChangeReason) {
    if gate.report(|| sink.report_device_change(reason)) {
        tracing::info!("PipeWire graph change reported {reason:?}");
    }
}

type Answer = Result<CaptureStream, CaptureError>;

/// The channel the thread answers `start` through: a rendezvous, so a send
/// completes only into a `start` still waiting. With a buffer, a late
/// answer would let the thread go on to watch, and a report could hold
/// [`Gate::close`] while `start` holds the session mutex.
fn answer_channel() -> (SyncSender<Answer>, Receiver<Answer>) {
    sync_channel(0)
}

/// `start`'s wait for the thread's answer, at most `limit`; a later answer
/// fails its send. No answer although the daemon answered (`connected`) is
/// [`CaptureError::DidNotRun`], held up by the graph; otherwise
/// `BackendFailed`.
fn await_answer(answered: Receiver<Answer>, limit: Duration, connected: &AtomicBool) -> Answer {
    let answer = answered.recv_timeout(limit);
    drop(answered);
    answer.unwrap_or_else(|_| {
        let silent = format!("the PipeWire thread did not answer within {limit:?}");
        Err(if connected.load(Ordering::Acquire) {
            CaptureError::DidNotRun(silent)
        } else {
            CaptureError::BackendFailed(silent)
        })
    })
}

/// Answers `start` and returns the capture to watch once `start` took its
/// stream; `None` when opening failed or `start` gave up, dropping the
/// capture unwatched.
fn hand_over<C>(
    answer: &SyncSender<Answer>,
    opened: Result<C, CaptureError>,
    info: impl FnOnce(&C) -> CaptureStream,
) -> Option<C> {
    match opened {
        Err(error) => {
            let _ = answer.send(Err(error));
            None
        }
        Ok(capture) => {
            if answer.send(Ok(info(&capture))).is_ok() {
                Some(capture)
            } else {
                start_log!(
                    warn,
                    "start gave up on the PipeWire capture; tearing it down"
                );
                None
            }
        }
    }
}

/// The `steno-pipewire` thread: start, answer, watch until the quit, tear
/// down.
fn run(
    lanes: &[AudioLane],
    input_device_uid: Option<&str>,
    sink: Arc<LaneFrameSink>,
    gate: Arc<Gate>,
    answer: &SyncSender<Answer>,
    quit: pw::channel::Receiver<()>,
    connected: &AtomicBool,
) {
    let opened = Capture::open(lanes, input_device_uid, sink, gate, connected);
    let Some(capture) = hand_over(answer, opened, |capture| capture.info.clone()) else {
        return;
    };
    start_log::quietly(false, || capture.watch(quit));
    tracing::debug!("the PipeWire capture got its quit; tearing down");
    drop(capture);
    tracing::debug!("the PipeWire capture is torn down");
}

struct Active {
    quit: pw::channel::Sender<()>,
    gate: Arc<Gate>,
    sink: Arc<LaneFrameSink>,
    ended: Receiver<()>,
    thread: JoinHandle<()>,
    /// The thread's kernel id, 0 when unknown, for the log of a hung
    /// teardown.
    thread_id: Arc<AtomicU32>,
}

/// Whether PipeWire's data-loop threads here run real-time: `Some(false)`
/// when one does not, `None` when none could be read.
fn data_loop_is_realtime() -> Option<bool> {
    let mut realtime = None;
    for task in std::fs::read_dir("/proc/self/task").ok()?.flatten() {
        let path = task.path();
        let named = std::fs::read_to_string(path.join("comm"))
            .is_ok_and(|name| name.trim_end().starts_with("data-loop"));
        if !named {
            continue;
        }
        let Some(this) = std::fs::read_to_string(path.join("stat"))
            .ok()
            .and_then(|stat| runs_realtime(&stat))
        else {
            continue;
        };
        realtime = Some(realtime.unwrap_or(true) && this);
    }
    realtime
}

/// Whether the task with this `/proc/.../stat` line runs `SCHED_FIFO` or
/// `SCHED_RR`. The name may hold any character, so fields count from the
/// last `)`: the policy is the 39th after it.
fn runs_realtime(stat: &str) -> Option<bool> {
    let (_, after) = stat.rsplit_once(')')?;
    let policy = after.split_whitespace().nth(38)?.parse::<u32>().ok()?;
    Some(matches!(policy, 1 | 2))
}

/// Logs once per process, at `warn`, that the data-loop thread runs without
/// real-time scheduling (`module-rt` got neither `RLIMIT_RTPRIO` nor
/// RealtimeKit), so a busy computer may miss cycles.
fn warn_without_realtime() {
    static WARNED: AtomicBool = AtomicBool::new(false);
    if !WARNED.load(Ordering::Relaxed)
        && data_loop_is_realtime() == Some(false)
        && !WARNED.swap(true, Ordering::Relaxed)
    {
        tracing::warn!(
            "PipeWire's data-loop thread runs without real-time scheduling (RealtimeKit \
             missing or refused, no RLIMIT_RTPRIO): a busy computer can make the capture \
             miss cycles, which are written as silence or counted"
        );
    }
}

fn kernel_thread_id() -> u32 {
    std::fs::read_link("/proc/thread-self")
        .ok()
        .and_then(|path| path.file_name()?.to_str()?.parse().ok())
        .unwrap_or(0)
}

/// Where thread `id` waits, from `/proc`, for the log of a hung teardown.
fn where_it_waits(id: u32) -> String {
    if id == 0 {
        return "its kernel id is unknown".into();
    }
    let read = |file: &str| {
        std::fs::read_to_string(format!("/proc/self/task/{id}/{file}")).map_or_else(
            |error| format!("unreadable ({error})"),
            |text| text.trim().to_owned(),
        )
    };
    format!(
        "thread {id} in system call {}, waiting in {}",
        read("syscall"),
        read("wchan")
    )
}

/// The PipeWire backend. Restartable: `stop()` closes the gate and joins
/// the capture thread within 2 s, and `start` connects afresh.
pub struct LiveCaptureBackend {
    active: Mutex<Option<Active>>,
}

impl Default for LiveCaptureBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for LiveCaptureBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiveCaptureBackend").finish_non_exhaustive()
    }
}

impl LiveCaptureBackend {
    /// How long a burst of graph changes settles before it is judged (a
    /// Bluetooth profile switch). A burst that never settles is judged 2 s
    /// after its first change.
    pub const COALESCE_DELAY: Duration = Duration::from_millis(500);

    #[must_use]
    pub fn new() -> Self {
        Self {
            active: Mutex::new(None),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<Active>> {
        self.active.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Closes the gate, asks the thread to quit and joins it, within
    /// `limit` (only a delivery inside the gate is waited for past it): a
    /// report or a thread that outlasts `limit` is logged and left behind
    /// the closed gate.
    fn end(active: Active, limit: Duration) {
        let deadline = Instant::now() + limit;
        let reports_left = active.gate.close(deadline);
        let _ = active.quit.send(());
        if !reports_left {
            // The report runs on the thread, which cannot have ended.
            tracing::error!(
                "a PipeWire device-change report is still in its handler {} s after \
                 the capture's gate closed ({}); it is left behind to finish, the \
                 session may act on it late, and the capture thread, cut off from the \
                 recording, ends once the report returns",
                limit.as_secs_f32(),
                where_it_waits(active.thread_id.load(Ordering::Relaxed))
            );
            return;
        }
        match active
            .ended
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        {
            Err(RecvTimeoutError::Timeout) => tracing::error!(
                "the PipeWire capture thread did not end within {} s ({}); it is left \
                 behind, cut off from the recording, and the devices may stay open \
                 until Steno quits",
                limit.as_secs_f32(),
                where_it_waits(active.thread_id.load(Ordering::Relaxed))
            ),
            Ok(()) | Err(RecvTimeoutError::Disconnected) => {
                if active.thread.join().is_err() {
                    tracing::warn!("the PipeWire capture thread panicked");
                }
            }
        }
    }
}

impl CaptureBackend for LiveCaptureBackend {
    /// Returns the stream once the first cycle arrived: always 48 kHz,
    /// both device latencies, one interleaved buffer.
    fn start(
        &self,
        lanes: &[AudioLane],
        input_device_uid: Option<&str>,
        sink: Arc<LaneFrameSink>,
    ) -> Result<CaptureStream, CaptureError> {
        let mut active = self.lock();
        if active.is_some() {
            return Err(CaptureError::InvalidState("backend already started".into()));
        }
        let (answer, answered) = answer_channel();
        let (quit, quit_receiver) = pw::channel::channel();
        let (ending, ended) = sync_channel::<()>(0);
        let gate = Arc::new(Gate::new());
        let lanes = lanes.to_vec();
        let input_device_uid = input_device_uid.map(str::to_owned);
        let thread_gate = Arc::clone(&gate);
        let thread_sink = Arc::clone(&sink);
        let thread_id = Arc::new(AtomicU32::new(0));
        let id_slot = Arc::clone(&thread_id);
        let connected = Arc::new(AtomicBool::new(false));
        let thread_connected = Arc::clone(&connected);
        let starts_quietly = start_log::is_quiet();
        let thread = std::thread::Builder::new()
            .name("steno-pipewire".into())
            .spawn(move || {
                id_slot.store(kernel_thread_id(), Ordering::Relaxed);
                let _ending = ending;
                start_log::quietly(starts_quietly, || {
                    run(
                        &lanes,
                        input_device_uid.as_deref(),
                        thread_sink,
                        thread_gate,
                        &answer,
                        quit_receiver,
                        &thread_connected,
                    );
                });
            })
            .map_err(|e| CaptureError::BackendFailed(format!("the PipeWire thread: {e}")))?;
        // The margin covers a thread slow to get scheduled.
        let outcome = await_answer(
            answered,
            START_TIMEOUT + LATENCY_TIMEOUT + Duration::from_secs(2),
            &connected,
        );
        let started = Active {
            quit,
            gate,
            sink,
            ended,
            thread,
            thread_id,
        };
        match outcome {
            Ok(_) => *active = Some(started),
            Err(_) => Self::end(started, STOP_TIMEOUT),
        }
        outcome
    }

    /// Returns within `STOP_TIMEOUT` once it has the backend, plus a
    /// delivery inside the gate. What the data-loop thread missed after its
    /// last cycle is written, except at a rebuild's stop, whose gap covers
    /// it.
    fn stop(&self) {
        let Some(active) = self.lock().take() else {
            return;
        };
        let stopped = monotonic_nanos();
        let (gate, sink) = (Arc::clone(&active.gate), Arc::clone(&active.sink));
        Self::end(active, STOP_TIMEOUT);
        // The gate is closed: no cycle writes the sink any more.
        if !sink.device_change_reported() {
            sink.write_missed(missed_at_stop(gate.last_cycle(), stopped));
        }
    }

    /// Every cycle delivers, the monitor included with nothing playing; a
    /// graph that stops is the session's watchdog to catch.
    fn delivers_continuously(&self, _lanes: &[AudioLane]) -> bool {
        true
    }

    fn probes_inputs(&self) -> bool {
        true
    }

    /// The chosen source's first cycle on a stream of its own; see
    /// `Capture::probe`.
    fn probe_input(&self, uid: &str) -> bool {
        let runs = Capture::probe(uid);
        tracing::info!("the input device {uid} runs on its own: {runs}");
        runs
    }
}

impl Drop for LiveCaptureBackend {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::TrySendError;

    use super::*;

    /// Stream ticks at the graph's rate become 48 kHz frames; a rate change
    /// or an unreadable time starts afresh and reports nothing.
    #[test]
    fn a_cycle_reads_its_jump_from_the_ticks_at_the_graph_rate() {
        let at = |denom| spa::utils::Fraction { num: 1, denom };
        let mut clock = CycleClock::new(48_000.0);
        let mut cycle = |ticks: u64, denom, frames| {
            missed_before(&mut clock, Some((ticks, at(denom))), frames).fill
        };
        assert_eq!(cycle(1_000, 48_000, 480), 0, "the first cycle");
        assert_eq!(cycle(1_480, 48_000, 480), 0);
        assert_eq!(cycle(2_920, 48_000, 480), 960, "two cycles missed");
        assert_eq!(
            cycle(3_400, 44_100, 480),
            0,
            "a new graph rate starts afresh"
        );
        assert_eq!(cycle(3_841, 44_100, 480), 0);
        assert_eq!(
            cycle(4_723, 44_100, 480),
            480,
            "441 ticks at 44.1 kHz are 480 frames"
        );
        assert_eq!(missed_before(&mut clock, None, 480).fill, 0, "no time");
        assert_eq!(
            missed_before(&mut clock, Some((6_000, at(44_100))), 480).fill,
            0,
            "after no time the clock starts afresh"
        );
        assert_eq!(
            missed_before(&mut clock, Some((7_000, at(0))), 480).fill,
            0,
            "no rate"
        );
        assert_eq!(
            missed_before(&mut clock, Some((7_441, at(44_100))), 480).fill,
            0
        );
        // A quantum going from 256 to 1 024 frames, as measured: the clock
        // jumps two new quanta, the wall clock one old and one new.
        let mut clock = CycleClock::new(48_000.0);
        let mut cycle =
            |ticks: u64, frames| missed_before(&mut clock, Some((ticks, at(48_000))), frames).fill;
        assert_eq!(cycle(0, 256), 0);
        assert_eq!(cycle(256, 256), 0);
        assert_eq!(cycle(2_304, 1_024), 0, "a new quantum starts afresh");
        assert_eq!(cycle(3_328, 1_024), 0);
        assert_eq!(cycle(5_376, 1_024), 1_024, "a missed cycle after it counts");
    }

    /// A cycle follows the zeros of what was missed before it; the
    /// unfilled frames are only counted.
    #[test]
    fn a_cycle_follows_the_zeros_of_what_it_missed() {
        let lanes = [AudioLane::Mic, AudioLane::System];
        let sink = LaneFrameSink::new(&lanes);
        let sources = [
            LaneSource {
                lane: AudioLane::Mic,
                left: crate::capture::ChannelRef::new(0, 0, 2),
                right: None,
            },
            LaneSource {
                lane: AudioLane::System,
                left: crate::capture::ChannelRef::new(0, 1, 2),
                right: None,
            },
        ];
        let memory = [0.5f32; 8];
        let view = SliceView {
            channels: 2,
            frames: 4,
            samples: Some(&memory),
        };
        deliver_cycle(
            view,
            Missed {
                fill: 3,
                unfilled: 0,
            },
            &sources,
            &sink,
        );
        deliver_cycle(
            view,
            Missed {
                fill: 0,
                unfilled: 50,
            },
            &sources,
            &sink,
        );
        for lane in 0..2 {
            let mut samples = [9.0f32; 11];
            assert!(sink.ring(lane).read(&mut samples));
            assert_eq!(
                samples,
                [0.0, 0.0, 0.0, 0.5, 0.5, 0.5, 0.5, 0.5, 0.5, 0.5, 0.5]
            );
        }
        assert_eq!(sink.filled_samples()[&AudioLane::System], 3);
        assert_eq!(sink.unfilled_samples()[&AudioLane::Mic], 50);
    }

    /// At the stop, the time since the last cycle beyond that cycle and the
    /// one in flight is missed.
    #[test]
    fn the_stop_reads_what_passed_since_the_last_cycle() {
        let at = 7_000_000_000;
        assert!(missed_at_stop(None, at).is_none(), "no cycle yet");
        let last = Some((at, 1_024));
        assert!(
            missed_at_stop(last, at + 40_000_000).is_none(),
            "40 ms: the cycle and the one in flight"
        );
        assert_eq!(
            missed_at_stop(last, at + 600_000_000).fill,
            28_800 - 2_048,
            "0.6 s stopped before the stop"
        );
        assert_eq!(
            missed_at_stop(last, at + 2_500_000_000).unfilled,
            120_000 - 2_048
        );
        assert!(
            missed_at_stop(last, at - 1).is_none(),
            "a cycle after the stop's reading"
        );
    }

    /// The scheduling policy is read past a name holding `) `.
    #[test]
    fn the_scheduling_policy_is_read_past_any_name() {
        let stat = |policy: u32| {
            let after: Vec<String> = (3..=52)
                .map(|field| {
                    if field == 41 {
                        policy.to_string()
                    } else {
                        field.to_string()
                    }
                })
                .collect();
            format!("4242 (data-loop) 1 (x) {}", after.join(" "))
        };
        assert_eq!(runs_realtime(&stat(0)), Some(false));
        assert_eq!(runs_realtime(&stat(1)), Some(true));
        assert_eq!(runs_realtime(&stat(2)), Some(true));
        assert_eq!(runs_realtime(&stat(3)), Some(false), "SCHED_BATCH");
        assert_eq!(runs_realtime("4242 (data-loop) S 1"), None, "cut short");
        assert_eq!(runs_realtime("no name"), None);
    }

    fn latency_pod(direction: u32, quantum: f32, rate: i32, ns: i64) -> Vec<u8> {
        use spa::pod::{Object, Property, PropertyFlags, Value};
        let property = |key: u32, value: Value| Property {
            key,
            flags: PropertyFlags::empty(),
            value,
        };
        let object = Object {
            type_: spa::utils::SpaTypes::ObjectParamLatency.as_raw(),
            id: spa::param::ParamType::Latency.as_raw(),
            properties: vec![
                property(
                    spa::sys::SPA_PARAM_LATENCY_direction,
                    Value::Id(spa::utils::Id(direction)),
                ),
                property(
                    spa::sys::SPA_PARAM_LATENCY_minQuantum,
                    Value::Float(quantum),
                ),
                property(
                    spa::sys::SPA_PARAM_LATENCY_maxQuantum,
                    Value::Float(quantum),
                ),
                property(spa::sys::SPA_PARAM_LATENCY_minRate, Value::Int(rate)),
                property(spa::sys::SPA_PARAM_LATENCY_maxRate, Value::Int(rate)),
                property(spa::sys::SPA_PARAM_LATENCY_minNs, Value::Long(ns)),
                property(spa::sys::SPA_PARAM_LATENCY_maxNs, Value::Long(ns)),
            ],
        };
        spa::pod::serialize::PodSerializer::serialize(
            std::io::Cursor::new(Vec::new()),
            &Value::Object(object),
        )
        .unwrap()
        .0
        .into_inner()
    }

    fn stream() -> CaptureStream {
        CaptureStream {
            sample_rate: SAMPLE_RATE,
            input_latency_frames: 0,
            output_latency_frames: 0,
            layout: None,
            input: None,
        }
    }

    /// On the fallback, the chosen node or one of its ports announced is a
    /// pending change; another app's node or the default's ports are not.
    #[test]
    fn the_chosen_source_announced_again_is_a_change() {
        fn props<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<&'a str> {
            move |key| pairs.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
        }
        let source = |name| [("node.name", name), ("media.class", "Audio/Source")];
        let port = |node| [("node.id", node), ("port.direction", "out")];
        let shared = Shared::default();
        shared.add_object(&ObjectType::Node, 41, props(&source("built-in")));
        shared.add_object(&ObjectType::Port, 55, props(&port("41")));
        shared.graph.borrow_mut().set_default(
            Some(graph::DEFAULT_SOURCE_KEY),
            Some(r#"{"name":"built-in"}"#),
        );
        let asked = Some("usb-mic");
        let targets = {
            let graph = shared.graph.borrow();
            graph
                .resolve(&[AudioLane::Mixed], graph.known_source(asked))
                .unwrap()
        };
        let baseline = shared.track(&targets, asked);
        assert_eq!(baseline.input_uid.as_deref(), Some("built-in"));
        shared.add_object(&ObjectType::Node, 90, props(&source("an-app")));
        shared.add_object(&ObjectType::Port, 56, props(&port("41")));
        assert_eq!(shared.due(), None, "nothing the capture asked for");

        shared.add_object(&ObjectType::Node, 42, props(&source("usb-mic")));
        assert!(shared.due().is_some(), "the chosen node is back");
        shared.pending.set(None);
        shared.add_object(&ObjectType::Port, 57, props(&port("42")));
        assert!(shared.due().is_some(), "and its port");
    }

    /// A lost connection is the audio service restarting, whatever the
    /// graph reads.
    #[test]
    fn a_lost_connection_is_the_audio_service_restarting() {
        let baseline = DeviceSnapshot {
            output_uid: Some("steno-test-sink".into()),
            default_output_uid: None,
            input_uid: Some("steno-test-mic".into()),
            output_alive: true,
            input_alive: true,
            sample_rate: SAMPLE_RATE,
        };
        let lost = DeviceSnapshot {
            output_alive: false,
            input_alive: false,
            ..baseline.clone()
        };
        let shared = Shared::default();
        shared.stream_state(&pw::stream::StreamState::Error("gone".into()));
        assert_eq!(
            shared.judgement(|| lost.clone(), &baseline),
            Some(DeviceChangeReason::OutputDeviceGone),
            "a failed stream is a lost device"
        );
        let shared = Shared::default();
        shared.lost_the_connection("connection error");
        assert_eq!(shared.lost.get(), Lost::ALL);
        assert!(shared.due().is_some(), "a change to judge");
        for snapshot in [lost, baseline.clone()] {
            assert_eq!(
                shared.judgement(|| snapshot, &baseline),
                Some(DeviceChangeReason::AudioServiceRestarted)
            );
        }
    }

    #[test]
    fn a_failed_stream_loses_both_lanes() {
        let shared = Shared::default();
        shared.stream_state(&pw::stream::StreamState::Paused);
        assert_eq!(shared.lost.get(), Lost::NONE);
        assert!(
            shared.due().is_none(),
            "a state that is no error is no change"
        );
        shared.stream_state(&pw::stream::StreamState::Error("gone".into()));
        assert_eq!(shared.lost.get(), Lost::ALL);
        assert!(shared.due().is_some(), "a change to judge");
    }

    #[test]
    fn what_each_failure_loses_adds_up() {
        let shared = Shared::default();
        let monitor = Lost {
            mic: false,
            output: true,
        };
        let mic = Lost {
            mic: true,
            output: false,
        };
        shared.fail("a capture link", "the server removed it", monitor);
        shared.fail("a capture link", "the server removed it", mic);
        assert_eq!(shared.lost.get(), Lost::ALL, "the monitor's loss stays");
        shared.fail("a capture link", "the server removed it", Lost::NONE);
        assert_eq!(shared.lost.get(), Lost::ALL);
    }

    /// Which of the gate's passes a test holds inside.
    #[derive(Clone, Copy)]
    enum Pass {
        Delivery,
        Report,
    }

    /// A pass of kind `pass` through `gate` on its own thread, held inside
    /// until the returned sender sends; the result is whether it ran.
    fn hold_inside(gate: &Arc<Gate>, pass: Pass) -> (SyncSender<()>, JoinHandle<bool>) {
        let (entered, inside) = sync_channel(0);
        let (release, released) = sync_channel::<()>(0);
        let thread = std::thread::spawn({
            let gate = Arc::clone(gate);
            move || {
                let work = || {
                    entered.send(()).unwrap();
                    released.recv().unwrap();
                };
                match pass {
                    Pass::Delivery => gate.deliver(work),
                    Pass::Report => gate.report(work),
                }
            }
        });
        inside.recv().unwrap();
        (release, thread)
    }

    /// `gate.close(deadline)` on its own thread, its result on the receiver.
    fn close_on_a_thread(gate: &Arc<Gate>, deadline: Instant) -> (Receiver<bool>, JoinHandle<()>) {
        let (returned, closed) = sync_channel(1);
        let thread = std::thread::spawn({
            let gate = Arc::clone(gate);
            move || returned.send(gate.close(deadline)).unwrap()
        });
        while gate.open.load(Ordering::SeqCst) {
            std::thread::yield_now();
        }
        (closed, thread)
    }

    #[test]
    fn nothing_passes_a_gate_once_it_closed() {
        let gate = Gate::new();
        let mut passed = 0;
        assert!(gate.deliver(|| passed += 1));
        assert!(gate.report(|| passed += 1));
        assert!(gate.close(Instant::now()), "nobody was inside");
        assert!(
            !gate.deliver(|| passed += 1),
            "the pass says it did not run"
        );
        assert!(!gate.report(|| passed += 1), "the pass says it did not run");
        assert_eq!(passed, 2);
        assert_eq!(gate.delivering.load(Ordering::SeqCst), 0);
        assert_eq!(*gate.reporting(), 0);
    }

    #[test]
    fn a_report_whose_handler_panics_leaves_the_gate() {
        let gate = Gate::new();
        let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            gate.report(|| panic!("the handler panics"))
        }));
        assert!(unwound.is_err());
        assert_eq!(*gate.reporting(), 0, "the report counted itself out");
        assert!(
            gate.close(Instant::now()),
            "close() finds nobody inside, rather than a report that died"
        );
    }

    #[test]
    fn a_delivery_never_takes_the_reports_lock() {
        let gate = Arc::new(Gate::new());
        let (ran, done) = sync_channel(1);
        let held = gate.reporting();
        let thread = std::thread::spawn({
            let gate = Arc::clone(&gate);
            move || ran.send(gate.deliver(|| ())).unwrap()
        });
        assert_eq!(
            done.recv_timeout(Duration::from_secs(1)),
            Ok(true),
            "a delivery waited for the lock a report holds"
        );
        drop(held);
        thread.join().unwrap();
    }

    #[test]
    fn close_waits_for_a_delivery_inside_past_its_deadline() {
        let gate = Arc::new(Gate::new());
        let (release, passer) = hold_inside(&gate, Pass::Delivery);
        // The deadline has passed already: only the delivery holds it.
        let (returned, closer) = close_on_a_thread(&gate, Instant::now());
        assert_eq!(
            returned.recv_timeout(Duration::from_millis(200)),
            Err(RecvTimeoutError::Timeout),
            "close() returned while a delivery was inside"
        );
        release.send(()).unwrap();
        assert_eq!(returned.recv_timeout(Duration::from_secs(10)), Ok(true));
        assert!(passer.join().unwrap(), "the delivery ran");
        closer.join().unwrap();
    }

    #[test]
    fn close_waits_for_a_report_that_leaves_before_its_deadline() {
        let gate = Arc::new(Gate::new());
        let (release, passer) = hold_inside(&gate, Pass::Report);
        let (returned, closer) = close_on_a_thread(&gate, Instant::now() + Duration::from_secs(60));
        assert_eq!(
            returned.recv_timeout(Duration::from_millis(200)),
            Err(RecvTimeoutError::Timeout),
            "close() returned while a report was inside, before its deadline"
        );
        release.send(()).unwrap();
        assert_eq!(returned.recv_timeout(Duration::from_secs(10)), Ok(true));
        assert!(passer.join().unwrap());
        closer.join().unwrap();
    }

    #[test]
    fn close_leaves_a_report_stuck_past_its_deadline_behind() {
        let gate = Arc::new(Gate::new());
        let (release, passer) = hold_inside(&gate, Pass::Report);
        // Without the bound, close() would wait for the release below.
        let (returned, closer) = close_on_a_thread(&gate, Instant::now());
        assert_eq!(
            returned.recv_timeout(Duration::from_secs(10)),
            Ok(false),
            "close() returns at its deadline and says a report is inside"
        );
        closer.join().unwrap();
        assert!(!gate.report(|| ()), "the gate is closed");
        release.send(()).unwrap();
        assert!(passer.join().unwrap(), "the late report still ran");
        assert_eq!(*gate.reporting(), 0);
    }

    /// `end(.., limit)` on its own thread for a capture thread that runs
    /// `body`; the receiver gets how long `end` took.
    fn end_on_a_thread(
        gate: &Arc<Gate>,
        limit: Duration,
        body: impl FnOnce() + Send + 'static,
    ) -> (Receiver<Duration>, JoinHandle<()>) {
        let (ending, ended) = sync_channel::<()>(0);
        let thread = std::thread::spawn(move || {
            let _ending = ending;
            body();
        });
        let (returned, took) = sync_channel(1);
        let stopper = std::thread::spawn({
            let gate = Arc::clone(gate);
            move || {
                let (quit, _quit_receiver) = pw::channel::channel();
                let active = Active {
                    quit,
                    gate,
                    sink: Arc::new(LaneFrameSink::new(&[AudioLane::Mixed])),
                    ended,
                    thread,
                    thread_id: Arc::new(AtomicU32::new(0)),
                };
                let started = Instant::now();
                LiveCaptureBackend::end(active, limit);
                returned.send(started.elapsed()).unwrap();
            }
        });
        (took, stopper)
    }

    #[test]
    fn end_closes_the_gate_before_it_waits_for_the_thread() {
        let gate = Arc::new(Gate::new());
        // A thread that ends only once the gate closed.
        let (took, stopper) = end_on_a_thread(&gate, STOP_TIMEOUT, {
            let gate = Arc::clone(&gate);
            move || {
                while gate.open.load(Ordering::SeqCst) {
                    std::thread::yield_now();
                }
            }
        });
        let took = took.recv().expect("end() returns");
        stopper.join().unwrap();
        assert!(!gate.open.load(Ordering::SeqCst));
        assert!(took < STOP_TIMEOUT, "the thread ended and was joined");
    }

    #[test]
    fn end_leaves_a_report_stuck_in_its_handler_and_its_thread_behind() {
        const LIMIT: Duration = Duration::from_millis(300);
        let gate = Arc::new(Gate::new());
        // The PipeWire thread, stuck in a report's handler until released.
        let (release, passer) = hold_inside(&gate, Pass::Report);
        let (done, thread_done) = sync_channel(1);
        let (took, stopper) = end_on_a_thread(&gate, LIMIT, move || {
            done.send(passer.join().unwrap()).unwrap();
        });
        let took = took
            .recv_timeout(Duration::from_secs(10))
            .expect("end() returns at its limit with the report still inside");
        assert!(
            (LIMIT..2 * LIMIT).contains(&took),
            "end() waits for the report until its limit, then returns: took {took:?}"
        );
        stopper.join().unwrap();
        assert!(!gate.report(|| ()), "the gate is closed");
        release.send(()).unwrap();
        assert_eq!(
            thread_done.recv_timeout(Duration::from_secs(10)),
            Ok(true),
            "the report ran late, and the thread left behind then ended"
        );
        assert_eq!(*gate.reporting(), 0);
    }

    #[test]
    fn end_gives_a_report_and_the_thread_one_deadline() {
        const LIMIT: Duration = Duration::from_secs(1);
        let gate = Arc::new(Gate::new());
        // A report that leaves halfway through the limit, and a thread that
        // ends only when released.
        let (release, passer) = hold_inside(&gate, Pass::Report);
        let (unstick, stuck) = sync_channel::<()>(0);
        let (took, stopper) = end_on_a_thread(&gate, LIMIT, move || {
            let _ = stuck.recv();
        });
        std::thread::sleep(LIMIT / 2);
        release.send(()).unwrap();
        assert!(passer.join().unwrap());
        let took = took
            .recv_timeout(Duration::from_secs(10))
            .expect("end() returns at its limit with the thread still running");
        // The join given a limit of its own would take until 1.5 s.
        assert!(
            (LIMIT..LIMIT + LIMIT * 2 / 5).contains(&took),
            "the report's wait and the join share one deadline: took {took:?}"
        );
        stopper.join().unwrap();
        drop(unstick);
    }

    #[test]
    fn a_thread_reads_its_kernel_id_for_the_hang_log() {
        let id = kernel_thread_id();
        if id == 0 {
            // A sandbox without `/proc/thread-self` (gVisor, some
            // containers): the hang log then says the id is unknown.
            assert_eq!(where_it_waits(id), "its kernel id is unknown");
            return;
        }
        assert!(where_it_waits(id).starts_with(&format!("thread {id} in system call ")));
    }

    /// A thread that does not answer in time is given up on: `DidNotRun`
    /// when the daemon had answered, else a backend failure.
    #[test]
    fn a_start_that_gave_up_drops_its_receiver() {
        let (answer, answered) = answer_channel();
        assert!(matches!(
            await_answer(answered, Duration::ZERO, &AtomicBool::new(true)),
            Err(CaptureError::DidNotRun(_))
        ));
        assert!(matches!(
            answer.try_send(Ok(stream())),
            Err(TrySendError::Disconnected(_))
        ));
        let (_answer, answered) = answer_channel();
        assert!(matches!(
            await_answer(answered, Duration::ZERO, &AtomicBool::new(false)),
            Err(CaptureError::BackendFailed(_))
        ));
    }

    /// Counts its drops, standing in for a `Capture`.
    struct Dropped(Arc<AtomicUsize>);

    impl Drop for Dropped {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn a_capture_nobody_took_is_dropped_unwatched() {
        let drops = Arc::new(AtomicUsize::new(0));
        let (answer, answered) = answer_channel();
        drop(answered);
        let watched = hand_over(&answer, Ok(Dropped(Arc::clone(&drops))), |_| stream());
        assert!(watched.is_none(), "nobody took the answer");
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_capture_start_took_is_handed_back_to_watch() {
        let drops = Arc::new(AtomicUsize::new(0));
        let (answer, answered) = answer_channel();
        let start = std::thread::spawn(move || {
            await_answer(answered, Duration::from_secs(10), &AtomicBool::new(true))
        });
        let watched = hand_over(&answer, Ok(Dropped(Arc::clone(&drops))), |_| stream());
        assert_eq!(start.join().unwrap(), Ok(stream()));
        assert!(watched.is_some());
        assert_eq!(drops.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_failed_open_is_answered_and_not_watched() {
        let (answer, answered) = answer_channel();
        let start = std::thread::spawn(move || {
            await_answer(answered, Duration::from_secs(10), &AtomicBool::new(true))
        });
        let opened: Result<Dropped, _> = Err(CaptureError::InputDeviceUnavailable);
        assert!(hand_over(&answer, opened, |_| stream()).is_none());
        assert_eq!(
            start.join().unwrap(),
            Err(CaptureError::InputDeviceUnavailable)
        );
    }

    #[test]
    fn changes_are_judged_after_the_last_and_at_most_the_limit_after_the_first() {
        let first = Instant::now();
        let delay = LiveCaptureBackend::COALESCE_DELAY;
        assert_eq!(judged_at(first, first), first + delay);
        let later = first + Duration::from_millis(300);
        assert_eq!(judged_at(first, later), later + delay);
        let late = first + COALESCE_LIMIT;
        assert_eq!(judged_at(first, late), first + COALESCE_LIMIT);
    }

    #[test]
    fn changes_keep_their_first_and_are_due_at_most_the_limit_after_it() {
        let shared = Shared::default();
        assert_eq!(shared.due(), None);
        let first = Instant::now();
        shared.pending.set(Some((first, first)));
        shared.changed();
        assert_eq!(shared.pending.get().unwrap().0, first, "the first stays");
        shared.pending.set(Some((
            first,
            first + COALESCE_LIMIT + Duration::from_secs(1),
        )));
        assert_eq!(shared.due(), Some(first + COALESCE_LIMIT));
    }

    #[test]
    fn the_latencies_are_counted_on_the_graph_clock_in_their_own_direction() {
        let input = Latency {
            quantum: 0.0,
            rate: 441,
            ns: 0,
        };
        let output = Latency {
            quantum: 1.0,
            rate: 0,
            ns: 0,
        };
        // A 44.1 kHz graph ticks `1/44100`: 441 samples are 10 ms, 480
        // frames at 48 kHz; one quantum is the first cycle.
        let clock = spa::utils::Fraction {
            num: 1,
            denom: 44_100,
        };
        assert_eq!(
            latency_frames((input, output), 1_024, Some(clock)),
            (480, 1_024)
        );
        assert_eq!(latency_frames((input, output), 1_024, None), (441, 1_024));
    }

    #[test]
    fn a_latency_param_parses_in_its_direction_only() {
        let bytes = latency_pod(spa::sys::SPA_DIRECTION_INPUT, 1.0, 256, 1_000);
        let pod = spa::pod::Pod::from_bytes(&bytes).unwrap();
        assert_eq!(
            parse_latency(pod, spa::sys::SPA_DIRECTION_INPUT),
            Some(Latency {
                quantum: 1.0,
                rate: 256,
                ns: 1_000
            })
        );
        assert_eq!(parse_latency(pod, spa::sys::SPA_DIRECTION_OUTPUT), None);
    }

    #[test]
    fn the_format_is_a_pod_with_aux_positions() {
        let bytes = format_pod(3).unwrap();
        let pod = spa::pod::Pod::from_bytes(&bytes).unwrap();
        let mut info = spa::param::audio::AudioInfoRaw::new();
        info.parse(pod).unwrap();
        assert_eq!(info.rate(), 48_000);
        assert_eq!(info.channels(), 3);
        assert_eq!(
            info.position()[..3],
            [
                spa::sys::SPA_AUDIO_CHANNEL_AUX0,
                spa::sys::SPA_AUDIO_CHANNEL_AUX0 + 1,
                spa::sys::SPA_AUDIO_CHANNEL_AUX0 + 2
            ]
        );
    }
}
