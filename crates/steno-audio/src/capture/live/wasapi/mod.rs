//! The live capture backend on Windows (WP10): WASAPI in shared mode, one
//! capture thread per stream, endpoint notifications and the rebuild report.
//!
//! **Compile-verified only.** There is no Windows machine in the fleet:
//! this backend is written against Microsoft's documentation and its
//! samples, compiled, linted and unit-tested on the `windows-latest` CI
//! runner, which has no audio device. Nothing here has captured a sample
//! on real hardware; `tests/live_windows.rs` holds the `--ignored` checks a
//! Windows machine must run before this ships (the plan's parity list).
//!
//! # Streams
//!
//! - **System lane:** process loopback (`ActivateAudioInterfaceAsync` on
//!   `VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK`,
//!   `AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK`) excluding Steno's own
//!   process tree, Windows 10 2004 and later. Where that activation fails,
//!   loopback of the default render endpoint, which records Steno's own
//!   output too.
//! - **Microphone:** the selected capture endpoint by id, or the default
//!   (`eCapture`, `eConsole`), shared mode, event-driven.
//!
//! Both are asked for 48 kHz 32-bit float (`AUTOCONVERTPCM`, so the engine
//! resamples from the device's mix format): mono for the microphone,
//! stereo for the system audio, folded to mono. The streams run on two
//! clocks and arrive on two threads; [`SplitStreamPlan`] picks the master
//! (the sink's only producer) and [`StreamBody`] is what each thread runs
//! per packet, with the all-or-nothing reservation across lanes and the
//! drop accounting of the IOProc path (see `realtime::streams`).
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
//! stop flag.

pub(crate) mod com;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, sync_channel};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use steno_core::AudioLane;

use self::com::{Apartment, CaptureClient, Enumerator, LoopbackKind, ProAudioThread};
use super::AudioDeviceInfo;
use crate::SAMPLE_RATE;
use crate::capture::split_streams::far_end_latencies;
use crate::capture::{
    CaptureBackend, CaptureError, CaptureStream, DeviceChangeReason, DeviceSnapshot,
    SplitStreamPlan, StreamSource,
};
use crate::detection::EndpointFlow;
use crate::realtime::{FollowerLane, LaneFrameSink, PacketRouter, StreamBody};

/// How long a stream thread may take to open its stream (process loopback
/// activation included) before `start` gives up on it.
const OPEN_TIMEOUT: Duration = Duration::from_secs(10);
/// How long the asynchronous process-loopback activation may take.
const ACTIVATION_TIMEOUT: Duration = Duration::from_secs(5);
/// The longest a capture thread waits for the engine's event; also how
/// late `stop()` may notice the stop flag.
const WAIT: Duration = Duration::from_millis(50);

/// What one stream thread found when it opened its stream.
#[derive(Debug, Clone)]
struct StreamInfo {
    buffer_frames: usize,
    period_frames: usize,
    latency_frames: usize,
    /// The endpoint the stream runs on: the microphone's, or for the
    /// system stream the default render endpoint at start.
    endpoint_id: Option<String>,
    loopback: Option<LoopbackKind>,
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
    /// The thread answered `start` at least once, so it is not stuck in a
    /// COM call and may be joined.
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

    /// From a notification callback: something may have changed.
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

    fn stop(&self) {
        self.lock().stop = true;
        self.condvar.notify_all();
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
    /// the microphone (the explicit one if still active, else the
    /// `eConsole` default), whether the endpoints the capture started on
    /// are still active. `default_output_uid` stays empty: no stream opens
    /// the `eCommunications` default, so its changes cost no rebuild. Fields
    /// for a stream the capture does not open stay empty too, so they never
    /// differ. The engine converts to 48 kHz, so the rate is always
    /// [`SAMPLE_RATE`]; a device format change invalidates the stream
    /// instead, which the capture thread reports as the device gone.
    fn resolve(&self, enumerator: &Enumerator) -> DeviceSnapshot {
        let input_uid = if self.needs_mic {
            match &self.input_device_uid {
                Some(uid) => enumerator
                    .endpoint(uid)
                    .ok()
                    .filter(com::Endpoint::is_active)
                    .and_then(|endpoint| endpoint.id().ok()),
                None => Self::endpoint_id(enumerator, EndpointFlow::Capture),
            }
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
    watcher: Arc<Watcher>,
    watcher_thread: Option<JoinHandle<()>>,
}

/// The WASAPI backend; see the module doc. Restartable: `stop()` joins
/// every thread it started, and `start` opens the streams afresh.
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

/// Joins COM and opens the stream `source` needs on this thread. The
/// apartment comes first so it is dropped last.
fn open(
    source: StreamSource,
    input_device_uid: Option<&str>,
) -> Result<(Apartment, Enumerator, CaptureClient, StreamInfo), CaptureError> {
    let apartment = Apartment::enter()?;
    let enumerator = Enumerator::new()?;
    let channels = source.channels();
    let (client, endpoint_id, loopback) = match source {
        StreamSource::Microphone => {
            let endpoint = match input_device_uid {
                Some(uid) => enumerator.endpoint(uid).ok(),
                None => enumerator.default_endpoint(EndpointFlow::Capture).ok(),
            }
            .filter(com::Endpoint::is_active)
            .ok_or(CaptureError::InputDeviceUnavailable)?;
            let client = CaptureClient::microphone(&endpoint, channels)?;
            (client, endpoint.id().ok(), None)
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
        buffer_frames: client.buffer_frames(),
        period_frames: client.period_frames(),
        latency_frames: client.latency_frames(),
        endpoint_id,
        loopback,
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
    if client.discontinuities() > 0 {
        tracing::info!(
            "{} stream: {} packets after a glitch (thread late)",
            source.as_str(),
            client.discontinuities()
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
            .register(Box::new(move |_| notify.note()))
            .map_err(|error| tracing::warn!("no device notifications: {error}"))
            .ok()
    });
    let baseline = enumerator.as_ref().map(|e| probe.baseline(e));
    let _ = ready.send(());
    let mut state = watcher.lock();
    loop {
        if state.stop {
            break;
        }
        let Some(last) = state.pending else {
            state = watcher
                .condvar
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
            continue;
        };
        let due = last + LiveCaptureBackend::COALESCE_DELAY;
        let now = Instant::now();
        if now < due {
            state = watcher
                .condvar
                .wait_timeout(state, due - now)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
            continue;
        }
        state.pending = None;
        let failed = state.failed.take();
        drop(state);
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
        state = watcher.lock();
    }
    drop(state);
    drop(registration);
    drop(enumerator);
    drop(apartment);
}

/// Stops and joins the stream threads that answered; a thread that never
/// answered `start` is left to finish on its own (it holds no body, so it
/// never writes to the sink).
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

/// The next event from a stream thread within `limit`.
fn next_event(launched: &Launched, limit: Duration) -> Result<StreamEvent, CaptureError> {
    launched.events.recv_timeout(limit).map_err(|error| {
        CaptureError::BackendFailed(match error {
            RecvTimeoutError::Timeout => {
                format!(
                    "{} stream did not open within {limit:?}",
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

/// The stream thread's next event, which must be `Opened` (`opening`) or
/// `Started` and a success.
fn expect_event(launched: &mut Launched, opening: bool) -> Result<(), CaptureError> {
    let event = next_event(launched, OPEN_TIMEOUT);
    launched.answered |= event.is_ok();
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
) -> Result<Vec<Launched>, CaptureError> {
    for index in 0..streams.len() {
        if let Err(error) = expect_event(&mut streams[index], true) {
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
) -> Result<Vec<Launched>, CaptureError> {
    // `spawn_streams` put the master first.
    for index in (0..streams.len()).rev() {
        let launched = &mut streams[index];
        let buffer_frames = launched.info.as_ref().map_or(0, |i| i.buffer_frames);
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
        if let Err(error) = expect_event(launched, false) {
            tear_down(stop, streams);
            return Err(error);
        }
    }
    Ok(streams)
}

/// Starts the watcher thread and waits for it to register. A capture
/// without device notifications still records, so a slow answer is
/// logged, not fatal.
fn spawn_watcher(
    watcher: &Arc<Watcher>,
    probe: DeviceProbe,
    sink: &Arc<LaneFrameSink>,
) -> Result<JoinHandle<()>, CaptureError> {
    let (ready, ready_receiver) = sync_channel(1);
    let thread_watcher = Arc::clone(watcher);
    let thread_sink = Arc::clone(sink);
    let thread = std::thread::Builder::new()
        .name("steno-devices".into())
        .spawn(move || run_watcher(&thread_watcher, &probe, &thread_sink, &ready))
        .map_err(|error| CaptureError::BackendFailed(format!("device watcher: {error}")))?;
    if ready_receiver.recv_timeout(OPEN_TIMEOUT).is_err() {
        tracing::warn!("device watcher did not start within {OPEN_TIMEOUT:?}");
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
        let stop = Arc::new(AtomicBool::new(false));
        let watcher = Arc::new(Watcher::default());

        let streams = spawn_streams(&plan, input_device_uid, &stop, &watcher)?;
        // Every stream opened, or none runs.
        let streams = open_streams(&stop, streams)?;
        let info = |source: StreamSource| {
            streams
                .iter()
                .find(|s| s.source == source)
                .and_then(|s| s.info.clone())
        };
        let mic = info(StreamSource::Microphone);
        let system = info(StreamSource::System);
        let follower = plan.follower.and_then(|source| {
            info(source).map(|i| Arc::new(FollowerLane::for_period(i.period_frames)))
        });
        let streams = start_streams(&stop, streams, &plan, follower.as_ref(), &sink)?;
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
        let watcher_thread = match spawn_watcher(&watcher, probe, &sink) {
            Ok(thread) => thread,
            Err(error) => {
                tear_down(&stop, streams);
                return Err(error);
            }
        };

        let (input_latency_frames, output_latency_frames) = far_end_latencies(
            mic.as_ref().map_or(0, |m| m.latency_frames),
            system.as_ref().map_or(0, |s| s.latency_frames),
            follower.as_ref().map_or(0, |f| f.target()),
        );
        *active = Some(Active {
            stop,
            streams,
            watcher,
            watcher_thread: Some(watcher_thread),
        });
        Ok(CaptureStream {
            sample_rate: SAMPLE_RATE,
            input_latency_frames,
            output_latency_frames,
            layout: Some(plan.layout),
        })
    }

    /// Stops the capture threads (no frame arrives after it returns), then
    /// the watcher.
    fn stop(&self) {
        let Some(mut active) = self.lock().take() else {
            return;
        };
        tear_down(&active.stop, std::mem::take(&mut active.streams));
        active.watcher.stop();
        if let Some(thread) = active.watcher_thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for LiveCaptureBackend {
    /// A backend dropped without `stop()` still joins its threads.
    fn drop(&mut self) {
        self.stop();
    }
}

/// The WASAPI endpoints as the input picker and `steno dev audio-devices`
/// see them. Every call joins COM on the calling thread for its duration.
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

    /// The default capture endpoint (`eConsole`).
    pub fn default_input() -> Result<AudioDeviceInfo, CaptureError> {
        Self::all()?
            .into_iter()
            .find(|d| d.is_default_input)
            .ok_or(CaptureError::InputDeviceUnavailable)
    }

    /// The default render endpoint (`eConsole`).
    pub fn default_output() -> Result<AudioDeviceInfo, CaptureError> {
        Self::all()?
            .into_iter()
            .find(|d| d.is_default_output)
            .ok_or(CaptureError::OutputDeviceUnavailable)
    }
}
