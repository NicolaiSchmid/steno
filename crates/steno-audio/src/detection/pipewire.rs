//! The Linux [process-activity source](super::ProcessAudioActivitySource):
//! the PipeWire registry's stream nodes, their links and their clients, kept
//! up to date by one long-lived PipeWire thread. No Swift counterpart (the
//! Swift app is macOS-only); the macOS counterpart reads the HAL's process
//! objects, the Windows one the audio sessions (`super::live`).
//!
//! # What counts
//!
//! A process holds the microphone while one of its `Stream/Input/Audio`
//! nodes is linked from a source (an `Audio/Source` node, a virtual one
//! included, or an `Audio/Duplex` device other than through its monitor
//! ports, which carry what it plays) and the node's state is not idle,
//! suspended or failed. The link, not the state alone, decides because the
//! registry reports links with both ends on the global, and because a
//! capture stream linked from a sink's monitor (a screen recorder taking the
//! desktop's sound) runs as well but holds no microphone. The state comes
//! from each stream node's info, which needs a bind; a node whose info has
//! not arrived yet counts as running. A process has running output while
//! one of its `Stream/Output/Audio` nodes is linked and running.
//!
//! Left out: Steno's own capture stream (its `node.name`, whatever process
//! runs it, so a `steno record` beside the app is no call), the halves of a
//! filter or loopback (a node with a `node.link-group`: an echo canceller's
//! or noise filter's capture stream holds the real microphone for as long
//! as the filter exists), and level meters (`stream.monitor`, which the
//! desktop's sound settings open on every source).
//!
//! # Who
//!
//! The pid is the stream node's `application.process.id` (streams of
//! PulseAudio clients carry it), else its client's, else the client's
//! `pipewire.sec.pid` (the socket's credentials). The "bundle id" is the
//! best stable identifier there is: `application.process.binary` (the
//! node's, else the client's), else `application.name`. A native PipeWire
//! client reports its own binary; a browser reports the browser's binary,
//! so a call in a browser tab names the browser, not the call service.
//!
//! # Threads
//!
//! PipeWire's objects are single-threaded: one `steno-pw-detect` thread
//! per source owns the connection, binds the stream nodes and the clients
//! for their info, and writes what it learns into a shared view under a
//! mutex. It starts with the first
//! [`snapshot`](ProcessAudioActivitySource::snapshot) or `changes()` call
//! and ends when the source drops, while it connects too. Every `changes()`
//! receiver hears from that one thread: a message whenever a node, link
//! or client appears or goes, or a stream node's or client's info changes,
//! and one right away; a dropped receiver is forgotten at the next
//! message. A lost connection ends the thread; the next call after
//! [`RETRY_AFTER`] connects afresh, and the calls before it answer the
//! error. A thread that ended without saying why (a panic) is started again
//! by the next call.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel, sync_channel};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use pipewire as pw;
use pw::types::ObjectType;

use super::activity::{ActivityError, ProcessAudioActivity, ProcessAudioActivitySource};
use crate::capture::live::pipewire::{
    PUMP_SLICE, STREAM_NODE_NAME, connect, failure, is_source_class,
};

/// How long a snapshot waits for a new connection's first view.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(3);
/// How long after a lost or failed connection the next call tries again;
/// the calls before it answer the error at once. Without a PipeWire daemon
/// the detector's 1 s poll would otherwise start a thread every second.
const RETRY_AFTER: Duration = Duration::from_secs(5);
/// How long dropping the source waits for its thread to end.
const STOP_TIMEOUT: Duration = Duration::from_secs(2);
/// The wait on the loop once connected; the quit wakes it earlier.
const IDLE_WAIT: Duration = Duration::from_secs(1);

/// Why a snapshot or the first view gave up on the daemon.
fn no_answer() -> String {
    format!(
        "PipeWire did not answer within {} s",
        ANSWER_TIMEOUT.as_secs()
    )
}

/// Whether a node in `state` runs: running, or still being created (a
/// stream's first state); idle, suspended or failed is not.
fn is_running(state: &pw::node::NodeState<'_>) -> bool {
    matches!(
        state,
        pw::node::NodeState::Running | pw::node::NodeState::Creating
    )
}

/// The identity properties of a stream node or a client; the info's
/// values, once read, replace the global's.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct Who {
    /// `application.process.id`.
    pid: Option<i32>,
    /// `pipewire.sec.pid`, a client's socket credentials.
    sec_pid: Option<i32>,
    /// `application.process.binary`.
    binary: Option<String>,
    /// `application.name`.
    name: Option<String>,
}

impl Who {
    fn read<'a>(props: &impl Fn(&str) -> Option<&'a str>) -> Self {
        let text = |key| props(key).filter(|v| !v.is_empty()).map(str::to_owned);
        let pid = |key| {
            props(key)
                .and_then(|v| v.parse().ok())
                .filter(|&p: &i32| p > 0)
        };
        Who {
            pid: pid("application.process.id"),
            sec_pid: pid("pipewire.sec.pid"),
            binary: text("application.process.binary"),
            name: text("application.name"),
        }
    }

    /// `newer`'s values where it has them, `self`'s elsewhere.
    fn merge(&mut self, newer: Who) {
        self.pid = newer.pid.or(self.pid);
        self.sec_pid = newer.sec_pid.or(self.sec_pid);
        self.binary = newer.binary.or(self.binary.take());
        self.name = newer.name.or(self.name.take());
    }
}

/// One `Stream/Input/Audio` or `Stream/Output/Audio` node.
#[derive(Debug, Clone, PartialEq, Eq)]
struct StreamNode {
    /// A capture stream (`Stream/Input/Audio`).
    input: bool,
    /// `client.id`.
    client: Option<u32>,
    /// Steno's own capture, a filter's half or a level meter.
    left_out: bool,
    who: Who,
    /// From the node's info: running, or idle, suspended or failed;
    /// `None` until the info arrived.
    running: Option<bool>,
}

/// What the registry and the bound objects' info say about streams; pure,
/// so the decisions are unit-tested without a daemon.
#[derive(Debug, Default, Clone)]
pub(crate) struct StreamGraph {
    /// The nodes a microphone is: sources and duplex devices.
    sources: BTreeSet<u32>,
    /// The duplex devices among them, whose monitor ports carry playback.
    duplex: BTreeSet<u32>,
    /// Every monitor port (`port.monitor`).
    monitor_ports: BTreeSet<u32>,
    streams: BTreeMap<u32, StreamNode>,
    /// Each link's output node, output port (when it says) and input node.
    links: BTreeMap<u32, Link>,
    clients: BTreeMap<u32, Who>,
}

/// One link's ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Link {
    output: u32,
    output_port: Option<u32>,
    input: u32,
}

/// Whether a node's properties leave it out of the activity: Steno's own
/// capture, a filter's or loopback's half, a level meter.
fn left_out<'a>(props: &impl Fn(&str) -> Option<&'a str>) -> bool {
    props("node.name") == Some(STREAM_NODE_NAME)
        || props("node.link-group").is_some()
        || props("stream.monitor") == Some("true")
}

impl StreamGraph {
    /// A node global; true when it is a stream node, whose info the caller
    /// then binds for.
    fn add_node<'a>(&mut self, id: u32, props: impl Fn(&str) -> Option<&'a str>) -> bool {
        let class = props("media.class").unwrap_or_default();
        if is_source_class(class) {
            self.sources.insert(id);
            if class == "Audio/Duplex" {
                self.duplex.insert(id);
            }
            return false;
        }
        let input = if class.starts_with("Stream/Input/Audio") {
            true
        } else if class.starts_with("Stream/Output/Audio") {
            false
        } else {
            return false;
        };
        self.streams.insert(
            id,
            StreamNode {
                input,
                client: props("client.id").and_then(|v| v.parse().ok()),
                left_out: left_out(&props),
                who: Who::read(&props),
                running: None,
            },
        );
        true
    }

    /// A stream node's info: its state when `running` is given, its
    /// properties when `props` is.
    fn node_info<'a>(
        &mut self,
        id: u32,
        running: Option<bool>,
        props: Option<impl Fn(&str) -> Option<&'a str>>,
    ) {
        let Some(stream) = self.streams.get_mut(&id) else {
            return;
        };
        if running.is_some() {
            stream.running = running;
        }
        if let Some(props) = props {
            stream.left_out |= left_out(&props);
            stream.who.merge(Who::read(&props));
        }
    }

    /// A client global or a client's info.
    fn client<'a>(&mut self, id: u32, props: impl Fn(&str) -> Option<&'a str>) {
        self.clients.entry(id).or_default().merge(Who::read(&props));
    }

    /// A port global: kept only when it is a monitor port; whether it is.
    fn add_port<'a>(&mut self, id: u32, props: impl Fn(&str) -> Option<&'a str>) -> bool {
        let monitor = props("port.monitor") == Some("true");
        if monitor {
            self.monitor_ports.insert(id);
        }
        monitor
    }

    /// A link global.
    fn add_link<'a>(&mut self, id: u32, props: impl Fn(&str) -> Option<&'a str>) {
        let id_of = |key| props(key).and_then(|v| v.parse().ok());
        if let (Some(output), Some(input)) = (id_of("link.output.node"), id_of("link.input.node")) {
            let output_port = id_of("link.output.port");
            self.links.insert(
                id,
                Link {
                    output,
                    output_port,
                    input,
                },
            );
        }
    }

    /// Whether `link` carries a microphone: from a source, and not from a
    /// duplex device's monitor port.
    fn carries_microphone(&self, link: &Link) -> bool {
        self.sources.contains(&link.output)
            && !(self.duplex.contains(&link.output)
                && link
                    .output_port
                    .is_some_and(|port| self.monitor_ports.contains(&port)))
    }

    /// A global went away; whether the view held it.
    fn remove(&mut self, id: u32) -> bool {
        // Not short-circuiting: an id is in at most one map, and each must
        // forget it.
        self.sources.remove(&id)
            | self.duplex.remove(&id)
            | self.monitor_ports.remove(&id)
            | self.streams.remove(&id).is_some()
            | self.links.remove(&id).is_some()
            | self.clients.remove(&id).is_some()
    }

    /// Every process with a stream node, in pid order; see the module doc.
    fn processes(&self) -> Vec<ProcessAudioActivity> {
        let mut processes: BTreeMap<i32, ProcessAudioActivity> = BTreeMap::new();
        for (&id, stream) in &self.streams {
            if stream.left_out {
                continue;
            }
            let client = stream.client.and_then(|c| self.clients.get(&c));
            let from_client = |field: fn(&Who) -> Option<i32>| client.and_then(field);
            let Some(pid) = stream
                .who
                .pid
                .or_else(|| from_client(|c| c.pid))
                .or_else(|| from_client(|c| c.sec_pid))
            else {
                continue;
            };
            let text = |field: fn(&Who) -> &Option<String>| {
                field(&stream.who)
                    .clone()
                    .or_else(|| client.and_then(|c| field(c).clone()))
            };
            let bundle = text(|w| &w.binary).or_else(|| text(|w| &w.name));
            let running = stream.running != Some(false);
            let holds_microphone = stream.input
                && running
                && self
                    .links
                    .values()
                    .any(|link| link.input == id && self.carries_microphone(link));
            let plays =
                !stream.input && running && self.links.values().any(|link| link.output == id);
            let process = processes
                .entry(pid)
                .or_insert_with(|| ProcessAudioActivity::new(pid, None, false));
            if process.bundle_id.is_none() {
                process.bundle_id = bundle;
            }
            process.is_running_input |= holds_microphone;
            process.is_running_output |= plays;
        }
        processes.into_values().collect()
    }
}

/// Where the thread is.
#[derive(Debug, Clone)]
enum Phase {
    /// Connecting and reading the first view.
    Connecting,
    /// The view is current.
    Ready,
    /// The connection failed or was lost, at `at`.
    Ended { error: String, at: Instant },
}

/// What the thread and the callers share.
struct State {
    graph: StreamGraph,
    phase: Phase,
    subscribers: Vec<Sender<()>>,
}

impl State {
    /// One message to every live `changes()` receiver, forgetting the
    /// dropped ones.
    fn notify(&mut self) {
        self.subscribers.retain(|s| s.send(()).is_ok());
    }

    /// [`Self::notify`] once the view is current.
    fn changed(&mut self) {
        if matches!(self.phase, Phase::Ready) {
            self.notify();
        }
    }
}

struct Shared {
    state: Mutex<State>,
    /// Notified when the phase changes.
    phase_changed: Condvar,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Applies `change` to the view and, once it is current, tells the
    /// receivers; `change`'s answer.
    fn update<R>(&self, change: impl FnOnce(&mut StreamGraph) -> R) -> R {
        let mut state = self.lock();
        let answer = change(&mut state.graph);
        state.changed();
        answer
    }

    /// A global went away: out of the view, and the receivers told if the
    /// view held it.
    fn forget(&self, id: u32) {
        let mut state = self.lock();
        if state.graph.remove(id) {
            state.changed();
        }
    }

    fn set_phase(&self, phase: Phase) {
        let mut state = self.lock();
        state.phase = phase;
        state.notify();
        drop(state);
        self.phase_changed.notify_all();
    }
}

/// A bound object and its info listener, kept for their drop; the
/// listener drops first.
enum Bound {
    Node {
        _listener: pw::node::NodeListener,
        _node: pw::node::Node,
    },
    Client {
        _listener: pw::client::ClientListener,
        _client: pw::client::Client,
    },
}

/// What the PipeWire callbacks share on the thread.
#[derive(Default)]
struct Local {
    /// The bound stream nodes and clients by global id.
    bound: RefCell<BTreeMap<u32, Bound>>,
    /// The last `done` of a core roundtrip.
    done: Cell<Option<pw::spa::utils::result::AsyncSeq>>,
    /// Why the connection failed, once it did.
    failed: RefCell<Option<String>>,
}

/// A registry global: into the view, and a bind for the info of a stream
/// node or a client.
fn announce(
    shared: &Arc<Shared>,
    local: &Local,
    registry: &pw::registry::RegistryRc,
    global: &pw::registry::GlobalObject<&pw::spa::utils::dict::DictRef>,
) {
    let Some(props) = global.props else {
        return;
    };
    let id = global.id;
    let get = |key: &str| props.get(key);
    let bound = match global.type_ {
        ObjectType::Node => {
            if !shared.update(|graph| graph.add_node(id, get)) {
                return;
            }
            let Ok(node) = registry.bind::<pw::node::Node, _>(global) else {
                tracing::debug!("binding PipeWire node {id} for its info failed");
                return;
            };
            let shared = Arc::clone(shared);
            let listener = node
                .add_listener_local()
                .info(move |info| {
                    let mask = info.change_mask();
                    let running = mask
                        .contains(pw::node::NodeChangeMask::STATE)
                        .then(|| is_running(&info.state()));
                    let props = info
                        .props()
                        .filter(|_| mask.contains(pw::node::NodeChangeMask::PROPS));
                    shared.update(|graph| {
                        graph.node_info(id, running, props.map(|p| move |k: &str| p.get(k)));
                    });
                })
                .register();
            Bound::Node {
                _listener: listener,
                _node: node,
            }
        }
        ObjectType::Client => {
            shared.update(|graph| graph.client(id, get));
            let Ok(client) = registry.bind::<pw::client::Client, _>(global) else {
                tracing::debug!("binding PipeWire client {id} for its info failed");
                return;
            };
            let shared = Arc::clone(shared);
            let listener = client
                .add_listener_local()
                .info(move |info| {
                    if let Some(props) = info.props() {
                        shared.update(|graph| graph.client(id, |k| props.get(k)));
                    }
                })
                .register();
            Bound::Client {
                _listener: listener,
                _client: client,
            }
        }
        ObjectType::Link => {
            shared.update(|graph| graph.add_link(id, get));
            return;
        }
        // Only monitor ports matter, so only they are a change.
        ObjectType::Port if get("port.monitor") == Some("true") => {
            shared.update(|graph| graph.add_port(id, get));
            return;
        }
        _ => return,
    };
    local.bound.borrow_mut().insert(id, bound);
}

/// The thread's connection; fields drop in order, the listeners before the
/// proxies, the bound objects before the registry, the core before the
/// context and the loop.
struct Connection {
    _registry_listener: pw::registry::Listener,
    _core_listener: pw::core::Listener,
    local: Rc<Local>,
    _registry: pw::registry::RegistryRc,
    core: pw::core::CoreRc,
    _context: pw::context::ContextRc,
    main_loop: pw::main_loop::MainLoopRc,
}

impl Connection {
    fn open(shared: &Arc<Shared>) -> Result<Self, String> {
        let (main_loop, context, core, registry) = connect()?;
        let local = Rc::new(Local::default());
        let core_listener = core
            .add_listener_local()
            .done({
                let local = Rc::clone(&local);
                move |id, seq| {
                    if id == pw::core::PW_ID_CORE {
                        local.done.set(Some(seq));
                    }
                }
            })
            .error({
                let local = Rc::clone(&local);
                move |id, _seq, res, message| {
                    if id == pw::core::PW_ID_CORE {
                        *local.failed.borrow_mut() = Some(format!(
                            "the connection to PipeWire failed: {message} ({res})"
                        ));
                    }
                }
            })
            .register();
        let registry_listener = registry
            .add_listener_local()
            .global({
                // Weak: the listener lives in the connection beside the
                // registry.
                let (shared, local, registry) =
                    (Arc::clone(shared), Rc::clone(&local), registry.downgrade());
                move |global| {
                    if let Some(registry) = registry.upgrade() {
                        announce(&shared, &local, &registry, global);
                    }
                }
            })
            .global_remove({
                let (shared, local) = (Arc::clone(shared), Rc::clone(&local));
                move |id| {
                    drop(local.bound.borrow_mut().remove(&id));
                    shared.forget(id);
                }
            })
            .register();
        Ok(Self {
            _registry_listener: registry_listener,
            _core_listener: core_listener,
            local,
            _registry: registry,
            core,
            _context: context,
            main_loop,
        })
    }

    /// Waits until the daemon has answered everything sent before, or the
    /// quit arrived.
    fn roundtrip(&self, deadline: Instant, quitting: &Cell<bool>) -> Result<(), String> {
        let pending = self.core.sync(0).map_err(failure("a PipeWire roundtrip"))?;
        while self.local.done.get() != Some(pending) && !quitting.get() {
            if let Some(error) = self.local.failed.borrow().clone() {
                return Err(error);
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(no_answer());
            }
            self.main_loop
                .loop_()
                .iterate(pw::loop_::Timeout::Finite((deadline - now).min(PUMP_SLICE)));
        }
        Ok(())
    }

    /// Runs the loop until the quit arrives (`None`) or the connection
    /// fails (why).
    fn watch(&self, quitting: &Cell<bool>) -> Option<String> {
        let main_loop = self.main_loop.loop_();
        while !quitting.get() {
            if let Some(error) = self.local.failed.borrow().clone() {
                return Some(error);
            }
            if main_loop.iterate(pw::loop_::Timeout::Finite(IDLE_WAIT)) < 0 {
                std::thread::sleep(PUMP_SLICE);
            }
        }
        None
    }
}

/// The `steno-pw-detect` thread: connect, read the first view (the
/// globals, then the bound objects' info), watch until the quit or a
/// failure.
fn run(shared: &Arc<Shared>, quit: pw::channel::Receiver<()>) {
    let ended = Connection::open(shared).and_then(|connection| {
        // Attached first, so a source dropped while this connects ends the
        // thread within a loop pass rather than after the roundtrips.
        let quitting = Rc::new(Cell::new(false));
        let _attached = quit.attach(connection.main_loop.loop_(), {
            let quitting = Rc::clone(&quitting);
            move |()| quitting.set(true)
        });
        let deadline = Instant::now() + ANSWER_TIMEOUT;
        connection.roundtrip(deadline, &quitting)?;
        connection.roundtrip(deadline, &quitting)?;
        if quitting.get() {
            return Ok(());
        }
        shared.set_phase(Phase::Ready);
        connection.watch(&quitting).map_or(Ok(()), Err)
    });
    if let Err(error) = ended {
        tracing::warn!("meeting detection lost PipeWire: {error}");
        shared.set_phase(Phase::Ended {
            error,
            at: Instant::now(),
        });
    }
}

/// The running thread as the source keeps it.
struct Watch {
    quit: pw::channel::Sender<()>,
    /// Disconnected once the thread is done, its teardown included.
    ended: Receiver<()>,
    thread: JoinHandle<()>,
}

impl Watch {
    fn spawn(shared: &Arc<Shared>) -> std::io::Result<Self> {
        let (quit, quit_receiver) = pw::channel::channel();
        let (ending, ended) = sync_channel::<()>(0);
        let shared = Arc::clone(shared);
        let thread = std::thread::Builder::new()
            .name("steno-pw-detect".into())
            .spawn(move || {
                let _ending = ending;
                run(&shared, quit_receiver);
            })?;
        Ok(Self {
            quit,
            ended,
            thread,
        })
    }

    /// Asks the thread to quit and joins it; one that has not ended within
    /// [`STOP_TIMEOUT`] is logged and left behind.
    fn stop(self) {
        let _ = self.quit.send(());
        match self.ended.recv_timeout(STOP_TIMEOUT) {
            Err(RecvTimeoutError::Timeout) => tracing::error!(
                "the PipeWire activity thread did not end within {} s; it is left behind",
                STOP_TIMEOUT.as_secs()
            ),
            Ok(()) | Err(RecvTimeoutError::Disconnected) => {
                if self.thread.join().is_err() {
                    tracing::warn!("the PipeWire activity thread panicked");
                }
            }
        }
    }
}

/// The PipeWire-backed source: a process holds the microphone while one
/// of its capture streams is linked from a source and running, Steno's own
/// capture, filters and level meters left out. One PipeWire thread per
/// source, started by the first call and ended by the drop, serves every
/// `changes()` receiver, because PipeWire's objects are single-threaded.
pub struct LiveProcessAudioActivity {
    shared: Arc<Shared>,
    watch: Mutex<Option<Watch>>,
}

impl Default for LiveProcessAudioActivity {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for LiveProcessAudioActivity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiveProcessAudioActivity")
            .finish_non_exhaustive()
    }
}

impl LiveProcessAudioActivity {
    /// No connection until the first `snapshot()` or `changes()`.
    #[must_use]
    pub fn new() -> Self {
        Self {
            shared: Arc::new(Shared {
                state: Mutex::new(State {
                    graph: StreamGraph::default(),
                    phase: Phase::Connecting,
                    subscribers: Vec::new(),
                }),
                phase_changed: Condvar::new(),
            }),
            watch: Mutex::new(None),
        }
    }

    /// Starts the thread unless it runs, or ended less than
    /// [`RETRY_AFTER`] ago; a thread that ended is joined first, one that
    /// finished without ending its phase (it panicked) included.
    fn ensure_watching(&self) {
        let mut watch = self.watch.lock().unwrap_or_else(PoisonError::into_inner);
        let ended = match &self.shared.lock().phase {
            Phase::Ended { at, .. } => Some(*at),
            Phase::Connecting | Phase::Ready => None,
        };
        match (&*watch, ended) {
            (Some(running), None) if !running.thread.is_finished() => return,
            (_, Some(at)) if at.elapsed() < RETRY_AFTER => return,
            _ => {}
        }
        if let Some(old) = watch.take() {
            old.stop();
        }
        {
            let mut state = self.shared.lock();
            state.graph = StreamGraph::default();
            state.phase = Phase::Connecting;
        }
        match Watch::spawn(&self.shared) {
            Ok(started) => *watch = Some(started),
            Err(error) => self.shared.set_phase(Phase::Ended {
                error: format!("the PipeWire activity thread: {error}"),
                at: Instant::now(),
            }),
        }
    }
}

impl ProcessAudioActivitySource for LiveProcessAudioActivity {
    /// Every process with a stream node, in pid order. Waits up to 3 s for a
    /// new connection's first view; an error while PipeWire is unreachable.
    fn snapshot(&self) -> Result<Vec<ProcessAudioActivity>, ActivityError> {
        self.ensure_watching();
        let state = self.shared.lock();
        let (state, _) = self
            .shared
            .phase_changed
            .wait_timeout_while(state, ANSWER_TIMEOUT, |state| {
                matches!(state.phase, Phase::Connecting)
            })
            .unwrap_or_else(PoisonError::into_inner);
        match &state.phase {
            Phase::Ready => Ok(state.graph.processes()),
            Phase::Ended { error, .. } => Err(ActivityError::Failed(error.clone())),
            Phase::Connecting => Err(ActivityError::Failed(no_answer())),
        }
    }

    /// One message right away, then one per change the thread sees; see
    /// the module doc. All receivers share the source's one thread.
    fn changes(&self) -> Receiver<()> {
        self.ensure_watching();
        let (sender, receiver) = channel();
        let _ = sender.send(());
        self.shared.lock().subscribers.push(sender);
        receiver
    }
}

impl Drop for LiveProcessAudioActivity {
    fn drop(&mut self) {
        let watch = self
            .watch
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(watch) = watch {
            watch.stop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn props<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<&'a str> {
        move |key| pairs.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
    }

    const MIC: u32 = 10;
    const SINK: u32 = 11;

    /// A built-in microphone, a sink, and the clients of a native
    /// recorder (pid 500) and of the sound server's compatibility layer
    /// (pid 300).
    fn desktop() -> StreamGraph {
        let mut graph = StreamGraph::default();
        assert!(!graph.add_node(MIC, props(&[("media.class", "Audio/Source")])));
        assert!(!graph.add_node(SINK, props(&[("media.class", "Audio/Sink")])));
        graph.client(
            20,
            props(&[
                ("application.name", "pw-record"),
                ("pipewire.sec.pid", "500"),
            ]),
        );
        graph.client(
            20,
            props(&[
                ("application.process.id", "500"),
                ("application.process.binary", "pw-cat"),
            ]),
        );
        // The compatibility layer's own client carries its own pid, so a
        // stream's pid must come from the stream first.
        graph.client(
            21,
            props(&[
                ("application.process.id", "300"),
                ("pipewire.sec.pid", "300"),
            ]),
        );
        graph
    }

    fn stream(graph: &mut StreamGraph, id: u32, class: &str, client: &str, extra: &[(&str, &str)]) {
        let mut pairs = vec![("media.class", class), ("client.id", client)];
        pairs.extend_from_slice(extra);
        assert!(graph.add_node(id, props(&pairs)));
    }

    fn link(graph: &mut StreamGraph, id: u32, from: u32, to: u32) {
        let (from, to) = (from.to_string(), to.to_string());
        graph.add_link(
            id,
            props(&[("link.output.node", &from), ("link.input.node", &to)]),
        );
    }

    /// Each process's pid and its input and output flags.
    fn running(graph: &StreamGraph) -> Vec<(i32, bool, bool)> {
        graph
            .processes()
            .iter()
            .map(|p| (p.pid, p.is_running_input, p.is_running_output))
            .collect()
    }

    /// No properties in a node's info.
    const NO_PROPS: Option<fn(&str) -> Option<&'static str>> = None;

    #[test]
    fn a_recorder_linked_from_the_microphone_holds_it_with_its_clients_pid() {
        let mut graph = desktop();
        stream(
            &mut graph,
            30,
            "Stream/Input/Audio",
            "20",
            &[("application.name", "pw-record")],
        );
        assert_eq!(running(&graph), vec![(500, false, false)], "not linked yet");
        link(&mut graph, 40, MIC, 30);
        let processes = graph.processes();
        assert_eq!(processes.len(), 1);
        assert_eq!(processes[0].pid, 500);
        assert_eq!(
            processes[0].bundle_id.as_deref(),
            Some("pw-cat"),
            "the binary first"
        );
        assert!(processes[0].is_running_input);
        graph.node_info(30, Some(false), NO_PROPS);
        assert_eq!(running(&graph), vec![(500, false, false)], "suspended");
        graph.node_info(30, Some(true), NO_PROPS);
        assert_eq!(running(&graph), vec![(500, true, false)]);
        assert!(graph.remove(40));
        assert_eq!(running(&graph), vec![(500, false, false)], "unlinked");
        assert!(graph.remove(30));
        assert!(!graph.remove(30), "gone already");
        assert_eq!(graph.processes(), vec![]);
    }

    #[test]
    fn a_compatibility_stream_names_its_own_process() {
        let mut graph = desktop();
        stream(&mut graph, 31, "Stream/Input/Audio", "21", &[]);
        graph.node_info(
            31,
            Some(true),
            Some(props(&[
                ("application.process.id", "4200"),
                ("application.name", "A Call App"),
            ])),
        );
        link(&mut graph, 41, MIC, 31);
        let processes = graph.processes();
        assert_eq!(
            (processes[0].pid, processes[0].bundle_id.as_deref()),
            (4200, Some("A Call App")),
            "the node's pid over the client's credentials; the name without a binary"
        );
        assert!(processes[0].is_running_input);
    }

    /// A browser call (A8): Firefox's input stream reaches PipeWire
    /// through the compatibility layer, so the node names the browser and
    /// its pid, and the process holds the microphone while it runs linked.
    #[test]
    fn a_browser_s_compatibility_stream_names_the_browser() {
        let mut graph = desktop();
        stream(&mut graph, 31, "Stream/Input/Audio", "21", &[]);
        graph.node_info(
            31,
            Some(true),
            Some(props(&[
                ("application.process.id", "4300"),
                ("application.process.binary", "firefox"),
                ("application.name", "Firefox"),
                ("media.name", "AudioCallbackDriver"),
            ])),
        );
        assert_eq!(
            running(&graph),
            vec![(4300, false, false)],
            "not linked yet"
        );
        link(&mut graph, 41, MIC, 31);
        let processes = graph.processes();
        assert_eq!(
            (processes[0].pid, processes[0].bundle_id.as_deref()),
            (4300, Some("firefox"))
        );
        assert!(processes[0].is_running_input);
        graph.node_info(31, Some(false), NO_PROPS);
        assert_eq!(running(&graph), vec![(4300, false, false)], "suspended");
    }

    #[test]
    fn the_client_credentials_stand_in_for_a_missing_pid() {
        let mut graph = desktop();
        stream(&mut graph, 32, "Stream/Input/Audio", "21", &[]);
        link(&mut graph, 42, MIC, 32);
        let processes = graph.processes();
        assert_eq!(
            (processes[0].pid, processes[0].bundle_id.as_deref()),
            (300, None)
        );
        stream(&mut graph, 33, "Stream/Input/Audio", "99", &[]);
        assert_eq!(graph.processes().len(), 1, "no pid at all: not listed");
    }

    #[test]
    fn recording_a_monitor_or_playing_is_not_holding_the_microphone() {
        let mut graph = desktop();
        stream(&mut graph, 34, "Stream/Input/Audio", "20", &[]);
        link(&mut graph, 44, SINK, 34);
        stream(&mut graph, 35, "Stream/Output/Audio", "20", &[]);
        assert_eq!(running(&graph), vec![(500, false, false)], "not linked yet");
        link(&mut graph, 45, 35, SINK);
        assert_eq!(
            running(&graph),
            vec![(500, false, true)],
            "one process for both streams; a sink's monitor is no microphone"
        );
    }

    #[test]
    fn steno_filters_and_level_meters_are_left_out() {
        let mut graph = desktop();
        stream(
            &mut graph,
            36,
            "Stream/Input/Audio",
            "20",
            &[("node.name", STREAM_NODE_NAME)],
        );
        stream(&mut graph, 37, "Stream/Input/Audio", "21", &[]);
        graph.node_info(
            37,
            None,
            Some(props(&[("node.link-group", "filter-chain-1-2")])),
        );
        stream(&mut graph, 38, "Stream/Input/Audio", "21", &[]);
        graph.node_info(38, None, Some(props(&[("stream.monitor", "true")])));
        for id in [36, 37, 38] {
            link(&mut graph, 100 + id, MIC, id);
        }
        assert_eq!(graph.processes(), vec![]);
    }

    #[test]
    fn a_virtual_or_duplex_source_is_a_microphone_and_others_are_not_streams() {
        let mut graph = desktop();
        assert!(!graph.add_node(12, props(&[("media.class", "Audio/Source/Virtual")])));
        assert!(!graph.add_node(13, props(&[("media.class", "Audio/Duplex")])));
        assert!(!graph.add_node(14, props(&[("media.class", "Video/Source")])));
        assert!(!graph.add_node(15, props(&[("media.class", "Stream/Input/Video")])));
        stream(&mut graph, 39, "Stream/Input/Audio", "20", &[]);
        link(&mut graph, 46, 12, 39);
        assert_eq!(running(&graph), vec![(500, true, false)]);
        graph.remove(46);
        link(&mut graph, 47, 13, 39);
        assert_eq!(running(&graph), vec![(500, true, false)]);
    }

    #[test]
    fn recording_a_duplex_device_s_monitor_is_not_holding_the_microphone() {
        let mut graph = desktop();
        assert!(!graph.add_node(13, props(&[("media.class", "Audio/Duplex")])));
        assert!(!graph.add_port(60, props(&[("port.monitor", "false")])));
        assert!(graph.add_port(61, props(&[("port.monitor", "true")])));
        stream(&mut graph, 39, "Stream/Input/Audio", "20", &[]);
        let link_from = |graph: &mut StreamGraph, id: u32, port: u32| {
            let port = port.to_string();
            graph.add_link(
                id,
                props(&[
                    ("link.output.node", "13"),
                    ("link.output.port", &port),
                    ("link.input.node", "39"),
                ]),
            );
        };
        link_from(&mut graph, 48, 61);
        assert_eq!(running(&graph), vec![(500, false, false)], "its playback");
        link_from(&mut graph, 49, 60);
        assert_eq!(running(&graph), vec![(500, true, false)], "its capture");
        assert!(graph.remove(61), "a monitor port is forgotten");
    }

    #[test]
    fn a_node_runs_while_running_or_being_created() {
        use pw::node::NodeState;
        for (state, runs) in [
            (NodeState::Running, true),
            (NodeState::Creating, true),
            (NodeState::Idle, false),
            (NodeState::Suspended, false),
            (NodeState::Error("failed"), false),
        ] {
            assert_eq!(is_running(&state), runs, "{state:?}");
        }
    }

    #[test]
    fn info_fills_in_what_the_global_lacks_and_keeps_the_rest() {
        let mut who = Who::read(&props(&[
            ("application.name", "Global"),
            ("pipewire.sec.pid", "7"),
        ]));
        who.merge(Who::read(&props(&[
            ("application.process.id", "8"),
            ("application.process.binary", ""),
        ])));
        assert_eq!(
            who,
            Who {
                pid: Some(8),
                sec_pid: Some(7),
                binary: None,
                name: Some("Global".into()),
            },
            "an empty binary is none"
        );
        assert_eq!(
            Who::read(&props(&[("application.process.id", "0")])).pid,
            None
        );
    }
}
