//! The live capture backend on Windows: WASAPI in shared mode, one capture
//! thread per stream (process loopback, falling back to endpoint loopback,
//! and the microphone), endpoint notifications and the rebuild report.
//! Not run on hardware: tested only on a CI runner without audio devices;
//! `tests/live_windows.rs`' `--ignored` tests must run on a real machine.

pub(crate) mod com;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, sync_channel};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use steno_core::AudioLane;

use self::com::{Apartment, CaptureClient, ComError, Enumerator, LoopbackKind, ProAudioThread};
use super::{AudioDeviceInfo, chosen_or_default};
use crate::SAMPLE_RATE;
use crate::capture::split_streams::{DevicePositions, StreamSizes, far_end_latencies};
use crate::capture::start_log::{self, start_log};
use crate::capture::{
    CaptureBackend, CaptureError, CaptureInput, CaptureStream, DeviceChangeReason, DeviceSnapshot,
    SplitStreamPlan, StreamSource,
};
use crate::detection::EndpointFlow;
use crate::realtime::{
    FollowerLane, LaneFrameSink, Missed, PacketRouter, StreamBody, count_at_stop,
};

/// How long `start` waits, in total, for the stream threads to open and
/// start their streams and for the watcher to register. One deadline for
/// every wait, so a hanging COM call holds the session's lock this long at
/// most.
const START_TIMEOUT: Duration = Duration::from_secs(10);
const ACTIVATION_TIMEOUT: Duration = Duration::from_secs(5);
/// The longest a capture thread waits for the engine's event; also how
/// late `stop()` may notice the stop flag.
const WAIT: Duration = Duration::from_millis(50);

#[derive(Debug, Clone)]
struct StreamInfo {
    sizes: StreamSizes,
    /// The endpoint the stream runs on.
    endpoint_id: Option<String>,
    loopback: Option<LoopbackKind>,
    /// The microphone stream's endpoint; `None` for the system stream.
    input: Option<CaptureInput>,
}

enum StreamEvent {
    Opened(Result<StreamInfo, CaptureError>),
    Started(Result<(), CaptureError>),
}

#[derive(Debug, Default, Clone, Copy)]
struct Ends {
    /// Frames missed before its first packet, beyond its start latency.
    before_first: usize,
    /// Missed after its last packet, `None` when it was not read.
    after_last: Option<Missed>,
}

struct Launched {
    source: StreamSource,
    thread: Option<JoinHandle<Ends>>,
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
    pending: Option<Instant>,
    failed: Option<StreamSource>,
    stop: bool,
    /// The watcher thread is past its start-up COM calls and in its loop,
    /// so `stop()` may join it.
    ready: bool,
}

/// What the watcher judges: a settled burst of notifications, with the
/// stream that failed meanwhile, if any, or the re-check while the
/// microphone records the fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Judged {
    Notification { failed: Option<StreamSource> },
    Recheck,
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

    /// From a notification callback. This lock is held only for a store and
    /// the watcher's own short sections, never across a WASAPI call, so the
    /// wait is bounded and cannot deadlock with the audio service.
    fn note(&self) {
        self.lock().pending = Some(Instant::now());
        self.condvar.notify_all();
    }

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

    /// The watcher thread's loop once registered: marks it ready, then
    /// hands every settled burst to `judge` outside the lock until stopped;
    /// with `recheck`, also each time that long passes quietly.
    fn watch(
        &self,
        ready: &SyncSender<()>,
        recheck: Option<Duration>,
        mut judge: impl FnMut(Judged),
    ) {
        let mut state = self.lock();
        state.ready = true;
        let _ = ready.send(());
        loop {
            if state.stop {
                break;
            }
            let Some(last) = state.pending else {
                let Some(recheck) = recheck else {
                    state = self
                        .condvar
                        .wait(state)
                        .unwrap_or_else(PoisonError::into_inner);
                    continue;
                };
                let (next, waited) = self
                    .condvar
                    .wait_timeout(state, recheck)
                    .unwrap_or_else(PoisonError::into_inner);
                state = next;
                if waited.timed_out() && state.pending.is_none() && !state.stop {
                    drop(state);
                    judge(Judged::Recheck);
                    state = self.lock();
                }
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
            judge(Judged::Notification { failed });
            state = self.lock();
        }
    }
}

#[derive(Debug, Clone)]
struct DeviceProbe {
    needs_mic: bool,
    needs_system: bool,
    /// `None`: the default capture endpoint.
    input_device_uid: Option<String>,
    /// The microphone records the fallback, so the watcher re-checks.
    is_fallback: bool,
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
    /// default render endpoint, the microphone as
    /// [`chosen_or_default_input`] picks it, whether the endpoints the
    /// capture started on are still active. `default_output_uid` stays
    /// empty: no stream opens the `eCommunications` default. The engine
    /// converts to 48 kHz, so the rate is always [`SAMPLE_RATE`].
    fn resolve(&self, enumerator: &Enumerator) -> DeviceSnapshot {
        let input_uid = if self.needs_mic {
            chosen_or_default_input(enumerator, self.input_device_uid.as_deref())
                .and_then(|(endpoint, _)| endpoint.id().ok())
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

    /// The snapshot the capture started on, with the endpoints the streams
    /// actually opened on, so a default that moved meanwhile still counts.
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

struct Active {
    stop: Arc<AtomicBool>,
    streams: Vec<Launched>,
    follower: Option<(Arc<FollowerLane>, usize)>,
    master: StreamSource,
    sink: Arc<LaneFrameSink>,
    first_start: bool,
    watcher: Arc<Watcher>,
    watcher_thread: JoinHandle<()>,
}

/// The WASAPI backend. Restartable: `stop()` joins the stream threads and
/// the watcher, unless the watcher is still in its start-up COM calls.
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
    /// resolved once.
    pub const COALESCE_DELAY: Duration = Duration::from_millis(500);

    /// How often a capture that records the fallback resolves the devices
    /// without a notification.
    pub const FALLBACK_RECHECK: Duration = Duration::from_secs(5);

    fn recheck_for(is_fallback: bool) -> Option<Duration> {
        is_fallback.then_some(Self::FALLBACK_RECHECK)
    }

    /// What a judgement reports of the endpoints `resolved` now against
    /// `baseline`: after a notification their first difference, else the
    /// failed stream's device gone; on a re-check only another microphone.
    fn judgement(
        judged: Judged,
        resolved: Option<&DeviceSnapshot>,
        baseline: Option<&DeviceSnapshot>,
    ) -> Option<DeviceChangeReason> {
        let compared = resolved.zip(baseline);
        match judged {
            Judged::Notification { failed } => compared
                .and_then(|(resolved, baseline)| resolved.difference(baseline))
                .or_else(|| {
                    failed.map(|source| match source {
                        StreamSource::Microphone => DeviceChangeReason::InputDeviceGone,
                        StreamSource::System => DeviceChangeReason::OutputDeviceGone,
                    })
                }),
            Judged::Recheck => {
                compared.and_then(|(resolved, baseline)| resolved.input_difference(baseline))
            }
        }
    }

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

/// The capture endpoint a capture asked for `uid` records now: the chosen
/// one while it is active, else the active default.
fn chosen_or_default_input(
    enumerator: &Enumerator,
    uid: Option<&str>,
) -> Option<(com::Endpoint, bool)> {
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
            let (endpoint, is_fallback) = chosen_or_default_input(&enumerator, input_device_uid)
                .ok_or(CaptureError::InputDeviceUnavailable)?;
            let id = endpoint.id().ok();
            if is_fallback {
                start_log!(
                    warn,
                    "the input device {} is not active; recording from the default input {}",
                    input_device_uid.unwrap_or_default(),
                    id.as_deref().unwrap_or_default()
                );
            }
            input = id.clone().map(|uid| CaptureInput {
                name: endpoint.friendly_name(),
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
                    start_log!(
                        info,
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
/// fails. Returns what its clocks read at the stop, on a stream that
/// delivers continuously.
fn run_stream(
    source: StreamSource,
    input_device_uid: Option<&str>,
    events: &SyncSender<StreamEvent>,
    bodies: &Receiver<StreamBody>,
    stop: &AtomicBool,
    watcher: &Watcher,
    starts_quietly: bool,
) -> Ends {
    let opened = start_log::quietly(starts_quietly, || open(source, input_device_uid));
    let (apartment, enumerator, mut client, info) = match opened {
        Ok(opened) => opened,
        Err(error) => {
            let _ = events.send(StreamEvent::Opened(Err(error)));
            return Ends::default();
        }
    };
    let loopback = info.loopback;
    let continuously = delivers_continuously(loopback);
    let _ = events.send(StreamEvent::Opened(Ok(info)));
    // `start` drops the sender when it gives up.
    let Ok(mut body) = bodies.recv() else {
        return Ends::default();
    };
    if let Err(error) = client.start(continuously) {
        let _ = events.send(StreamEvent::Started(Err(error.into())));
        return Ends::default();
    }
    let _ = events.send(StreamEvent::Started(Ok(())));
    let pro_audio = ProAudioThread::enter();
    if pro_audio.is_none() {
        tracing::info!("{} stream runs without MMCSS Pro Audio", source.as_str());
    }
    let mut failed = false;
    while !stop.load(Ordering::Acquire) {
        client.wait(WAIT);
        if let Err(error) = drain_into(&mut client, &mut body) {
            let why = if error.is_device_invalidated() {
                "its device went away or changed format"
            } else {
                "a capture call failed"
            };
            tracing::warn!("{} stream stopped, {why}: {error}", source.as_str());
            watcher.note_failure(source);
            failed = true;
            break;
        }
    }
    // What the engine still holds at the stop, then the counter against the
    // last packet for what no packet flags.
    let mut ends = Ends::default();
    if !failed {
        let _ = drain_into(&mut client, &mut body);
        if continuously {
            ends.after_last = Some(client.missed_at_stop());
        }
    }
    let _ = client.stop();
    if continuously {
        ends.before_first = client.missed_before_first();
    }
    if ends.before_first > 0 {
        tracing::info!(
            "{} stream: {:.3} s missed before its first packet",
            source.as_str(),
            ends.before_first as f64 / SAMPLE_RATE
        );
    }
    if let Some(end) = ends.after_last.filter(|end| !end.is_none()) {
        tracing::info!(
            "{} stream: {:.3} s missed after its last packet",
            source.as_str(),
            end.unfilled as f64 / SAMPLE_RATE
        );
    }
    let discontinuities = client.discontinuities();
    if discontinuities > 0 {
        tracing::info!(
            "{} stream: {discontinuities} packets after a glitch (thread late), {:.3} s \
             missed",
            source.as_str(),
            client.missed_frames() as f64 / SAMPLE_RATE
        );
    }
    log_positions(source, loopback, client.positions(), discontinuities);
    drop(pro_audio);
    drop(client);
    drop(enumerator);
    drop(apartment);
    ends
}

/// Whether a stream on `loopback` (`None` for the microphone) delivers
/// continuously: every stream but endpoint loopback, which delivers
/// nothing while nothing plays.
fn delivers_continuously(loopback: Option<LoopbackKind>) -> bool {
    loopback != Some(LoopbackKind::Endpoint)
}

/// The stop's line about `source`'s clocks: at `warn` when the stream had
/// `discontinuities` it could not fill (no sane QPC), or when a continuous
/// stream had its QPC judged not sane or ran 2 s or more without enough
/// clean QPC pairs to judge it; at `info` otherwise.
fn log_positions(
    source: StreamSource,
    loopback: Option<LoopbackKind>,
    positions: &DevicePositions,
    discontinuities: u64,
) {
    let on = loopback.map_or_else(String::new, |kind| format!(" on {kind:?} loopback"));
    let counted = if discontinuities > 0 && !positions.fills() {
        format!(
            "; what its {discontinuities} discontinuities lost was counted, not written as \
             silence"
        )
    } else {
        String::new()
    };
    // Without a sane QPC, the microphone counts what its positions cannot
    // read only past the drift allowance, and process loopback counts no
    // loss before its first packet.
    let qpc_sane = if delivers_continuously(loopback) {
        positions.qpc_judged()
    } else {
        Some(true)
    };
    let no_sane_qpc = match qpc_sane {
        Some(true) => "",
        Some(false) => {
            "; it ran without a sane QPC, so a loss before its first packet or with its \
             positions standing still may not be counted"
        }
        None => {
            "; its QPC was not judged (under 2 s), so a loss before its first packet or with \
             its positions standing still may not be counted"
        }
    };
    if counted.is_empty() && qpc_sane != Some(false) {
        tracing::info!("{} stream{on}: {positions}{no_sane_qpc}", source.as_str());
    } else {
        tracing::warn!(
            "{} stream{on}: {positions}{counted}{no_sane_qpc}",
            source.as_str()
        );
    }
}

/// Every packet the client holds into `body`, a flagged one after what was
/// missed before it.
fn drain_into(client: &mut CaptureClient, body: &mut StreamBody) -> Result<(), ComError> {
    client.drain(|packet, missed| {
        if let Some(missed) = missed {
            body.write_missed(&packet, missed);
        }
        body.handle(packet);
    })
}

/// The watcher thread: registers for endpoint notifications, takes the
/// baseline, then judges every settled burst.
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
    let recheck = LiveCaptureBackend::recheck_for(probe.is_fallback);
    watcher.watch(ready, recheck, |judged| {
        // The WASAPI reads run outside the lock.
        let resolved = enumerator.as_ref().map(|e| probe.resolve(e));
        let reason = LiveCaptureBackend::judgement(judged, resolved.as_ref(), baseline.as_ref());
        if let Some(reason) = reason {
            tracing::info!("device change reported: {reason:?}");
            sink.report_device_change(reason);
        } else if judged != Judged::Recheck {
            tracing::info!("ignored device notification");
        }
    });
    drop(registration);
    drop(enumerator);
    drop(apartment);
}

/// Stops and joins the stream threads that answered, and returns what
/// each one's clocks read at its stop; the others are left to finish on
/// their own and never write to the sink.
fn tear_down(stop: &AtomicBool, streams: Vec<Launched>) -> Vec<(StreamSource, Ends)> {
    stop.store(true, Ordering::Release);
    let mut ends = Vec::new();
    for mut launched in streams {
        launched.body.take();
        if let Some(thread) = launched.thread.take()
            && launched.answered
            && let Ok(end) = thread.join()
        {
            ends.push((launched.source, end));
        }
    }
    ends
}

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
    // The threads' start lines are as loud as this one's.
    let starts_quietly = start_log::is_quiet();
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
                    starts_quietly,
                )
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
    // Only this answer counts: a thread that missed the deadline may sit
    // in a COM call.
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
/// start, the follower before the master (the sink's only producer), so a
/// start that fails has written nothing.
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

/// Starts the watcher thread and waits for it to register. A slow answer
/// is logged, not fatal: such a watcher may sit in a COM call, which
/// `stop()` must not wait on.
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
    /// Opens the streams the lanes need on their own threads, starts them,
    /// then the watcher. Returns the 48 kHz stream with the latencies the
    /// far-end delay is built from ([`far_end_latencies`]).
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
                Arc::new(
                    FollowerLane::for_streams_filling(i.sizes.period_frames, master_buffer)
                        .with_reprime_hold(i.sizes.buffer_frames + i.sizes.period_frames),
                )
            })
        });
        let streams = start_streams(&stop, streams, &plan, follower.as_ref(), &sink, deadline)?;
        if let Some(kind) = system.as_ref().and_then(|s| s.loopback) {
            start_log!(info, "system lane on {kind:?} loopback");
        }

        let probe = DeviceProbe {
            needs_mic: mic.is_some(),
            needs_system: system.is_some(),
            input_device_uid: input_device_uid.map(str::to_owned),
            is_fallback: mic
                .as_ref()
                .and_then(|m| m.input.as_ref())
                .is_some_and(|input| input.is_fallback),
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
        let follower_lane = PacketRouter::follower_lane(&plan.layout.sources);
        *active = Some(Active {
            stop,
            streams,
            follower: follower.zip(follower_lane),
            master: plan.master,
            first_start: sink.first_start(),
            sink,
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

    /// Stops the capture threads (no frame arrives after it returns), hands
    /// on what they missed, then stops the watcher.
    fn stop(&self) {
        let Some(mut active) = self.lock().take() else {
            return;
        };
        let ends = tear_down(&active.stop, std::mem::take(&mut active.streams));
        let end = |master: bool| {
            ends.iter()
                .find(|(source, _)| (*source == active.master) == master)
                .map_or_else(Ends::default, |(_, ends)| *ends)
        };
        count_at_stop(
            &active.sink,
            active
                .follower
                .as_ref()
                .map(|(lane, index)| (lane.as_ref(), *index)),
            end(true).after_last.unwrap_or_default(),
            end(false).after_last,
        );
        if active.first_start {
            active.sink.count_before_first(
                end(true).before_first,
                active
                    .follower
                    .as_ref()
                    .map(|(_, index)| (*index, end(false).before_first)),
            );
        }
        if let Some((lane, _)) = &active.follower {
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

    /// The microphone, the master whenever a lane needs it, delivers
    /// continuously; the system stream alone does not. The recorder's
    /// `microphone_is_the_master` (steno-services) states the same rule; a
    /// change here goes there too.
    fn delivers_continuously(&self, lanes: &[AudioLane]) -> bool {
        SplitStreamPlan::new(lanes).is_ok_and(|plan| plan.master == StreamSource::Microphone)
    }
}

impl Drop for LiveCaptureBackend {
    /// A backend dropped without `stop()` still joins its threads.
    fn drop(&mut self) {
        self.stop();
    }
}

/// The WASAPI endpoints for the input picker, under the macOS
/// `AudioDevices` names. Every call joins COM on the calling thread for
/// its duration.
pub struct AudioDevices;

impl AudioDevices {
    /// Every active endpoint, capture endpoints first. `id` is the index in
    /// that list (WASAPI has no numeric ids); `uid` is the endpoint id
    /// `Settings` stores. The `eConsole` render default is both the default
    /// output and the default system output.
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

    /// The session watches a capture whose master is the microphone; a
    /// capture of the system lane alone is not watched.
    #[test]
    fn only_a_capture_with_a_microphone_master_delivers_continuously() {
        let backend = LiveCaptureBackend::new();
        for (lanes, continuously) in [
            (&[AudioLane::Mic, AudioLane::System][..], true),
            (&[AudioLane::Mixed][..], true),
            (&[AudioLane::Mic][..], true),
            (&[AudioLane::System][..], false),
            (&[][..], false),
        ] {
            assert_eq!(
                backend.delivers_continuously(lanes),
                continuously,
                "{lanes:?}"
            );
        }
    }

    /// A watcher that is judging a burst when `stop()` comes is joined: its
    /// report lands before `stop()` returns, never after.
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
                watcher.watch(&ready, None, |_| {
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

    /// A capture on the fallback judges the endpoints again with no
    /// notification; one on the endpoint it asked for waits for one.
    #[test]
    fn only_a_capture_on_the_fallback_rechecks_without_a_notification() {
        for recheck in [Some(Duration::from_millis(50)), None] {
            let watcher = Arc::new(Watcher::default());
            let (ready, answered) = sync_channel(1);
            let (judged, judgements) = channel();
            let thread = {
                let watcher = Arc::clone(&watcher);
                std::thread::spawn(move || {
                    watcher.watch(&ready, recheck, |judgement| {
                        let _ = judged.send(judgement);
                    });
                })
            };
            answered
                .recv_timeout(WAIT)
                .expect("the watcher reached its loop");
            if recheck.is_some() {
                assert_eq!(judgements.recv_timeout(WAIT), Ok(Judged::Recheck));
                assert_eq!(judgements.recv_timeout(WAIT), Ok(Judged::Recheck));
            } else {
                assert!(judgements.recv_timeout(Duration::from_millis(300)).is_err());
                watcher.note();
                assert_eq!(
                    judgements.recv_timeout(WAIT),
                    Ok(Judged::Notification { failed: None })
                );
            }
            watcher.stop_and_join(thread);
        }
    }

    /// A watcher still in its start-up calls when `stop()` comes is not
    /// waited for, and exits without judging.
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
                watcher.watch(&ready, None, |_| judged = true);
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

    /// Only a capture on the fallback is re-checked.
    #[test]
    fn the_recheck_runs_on_the_fallback_alone() {
        assert_eq!(
            LiveCaptureBackend::recheck_for(true),
            Some(LiveCaptureBackend::FALLBACK_RECHECK)
        );
        assert_eq!(LiveCaptureBackend::recheck_for(false), None);
    }

    /// A notification reports the first difference, else the failed
    /// stream's device gone; a re-check only another microphone.
    #[test]
    fn a_recheck_reports_another_microphone_alone() {
        let baseline = DeviceSnapshot {
            output_uid: Some("speakers".into()),
            default_output_uid: None,
            input_uid: Some("built-in".into()),
            output_alive: true,
            input_alive: true,
            sample_rate: SAMPLE_RATE,
        };
        let back = DeviceSnapshot {
            input_uid: Some("usb-microphone".into()),
            ..baseline.clone()
        };
        let misread = DeviceSnapshot {
            output_uid: None,
            ..baseline.clone()
        };
        let unresolved = DeviceSnapshot {
            input_uid: None,
            ..baseline.clone()
        };
        let quiet = Judged::Notification { failed: None };
        let failed = Judged::Notification {
            failed: Some(StreamSource::Microphone),
        };
        let table = [
            (quiet, Some(&baseline), None),
            (
                quiet,
                Some(&back),
                Some(DeviceChangeReason::DefaultInputChanged),
            ),
            (
                quiet,
                Some(&misread),
                Some(DeviceChangeReason::DefaultOutputChanged),
            ),
            (
                failed,
                Some(&baseline),
                Some(DeviceChangeReason::InputDeviceGone),
            ),
            (failed, None, Some(DeviceChangeReason::InputDeviceGone)),
            (quiet, None, None),
            (Judged::Recheck, Some(&baseline), None),
            (
                Judged::Recheck,
                Some(&back),
                Some(DeviceChangeReason::DefaultInputChanged),
            ),
            (Judged::Recheck, Some(&misread), None),
            (Judged::Recheck, Some(&unresolved), None),
            (Judged::Recheck, None, None),
        ];
        for (judged, resolved, reported) in table {
            assert_eq!(
                LiveCaptureBackend::judgement(judged, resolved, Some(&baseline)),
                reported,
                "{judged:?} of {resolved:?}"
            );
        }
    }
}
