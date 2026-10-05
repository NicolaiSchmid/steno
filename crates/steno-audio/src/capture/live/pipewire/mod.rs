//! The live backend on Linux: one PipeWire capture stream that Steno links
//! itself to the microphone and to the default sink's monitor, so every
//! graph cycle delivers all lanes in one interleaved buffer, aligned on the
//! graph clock as the macOS aggregate aligns them on its clock master.
//! WP5b of `.plans/2026-10-02-rust-core-and-tauri-shell.md`. No Swift
//! counterpart (the Swift app is macOS-only); the macOS backend is
//! `capture::live::backend` (Swift:
//! `Sources/StenoAudio/Capture/LiveCaptureBackend.swift`).
//!
//! # Threads
//!
//! PipeWire's objects are single-threaded, so every capture gets its own
//! `steno-pipewire` thread owning the main loop, the context, the core,
//! the registry, the stream and the links. `start` spawns it and waits for
//! its answer (the [`CaptureStream`] or the error). `stop()` closes the
//! capture's [`Gate`] to the sink, sends a quit through a
//! `pipewire::channel` and joins the thread; a thread that has not ended
//! within [`STOP_TIMEOUT`] is logged and left behind the closed gate, so
//! `stop()` returns, and nothing reaches the sink after it either way. The
//! stream runs with `RT_PROCESS`, so its `process` callback runs on
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
//! that does not run within [`START_TIMEOUT`] is an error rather than a
//! silent recording, and so is a link that failed. The latencies come
//! from the `SPA_PARAM_Latency` of the microphone port (capture side) and
//! of the sink's first playback port (playback side), in frames of the
//! first cycle's length.
//!
//! # Device changes
//!
//! The `default.audio.sink` and `default.audio.source` metadata changing
//! or going away, a node or port going away, or the connection, the
//! stream or a link failing mark a change;
//! [`LiveCaptureBackend::COALESCE_DELAY`] after the last one the graph is
//! compared with the devices the targets resolved to
//! ([`DeviceSnapshot::difference`]), and a difference goes to the sink as
//! a [`DeviceChangeReason`](crate::capture::DeviceChangeReason), from this
//! thread, never during `start`. A change during `start` is judged once
//! the capture runs. The session then rebuilds through `stop()` and
//! `start`, as on the Mac. The capture never follows a default on its own.
//! A lost connection, stream or link reads as the output gone (the input
//! for an in-person capture), and the sample rate never changes: the
//! adapter resamples.
//!
//! The system lane is the whole default sink, Steno's own output included
//! (the Mac's tap excludes Steno's process; Steno plays nothing during a
//! recording). Linking the sink's monitor keeps the sink running, so,
//! unlike the Mac's call mode, cycles arrive with nothing playing.

mod graph;

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
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
use crate::capture::{CaptureBackend, CaptureError, CaptureStream, DeviceSnapshot, LaneSource};
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

/// How long `stop()` waits for the PipeWire thread to tear down, a few
/// milliseconds normally.
const STOP_TIMEOUT: Duration = Duration::from_secs(2);

/// The capture stream's `node.name`.
const STREAM_NODE_NAME: &str = "steno-capture";

/// The capture's way into the sink, closed for good by `stop()` before it
/// waits for the thread: once [`Gate::close`] returned, neither a cycle's
/// [`deliver_slices`] nor a device-change report reaches the sink,
/// whatever the thread does next. Lock-free on the real-time side: an
/// increment, a load and a decrement, all `SeqCst`, so either a pass sees
/// the gate closed or `close` sees the pass inside and waits for it to
/// leave.
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

    /// Runs `work` if the gate is open, counted inside while it does.
    #[inline(always)]
    fn pass(&self, work: impl FnOnce()) {
        self.inside.fetch_add(1, Ordering::SeqCst);
        if self.open.load(Ordering::SeqCst) {
            work();
        }
        self.inside.fetch_sub(1, Ordering::SeqCst);
    }

    /// Closes the gate and waits for whoever is inside: a cycle's
    /// `deliver_slices` (microseconds), or a report and its handler.
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
/// view and `deliver_slices` index only through checked lookups,
/// `channels` is at least 1).
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
    /// The last change since the graph was last judged.
    pending: Cell<Option<Instant>>,
}

impl Shared {
    fn changed(&self) {
        self.pending.set(Some(Instant::now()));
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
                            && shared.graph.borrow_mut().set_default(key, value)
                        {
                            shared.changed();
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
/// listen on, the proxies before the core, the core before the context and
/// the loop.
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
/// the stream destroyed, the links destroyed, then the connection.
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
    /// The devices the targets resolved to, taken at `resolve`.
    baseline: DeviceSnapshot,
}

/// One of Steno's links and the listeners that mark the capture lost when
/// it fails: an error on its proxy, or the server putting it in its error
/// state. Fields drop in order, the listeners first.
struct WatchedLink {
    _info: pw::link::LinkListener,
    _error: pw::proxy::ProxyListener,
    _link: pw::link::Link,
}

impl Drop for Capture {
    fn drop(&mut self) {
        // `pw_stream_disconnect` returns 0 whatever happens; if it ever
        // failed, the rt listener would still be freed right after this
        // while `process` might run.
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
        let baseline = connection
            .shared
            .graph
            .borrow()
            .snapshot(&targets, input_device_uid, false);
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

    /// `link` with the listeners that fail the capture when it fails.
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
        let graph_rate = self.stream.time().map_or(0, |time| time.rate().denom);
        // Its own deadline: a first cycle late in the start's must not cut
        // the read short and leave the far-end delay at zero.
        let (input, output) = self.latencies(Instant::now() + LATENCY_TIMEOUT);
        if connection.shared.lost.get() {
            return Err(connection.stalled("run the capture"));
        }
        self.info.input_latency_frames = input.frames(cycle, graph_rate);
        self.info.output_latency_frames = output.frames(cycle, graph_rate);
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
            let wait = match shared.pending.get() {
                None => IDLE_WAIT,
                Some(last) => {
                    let due = last + LiveCaptureBackend::COALESCE_DELAY;
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
            Some(reason) => self.gate.pass(|| {
                tracing::info!("PipeWire graph change reported {reason:?}");
                self.sink.report_device_change(reason);
            }),
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
    answer: &SyncSender<Result<CaptureStream, CaptureError>>,
    quit: pw::channel::Receiver<()>,
) {
    match Capture::open(lanes, input_device_uid, sink, gate) {
        Err(error) => {
            let _ = answer.send(Err(error));
        }
        Ok(capture) => {
            // A `start` that gave up has sent the quit already; it is
            // queued and ends `watch` at once.
            let _ = answer.send(Ok(capture.info.clone()));
            capture.watch(quit);
            tracing::debug!("the PipeWire capture got its quit; tearing down");
            drop(capture);
            tracing::debug!("the PipeWire capture is torn down");
        }
    }
}

/// One started capture as `start` keeps it.
struct Active {
    quit: pw::channel::Sender<()>,
    gate: Arc<Gate>,
    /// Disconnected once the thread is done, its teardown included.
    ended: Receiver<()>,
    thread: JoinHandle<()>,
}

/// The Linux capture backend; see the module doc.
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
    /// and moves both defaults within it.
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
    /// logged, behind the closed gate.
    fn end(active: Active) {
        active.gate.close();
        let _ = active.quit.send(());
        match active.ended.recv_timeout(STOP_TIMEOUT) {
            Err(RecvTimeoutError::Timeout) => tracing::error!(
                "the PipeWire capture thread did not end within {} s; it is left behind, \
                 cut off from the recording",
                STOP_TIMEOUT.as_secs()
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
        let (answer, answered) = sync_channel(1);
        let (quit, quit_receiver) = pw::channel::channel();
        let (ending, ended) = sync_channel::<()>(0);
        let gate = Arc::new(Gate::new());
        let lanes = lanes.to_vec();
        let input_device_uid = input_device_uid.map(str::to_owned);
        let thread_gate = Arc::clone(&gate);
        let thread = std::thread::Builder::new()
            .name("steno-pipewire".into())
            .spawn(move || {
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
        let outcome = answered
            .recv_timeout(START_TIMEOUT + LATENCY_TIMEOUT + Duration::from_secs(2))
            .unwrap_or_else(|_| {
                Err(CaptureError::BackendFailed(
                    "the PipeWire thread did not answer".into(),
                ))
            });
        let started = Active {
            quit,
            gate,
            ended,
            thread,
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

    #[test]
    fn nothing_passes_a_gate_once_it_closed() {
        let gate = Arc::new(Gate::new());
        let passed = Arc::new(AtomicUsize::new(0));
        let done = Arc::new(AtomicBool::new(false));
        let worker = std::thread::spawn({
            let (gate, passed, done) = (Arc::clone(&gate), Arc::clone(&passed), Arc::clone(&done));
            move || {
                while !done.load(Ordering::Relaxed) {
                    gate.pass(|| {
                        passed.fetch_add(1, Ordering::Relaxed);
                    });
                }
            }
        });
        while passed.load(Ordering::Relaxed) < 1_000 {
            std::thread::yield_now();
        }
        gate.close();
        let at_close = passed.load(Ordering::Relaxed);
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(passed.load(Ordering::Relaxed), at_close);
        done.store(true, Ordering::Relaxed);
        worker.join().unwrap();
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
