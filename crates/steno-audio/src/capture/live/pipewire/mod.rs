//! The live backend on Linux: one PipeWire capture stream that Steno links
//! itself to the microphone and to the default sink's monitor, so every
//! graph cycle delivers all lanes in one interleaved buffer, aligned on the
//! graph clock as the macOS aggregate aligns them on its clock master.
//! WP5b of `.plans/2026-10-02-rust-core-and-tauri-shell.md`. No Swift
//! counterpart (the Swift app is macOS-only); the macOS counterpart is
//! `capture::live::backend`.
//!
//! # Threads
//!
//! PipeWire's objects are single-threaded, so every capture gets its own
//! `steno-pipewire` thread owning the main loop, the context, the core,
//! the registry, the stream and the links. `start` spawns it and waits for
//! its answer (the [`CaptureStream`] or the error) through a rendezvous:
//! a thread that answers after `start` gave up finds nobody to take it
//! and tears down without watching, so no device-change report begins
//! before `start` took the answer. `stop()` closes the capture's `Gate` to
//! the sink, sends a quit through a `pipewire::channel` and joins the
//! thread; a thread that has not ended within `STOP_TIMEOUT` (2 s) is
//! left behind the closed gate, so `stop()` returns (within the rule on
//! `Gate::close`), with a log of where it waits, and nothing reaches the
//! sink after it either way. Logs go through the subscriber the binary
//! installed, synchronously unless it buffers them: a log write that
//! blocks (stderr on a stalled disk) can hold the thread past
//! `STOP_TIMEOUT`, and then `stop()` in its own log of that.
//!
//! The stream runs with `RT_PROCESS`, so its `process` callback runs on
//! PipeWire's data-loop thread, which is the real-time path here:
//! `process` dequeues the buffer, turns its chunk into a
//! [`SliceView`](crate::realtime::SliceView) with [`interleaved_view`]
//! and, while the gate is open, hands it to [`deliver_slices`], the safe
//! form of the IOProc body the macOS backend uses. No allocation, no lock,
//! no log (proven for the body in `tests/realtime.rs` and on the real
//! data-loop thread in `tests/pipewire.rs`).
//!
//! # Start
//!
//! Two roundtrips bring the registry's nodes and ports and the `default`
//! metadata (`graph` keeps them). The targets resolve from it: the input
//! node (by UID, its `node.name`, or the default source) and the default
//! sink. The stream asks for 48 kHz `f32` with one `AUXn` channel per
//! linked port; PipeWire's adapter resamples whatever the graph runs at,
//! so the rate is always [`SAMPLE_RATE`] and never a mismatch. The stream
//! is connected without `AUTOCONNECT`, so the session manager leaves it
//! alone, and once its ports exist Steno creates one link per channel
//! through the server's `link-factory` (not lingering: the links die with
//! the connection). `start` returns once the first cycle arrived; a graph
//! that does not run within `START_TIMEOUT` (3 s) is an error rather than a
//! silent recording, and so is a link that failed. The latencies come
//! from the `SPA_PARAM_Latency` of the microphone port (capture side) and
//! of the sink's first playback port (playback side), in frames of the
//! first cycle's length.
//!
//! # Device changes
//!
//! The `default.audio.sink` and `default.audio.source` metadata changing
//! or going away, a node the capture's snapshot reads going away (a
//! linked device, the default sink, the source the microphone follows) or
//! a port of one, or the connection, the stream or a link failing (a link
//! removed from outside included) mark a change; another app's stream
//! ending does not. [`LiveCaptureBackend::COALESCE_DELAY`] after the last
//! change, and at most `COALESCE_LIMIT` (2 s) after the first, the graph is
//! compared with the devices
//! the targets resolved to ([`DeviceSnapshot::difference`]), and a
//! difference goes to the sink as a [`DeviceChangeReason`], from this
//! thread, never during `start`. A change during `start` is judged once
//! the capture runs. The session then rebuilds through `stop()` and
//! `start`, as on the Mac. The capture never follows a default on its own.
//! A lost connection, stream or link reads as the output gone (the input
//! for an in-person capture), and so does a microphone that vanishes
//! during a call: the server removes Steno's link to it, and a lost link
//! takes both lanes. The sample rate never changes: the adapter
//! resamples.
//!
//! The system lane is the whole default sink, Steno's own output included
//! (the Mac's tap excludes Steno's process; Steno plays nothing during a
//! recording). Linking the sink's monitor keeps the sink running, so,
//! unlike the Mac's call mode, cycles arrive with nothing playing.

mod graph;

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, sync_channel};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use pipewire as pw;
use pw::proxy::ProxyT;
use pw::spa;
use pw::types::ObjectType;
use steno_core::AudioLane;

use self::graph::{Graph, Latency, Targets};
use crate::SAMPLE_RATE;
use crate::capture::{
    CaptureBackend, CaptureError, CaptureStream, DeviceChangeReason, DeviceSnapshot, LaneSource,
};
use crate::realtime::{LaneFrameSink, deliver_slices, interleaved_view};

/// How long `start` lets PipeWire answer, link and run the first cycle.
const START_TIMEOUT: Duration = Duration::from_secs(3);
/// How long `start` waits for the device latencies after the first cycle,
/// on top of [`START_TIMEOUT`].
const LATENCY_TIMEOUT: Duration = Duration::from_millis(500);
/// The longest single wait on the loop while starting, so the deadline is
/// checked often.
const PUMP_SLICE: Duration = Duration::from_millis(20);
/// The wait on the loop while nothing is pending.
const IDLE_WAIT: Duration = Duration::from_secs(1);

/// The longest a stream of changes defers its judgement: changes that keep
/// coming less than [`LiveCaptureBackend::COALESCE_DELAY`] apart are
/// judged this long after the first of them, and the changes after that
/// judgement start a new stream.
const COALESCE_LIMIT: Duration = Duration::from_secs(2);

/// How long `stop()` waits for the PipeWire thread to tear down after the
/// gate closed, a few milliseconds normally; past it the thread is left
/// behind.
const STOP_TIMEOUT: Duration = Duration::from_secs(2);

/// The capture stream's `node.name`.
const STREAM_NODE_NAME: &str = "steno-capture";

/// The capture's way into the sink, closed for good by `stop()`, or by a
/// `start` that failed, before it waits for the thread: once
/// [`Gate::close`] returned, neither a cycle's [`deliver_slices`] nor a
/// device-change report reaches the sink, whatever the thread does next.
/// Lock-free on the real-time side: an increment, a load and a decrement,
/// all `SeqCst`, so either a pass sees the gate closed or `close` sees the
/// pass inside and waits for it to leave.
#[derive(Debug)]
struct Gate {
    open: AtomicBool,
    inside: AtomicUsize,
}

impl Gate {
    fn new() -> Self {
        Self {
            open: AtomicBool::new(true),
            inside: AtomicUsize::new(0),
        }
    }

    /// Runs `work` if the gate is open, counted inside while it does;
    /// whether it ran.
    #[inline(always)]
    fn pass(&self, work: impl FnOnce()) -> bool {
        self.inside.fetch_add(1, Ordering::SeqCst);
        let open = self.open.load(Ordering::SeqCst);
        if open {
            work();
        }
        self.inside.fetch_sub(1, Ordering::SeqCst);
        open
    }

    /// Closes the gate and waits, without a limit, for whoever is inside: a
    /// cycle's `deliver_slices` (microseconds), or a report and its
    /// handler.
    ///
    /// So a handler must not wait on the caller of `close`. The session's
    /// handler takes the session mutex. The session never holds it across
    /// `backend.stop()`; it does hold it across `backend.start()`, whose
    /// failure path closes the gate, but no report can begin before
    /// `start` took the thread's stream ([`hand_over`]), and a failed
    /// `start` took none. A handler that stops or starts this backend, or
    /// drops the last owner of it, would wait on itself here.
    fn close(&self) {
        self.open.store(false, Ordering::SeqCst);
        while self.inside.load(Ordering::SeqCst) != 0 {
            std::thread::yield_now();
        }
    }
}

/// What the data-loop thread owns: moved into the stream listener at
/// registration, read only by [`process`]. It crosses to PipeWire's
/// data-loop thread, which `pipewire` does not check, so it must be `Send`
/// (asserted below).
struct RealTime {
    sink: Arc<LaneFrameSink>,
    gate: Arc<Gate>,
    sources: Vec<LaneSource>,
    channels: usize,
    /// Frames in the last cycle; `start` waits for the first.
    cycle_frames: Arc<AtomicUsize>,
}

const _: () = {
    const fn sendable<T: Send>() {}
    sendable::<RealTime>();
};

/// The stream's `process` callback, on PipeWire's data-loop thread: one
/// buffer through [`deliver_slices`] while the gate is open, then back to
/// the stream. Nothing allocates, locks or logs; nothing can panic (the
/// view and `deliver_slices` index only within bounds they checked or
/// computed, `channels` is at least 1).
fn process(stream: &pw::stream::Stream, rt: &mut RealTime) {
    let Some(mut buffer) = stream.dequeue_buffer() else {
        return;
    };
    let Some(data) = buffer.datas_mut().first_mut() else {
        return;
    };
    let chunk = data.chunk();
    let (offset, size, stride) = (chunk.offset(), chunk.size(), chunk.stride());
    // The buffer's memory, mapped by `MAP_BUFFERS` and valid while the
    // buffer stays dequeued, until `buffer` drops at the end of this call;
    // the view borrows it.
    let memory = data.data();
    let view = interleaved_view(memory.as_deref(), offset, size, stride, rt.channels);
    rt.gate
        .pass(|| deliver_slices(&[view], &rt.sources, &rt.sink));
    // Release: `start` returns on seeing it, and the frames
    // `deliver_slices` wrote are then in the rings.
    rt.cycle_frames.store(view.frames, Ordering::Release);
}

/// The capture and playback latencies in frames at [`SAMPLE_RATE`], for a
/// first cycle of `cycle` frames on a graph whose clock ticks `clock` (the
/// stream's `1/48000` at 48 kHz; none or a zero denominator reads as 48
/// kHz).
fn latency_frames(
    (input, output): (Latency, Latency),
    cycle: usize,
    clock: Option<spa::utils::Fraction>,
) -> (usize, usize) {
    let rate = clock.map_or(0, |clock| clock.denom);
    (input.frames(cycle, rate), output.frames(cycle, rate))
}

/// A `pipewire` error as a backend failure naming what failed.
fn failed(what: &'static str) -> impl FnOnce(pw::Error) -> CaptureError {
    move |error| CaptureError::BackendFailed(format!("{what}: {error}"))
}

/// What the PipeWire callbacks share on the PipeWire thread.
#[derive(Default)]
struct Shared {
    graph: RefCell<Graph>,
    /// The `default` metadata once bound: its global id, its listener,
    /// then the proxy, so the listener drops first.
    metadata: RefCell<Option<(u32, pw::metadata::MetadataListener, pw::metadata::Metadata)>>,
    /// The last `done` of a core roundtrip.
    done: Cell<Option<spa::utils::result::AsyncSeq>>,
    /// The connection, the stream or one of Steno's links failed.
    lost: Cell<bool>,
    /// The first and the last change since the graph was last judged.
    pending: Cell<Option<(Instant, Instant)>>,
}

impl Shared {
    fn changed(&self) {
        let now = Instant::now();
        let first = self.pending.get().map_or(now, |(first, _)| first);
        self.pending.set(Some((first, now)));
    }

    /// When the pending changes are judged ([`judged_at`]); `None` while
    /// nothing is pending.
    fn due(&self) -> Option<Instant> {
        self.pending
            .get()
            .map(|(first, last)| judged_at(first, last))
    }

    /// `what` failed for good (logged with PipeWire's `message`): the
    /// capture is lost.
    fn fail(&self, what: &str, message: &str) {
        tracing::warn!("{what} failed: {message}");
        self.lost.set(true);
        self.changed();
    }

    /// A registry global: nodes and ports into the graph, the `default`
    /// metadata bound once with a listener for its properties.
    fn announce(
        self: &Rc<Self>,
        registry: &pw::registry::RegistryWeak,
        global: &pw::registry::GlobalObject<&spa::utils::dict::DictRef>,
    ) {
        let Some(props) = global.props else {
            return;
        };
        match global.type_ {
            ObjectType::Node => self
                .graph
                .borrow_mut()
                .add_node(global.id, |k| props.get(k)),
            ObjectType::Port => self
                .graph
                .borrow_mut()
                .add_port(global.id, |k| props.get(k)),
            ObjectType::Metadata
                if props.get("metadata.name") == Some("default")
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

    /// A registry global went away: a node or a port out of the graph, or
    /// the bound `default` metadata, whose defaults go with it until it is
    /// announced again (WirePlumber restarting).
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
/// Fields drop in declaration order: the listeners before the proxies they
/// listen on, `shared` (and the metadata proxy in it) after the listeners
/// whose closures hold it, the proxies before the core, the core before
/// the context and the loop.
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
        // A teardown step marker: in a `Capture` this runs after the
        // stream and the links are destroyed, before the connection's own
        // fields go.
        tracing::debug!("closing the PipeWire connection");
    }
}

impl Connection {
    fn open() -> Result<Self, CaptureError> {
        pw::init();
        let main_loop =
            pw::main_loop::MainLoopRc::new(None).map_err(failed("the PipeWire main loop"))?;
        let context = pw::context::ContextRc::new(&main_loop, None)
            .map_err(failed("the PipeWire context"))?;
        let core = context
            .connect_rc(None)
            .map_err(failed("connecting to PipeWire (is it running?)"))?;
        let registry = core
            .get_registry_rc()
            .map_err(failed("the PipeWire registry"))?;
        let shared = Rc::new(Shared::default());
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
                        shared.fail("the connection to PipeWire", message);
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

    /// Runs the loop until `ready` holds, the deadline passes or the
    /// connection is lost; whether `ready` held.
    fn pump_until(&self, deadline: Instant, mut ready: impl FnMut() -> bool) -> bool {
        loop {
            if ready() {
                return true;
            }
            let now = Instant::now();
            if now >= deadline || self.shared.lost.get() {
                return false;
            }
            self.main_loop
                .loop_()
                .iterate(pw::loop_::Timeout::Finite((deadline - now).min(PUMP_SLICE)));
        }
    }

    /// Waits until the daemon has answered everything sent before.
    fn roundtrip(&self, deadline: Instant) -> Result<(), CaptureError> {
        let pending = self.core.sync(0).map_err(failed("a PipeWire roundtrip"))?;
        if self.pump_until(deadline, || self.shared.done.get() == Some(pending)) {
            Ok(())
        } else {
            Err(self.stalled("answer"))
        }
    }

    /// The error for a start step that did not finish: the connection's
    /// loss, or the deadline.
    fn stalled(&self, step: &str) -> CaptureError {
        if self.shared.lost.get() {
            CaptureError::BackendFailed(
                "the connection to PipeWire, the capture stream or a link failed".into(),
            )
        } else {
            CaptureError::BackendFailed(format!(
                "PipeWire did not {step} within {} s",
                START_TIMEOUT.as_secs()
            ))
        }
    }
}

/// The capture stream's format: 48 kHz native-endian `f32`, `channels`
/// interleaved channels at positions `AUX0`, `AUX1`, …, so the adapter
/// neither remixes nor reorders.
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

/// The lower bounds of a `SPA_PARAM_Latency` object for `direction`
/// (`SPA_DIRECTION_INPUT` or `_OUTPUT`); `None` for the other direction or
/// a pod that is not one.
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
/// stream (PipeWire removes its node from the data loop before that
/// returns, so no `process` runs after it); the fields then drop in
/// declaration order: the listeners removed while the stream still exists,
/// the stream destroyed, the links destroyed, then the connection. The
/// order is what keeps it sound: a stream destroyed before its listeners,
/// or links destroyed after the connection, would be a use after free.
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
    /// Frames in the stream's last cycle, written by [`process`].
    cycle_frames: Arc<AtomicUsize>,
    /// What `start` answers.
    info: CaptureStream,
    /// The devices the targets resolved to, taken in `new`, before the loop
    /// runs again after `resolve`.
    baseline: DeviceSnapshot,
}

/// One of Steno's links and the listeners that mark the capture lost when
/// it fails: an error on its proxy, the server putting it in its error
/// state, or the server removing it. Fields drop in order, the listeners
/// first.
struct WatchedLink {
    _info: pw::link::LinkListener,
    _error: pw::proxy::ProxyListener,
    _link: pw::link::Link,
}

impl Drop for Capture {
    fn drop(&mut self) {
        // No `process` runs once this returned, so the rt listener's user
        // data can go next. `pw_stream_disconnect` returns 0 whatever
        // happens; the log is for a version that does not.
        if let Err(error) = self.stream.disconnect() {
            tracing::warn!("disconnecting the PipeWire capture stream failed: {error}");
        }
        tracing::debug!("the PipeWire capture stream is disconnected");
    }
}

impl Capture {
    /// Everything `start` does, on the PipeWire thread; see the module doc.
    /// Once `new` returned, an early return tears down through `Drop`.
    fn open(
        lanes: &[AudioLane],
        input_device_uid: Option<&str>,
        sink: Arc<LaneFrameSink>,
        gate: Arc<Gate>,
    ) -> Result<Self, CaptureError> {
        let deadline = Instant::now() + START_TIMEOUT;
        let connection = Connection::open()?;
        // The first roundtrip brings the globals and binds the `default`
        // metadata, the second its properties.
        connection.roundtrip(deadline)?;
        connection.roundtrip(deadline)?;
        let targets = connection
            .shared
            .graph
            .borrow()
            .resolve(lanes, input_device_uid)?;
        let mut capture = Self::new(connection, targets, input_device_uid, sink, gate)?;
        capture.link(deadline)?;
        capture.measure(deadline)?;
        Ok(capture)
    }

    /// The capture stream for `targets` with its two listeners, not yet
    /// connected.
    fn new(
        connection: Connection,
        targets: Targets,
        input_device_uid: Option<&str>,
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
        // Two listeners: `process` runs on the data-loop thread with its
        // own user data; `state_changed` runs on this thread. One listener
        // would hand both the same `&mut` user data from two threads.
        let rt_listener = stream
            .add_local_listener_with_user_data(RealTime {
                sink: Arc::clone(&sink),
                gate: Arc::clone(&gate),
                sources: targets.layout.sources.clone(),
                channels: targets.channels(),
                cycle_frames: Arc::clone(&cycle_frames),
            })
            .process(process)
            .register()
            .map_err(failed("the capture stream's process callback"))?;
        let state_listener = stream
            .add_local_listener_with_user_data(())
            .state_changed({
                let shared = Rc::clone(&connection.shared);
                move |_, (), _old, new| {
                    if let pw::stream::StreamState::Error(message) = new {
                        shared.fail("the PipeWire capture stream", &message);
                    }
                }
            })
            .register()
            .map_err(failed("the capture stream's state callback"))?;
        let baseline = {
            let mut graph = connection.shared.graph.borrow_mut();
            graph.track(&targets, input_device_uid);
            graph.snapshot(&targets, input_device_uid, false)
        };
        Ok(Capture {
            _rt_listener: rt_listener,
            _state_listener: state_listener,
            stream,
            links: Vec::new(),
            connection,
            // `measure` fills in the latencies.
            info: CaptureStream {
                sample_rate: SAMPLE_RATE,
                input_latency_frames: 0,
                output_latency_frames: 0,
                layout: Some(targets.layout.clone()),
            },
            targets,
            input_device_uid: input_device_uid.map(str::to_owned),
            sink,
            gate,
            cycle_frames,
            baseline,
        })
    }

    /// Connects the stream, waits for its ports, and links one port of the
    /// targets to each.
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
            return Err(connection.stalled("create the capture stream's ports"));
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
            links.push(Self::watch_link(&connection.shared, link));
        }
        self.links = links;
        Ok(())
    }

    /// `link` with the listeners that fail the capture when it fails or the
    /// server removes it (a patchbay, a device end gone). Steno's own
    /// teardown never fires them: they drop before the link, and the loop
    /// does not run in between.
    fn watch_link(shared: &Rc<Shared>, link: pw::link::Link) -> WatchedLink {
        let info = link
            .add_listener_local()
            .info({
                let shared = Rc::downgrade(shared);
                move |info| {
                    if let pw::link::LinkState::Error(message) = info.state()
                        && let Some(shared) = shared.upgrade()
                    {
                        shared.fail("a capture link", message);
                    }
                }
            })
            .register();
        let error = link
            .upcast_ref()
            .add_listener_local()
            .error({
                let shared = Rc::downgrade(shared);
                move |_seq, _res, message| {
                    if let Some(shared) = shared.upgrade() {
                        shared.fail("a capture link", message);
                    }
                }
            })
            .removed({
                let shared = Rc::downgrade(shared);
                move || {
                    if let Some(shared) = shared.upgrade() {
                        shared.fail("a capture link", "the server removed it");
                    }
                }
            })
            .register();
        WatchedLink {
            _info: info,
            _error: error,
            _link: link,
        }
    }

    /// Waits for the first cycle, then reads the latencies in its length.
    /// A link that failed meanwhile fails the start, even when the other
    /// links run.
    fn measure(&mut self, deadline: Instant) -> Result<(), CaptureError> {
        let connection = &self.connection;
        if !connection.pump_until(deadline, || self.cycle_frames.load(Ordering::Acquire) > 0) {
            return Err(connection.stalled("run the capture"));
        }
        let cycle = self.cycle_frames.load(Ordering::Relaxed);
        let clock = self.stream.time().ok().map(|time| time.rate());
        // Its own deadline: a first cycle late in the start's must not cut
        // the read short and leave the far-end delay at zero.
        let latencies = self.latencies(Instant::now() + LATENCY_TIMEOUT);
        if connection.shared.lost.get() {
            return Err(connection.stalled("run the capture"));
        }
        (
            self.info.input_latency_frames,
            self.info.output_latency_frames,
        ) = latency_frames(latencies, cycle, clock);
        Ok(())
    }

    /// The microphone port's capture latency and the sink's playback
    /// latency, from their `SPA_PARAM_Latency`; zero where a port has none
    /// or the daemon does not answer (logged, not fatal: the Speex tail
    /// absorbs what is missing).
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
        // Listener before proxy in each pair, so the listener drops first.
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
    /// until the quit arrives.
    fn watch(&self, quit: pw::channel::Receiver<()>) {
        let quitting = Rc::new(Cell::new(false));
        let main_loop = self.connection.main_loop.loop_();
        let _attached = quit.attach(main_loop, {
            let quitting = Rc::clone(&quitting);
            move |()| quitting.set(true)
        });
        // A change while starting stays pending and is judged against the
        // resolve-time baseline like any other.
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
                // A failing loop (the daemon gone) returns at once; do not
                // spin while the session reacts to the report.
                std::thread::sleep(PUMP_SLICE);
            }
        }
    }

    /// Compares the graph with the baseline and reports the first
    /// difference to the sink while the gate is open.
    fn judge(&self) {
        let shared = &self.connection.shared;
        let snapshot = shared.graph.borrow().snapshot(
            &self.targets,
            self.input_device_uid.as_deref(),
            shared.lost.get(),
        );
        match snapshot.difference(&self.baseline) {
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

/// Reports `reason` to `sink` through `gate`: not at all once it closed.
/// The log comes after the pass, so a log write that blocks never keeps
/// [`Gate::close`] waiting.
fn report(gate: &Gate, sink: &LaneFrameSink, reason: DeviceChangeReason) {
    if gate.pass(|| sink.report_device_change(reason)) {
        tracing::info!("PipeWire graph change reported {reason:?}");
    }
}

/// What `start` waits for: the stream the thread opened, or why it did not.
type Answer = Result<CaptureStream, CaptureError>;

/// The channel the thread answers `start` through: a rendezvous, so a send
/// completes only into a `start` still waiting in [`await_answer`]. With a
/// buffer, an answer sent right after `start` gave up would land in it,
/// and the thread would go on to watch and report while `start` closes
/// the gate, which [`Gate::close`] forbids.
fn answer_channel() -> (SyncSender<Answer>, Receiver<Answer>) {
    sync_channel(0)
}

/// `start`'s wait for the thread's answer, at most `limit`. The receiver
/// drops on return, so a thread that answers later fails its send and
/// tears down without watching ([`hand_over`]).
fn await_answer(answered: Receiver<Answer>, limit: Duration) -> Answer {
    let answer = answered.recv_timeout(limit);
    drop(answered);
    answer.unwrap_or_else(|_| {
        Err(CaptureError::BackendFailed(
            "the PipeWire thread did not answer".into(),
        ))
    })
}

/// Answers `start` with what opening gave and returns the capture to
/// watch, once `start` took its stream. `None` when opening failed, or
/// when `start` gave up waiting: the capture then drops here, unwatched,
/// so no report can begin for a `start` that did not take the answer.
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
                tracing::warn!("start gave up on the PipeWire capture; tearing it down");
                None
            }
        }
    }
}

/// The `steno-pipewire` thread: start, answer, watch until the quit, tear
/// down (by dropping the capture) before the thread ends.
fn run(
    lanes: &[AudioLane],
    input_device_uid: Option<&str>,
    sink: Arc<LaneFrameSink>,
    gate: Arc<Gate>,
    answer: &SyncSender<Answer>,
    quit: pw::channel::Receiver<()>,
) {
    let opened = Capture::open(lanes, input_device_uid, sink, gate);
    let Some(capture) = hand_over(answer, opened, |capture| capture.info.clone()) else {
        return;
    };
    capture.watch(quit);
    tracing::debug!("the PipeWire capture got its quit; tearing down");
    drop(capture);
    tracing::debug!("the PipeWire capture is torn down");
}

/// One started capture as `start` keeps it.
struct Active {
    quit: pw::channel::Sender<()>,
    gate: Arc<Gate>,
    /// Disconnected once the thread is done, its teardown included.
    ended: Receiver<()>,
    thread: JoinHandle<()>,
    /// The thread's kernel id, 0 until it runs or when `/proc` does not give
    /// it; the log of a hung teardown reads `/proc/self/task/<id>` with it.
    thread_id: Arc<AtomicU32>,
}

/// The calling thread's kernel id, from `/proc/thread-self`; 0 when that
/// is unreadable.
fn kernel_thread_id() -> u32 {
    std::fs::read_link("/proc/thread-self")
        .ok()
        .and_then(|path| path.file_name()?.to_str()?.parse().ok())
        .unwrap_or(0)
}

/// Where thread `id` of this process waits, for the log of a teardown
/// that hung: its system call and kernel wait channel from `/proc`.
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

/// The PipeWire backend; see the module doc. Restartable: `stop()` closes
/// the gate and joins the capture thread (one that hangs past 2 s is
/// logged and left behind), and `start` connects afresh.
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
    /// How long a burst of graph changes settles before it is judged once,
    /// as on the Mac. A Bluetooth profile switch removes and adds nodes
    /// and moves both defaults within it. Unlike the Mac, a burst that
    /// never settles is judged at most 2 s after its first change.
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

    /// Closes the gate, asks the thread to quit and joins it: the stream,
    /// the links and the connection are gone when this returns, unless
    /// the thread hangs past [`STOP_TIMEOUT`]; it is then left behind,
    /// logged with where it waits, behind the closed gate.
    fn end(active: Active) {
        active.gate.close();
        let _ = active.quit.send(());
        match active.ended.recv_timeout(STOP_TIMEOUT) {
            Err(RecvTimeoutError::Timeout) => tracing::error!(
                "the PipeWire capture thread did not end within {} s ({}); it is left \
                 behind, cut off from the recording, and the devices may stay open \
                 until Steno quits",
                STOP_TIMEOUT.as_secs(),
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
    /// Returns the stream it opened once the first cycle arrived: always
    /// 48 kHz (the adapter resamples), both device latencies, and the
    /// layout of the one interleaved buffer.
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
        let thread_id = Arc::new(AtomicU32::new(0));
        let id_slot = Arc::clone(&thread_id);
        let thread = std::thread::Builder::new()
            .name("steno-pipewire".into())
            .spawn(move || {
                id_slot.store(kernel_thread_id(), Ordering::Relaxed);
                // Dropped last, when the teardown is done.
                let _ending = ending;
                run(
                    &lanes,
                    input_device_uid.as_deref(),
                    sink,
                    thread_gate,
                    &answer,
                    quit_receiver,
                );
            })
            .map_err(|e| CaptureError::BackendFailed(format!("the PipeWire thread: {e}")))?;
        // The thread answers by its own deadlines; the margin covers a
        // thread that is slow to get scheduled.
        let outcome = await_answer(
            answered,
            START_TIMEOUT + LATENCY_TIMEOUT + Duration::from_secs(2),
        );
        let started = Active {
            quit,
            gate,
            ended,
            thread,
            thread_id,
        };
        match outcome {
            Ok(_) => *active = Some(started),
            Err(_) => Self::end(started),
        }
        outcome
    }

    fn stop(&self) {
        let Some(active) = self.lock().take() else {
            return;
        };
        Self::end(active);
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

    /// A stream as `start` answers it, for the answer channel's tests.
    fn stream() -> CaptureStream {
        CaptureStream {
            sample_rate: SAMPLE_RATE,
            input_latency_frames: 0,
            output_latency_frames: 0,
            layout: None,
        }
    }

    #[test]
    fn nothing_passes_a_gate_once_it_closed() {
        let gate = Gate::new();
        let mut passed = 0;
        assert!(gate.pass(|| passed += 1));
        gate.close();
        assert!(!gate.pass(|| passed += 1), "the pass says it did not run");
        assert_eq!(passed, 1);
        assert_eq!(gate.inside.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn close_waits_for_the_pass_inside() {
        let gate = Arc::new(Gate::new());
        let (entered, inside) = sync_channel(0);
        let (release, released) = sync_channel::<()>(0);
        let passer = std::thread::spawn({
            let gate = Arc::clone(&gate);
            move || {
                gate.pass(|| {
                    entered.send(()).unwrap();
                    released.recv().unwrap();
                });
            }
        });
        inside.recv().unwrap();
        let (returned, closing) = sync_channel(1);
        let stopper = std::thread::spawn({
            let gate = Arc::clone(&gate);
            move || {
                gate.close();
                returned.send(()).unwrap();
            }
        });
        // The closer has closed the gate; it must still be waiting.
        while gate.open.load(Ordering::SeqCst) {
            std::thread::yield_now();
        }
        assert_eq!(
            closing.recv_timeout(Duration::from_millis(200)),
            Err(RecvTimeoutError::Timeout),
            "close() returned while a pass was inside"
        );
        release.send(()).unwrap();
        closing
            .recv_timeout(Duration::from_secs(10))
            .expect("close() returns once the pass left");
        passer.join().unwrap();
        stopper.join().unwrap();
    }

    #[test]
    fn a_report_through_a_closed_gate_reaches_no_handler() {
        let reports = Arc::new(AtomicUsize::new(0));
        let sink = LaneFrameSink::with_handler(
            &[AudioLane::Mic],
            SAMPLE_RATE,
            0.1,
            Box::new({
                let reports = Arc::clone(&reports);
                move |_| {
                    reports.fetch_add(1, Ordering::SeqCst);
                }
            }),
        );
        let gate = Gate::new();
        gate.close();
        report(&gate, &sink, DeviceChangeReason::OutputDeviceGone);
        assert_eq!(reports.load(Ordering::SeqCst), 0);
        let open = Gate::new();
        report(&open, &sink, DeviceChangeReason::OutputDeviceGone);
        assert_eq!(reports.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn end_closes_the_gate_before_it_waits_for_the_thread() {
        let gate = Arc::new(Gate::new());
        let (quit, _quit_receiver) = pw::channel::channel();
        let (ending, ended) = sync_channel::<()>(0);
        // A thread that ends only once the gate closed.
        let thread = std::thread::spawn({
            let gate = Arc::clone(&gate);
            move || {
                let _ending = ending;
                while gate.open.load(Ordering::SeqCst) {
                    std::thread::yield_now();
                }
            }
        });
        let started = Instant::now();
        LiveCaptureBackend::end(Active {
            quit,
            gate: Arc::clone(&gate),
            ended,
            thread,
            thread_id: Arc::new(AtomicU32::new(0)),
        });
        assert!(!gate.open.load(Ordering::SeqCst));
        assert!(
            started.elapsed() < STOP_TIMEOUT,
            "the thread ended and was joined"
        );
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

    #[test]
    fn the_answer_channel_holds_no_answer_nobody_waits_for() {
        let (answer, _answered) = answer_channel();
        assert!(
            matches!(answer.try_send(Ok(stream())), Err(TrySendError::Full(_))),
            "a buffered answer would outlive a start that gave up"
        );
    }

    #[test]
    fn a_start_that_gave_up_drops_its_receiver() {
        let (answer, answered) = answer_channel();
        assert!(await_answer(answered, Duration::ZERO).is_err());
        assert!(matches!(
            answer.try_send(Ok(stream())),
            Err(TrySendError::Disconnected(_))
        ));
    }

    #[test]
    fn a_waiting_start_takes_the_answer() {
        let (answer, answered) = answer_channel();
        let thread = std::thread::spawn(move || answer.send(Ok(stream())));
        assert_eq!(
            await_answer(answered, Duration::from_secs(10)),
            Ok(stream())
        );
        assert!(thread.join().unwrap().is_ok());
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
        let start = std::thread::spawn(move || await_answer(answered, Duration::from_secs(10)));
        let watched = hand_over(&answer, Ok(Dropped(Arc::clone(&drops))), |_| stream());
        assert_eq!(start.join().unwrap(), Ok(stream()));
        assert!(watched.is_some());
        assert_eq!(drops.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_failed_open_is_answered_and_not_watched() {
        let (answer, answered) = answer_channel();
        let start = std::thread::spawn(move || await_answer(answered, Duration::from_secs(10)));
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
