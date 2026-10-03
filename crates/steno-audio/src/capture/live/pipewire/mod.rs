//! The live backend on Linux: one PipeWire capture stream that Steno links
//! itself to the microphone and to the default sink's monitor, so every
//! graph cycle delivers all lanes in one interleaved buffer, aligned on the
//! graph clock as the macOS aggregate aligns them on its clock master.
//! WP5b of `.plans/2026-10-02-rust-core-and-tauri-shell.md`; the macOS
//! counterpart is `capture::live::backend`.
//!
//! # Threads
//!
//! PipeWire's objects are single-threaded, so every capture gets its own
//! `steno-pipewire` thread owning the main loop, the context, the core,
//! the registry, the stream and the links. `start` spawns it and waits for
//! its answer (the [`CaptureStream`] or the error); `stop()` sends a quit
//! through a `pipewire::channel` and joins it. The stream runs with
//! `RT_PROCESS`, so its `process` callback runs on PipeWire's data-loop
//! thread, which is the real-time path here: `process` dequeues the
//! buffer, turns its chunk into a [`BufferView`](crate::realtime::BufferView)
//! with [`interleaved_view`] and hands it to [`deliver`], the IOProc body
//! the macOS backend uses. No allocation, no lock, no log (proven for the
//! body in `tests/realtime.rs` and on the real data-loop thread in
//! `tests/pipewire.rs`).
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
//! silent recording. The latencies come from the `SPA_PARAM_Latency` of
//! the microphone port (capture side) and of the sink's first playback
//! port (playback side), in frames of the first cycle's length.
//!
//! # Device changes
//!
//! The `default.audio.sink` and `default.audio.source` metadata changing,
//! a linked node going away, or the connection or the stream failing mark
//! a change; [`LiveCaptureBackend::COALESCE_DELAY`] after the last one the
//! graph is compared with what the capture started on
//! ([`DeviceSnapshot::difference`]), and a difference goes to the sink as
//! a [`DeviceChangeReason`](crate::capture::DeviceChangeReason), from this
//! thread, never during `start`. The session then rebuilds through
//! `stop()` and `start`, as on the Mac. The capture never follows a
//! default on its own.
//!
//! The system lane is the whole default sink, Steno's own output included
//! (the Mac's tap excludes Steno's process; Steno plays nothing during a
//! recording). Linking the sink's monitor keeps the sink running, so,
//! unlike the Mac's call mode, cycles arrive with nothing playing.

mod graph;

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{SyncSender, sync_channel};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use pipewire as pw;
use pw::spa;
use pw::types::ObjectType;
use steno_core::AudioLane;

use self::graph::{Graph, Latency, Targets};
use crate::SAMPLE_RATE;
use crate::capture::{CaptureBackend, CaptureError, CaptureStream, DeviceSnapshot, LaneSource};
use crate::realtime::{LaneFrameSink, deliver, interleaved_view};

/// How long `start` lets PipeWire answer, link and run the first cycle.
const START_TIMEOUT: Duration = Duration::from_secs(3);
/// The longest single wait on the loop while starting, so the deadline is
/// checked often.
const PUMP_SLICE: Duration = Duration::from_millis(20);
/// The wait on the loop while nothing is pending.
const IDLE_WAIT: Duration = Duration::from_secs(1);

/// The capture stream's `node.name`.
const STREAM_NODE_NAME: &str = "steno-capture";

/// What the data-loop thread owns: moved into the stream listener at
/// registration, read only by [`process`]. It crosses to PipeWire's
/// data-loop thread, which `pipewire` does not check, so it must be `Send`
/// (asserted below).
struct RealTime {
    sink: Arc<LaneFrameSink>,
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
/// buffer through [`deliver`], then back to the stream. Nothing allocates,
/// locks or logs; nothing can panic (the view and `deliver` index only
/// through checked lookups, `channels` is at least 1).
fn process(stream: &pw::stream::Stream, rt: &mut RealTime) {
    let Some(mut buffer) = stream.dequeue_buffer() else {
        return;
    };
    let Some(data) = buffer.datas_mut().first_mut() else {
        return;
    };
    let chunk = data.chunk();
    let (offset, size, stride) = (chunk.offset(), chunk.size(), chunk.stride());
    let view = interleaved_view(data.data().as_deref(), offset, size, stride, rt.channels);
    // SAFETY: the view points into the buffer's memory, mapped by
    // `MAP_BUFFERS` and valid for `byte_size` bytes while the buffer stays
    // dequeued, which it does until `buffer` drops at the end of this call.
    unsafe { deliver(&[view], &rt.sources, &rt.sink) };
    rt.cycle_frames
        .store(view.byte_size / (rt.channels * 4), Ordering::Relaxed);
}

/// A `pipewire` error as a backend failure naming what failed.
fn failed(what: &'static str) -> impl FnOnce(pw::Error) -> CaptureError {
    move |error| CaptureError::BackendFailed(format!("{what}: {error}"))
}

/// What the PipeWire callbacks share on the PipeWire thread.
#[derive(Default)]
struct Shared {
    graph: RefCell<Graph>,
    /// The `default` metadata once bound: its listener, then the proxy, so
    /// the listener drops first.
    metadata: RefCell<Option<(pw::metadata::MetadataListener, pw::metadata::Metadata)>>,
    /// The last `done` of a core roundtrip.
    done: Cell<Option<spa::utils::result::AsyncSeq>>,
    /// The connection or the stream failed.
    lost: Cell<bool>,
    /// The last change since the graph was last judged.
    pending: Cell<Option<Instant>>,
}

impl Shared {
    fn changed(&self) {
        self.pending.set(Some(Instant::now()));
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
                *self.metadata.borrow_mut() = Some((listener, metadata));
            }
            _ => {}
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
                    tracing::warn!("PipeWire error on object {id}: {message} ({res})");
                    if id == pw::core::PW_ID_CORE {
                        shared.lost.set(true);
                        shared.changed();
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
                move |id| {
                    if shared.graph.borrow_mut().remove(id) {
                        shared.changed();
                    }
                }
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
            CaptureError::BackendFailed("the connection to PipeWire failed".into())
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

/// One started capture on its PipeWire thread. `Drop` tears it down in
/// order: the stream disconnected (PipeWire removes its node from the data
/// loop before that returns, so no `process` runs after it), the listeners
/// removed while the stream still exists, the stream destroyed, the links
/// destroyed, then the connection.
struct Capture {
    stream: Option<pw::stream::StreamRc>,
    rt_listener: Option<pw::stream::StreamListener<RealTime>>,
    state_listener: Option<pw::stream::StreamListener<()>>,
    links: Vec<pw::link::Link>,
    connection: Connection,
    targets: Targets,
    input_device_uid: Option<String>,
    sink: Arc<LaneFrameSink>,
    /// Frames in the stream's last cycle, written by [`process`].
    cycle_frames: Arc<AtomicUsize>,
    /// What `start` answers.
    info: CaptureStream,
    /// The devices the capture started on.
    baseline: DeviceSnapshot,
}

impl Drop for Capture {
    fn drop(&mut self) {
        if let Some(stream) = &self.stream
            && let Err(error) = stream.disconnect()
        {
            tracing::warn!("disconnecting the PipeWire capture stream failed: {error}");
        }
        drop(self.rt_listener.take());
        drop(self.state_listener.take());
        drop(self.stream.take());
        self.links.clear();
    }
}

impl Capture {
    /// Everything `start` does, on the PipeWire thread; see the module doc.
    /// Once `new` returned, an early return tears down through `Drop`.
    fn open(
        lanes: &[AudioLane],
        input_device_uid: Option<&str>,
        sink: Arc<LaneFrameSink>,
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
        let mut capture = Self::new(connection, targets, input_device_uid, sink)?;
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
    ) -> Result<Self, CaptureError> {
        let mut props = pw::properties::PropertiesBox::new();
        props.insert(*pw::keys::MEDIA_TYPE, "Audio");
        props.insert(*pw::keys::MEDIA_CATEGORY, "Capture");
        props.insert(*pw::keys::NODE_NAME, STREAM_NODE_NAME);
        props.insert(*pw::keys::NODE_DESCRIPTION, "Steno recording");
        props.insert(*pw::keys::APP_NAME, "Steno");
        // Steno links the stream itself; the session manager must not.
        props.insert("node.autoconnect", "false");
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
                        tracing::warn!("the PipeWire capture stream failed: {message}");
                        shared.lost.set(true);
                        shared.changed();
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
            stream: Some(stream),
            rt_listener: Some(rt_listener),
            state_listener: Some(state_listener),
            links: Vec::new(),
            connection,
            info: CaptureStream {
                layout: Some(targets.layout.clone()),
                ..CaptureStream::SYNTHETIC
            },
            targets,
            input_device_uid: input_device_uid.map(str::to_owned),
            sink,
            cycle_frames,
            baseline,
        })
    }

    /// The stream as `new` made it, until `Drop` takes it.
    fn stream(&self) -> &pw::stream::StreamRc {
        self.stream.as_ref().expect("taken only by Drop")
    }

    /// Connects the stream, waits for its ports, and links one port of the
    /// targets to each.
    fn link(&mut self, deadline: Instant) -> Result<(), CaptureError> {
        let channels = self.targets.channels();
        let format = format_pod(channels)?;
        let pod = spa::pod::Pod::from_bytes(&format)
            .ok_or_else(|| CaptureError::BackendFailed("the capture format is not a pod".into()))?;
        let stream = self.stream();
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
            props.insert("link.output.node", node.to_string());
            props.insert("link.output.port", port.to_string());
            props.insert("link.input.node", stream_node.to_string());
            props.insert("link.input.port", input.to_string());
            props.insert("object.linger", "false");
            links.push(
                connection
                    .core
                    .create_object::<pw::link::Link>("link-factory", &props)
                    .map_err(failed("linking the capture stream"))?,
            );
        }
        self.links = links;
        Ok(())
    }

    /// Waits for the first cycle, then reads the latencies in its length
    /// and takes the baseline the changes are judged against.
    fn measure(&mut self, deadline: Instant) -> Result<(), CaptureError> {
        let connection = &self.connection;
        if !connection.pump_until(deadline, || self.cycle_frames.load(Ordering::Relaxed) > 0) {
            return Err(connection.stalled("run the capture"));
        }
        let cycle = self.cycle_frames.load(Ordering::Relaxed);
        let graph_rate = self.stream().time().map_or(0, |time| time.rate().denom);
        let (input, output) = self.latencies(deadline);
        self.info.input_latency_frames = input.frames(cycle, graph_rate);
        self.info.output_latency_frames = output.frames(cycle, graph_rate);
        self.baseline = self.connection.shared.graph.borrow().snapshot(
            &self.targets,
            self.input_device_uid.as_deref(),
            false,
        );
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
        let shared = &self.connection.shared;
        // Whatever moved while starting is in the baseline.
        shared.pending.set(None);
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
    /// difference to the sink.
    fn judge(&self) {
        let shared = &self.connection.shared;
        let snapshot = shared.graph.borrow().snapshot(
            &self.targets,
            self.input_device_uid.as_deref(),
            shared.lost.get(),
        );
        match snapshot.difference(&self.baseline) {
            None => tracing::info!("ignored a PipeWire graph change"),
            Some(reason) => {
                tracing::info!("PipeWire graph change reported {reason:?}");
                self.sink.report_device_change(reason);
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
    answer: &SyncSender<Result<CaptureStream, CaptureError>>,
    quit: pw::channel::Receiver<()>,
) {
    match Capture::open(lanes, input_device_uid, sink) {
        Err(error) => {
            let _ = answer.send(Err(error));
        }
        Ok(capture) => {
            // A `start` that gave up has sent the quit already; it is
            // queued and ends `watch` at once.
            let _ = answer.send(Ok(capture.info.clone()));
            capture.watch(quit);
        }
    }
}

/// One started capture as `start` keeps it.
struct Active {
    quit: pw::channel::Sender<()>,
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

    /// Asks the thread to quit and joins it: the stream, the links and the
    /// connection are gone when this returns.
    fn end(active: Active) {
        let _ = active.quit.send(());
        if active.thread.join().is_err() {
            tracing::warn!("the PipeWire capture thread panicked");
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
        let lanes = lanes.to_vec();
        let input_device_uid = input_device_uid.map(str::to_owned);
        let thread = std::thread::Builder::new()
            .name("steno-pipewire".into())
            .spawn(move || {
                run(
                    &lanes,
                    input_device_uid.as_deref(),
                    sink,
                    &answer,
                    quit_receiver,
                );
            })
            .map_err(|e| CaptureError::BackendFailed(format!("the PipeWire thread: {e}")))?;
        // The thread answers by its own deadline; the margin covers a
        // thread that is slow to get scheduled.
        let outcome = answered
            .recv_timeout(START_TIMEOUT + Duration::from_secs(2))
            .unwrap_or_else(|_| {
                Err(CaptureError::BackendFailed(
                    "the PipeWire thread did not answer".into(),
                ))
            });
        let started = Active { quit, thread };
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
