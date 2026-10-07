//! The live capture backend on Windows: WASAPI in shared mode, one capture
//! thread per stream, endpoint notifications and the rebuild report. WP10a
//! of `.plans/2026-10-02-rust-core-and-tauri-shell.md`; the macOS
//! counterpart is `capture::live::backend`. No Swift counterpart.
//!
//! **Not run on hardware.** No Windows machine with audio devices has run
//! it: this backend is written against Microsoft's documentation and its
//! samples, compiled, linted and tested on the `windows-latest` CI runner.
//! The runner has no audio endpoint, so no microphone stream ever opens
//! there; process loopback does run and delivers silence, and
//! `tests/live_windows.rs` checks that system-lane capture starts,
//! delivers, stops and restarts. Its `--ignored` tests are the checks a
//! Windows machine must run before this ships (the plan's parity list).
//!
//! # Streams
//!
//! - **System lane:** process loopback (`ActivateAudioInterfaceAsync` on
//!   `VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK`,
//!   `AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK`) excluding Steno's own
//!   process tree (Microsoft's API page names build 20438, its sample
//!   build 20348; reported to work from Windows 10 2004, unverified).
//!   Where that activation fails for any reason, its timeout included,
//!   loopback of the default render endpoint, which records Steno's own
//!   output too; the switch is only logged.
//! - **Microphone:** the selected capture endpoint by id, or the default
//!   (`eCapture`, `eConsole`), shared mode, event-driven. A selected
//!   endpoint that is not active records the default instead, and the
//!   watcher reports the selected one coming back, so the rebuild returns
//!   to it (the macOS backend does the same; Swift, macOS-only, fails the
//!   start there).
//!
//! Both are asked for 48 kHz 32-bit float (`AUTOCONVERTPCM`, so the engine
//! resamples from the device's mix format): mono for the microphone,
//! stereo for the system audio, folded to mono. The streams run on two
//! clocks and arrive on two threads; [`SplitStreamPlan`] picks the master
//! (the sink's only producer) and [`StreamBody`] is what each thread runs
//! per packet, with the all-or-nothing reservation across lanes and the
//! drop accounting of the IOProc path (see `realtime::streams`).
//!
//! When the system stream is the only one (a `[System]` lane override),
//! it is the master, and the timeline advances only as it delivers. Endpoint
//! loopback delivers no packet while nothing plays, so such a recording is
//! shorter than the time it ran, its silences left out; nothing pads them.
//!
//! # Device changes
//!
//! An `IMMNotificationClient` (default-device changes, devices added,
//! removed or changing state) wakes a watcher thread, which coalesces a
//! burst for [`LiveCaptureBackend::COALESCE_DELAY`] as the macOS backend
//! does, resolves the devices again and compares them with what the
//! capture started on ([`DeviceSnapshot::difference`]). A capture thread
//! whose stream fails (`AUDCLNT_E_DEVICE_INVALIDATED` after a format change
//! or an unplug) stops and tells the watcher, which reports the lost
//! device when nothing else differs. The sink gets one
//! [`DeviceChangeReason`] and the session rebuilds by calling `stop()` and
//! `start` again.
//!
//! # Threads and COM
//!
//! Every thread that touches COM holds its own multithreaded apartment and
//! creates, uses and releases its interfaces itself (`com`'s invariants):
//! the capture threads open their streams, the watcher registers the
//! notifications. `start` only exchanges plain values with them over
//! channels, so it works from any thread, a UI thread in a single-threaded
//! apartment included.
//!
//! The per-packet path is `com::CaptureClient::drain` calling
//! [`StreamBody::handle`]: no allocation, no lock. Between packets the
//! thread waits on the stream's event with a timeout (and drains on the
//! timeout too, so a polled loopback stream works), checking an atomic
//! stop flag. `start` starts the follower before the master, the sink's
//! only producer, so a start that fails has written nothing. Data the
//! engine itself lost (a packet flagged as a discontinuity, the first one
//! aside) is counted and logged at stop, not added to the sink's drop
//! count: WASAPI does not say how much was lost. The follower's underrun,
//! slip and trim counts are logged at stop too. All of these logs are at
//! `info`, below the shell's default filter: set
//! `RUST_LOG=steno_audio=info` to see them.

pub(crate) mod com;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, sync_channel};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use steno_core::AudioLane;

use self::com::{Apartment, CaptureClient, Enumerator, LoopbackKind, ProAudioThread};
use super::{AudioDeviceInfo, chosen_or_default};
use crate::SAMPLE_RATE;
use crate::capture::split_streams::{StreamSizes, far_end_latencies};
use crate::capture::{
    CaptureBackend, CaptureError, CaptureInput, CaptureStream, DeviceChangeReason, DeviceSnapshot,
    SplitStreamPlan, StreamSource,
};
use crate::detection::EndpointFlow;
use crate::realtime::{FollowerLane, LaneFrameSink, PacketRouter, StreamBody};

/// How long `start` waits, in total, for the stream threads to open and
/// start their streams (process loopback activation included) and for the
/// watcher to register, before it gives up on a stream; a watcher that
/// misses it is logged, not a failure. One deadline for every wait, so a
/// hanging COM call holds the session's lock this long at most.
const START_TIMEOUT: Duration = Duration::from_secs(10);
/// How long the asynchronous process-loopback activation may take.
const ACTIVATION_TIMEOUT: Duration = Duration::from_secs(5);
/// The longest a capture thread waits for the engine's event; also how
/// late `stop()` may notice the stop flag.
const WAIT: Duration = Duration::from_millis(50);

/// What one stream thread found when it opened its stream.
#[derive(Debug, Clone)]
struct StreamInfo {
    sizes: StreamSizes,
    /// The endpoint the stream runs on: the microphone's, or for the
    /// system stream the default render endpoint at start.
    endpoint_id: Option<String>,
    loopback: Option<LoopbackKind>,
    /// The microphone stream's endpoint, as [`CaptureStream::input`] names
    /// it; `None` for the system stream.
    input: Option<CaptureInput>,
}

/// A stream thread's reports to `start`.
enum StreamEvent {
    Opened(Result<StreamInfo, CaptureError>),
    Started(Result<(), CaptureError>),
}

/// One stream thread as `start` holds it.
struct Launched {
    source: StreamSource,
    thread: Option<JoinHandle<()>>,
    events: Receiver<StreamEvent>,
    /// Dropped to make a thread that waits for its body exit.
    body: Option<SyncSender<StreamBody>>,
    info: Option<StreamInfo>,
    /// The thread answered `start`'s last request, so it is not stuck in
    /// a COM call and may be joined.
    answered: bool,
}

/// What the notification callbacks and the capture threads share with the
/// watcher thread.
#[derive(Default)]
struct WatchState {
    /// The last notification's arrival, `None` once judged.
    pending: Option<Instant>,
    /// A stream that stopped on an error, not yet judged.
    failed: Option<StreamSource>,
    stop: bool,
    /// The watcher thread is past its start-up COM calls and in its loop,
    /// so `stop()` may join it.
    ready: bool,
}

#[derive(Default)]
struct Watcher {
    state: Mutex<WatchState>,
    condvar: Condvar,
}

impl Watcher {
    fn lock(&self) -> MutexGuard<'_, WatchState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// From a notification callback: something may have changed. Microsoft
    /// asks `IMMNotificationClient` callbacks never to wait on a
    /// synchronization object; this lock is held only for a store and the
    /// watcher's own short sections, never across a WASAPI call, so the
    /// wait is bounded and cannot deadlock with the audio service.
    fn note(&self) {
        self.lock().pending = Some(Instant::now());
        self.condvar.notify_all();
    }

    /// From a capture thread leaving on an error (not the per-packet path).
    fn note_failure(&self, source: StreamSource) {
        let mut state = self.lock();
        state.failed.get_or_insert(source);
        state.pending = Some(Instant::now());
        drop(state);
        self.condvar.notify_all();
    }

    /// Stops the watcher thread and joins it if it reached its loop. One
    /// still in its start-up COM calls is left to finish them: it then
    /// finds `stop` under the same lock and exits without reporting, so
    /// nothing is reported once this returns.
    fn stop_and_join(&self, thread: JoinHandle<()>) {
        let mut state = self.lock();
        state.stop = true;
        let ready = state.ready;
        drop(state);
        self.condvar.notify_all();
        if ready {
            let _ = thread.join();
        }
    }

    /// The watcher thread's loop once registered: marks it ready, answers
    /// `start` through `ready`, then hands every settled burst to `judge`
    /// with the stream that failed meanwhile, if any, outside the lock,
    /// until stopped.
    fn watch(&self, ready: &SyncSender<()>, mut judge: impl FnMut(Option<StreamSource>)) {
        let mut state = self.lock();
        state.ready = true;
        let _ = ready.send(());
        loop {
            if state.stop {
                break;
            }
            let Some(last) = state.pending else {
                state = self
                    .condvar
                    .wait(state)
                    .unwrap_or_else(PoisonError::into_inner);
                continue;
            };
            let due = last + LiveCaptureBackend::COALESCE_DELAY;
            let now = Instant::now();
            if now < due {
                state = self
                    .condvar
                    .wait_timeout(state, due - now)
                    .unwrap_or_else(PoisonError::into_inner)
                    .0;
                continue;
            }
            state.pending = None;
            let failed = state.failed.take();
            drop(state);
            judge(failed);
            state = self.lock();
        }
    }
}

/// What the watcher resolves after a notification, fixed at `start`.
#[derive(Debug, Clone)]
struct DeviceProbe {
    needs_mic: bool,
    needs_system: bool,
    /// `None`: the default capture endpoint.
    input_device_uid: Option<String>,
    mic_endpoint_id: Option<String>,
    render_endpoint_id: Option<String>,
}

impl DeviceProbe {
    fn endpoint_id(enumerator: &Enumerator, flow: EndpointFlow) -> Option<String> {
        enumerator
            .default_endpoint(flow)
            .and_then(|endpoint| endpoint.id())
            .ok()
    }

    fn is_alive(enumerator: &Enumerator, id: Option<&str>) -> bool {
        id.is_some_and(|id| {
            enumerator
                .endpoint(id)
                .is_ok_and(|endpoint| endpoint.is_active())
        })
    }

    /// The devices as they are now, in [`DeviceSnapshot`]'s terms: the
    /// default render endpoint (`eConsole`, the one both loopbacks follow),
    /// the microphone as [`chosen_or_default_input`] picks it (so the explicit
    /// one coming back reads as a change, as on the Mac), whether the
    /// endpoints the capture started on are still active.
    /// `default_output_uid` stays empty: no stream opens the
    /// `eCommunications` default, so its changes cost no rebuild. Fields
    /// for a stream the capture does not open stay empty too, so they never
    /// differ. The engine converts to 48 kHz, so the rate is always
    /// [`SAMPLE_RATE`]; a device format change invalidates the stream
    /// instead, which the capture thread reports as the device gone.
    fn resolve(&self, enumerator: &Enumerator) -> DeviceSnapshot {
        let input_uid = if self.needs_mic {
            chosen_or_default_input(enumerator, self.input_device_uid.as_deref())
                .0
                .and_then(|endpoint| endpoint.id().ok())
        } else {
            None
        };
        DeviceSnapshot {
            output_uid: self
                .needs_system
                .then(|| Self::endpoint_id(enumerator, EndpointFlow::Render))
                .flatten(),
            default_output_uid: None,
            input_uid,
            output_alive: self.needs_system
                && Self::is_alive(enumerator, self.render_endpoint_id.as_deref()),
            input_alive: self.needs_mic
                && Self::is_alive(enumerator, self.mic_endpoint_id.as_deref()),
            sample_rate: SAMPLE_RATE,
        }
    }

    /// The snapshot the capture started on: as resolved now, with the
    /// endpoints the streams actually opened on, so a default that moved
    /// between the open and this read still counts as a change.
    fn baseline(&self, enumerator: &Enumerator) -> DeviceSnapshot {
        let mut snapshot = self.resolve(enumerator);
        if self.needs_mic {
            snapshot.input_uid.clone_from(&self.mic_endpoint_id);
        }
        if self.needs_system {
            snapshot.output_uid.clone_from(&self.render_endpoint_id);
        }
        snapshot
    }
}

/// What one started capture holds.
struct Active {
    stop: Arc<AtomicBool>,
    streams: Vec<Launched>,
    /// The system stream's staging, for its counts at `stop()`.
    follower: Option<Arc<FollowerLane>>,
    watcher: Arc<Watcher>,
    watcher_thread: JoinHandle<()>,
}

/// The WASAPI backend; see the module doc. Restartable: `stop()` joins the
/// stream threads and the watcher, unless the watcher is still in its
/// start-up COM calls (left to finish on its own), and `start` opens the
/// streams afresh.
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
    /// How long a burst of notifications settles before the devices are
    /// resolved once; the macOS backend's value.
    pub const COALESCE_DELAY: Duration = Duration::from_millis(500);

    /// No capture running.
    #[must_use]
    pub fn new() -> Self {
        Self {
            active: Mutex::new(None),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Option<Active>> {
        self.active.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The capture endpoint a capture asked for `uid` records now
/// ([`chosen_or_default`]): the chosen one while it is active, else the
/// active default.
fn chosen_or_default_input(
    enumerator: &Enumerator,
    uid: Option<&str>,
) -> (Option<com::Endpoint>, bool) {
    chosen_or_default(
        uid,
        |uid| enumerator.endpoint(uid).ok(),
        || enumerator.default_endpoint(EndpointFlow::Capture).ok(),
        com::Endpoint::is_active,
    )
}

/// Joins COM and opens the stream `source` needs on this thread. The
/// apartment comes first so it is dropped last.
fn open(
    source: StreamSource,
    input_device_uid: Option<&str>,
) -> Result<(Apartment, Enumerator, CaptureClient, StreamInfo), CaptureError> {
    let apartment = Apartment::enter()?;
    let enumerator = Enumerator::new()?;
    let channels = source.channels();
    let mut input = None;
    let (client, endpoint_id, loopback) = match source {
        StreamSource::Microphone => {
            let (endpoint, is_fallback) = chosen_or_default_input(&enumerator, input_device_uid);
            let endpoint = endpoint.ok_or(CaptureError::InputDeviceUnavailable)?;
            let id = endpoint.id().ok();
            if is_fallback {
                tracing::warn!(
                    "the input device {} is not active; recording from the default input {}",
                    input_device_uid.unwrap_or_default(),
                    id.as_deref().unwrap_or_default()
                );
            }
            input = id.clone().map(|uid| CaptureInput {
                name: endpoint.friendly_name().unwrap_or_else(|| uid.clone()),
                uid,
                is_fallback,
            });
            let client = CaptureClient::microphone(&endpoint, channels)?;
            (client, id, None)
        }
        StreamSource::System => {
            let render = enumerator.default_endpoint(EndpointFlow::Render);
            let render_id = render.as_ref().ok().and_then(|e| e.id().ok());
            match CaptureClient::process_loopback(std::process::id(), channels, ACTIVATION_TIMEOUT)
            {
                Ok(client) => (client, render_id, Some(LoopbackKind::Process)),
                Err(error) => {
                    tracing::info!(
                        "process loopback unavailable ({error}); falling back to loopback of \
                         the default render endpoint, which records Steno's own output too"
                    );
                    let render = render.map_err(|_| CaptureError::OutputDeviceUnavailable)?;
                    let client = CaptureClient::endpoint_loopback(&render, channels)?;
                    (client, render_id, Some(LoopbackKind::Endpoint))
                }
            }
        }
    };
    let info = StreamInfo {
        sizes: client.sizes(),
        endpoint_id,
        loopback,
        input,
    };
    Ok((apartment, enumerator, client, info))
}

/// A stream thread: opens its stream, reports, waits for its body, starts,
/// then drains packets into the body until `stop` is set or the stream
/// fails.
fn run_stream(
    source: StreamSource,
    input_device_uid: Option<&str>,
    events: &SyncSender<StreamEvent>,
    bodies: &Receiver<StreamBody>,
    stop: &AtomicBool,
    watcher: &Watcher,
) {
    let (apartment, enumerator, mut client, info) = match open(source, input_device_uid) {
        Ok(opened) => opened,
        Err(error) => {
            let _ = events.send(StreamEvent::Opened(Err(error)));
            return;
        }
    };
    let _ = events.send(StreamEvent::Opened(Ok(info)));
    // `start` drops the sender when it gives up.
    let Ok(mut body) = bodies.recv() else {
        return;
    };
    if let Err(error) = client.start() {
        let _ = events.send(StreamEvent::Started(Err(error.into())));
        return;
    }
    let _ = events.send(StreamEvent::Started(Ok(())));
    let pro_audio = ProAudioThread::enter();
    if pro_audio.is_none() {
        tracing::info!("{} stream runs without MMCSS Pro Audio", source.as_str());
    }
    while !stop.load(Ordering::Acquire) {
        client.wait(WAIT);
        if stop.load(Ordering::Acquire) {
            break;
        }
        if let Err(error) = client.drain(|packet| body.handle(packet)) {
            let why = if error.is_device_invalidated() {
                "its device went away or changed format"
            } else {
                "a capture call failed"
            };
            tracing::warn!("{} stream stopped, {why}: {error}", source.as_str());
            watcher.note_failure(source);
            break;
        }
    }
    let _ = client.stop();
    let discontinuities = client.discontinuities();
    if discontinuities > 0 {
        tracing::info!(
            "{} stream: {discontinuities} packets after a glitch (thread late)",
            source.as_str()
        );
    }
    drop(pro_audio);
    drop(client);
    drop(enumerator);
    drop(apartment);
}

/// The watcher thread: registers for endpoint notifications, takes the
/// baseline, then judges every settled burst (see the module doc).
fn run_watcher(
    watcher: &Arc<Watcher>,
    probe: &DeviceProbe,
    sink: &LaneFrameSink,
    ready: &SyncSender<()>,
) {
    let apartment = Apartment::enter().ok();
    let enumerator = Enumerator::new().ok();
    let registration = enumerator.as_ref().and_then(|enumerator| {
        let notify = Arc::clone(watcher);
        enumerator
            .register(Box::new(move || notify.note()))
            .map_err(|error| tracing::warn!("no device notifications: {error}"))
            .ok()
    });
    let baseline = enumerator.as_ref().map(|e| probe.baseline(e));
    watcher.watch(ready, |failed| {
        // The WASAPI reads run outside the lock.
        let difference = match (&enumerator, &baseline) {
            (Some(enumerator), Some(baseline)) => probe.resolve(enumerator).difference(baseline),
            _ => None,
        };
        let reason = difference.or_else(|| {
            failed.map(|source| match source {
                StreamSource::Microphone => DeviceChangeReason::InputDeviceGone,
                StreamSource::System => DeviceChangeReason::OutputDeviceGone,
            })
        });
        if let Some(reason) = reason {
            tracing::info!("device change reported: {reason:?}");
            sink.report_device_change(reason);
        } else {
            tracing::info!("ignored device notification");
        }
    });
    drop(registration);
    drop(enumerator);
    drop(apartment);
}

/// Stops and joins the stream threads that answered
/// ([`Launched::answered`]); the others are left to finish on their own.
/// Such a thread never writes to the sink: it holds no body, or it is
/// starting its stream and finds `stop` set before its first drain.
fn tear_down(stop: &AtomicBool, streams: Vec<Launched>) {
    stop.store(true, Ordering::Release);
    for mut launched in streams {
        launched.body.take();
        if let Some(thread) = launched.thread.take()
            && launched.answered
        {
            let _ = thread.join();
        }
    }
}

/// The next event from a stream thread before `deadline`.
fn next_event(launched: &Launched, deadline: Instant) -> Result<StreamEvent, CaptureError> {
    let limit = deadline.saturating_duration_since(Instant::now());
    launched.events.recv_timeout(limit).map_err(|error| {
        CaptureError::BackendFailed(match error {
            RecvTimeoutError::Timeout => {
                format!(
                    "{} stream did not answer within {START_TIMEOUT:?} of the start",
                    launched.source.as_str()
                )
            }
            RecvTimeoutError::Disconnected => {
                format!("{} stream thread ended", launched.source.as_str())
            }
        })
    })
}

/// Spawns one thread per stream in `plan`, master first.
fn spawn_streams(
    plan: &SplitStreamPlan,
    input_device_uid: Option<&str>,
    stop: &Arc<AtomicBool>,
    watcher: &Arc<Watcher>,
) -> Result<Vec<Launched>, CaptureError> {
    let mut streams = Vec::new();
    for source in plan.streams() {
        let (event_sender, events) = sync_channel(2);
        let (body, bodies) = sync_channel(1);
        let uid = input_device_uid.map(str::to_owned);
        let thread_stop = Arc::clone(stop);
        let thread_watcher = Arc::clone(watcher);
        let spawned = std::thread::Builder::new()
            .name(format!("steno-wasapi-{}", source.as_str()))
            .spawn(move || {
                run_stream(
                    source,
                    uid.as_deref(),
                    &event_sender,
                    &bodies,
                    &thread_stop,
                    &thread_watcher,
                );
            });
        match spawned {
            Ok(thread) => streams.push(Launched {
                source,
                thread: Some(thread),
                events,
                body: Some(body),
                info: None,
                answered: false,
            }),
            Err(error) => {
                tear_down(stop, streams);
                return Err(CaptureError::BackendFailed(format!(
                    "{} stream thread: {error}",
                    source.as_str()
                )));
            }
        }
    }
    Ok(streams)
}

/// The stream thread's next event before `deadline`, which must be
/// `Opened` (`opening`) or `Started` and a success.
fn expect_event(
    launched: &mut Launched,
    opening: bool,
    deadline: Instant,
) -> Result<(), CaptureError> {
    let event = next_event(launched, deadline);
    // Only this answer counts: a thread that gave it then waits on
    // `start`'s channel or runs its capture loop, and either way it sees
    // `stop`. One that missed the deadline may sit in a COM call (the open
    // or `Start`).
    launched.answered = event.is_ok();
    match event {
        Ok(StreamEvent::Opened(Ok(info))) if opening => {
            launched.info = Some(info);
            Ok(())
        }
        Ok(StreamEvent::Started(Ok(()))) if !opening => Ok(()),
        Ok(StreamEvent::Opened(Err(error)) | StreamEvent::Started(Err(error))) | Err(error) => {
            Err(error)
        }
        Ok(_) => Err(CaptureError::BackendFailed(
            "stream thread out of step".into(),
        )),
    }
}

/// Waits for every stream thread to open its stream; tears everything
/// down on the first failure.
fn open_streams(
    stop: &AtomicBool,
    mut streams: Vec<Launched>,
    deadline: Instant,
) -> Result<Vec<Launched>, CaptureError> {
    for index in 0..streams.len() {
        if let Err(error) = expect_event(&mut streams[index], true, deadline) {
            tear_down(stop, streams);
            return Err(error);
        }
    }
    Ok(streams)
}

/// Hands each opened stream its per-packet body and waits for it to
/// start, the follower before the master: the master routes into the
/// sink, its only producer, so a start that fails has written nothing.
/// Tears everything down on the first failure.
fn start_streams(
    stop: &AtomicBool,
    mut streams: Vec<Launched>,
    plan: &SplitStreamPlan,
    follower: Option<&Arc<FollowerLane>>,
    sink: &Arc<LaneFrameSink>,
    deadline: Instant,
) -> Result<Vec<Launched>, CaptureError> {
    // `spawn_streams` put the master first.
    for index in (0..streams.len()).rev() {
        let launched = &mut streams[index];
        let buffer_frames = launched.info.as_ref().map_or(0, |i| i.sizes.buffer_frames);
        let body = if launched.source == plan.master {
            Some(StreamBody::Master {
                router: PacketRouter::new(
                    plan.layout.sources.clone(),
                    follower.cloned(),
                    buffer_frames,
                ),
                sink: Arc::clone(sink),
            })
        } else {
            follower.map(|lane| StreamBody::follower(Arc::clone(lane), buffer_frames))
        };
        // Without a body the thread exits, which `expect_event` reports.
        if let (Some(body), Some(sender)) = (body, launched.body.take()) {
            let _ = sender.send(body);
        }
        if let Err(error) = expect_event(launched, false, deadline) {
            tear_down(stop, streams);
            return Err(error);
        }
    }
    Ok(streams)
}

/// Starts the watcher thread and waits for it to register. A capture
/// without device notifications still records, so a slow answer is
/// logged, not fatal: such a watcher may sit in a COM call, which
/// `stop()` must not wait on, and once it comes alive it watches as an
/// on-time one does ([`Watcher::stop_and_join`]).
fn spawn_watcher(
    watcher: &Arc<Watcher>,
    probe: DeviceProbe,
    sink: &Arc<LaneFrameSink>,
    deadline: Instant,
) -> Result<JoinHandle<()>, CaptureError> {
    let (ready, ready_receiver) = sync_channel(1);
    let thread_watcher = Arc::clone(watcher);
    let thread_sink = Arc::clone(sink);
    let thread = std::thread::Builder::new()
        .name("steno-devices".into())
        .spawn(move || run_watcher(&thread_watcher, &probe, &thread_sink, &ready))
        .map_err(|error| CaptureError::BackendFailed(format!("device watcher: {error}")))?;
    let limit = deadline.saturating_duration_since(Instant::now());
    if ready_receiver.recv_timeout(limit).is_err() {
        tracing::warn!("device watcher did not start within {START_TIMEOUT:?} of the start");
    }
    Ok(thread)
}

impl CaptureBackend for LiveCaptureBackend {
    /// Opens the streams the lanes need on their own threads, hands each
    /// its per-packet body, starts them, then the watcher. Returns the
    /// 48 kHz stream with the latencies the session's far-end delay is
    /// built from: the microphone's and the system stream's
    /// `GetStreamLatency`, less what the follower's staging already delays
    /// the system lane by ([`far_end_latencies`]).
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
        let plan = SplitStreamPlan::new(lanes)?;
        let deadline = Instant::now() + START_TIMEOUT;
        let stop = Arc::new(AtomicBool::new(false));
        let watcher = Arc::new(Watcher::default());

        let streams = spawn_streams(&plan, input_device_uid, &stop, &watcher)?;
        // Every stream opened, or none runs.
        let streams = open_streams(&stop, streams, deadline)?;
        let info = |source: StreamSource| {
            streams
                .iter()
                .find(|s| s.source == source)
                .and_then(|s| s.info.clone())
        };
        let mic = info(StreamSource::Microphone);
        let system = info(StreamSource::System);
        let master_buffer = info(plan.master).map_or(0, |i| i.sizes.buffer_frames);
        let follower = plan.follower.and_then(|source| {
            info(source).map(|i| {
                Arc::new(FollowerLane::for_streams(
                    i.sizes.period_frames,
                    master_buffer,
                ))
            })
        });
        let streams = start_streams(&stop, streams, &plan, follower.as_ref(), &sink, deadline)?;
        if let Some(kind) = system.as_ref().and_then(|s| s.loopback) {
            tracing::info!("system lane on {kind:?} loopback");
        }

        let probe = DeviceProbe {
            needs_mic: mic.is_some(),
            needs_system: system.is_some(),
            input_device_uid: input_device_uid.map(str::to_owned),
            mic_endpoint_id: mic.as_ref().and_then(|m| m.endpoint_id.clone()),
            render_endpoint_id: system.as_ref().and_then(|s| s.endpoint_id.clone()),
        };
        let watcher_thread = match spawn_watcher(&watcher, probe, &sink, deadline) {
            Ok(thread) => thread,
            Err(error) => {
                tear_down(&stop, streams);
                return Err(error);
            }
        };

        let input_latency = mic.as_ref().map_or(0, |m| m.sizes.latency_frames);
        let output_latency = system.as_ref().map_or(0, |s| s.sizes.latency_frames);
        let follower_delay = follower.as_ref().map_or(0, |f| f.target());
        if follower_delay > input_latency + output_latency {
            tracing::info!(
                "the system lane's staging ({follower_delay} frames) exceeds both streams' \
                 latency ({input_latency} + {output_latency} frames): the far end runs \
                 that much later than the echo canceller expects"
            );
        }
        let (input_latency_frames, output_latency_frames) =
            far_end_latencies(input_latency, output_latency, follower_delay);
        *active = Some(Active {
            stop,
            streams,
            follower,
            watcher,
            watcher_thread,
        });
        Ok(CaptureStream {
            sample_rate: SAMPLE_RATE,
            input_latency_frames,
            output_latency_frames,
            layout: Some(plan.layout),
            input: mic.and_then(|m| m.input),
        })
    }

    /// Stops the capture threads (no frame arrives after it returns), then
    /// the watcher.
    fn stop(&self) {
        let Some(mut active) = self.lock().take() else {
            return;
        };
        tear_down(&active.stop, std::mem::take(&mut active.streams));
        if let Some(lane) = &active.follower {
            tracing::info!(
                "system stream staging: {} frames padded with zeros (underrun), {} skipped \
                 (slipped), {} from before the first pull trimmed",
                lane.underrun_frames(),
                lane.slipped_frames(),
                lane.trimmed_frames()
            );
        }
        active.watcher.stop_and_join(active.watcher_thread);
    }
}

impl Drop for LiveCaptureBackend {
    /// A backend dropped without `stop()` still joins its threads.
    fn drop(&mut self) {
        self.stop();
    }
}

/// The WASAPI endpoints for the input picker, under the macOS
/// `capture::live::devices::AudioDevices` names for the calls both have
/// (`all`, `inputs`, `device`, `default_input`, `default_system_output`;
/// Swift: `Sources/StenoAudio/Capture/AudioDevices.swift`); errors are
/// [`CaptureError`]. Every call joins COM on the calling thread for its
/// duration.
pub struct AudioDevices;

impl AudioDevices {
    /// Every active endpoint, capture endpoints first, in the order the
    /// enumerator lists them. `id` is the index in that list (WASAPI has
    /// no numeric ids); `uid` is the endpoint id `Settings` stores; the
    /// channel count and rate are the shared-mode mix format's. Windows has
    /// one render default that matters here, `eConsole`, which both
    /// loopbacks follow and [`DeviceSnapshot::output_uid`] tracks, so it is
    /// both the default output and the default system output.
    pub fn all() -> Result<Vec<AudioDeviceInfo>, CaptureError> {
        let apartment = Apartment::enter()?;
        let enumerator = Enumerator::new()?;
        let default = |flow| DeviceProbe::endpoint_id(&enumerator, flow);
        let default_input = default(EndpointFlow::Capture);
        let default_output = default(EndpointFlow::Render);
        let mut devices = Vec::new();
        for flow in [EndpointFlow::Capture, EndpointFlow::Render] {
            for endpoint in enumerator.active_endpoints(flow)? {
                let Ok(uid) = endpoint.id() else {
                    continue;
                };
                let (channels, rate) = endpoint.mix_format().unwrap_or((0, 0));
                let is_input = flow == EndpointFlow::Capture;
                let is_default = |default: Option<&str>| default == Some(uid.as_str());
                let is_default_output = !is_input && is_default(default_output.as_deref());
                devices.push(AudioDeviceInfo {
                    id: u32::try_from(devices.len()).unwrap_or(u32::MAX),
                    name: endpoint.friendly_name().unwrap_or_else(|| uid.clone()),
                    input_channels: if is_input { channels } else { 0 },
                    output_channels: if is_input { 0 } else { channels },
                    nominal_sample_rate: f64::from(rate),
                    transport_type: "WASAPI endpoint".into(),
                    is_running_somewhere: false,
                    is_default_input: is_input && is_default(default_input.as_deref()),
                    is_default_output,
                    is_default_system_output: is_default_output,
                    uid,
                });
            }
        }
        drop(enumerator);
        drop(apartment);
        Ok(devices)
    }

    /// The capture endpoints.
    pub fn inputs() -> Result<Vec<AudioDeviceInfo>, CaptureError> {
        Ok(Self::all()?
            .into_iter()
            .filter(AudioDeviceInfo::is_input)
            .collect())
    }

    /// The active endpoint with `uid`, or `None` when it is not connected.
    pub fn device(uid: &str) -> Result<Option<AudioDeviceInfo>, CaptureError> {
        Ok(Self::all()?.into_iter().find(|d| d.uid == uid))
    }

    /// The default capture endpoint (`eConsole`).
    pub fn default_input() -> Result<AudioDeviceInfo, CaptureError> {
        Self::all()?
            .into_iter()
            .find(|d| d.is_default_input)
            .ok_or(CaptureError::InputDeviceUnavailable)
    }

    /// The default render endpoint (`eConsole`), which both loopbacks
    /// follow.
    pub fn default_system_output() -> Result<AudioDeviceInfo, CaptureError> {
        Self::all()?
            .into_iter()
            .find(|d| d.is_default_system_output)
            .ok_or(CaptureError::OutputDeviceUnavailable)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::channel;

    use super::*;

    const WAIT: Duration = Duration::from_secs(5);

    /// A watcher that is judging a burst when `stop()` comes (a late one
    /// that came alive behaves the same) is joined: its report lands
    /// before `stop()` returns, never after.
    #[test]
    fn a_watcher_stopped_while_it_judges_is_joined_before_stop_returns() {
        let watcher = Arc::new(Watcher::default());
        let stopped = Arc::new(AtomicBool::new(false));
        let (ready, answered) = sync_channel(1);
        let (entered, judging) = channel();
        let (open, gate) = channel::<()>();
        let (judged, judgements) = channel();
        let thread = {
            let watcher = Arc::clone(&watcher);
            let stopped = Arc::clone(&stopped);
            std::thread::spawn(move || {
                watcher.watch(&ready, |_| {
                    let _ = entered.send(());
                    let _ = gate.recv_timeout(WAIT);
                    let _ = judged.send(stopped.load(Ordering::SeqCst));
                });
            })
        };
        answered
            .recv_timeout(WAIT)
            .expect("the watcher reached its loop");
        watcher.note();
        judging.recv_timeout(WAIT).expect("the burst settled");
        // The gate opens once `stop_and_join` returns, or after a moment
        // while it rightly waits for the judgement.
        let (returned, returns) = channel::<()>();
        let opener = std::thread::spawn(move || {
            let _ = returns.recv_timeout(Duration::from_millis(500));
            let _ = open.send(());
        });
        watcher.stop_and_join(thread);
        stopped.store(true, Ordering::SeqCst);
        let _ = returned.send(());
        opener.join().unwrap();
        assert_eq!(
            judgements.recv_timeout(WAIT),
            Ok(false),
            "judged before stop_and_join returned"
        );
    }

    /// A watcher still in its start-up calls when `stop()` comes is not
    /// waited for; once they return it exits without judging the burst
    /// that is pending.
    #[test]
    fn a_watcher_stopped_before_its_loop_is_not_joined_and_judges_nothing() {
        let watcher = Arc::new(Watcher::default());
        let (ready, _answered) = sync_channel(1);
        let (open, gate) = channel::<()>();
        let (done, outcome) = channel();
        watcher.note();
        let thread = {
            let watcher = Arc::clone(&watcher);
            std::thread::spawn(move || {
                // Stands in for the apartment, the registration and the
                // baseline.
                let opened = gate.recv_timeout(WAIT).is_ok();
                let mut judged = false;
                watcher.watch(&ready, |_| judged = true);
                let _ = done.send((opened, judged));
            })
        };
        watcher.stop_and_join(thread);
        let _ = open.send(());
        assert_eq!(
            outcome.recv_timeout(WAIT),
            Ok((true, false)),
            "stop did not wait for the start-up, and nothing was judged"
        );
    }
}
