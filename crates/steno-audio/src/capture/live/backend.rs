//! The real backend on macOS: process tap + private aggregate device + one
//! IOProc, with device-change listeners, coalescing and the report that
//! makes the session rebuild.
//! Swift: `Sources/StenoAudio/Capture/LiveCaptureBackend.swift`.
//!
//! `System` comes from the tap, `Mic` and `Mixed` from the first channel of
//! the chosen input device, which the aggregate resamples to the output
//! device's clock. That clock is 48 kHz where the output device accepts it;
//! a Bluetooth headset in the hands-free profile keeps 24, 16 or 8 kHz,
//! and the stream then reports that rate for the processing thread to
//! convert.
//! A chosen device that is not connected records the default input
//! instead (`chosen_or_default_input`), and the device list is watched so
//! the rebuild returns to it once it is back; Swift fails the start with
//! `InputDeviceUnavailable` there (a deliberate parity change: no recording
//! is lost to a missing microphone).
//!
//! Device notifications (a default device moving, a sub-device dying, the
//! aggregate changing rate) arrive on the HAL's notification thread and
//! are coalesced for [`LiveCaptureBackend::COALESCE_DELAY`] on a watcher
//! thread, then the devices are resolved again and compared with what the
//! capture started on ([`DeviceSnapshot::difference`]); a capture on the
//! fallback also resolves them every
//! [`LiveCaptureBackend::FALLBACK_RECHECK`] without a notification and
//! looks only for another microphone
//! ([`DeviceSnapshot::input_difference`]). A burst that holds
//! `kAudioHardwarePropertyServiceRestarted` (`coreaudiod` restarted, and
//! the aggregate with it) is reported as
//! [`DeviceChangeReason::AudioServiceRestarted`] whatever the devices read
//! (Rust only: Swift does not listen for it). Nothing changed means the burst
//! is logged and ignored; otherwise the sink gets one
//! [`DeviceChangeReason`] and the session rebuilds by calling `stop()` and
//! `start` again. Nothing here runs on the IO thread except `io_proc`,
//! which only calls [`deliver`] and marks the first callback's host time
//! ([`FirstCallback`]), and `silent_io_proc` (below), which only zeroes
//! its output; `stop()` logs that callback's offset from the start
//! at `info`, so a call recording shows whether the IOProc ran at once.
//!
//! Teardown order: watcher thread, `AudioDeviceStop`,
//! `AudioDeviceDestroyIOProcID`, the callback context, listeners,
//! `AudioHardwareDestroyAggregateDevice`, `AudioHardwareDestroyProcessTap`,
//! and last the silent output IOProc (below), which starts before the
//! aggregate's IOProc. A `start` that fails part way drops what it built in
//! the same order.
//!
//! Call mode is its own output client (A10 of
//! `.plans/2026-10-07-stable-promotion.md`). An aggregate with a process
//! tap runs only while a process the tap includes drives the output: with
//! nothing playing, `AudioDeviceStart` succeeds and the IOProc is never
//! called, so neither the tap nor the microphone delivered until some app
//! played (0 callbacks on a Mac over SSH; a silent `afplay` already
//! playing gave the first at 67 ms). So the tap includes Steno's own
//! process, and a call capture starts a silent IOProc of its own
//! (`silent_io_proc`) on the aggregate's clock master, the system output,
//! read from the aggregate once it is built, and before the aggregate's
//! IOProc: the first callback then comes within 100 ms of `start`
//! returning. On a Mac whose default output was another device than the
//! system output, the tap aggregate ran with the silent IOProc on the
//! clock master alone, the default output never started. The silent
//! IOProc's input streams are off. It is rebuilt with the capture, so a
//! change of the system output moves it to the new clock master; one that
//! does not start is logged, and the capture then records only while
//! another app plays, as before A10. No in-app playback while recording,
//! enforced by [`Playback`](crate::playback::Playback): Steno's own output
//! would land in the system lane, and the capture session holds that gate
//! while it records. In-person mode has no tap and needs no output client.

use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use objc2_core_audio::{
    AudioObjectPropertySelector, kAudioDevicePropertyDeviceIsAlive,
    kAudioDevicePropertyNominalSampleRate, kAudioHardwarePropertyDefaultInputDevice,
    kAudioHardwarePropertyDefaultOutputDevice, kAudioHardwarePropertyDefaultSystemOutputDevice,
    kAudioHardwarePropertyDevices, kAudioHardwarePropertyServiceRestarted,
    kAudioObjectPropertyScopeGlobal, kAudioObjectPropertyScopeInput,
    kAudioObjectPropertyScopeOutput,
};
use objc2_core_audio_types::{AudioBufferList, AudioTimeStamp};
use steno_core::AudioLane;

use super::hal::{
    self, AggregateDevice, Id, IoProc, OSStatus, ProcessTap, PropertyListener, SYSTEM,
};
use super::{AudioDeviceInfo, AudioDevices, chosen_or_default};
use crate::SAMPLE_RATE;
use crate::capture::{
    CaptureBackend, CaptureError, CaptureInput, CaptureStream, DeviceChangeReason, DeviceSnapshot,
    LaneSource, NominalSampleRate, StreamLayout,
};
use crate::realtime::{
    BufferView, FirstCallback, LaneFrameSink, MAX_BUFFERS, RateConverter, deliver,
    first_callback_line, silence_output,
};

/// Shared with the IOProc through a raw pointer; boxed so it never moves,
/// alive until the `IoProc` is dropped.
struct CallbackContext {
    sink: Arc<LaneFrameSink>,
    sources: Vec<LaneSource>,
    first_callback: FirstCallback,
}

/// The IOProc. Builds a stack array of [`BufferView`]s from the HAL's
/// buffer list and hands it to [`deliver`], after marking the first
/// callback's host time: no allocation, no lock, no syscall; behind
/// [`hal::abort_on_panic`].
unsafe extern "C-unwind" fn io_proc(
    _device: Id,
    now: NonNull<AudioTimeStamp>,
    input: NonNull<AudioBufferList>,
    _input_time: NonNull<AudioTimeStamp>,
    _output: NonNull<AudioBufferList>,
    _output_time: NonNull<AudioTimeStamp>,
    client: *mut c_void,
) -> OSStatus {
    hal::abort_on_panic(|| {
        // SAFETY: `client` is the boxed `CallbackContext` registered in
        // `start`, alive until the IOProc is destroyed (which happens before
        // the box is dropped); `input` and `now` are the HAL's, valid for
        // the call.
        unsafe {
            let ctx = &*client.cast::<CallbackContext>();
            ctx.first_callback.mark(now.as_ref().mHostTime);
            let list = input.as_ref();
            let count = (list.mNumberBuffers as usize).min(MAX_BUFFERS);
            let mut views = [BufferView {
                channels: 0,
                data: None,
                byte_size: 0,
            }; MAX_BUFFERS];
            let buffers = list.mBuffers.as_ptr();
            for (index, view) in views.iter_mut().enumerate().take(count) {
                let buffer = &*buffers.add(index);
                *view = BufferView {
                    channels: buffer.mNumberChannels as usize,
                    data: (!buffer.mData.is_null() && buffer.mDataByteSize > 0)
                        .then_some(buffer.mData.cast_const().cast::<f32>()),
                    byte_size: buffer.mDataByteSize as usize,
                };
            }
            deliver(&views[..count], &ctx.sources, &ctx.sink);
        }
    });
    0
}

/// The call capture's silent output IOProc on the aggregate's clock
/// master: writes zeros over its output buffers ([`silence_output`]; the
/// HAL has zeroed them already) and reads nothing, so the capture is a
/// client of the output that the tap includes. No allocation, no lock, no
/// syscall; behind [`hal::abort_on_panic`]. See the module doc.
unsafe extern "C-unwind" fn silent_io_proc(
    _device: Id,
    _now: NonNull<AudioTimeStamp>,
    _input: NonNull<AudioBufferList>,
    _input_time: NonNull<AudioTimeStamp>,
    output: NonNull<AudioBufferList>,
    _output_time: NonNull<AudioTimeStamp>,
    _client: *mut c_void,
) -> OSStatus {
    hal::abort_on_panic(|| {
        // SAFETY: `output` is the HAL's output list for this cycle, its
        // buffers writable for their `mDataByteSize` for the call.
        unsafe { silence_output(output) };
    });
    0
}

/// Starts [`silent_io_proc`], output only, on the aggregate's clock master
/// as the aggregate reports it, read once; on `requested`, the device the
/// aggregate was built with as its main sub-device, when that read fails.
/// The device it runs on is logged at `info` (its name read for the line
/// alone), and a failure to start at `warn`: the capture then goes on
/// without it, as it did before A10.
fn start_silent_output(aggregate: &AggregateDevice, requested: Id) -> Option<IoProc> {
    let device = aggregate.main_sub_device().unwrap_or_else(|error| {
        tracing::warn!("the aggregate's clock master did not read ({error}); using {requested}");
        requested
    });
    // SAFETY: `silent_io_proc` reads no client data, so a null client
    // stays valid for as long as the IOProc runs.
    match unsafe { IoProc::start_output_only(device, Some(silent_io_proc), std::ptr::null_mut()) } {
        Ok(io) => {
            tracing::info!(
                "the silent output runs on {} (audio device {device}), the aggregate's clock master",
                hal::name(device)
            );
            Some(io)
        }
        Err(error) => {
            tracing::warn!(
                "the silent output on audio device {device} did not start ({error}); \
                 the call capture runs only while another app plays"
            );
            None
        }
    }
}

/// The input a capture asked for `uid` records now ([`chosen_or_default`]):
/// the chosen device while it is connected and has input channels, else
/// the default input, with `true` beside it. `None` when no input resolves
/// at all.
fn chosen_or_default_input(uid: Option<&str>) -> Option<(AudioDeviceInfo, bool)> {
    chosen_or_default(
        uid,
        |uid| AudioDevices::device(uid).ok().flatten(),
        || AudioDevices::default_input().ok(),
        AudioDeviceInfo::is_input,
    )
}

/// The HAL reads behind one [`DeviceSnapshot`], fixed at `start` so every
/// look after a notification asks about the objects the capture began on.
#[derive(Debug, Clone)]
struct DeviceProbe {
    output_id: Id,
    mic_id: Option<Id>,
    input_device_uid: Option<String>,
    aggregate_id: Id,
}

impl DeviceProbe {
    /// The devices as they are now: the defaults resolved again (the input
    /// as [`chosen_or_default_input`] picks it, so a chosen device coming
    /// back reads as a change), the started devices' `DeviceIsAlive`, the
    /// aggregate's rate (0 once it is gone).
    fn resolve(&self) -> DeviceSnapshot {
        let output = AudioDevices::default_system_output().ok();
        let input = self
            .mic_id
            .and_then(|_| chosen_or_default_input(self.input_device_uid.as_deref()))
            .map(|(device, _)| device);
        DeviceSnapshot {
            output_uid: output.map(|d| d.uid),
            default_output_uid: AudioDevices::default_output_uid(),
            input_uid: input.map(|d| d.uid),
            output_alive: hal::is_alive(self.output_id),
            input_alive: self.mic_id.is_some_and(hal::is_alive),
            sample_rate: hal::read_f64(self.aggregate_id, kAudioDevicePropertyNominalSampleRate)
                .unwrap_or(0.0),
        }
    }
}

/// What the listeners share with the watcher thread.
#[derive(Default)]
struct WatchState {
    /// The last notification's selector (a service restart's kept over
    /// later ones) and arrival, `None` once judged.
    pending: Option<(AudioObjectPropertySelector, Instant)>,
    stop: bool,
}

/// What the watcher judges: a settled burst of notifications, or the
/// re-check while the capture records the fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Judged {
    Notification(AudioObjectPropertySelector),
    Recheck,
}

struct Watcher {
    state: Mutex<WatchState>,
    condvar: Condvar,
}

impl Watcher {
    fn lock(&self) -> std::sync::MutexGuard<'_, WatchState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// One notification for the watch loop: the latest of its burst,
    /// judged once the burst settles. A service restart outranks the
    /// notifications that follow it in the same burst.
    fn notify(&self, selector: AudioObjectPropertySelector) {
        let mut state = self.lock();
        let kept = match state.pending {
            Some((pending, _)) if pending == kAudioHardwarePropertyServiceRestarted => pending,
            _ => selector,
        };
        state.pending = Some((kept, Instant::now()));
        self.condvar.notify_all();
    }
}

/// What one started capture holds, in teardown order: the fields drop in
/// the order `stop()` drops them explicitly.
struct Active {
    _io_proc: IoProc,
    _context: Box<CallbackContext>,
    _listeners: Vec<PropertyListener>,
    _aggregate: AggregateDevice,
    _tap: Option<ProcessTap>,
    /// Started before the aggregate's IOProc, stopped after the tap is
    /// destroyed; `None` in-person, and when it did not start.
    _silent_output: Option<IoProc>,
    watcher: Arc<Watcher>,
    watcher_thread: Option<JoinHandle<()>>,
    /// The host time just before the device started, and the same moment
    /// on the monotonic clock: where the first callback is measured from.
    started: (u64, Instant),
}

/// The macOS capture backend; see the module doc.
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
    /// resolved once. A Bluetooth profile switch fires several within it.
    pub const COALESCE_DELAY: Duration = Duration::from_millis(500);

    /// How often a capture that records the fallback resolves the devices
    /// without a notification. A chosen device can come back with no
    /// notification of its own: one that read no input channels for a
    /// moment while the device list changed, one that settled after its
    /// `Devices` notification, or one that came back before the
    /// listeners were registered. Rust only: Swift has no fallback.
    pub const FALLBACK_RECHECK: Duration = Duration::from_secs(5);

    #[must_use]
    pub fn new() -> Self {
        Self {
            active: Mutex::new(None),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<Active>> {
        self.active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The watcher thread: waits for a notification, lets the burst settle
    /// for `COALESCE_DELAY` after the last one, then hands it to `judge`,
    /// outside the lock; with `recheck`, also each time that long passes
    /// without a notification. It belongs to one `start`: `stop()` raises
    /// `WatchState::stop` and joins it before the backend can start again,
    /// so a report never reaches a later capture (Swift compared a
    /// generation counter instead; the join makes that unnecessary here).
    fn watch(watcher: &Watcher, recheck: Option<Duration>, mut judge: impl FnMut(Judged)) {
        let mut state = watcher.lock();
        loop {
            if state.stop {
                return;
            }
            let Some((selector, last)) = state.pending else {
                let Some(recheck) = recheck else {
                    state = watcher
                        .condvar
                        .wait(state)
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    continue;
                };
                let (next, waited) = watcher
                    .condvar
                    .wait_timeout(state, recheck)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                state = next;
                if waited.timed_out() && state.pending.is_none() && !state.stop {
                    drop(state);
                    judge(Judged::Recheck);
                    state = watcher.lock();
                }
                continue;
            };
            let due = last + Self::COALESCE_DELAY;
            let now = Instant::now();
            if now < due {
                state = watcher
                    .condvar
                    .wait_timeout(state, due - now)
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .0;
                continue;
            }
            state.pending = None;
            drop(state);
            judge(Judged::Notification(selector));
            state = watcher.lock();
        }
    }

    /// The notification the aggregate's rate, read again once the
    /// listeners are in place, stands for: a rate notification when it is
    /// no longer the rate the capture started at, none otherwise. A device
    /// that relocks after [`NominalSampleRate::settle`] gave up, but before
    /// the rate listener was registered, fires no notification of its own,
    /// and the stream would keep the old rate's label for the whole
    /// recording.
    fn late_rate_notification(started: f64, now: f64) -> Option<AudioObjectPropertySelector> {
        (now != started).then_some(kAudioDevicePropertyNominalSampleRate)
    }

    /// The same for the system output, read again once the listeners are
    /// in place (`now`, `None` when none resolves): a system output
    /// notification when it is not the one the capture was built on. Its
    /// listener reports only a switch after it was added, so a switch while
    /// the aggregate was built would leave the capture and its silent
    /// output on the old clock master.
    fn late_output_notification(
        started: &str,
        now: Option<&str>,
    ) -> Option<AudioObjectPropertySelector> {
        (now != Some(started)).then_some(kAudioHardwarePropertyDefaultSystemOutputDevice)
    }

    /// The microphone's latency of `frames` in the stream's frames: as it
    /// is when the microphone is the clock master, rescaled from its own
    /// `mic_rate` to `stream_rate` otherwise (rounded down; an unreadable
    /// rate leaves it as it is).
    fn mic_latency_frames(
        frames: usize,
        on_clock_master: bool,
        mic_rate: f64,
        stream_rate: f64,
    ) -> usize {
        if on_clock_master {
            frames
        } else {
            CaptureStream::rescaled(frames, mic_rate, stream_rate)
        }
    }

    /// The watcher's re-check interval: [`Self::FALLBACK_RECHECK`] for a
    /// capture on the fallback, none otherwise.
    fn recheck_for(is_fallback: bool) -> Option<Duration> {
        is_fallback.then_some(Self::FALLBACK_RECHECK)
    }

    /// What a judgement reports of the devices `resolved` now: after a
    /// notification their first difference from `baseline`, on a re-check
    /// only another microphone ([`DeviceSnapshot::input_difference`]).
    /// A restart of the audio service is a change whatever they read: the
    /// aggregate is gone, and the devices may resolve as before.
    fn judgement(
        judged: Judged,
        resolved: &DeviceSnapshot,
        baseline: &DeviceSnapshot,
    ) -> Option<DeviceChangeReason> {
        match judged {
            Judged::Notification(selector)
                if selector == kAudioHardwarePropertyServiceRestarted =>
            {
                Some(DeviceChangeReason::AudioServiceRestarted)
            }
            Judged::Notification(_) => resolved.difference(baseline),
            Judged::Recheck => resolved.input_difference(baseline),
        }
    }

    /// Resolves the devices (the HAL reads, outside the watcher's lock)
    /// and reports what [`Self::judgement`] finds.
    fn judge(judged: Judged, probe: &DeviceProbe, baseline: &DeviceSnapshot, sink: &LaneFrameSink) {
        let what = match judged {
            Judged::Notification(selector) => {
                format!("device notification {}", hal::selector_name(selector))
            }
            Judged::Recheck => "fallback re-check".to_owned(),
        };
        match Self::judgement(judged, &probe.resolve(), baseline) {
            // Every `FALLBACK_RECHECK`, so not logged.
            None if judged == Judged::Recheck => {}
            None => tracing::info!("ignored {what}"),
            Some(reason) => {
                tracing::info!("{what} reported {reason:?}");
                sink.report_device_change(reason);
            }
        }
    }
}

impl CaptureBackend for LiveCaptureBackend {
    /// Returns the stream it opened: the confirmed rate, both device
    /// latencies for the far-end delay, and the resolved [`StreamLayout`].
    /// One long function on purpose: it is the Swift `start` step for
    /// step, and every early return tears down what was created by drop.
    #[allow(clippy::too_many_lines)]
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

        let needs_mic = lanes.contains(&AudioLane::Mic) || lanes.contains(&AudioLane::Mixed);
        let needs_tap = lanes.contains(&AudioLane::System);

        let output = AudioDevices::default_system_output()
            .map_err(|_| CaptureError::OutputDeviceUnavailable)?;
        let (mic, is_fallback) = if needs_mic {
            let (device, is_fallback) = chosen_or_default_input(input_device_uid)
                .ok_or(CaptureError::InputDeviceUnavailable)?;
            if is_fallback {
                tracing::warn!(
                    "the input device {} is not connected; recording from the default input {}",
                    input_device_uid.unwrap_or_default(),
                    device.uid
                );
            }
            (Some(device), is_fallback)
        } else {
            (None, false)
        };

        // Declared before the tap and the aggregate, so a `start` that fails
        // once it runs drops it after them, in the teardown order.
        let silent_output: Option<IoProc>;
        let tap = if needs_tap {
            Some(ProcessTap::new("Steno system lane")?)
        } else {
            None
        };

        // Sub-devices in aggregate order: the output device (clock master),
        // then the microphone unless it is the same physical device.
        let mut sub_device_uids = vec![output.uid.clone()];
        let mut sub_device_counts = vec![hal::channel_counts(
            output.id,
            kAudioObjectPropertyScopeInput,
        )];
        let mut mic_sub_device = None;
        if let Some(mic) = &mic {
            if mic.uid == output.uid {
                mic_sub_device = Some(0);
            } else {
                sub_device_uids.push(mic.uid.clone());
                sub_device_counts.push(hal::channel_counts(mic.id, kAudioObjectPropertyScopeInput));
                mic_sub_device = Some(1);
            }
        }
        let tap_uids: Vec<String> = tap.iter().map(|t| t.uid.clone()).collect();
        let aggregate =
            AggregateDevice::new("Steno capture", &output.uid, &sub_device_uids, &tap_uids)?;

        // The aggregate inherits the clock master's rate. Ask for 48 kHz,
        // then read it back: the HAL applies the change asynchronously, and
        // a device that cannot run at 48 kHz (a headset in the hands-free
        // profile) keeps its own. The stream then reports that rate, which
        // the processing thread converts; labelling it 48 kHz would leave a
        // pitch-shifted master. A rate that settles only after this gave up
        // is caught once the listeners are in place (below).
        if aggregate.nominal_sample_rate() != SAMPLE_RATE {
            let _ = aggregate.set_nominal_sample_rate(SAMPLE_RATE);
        }
        let sample_rate = NominalSampleRate::settle(
            SAMPLE_RATE,
            NominalSampleRate::ATTEMPTS,
            || aggregate.nominal_sample_rate(),
            || std::thread::sleep(NominalSampleRate::INTERVAL),
        );
        if !RateConverter::supports(sample_rate) {
            // Whole hertz.
            return Err(CaptureError::UnsupportedSampleRate {
                actual: sample_rate as u32,
            });
        }

        let layout = StreamLayout::resolve(
            lanes,
            &aggregate.input_channel_counts(),
            &sub_device_counts,
            &tap.as_ref()
                .map(ProcessTap::buffer_channel_counts)
                .unwrap_or_default(),
            mic_sub_device,
        )?;

        // The call capture's own output client, started once the aggregate
        // is built (building it over a running output device took about
        // 300 ms longer on a Mac) and before the aggregate's IOProc; see the
        // module doc.
        silent_output = if needs_tap {
            start_silent_output(&aggregate, output.id)
        } else {
            None
        };

        let context = Box::new(CallbackContext {
            sink: Arc::clone(&sink),
            sources: layout.sources.clone(),
            first_callback: FirstCallback::new(),
        });
        let context_ptr: *const CallbackContext = &raw const *context;
        let started = (hal::host_time_now(), Instant::now());
        // SAFETY: `context` is boxed and stored in `Active` beside the
        // `IoProc`, whose drop (stop + destroy) runs before the box is
        // freed: `Active` declares the IoProc first and `stop()` drops it
        // first.
        let io = unsafe {
            IoProc::start(
                aggregate.id,
                Some(io_proc),
                context_ptr.cast_mut().cast::<c_void>(),
            )?
        };

        // Device changes: the default devices moving, a sub-device dying,
        // the aggregate changing rate. The tap mirrors the default output
        // device (where the call plays), the clock follows the system
        // output device (alerts); a change of either moves the far-end
        // alignment, so both are watched. The listener carries no value,
        // so every notification is judged by resolving the devices again
        // after the burst settles. A restart of `coreaudiod` destroys the
        // aggregate and the tap, which no device property tells.
        let mut selectors: Vec<(Id, AudioObjectPropertySelector)> = vec![
            (SYSTEM, kAudioHardwarePropertyServiceRestarted),
            (SYSTEM, kAudioHardwarePropertyDefaultSystemOutputDevice),
            (SYSTEM, kAudioHardwarePropertyDefaultOutputDevice),
            (output.id, kAudioDevicePropertyDeviceIsAlive),
            (aggregate.id, kAudioDevicePropertyNominalSampleRate),
        ];
        // The default input matters while it is what the microphone lane
        // records; the device list while a chosen device could come back
        // (or go: its own `DeviceIsAlive` covers that).
        if let Some(mic) = &mic {
            selectors.push((mic.id, kAudioDevicePropertyDeviceIsAlive));
            if input_device_uid.is_none() || is_fallback {
                selectors.push((SYSTEM, kAudioHardwarePropertyDefaultInputDevice));
            }
            if input_device_uid.is_some() {
                selectors.push((SYSTEM, kAudioHardwarePropertyDevices));
            }
        }
        let watcher = Arc::new(Watcher {
            state: Mutex::new(WatchState::default()),
            condvar: Condvar::new(),
        });
        let listeners: Vec<PropertyListener> = selectors
            .into_iter()
            .filter_map(|(object, selector)| {
                let watcher = Arc::clone(&watcher);
                PropertyListener::add(
                    object,
                    selector,
                    kAudioObjectPropertyScopeGlobal,
                    Box::new(move |selector| watcher.notify(selector)),
                )
                // Without the listener that change goes unnoticed until the
                // session's stall watchdog sees the capture stop.
                .inspect_err(|_| {
                    tracing::warn!(
                        "could not listen for {}; that change goes unnoticed",
                        hal::selector_name(selector)
                    );
                })
                .ok()
            })
            .collect();
        // Any change from here on notifies; one before its listener was in
        // place (the rate, or the system output the aggregate and its silent
        // output were built on) is judged as if it had.
        let late = [
            Self::late_rate_notification(sample_rate, aggregate.nominal_sample_rate()),
            Self::late_output_notification(
                &output.uid,
                AudioDevices::default_system_output_uid().as_deref(),
            ),
        ];
        for selector in late.into_iter().flatten() {
            watcher.notify(selector);
        }

        // The far-end delay: the microphone's input path plus the
        // loudspeaker's output path, each latency plus safety offset, read
        // on the devices themselves rather than the aggregate, so each is in
        // its own device's frames. The output device is the clock master,
        // at `sample_rate`; a microphone on another device keeps its own
        // rate (a 48 kHz built-in microphone beside a headset at 24 kHz) and
        // is rescaled to the stream's.
        let input_latency = mic.as_ref().map_or(0, |m| {
            Self::mic_latency_frames(
                hal::latency_frames(m.id, kAudioObjectPropertyScopeInput),
                mic_sub_device == Some(0),
                hal::nominal_sample_rate(m.id),
                sample_rate,
            )
        });
        let output_latency = hal::latency_frames(output.id, kAudioObjectPropertyScopeOutput);

        let baseline = DeviceSnapshot {
            output_uid: Some(output.uid.clone()),
            default_output_uid: AudioDevices::default_output_uid(),
            input_uid: mic.as_ref().map(|m| m.uid.clone()),
            output_alive: true,
            input_alive: mic.is_some(),
            sample_rate,
        };
        let probe = DeviceProbe {
            output_id: output.id,
            mic_id: mic.as_ref().map(|m| m.id),
            input_device_uid: input_device_uid.map(str::to_owned),
            aggregate_id: aggregate.id,
        };
        let watcher_thread = {
            let watcher = Arc::clone(&watcher);
            let sink = Arc::clone(&sink);
            std::thread::Builder::new()
                .name("steno-devices".into())
                .spawn(move || {
                    let recheck = LiveCaptureBackend::recheck_for(is_fallback);
                    LiveCaptureBackend::watch(&watcher, recheck, |judged| {
                        LiveCaptureBackend::judge(judged, &probe, &baseline, &sink);
                    });
                })
                .map_err(|e| CaptureError::BackendFailed(format!("device watcher: {e}")))?
        };

        *active = Some(Active {
            _silent_output: silent_output,
            _tap: tap,
            _aggregate: aggregate,
            _io_proc: io,
            _context: context,
            _listeners: listeners,
            watcher,
            watcher_thread: Some(watcher_thread),
            started,
        });
        Ok(CaptureStream {
            sample_rate,
            input_latency_frames: input_latency,
            output_latency_frames: output_latency,
            layout: Some(layout),
            input: mic.map(|mic| CaptureInput {
                uid: mic.uid,
                name: Some(mic.name).filter(|name| !name.is_empty()),
                is_fallback,
            }),
        })
    }

    fn stop(&self) {
        let Some(mut active) = self.lock().take() else {
            return;
        };
        {
            let mut state = active.watcher.lock();
            state.stop = true;
            active.watcher.condvar.notify_all();
        }
        if let Some(thread) = active.watcher_thread.take() {
            let _ = thread.join();
        }
        // The teardown order: IOProc (stop, destroy) before the context it
        // reads, then listeners, aggregate, tap, silent output.
        let Active {
            _io_proc: io_proc,
            _context: context,
            _listeners: listeners,
            _aggregate: aggregate,
            _tap: tap,
            _silent_output: silent_output,
            started: (host_started, started_at),
            ..
        } = active;
        drop(io_proc);
        // The IOProc is stopped: nothing writes the mark any more.
        tracing::info!(
            "{}",
            first_callback_line(
                context
                    .first_callback
                    .offset(host_started, hal::host_time_to_nanos),
                started_at.elapsed(),
                tap.is_some(),
            )
        );
        drop(context);
        drop(listeners);
        drop(aggregate);
        drop(tap);
        drop(silent_output);
    }

    /// The IOProc runs on the aggregate's clock, silence included (in call
    /// mode only with the capture permission; see the module doc).
    fn delivers_continuously(&self, _lanes: &[AudioLane]) -> bool {
        true
    }
}

impl Drop for LiveCaptureBackend {
    /// A backend dropped without `stop()` still tears the HAL objects down
    /// in order instead of leaving it to field destruction order.
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::channel;

    use super::*;

    const WAIT: Duration = Duration::from_secs(5);

    /// Runs the watch loop on its own thread; every judgement goes to the
    /// returned receiver.
    fn watching(
        recheck: Option<Duration>,
    ) -> (
        Arc<Watcher>,
        JoinHandle<()>,
        std::sync::mpsc::Receiver<Judged>,
    ) {
        let watcher = Arc::new(Watcher {
            state: Mutex::new(WatchState::default()),
            condvar: Condvar::new(),
        });
        let (judged, judgements) = channel();
        let thread = {
            let watcher = Arc::clone(&watcher);
            std::thread::spawn(move || {
                LiveCaptureBackend::watch(&watcher, recheck, |judgement| {
                    let _ = judged.send(judgement);
                });
            })
        };
        (watcher, thread, judgements)
    }

    fn stop(watcher: &Watcher, thread: JoinHandle<()>) {
        watcher.lock().stop = true;
        watcher.condvar.notify_all();
        thread.join().unwrap();
    }

    /// A capture on the fallback judges the devices again with no
    /// notification, so a chosen device that came back unannounced is
    /// found; one on the device it asked for waits for a notification.
    #[test]
    fn only_a_capture_on_the_fallback_rechecks_without_a_notification() {
        let (watcher, thread, judgements) = watching(Some(Duration::from_millis(50)));
        assert_eq!(judgements.recv_timeout(WAIT), Ok(Judged::Recheck));
        assert_eq!(judgements.recv_timeout(WAIT), Ok(Judged::Recheck));
        stop(&watcher, thread);

        let (watcher, thread, judgements) = watching(None);
        assert!(judgements.recv_timeout(Duration::from_millis(300)).is_err());
        watcher.lock().pending = Some((kAudioHardwarePropertyDevices, Instant::now()));
        watcher.condvar.notify_all();
        assert_eq!(
            judgements.recv_timeout(WAIT),
            Ok(Judged::Notification(kAudioHardwarePropertyDevices))
        );
        stop(&watcher, thread);
    }

    /// A rate that settled after `settle` gave up, before the rate listener
    /// was registered, is judged as a rate notification and reported, so
    /// the session rebuilds at the rate the device runs at; an unchanged
    /// rate raises nothing.
    #[test]
    fn a_rate_that_settled_before_its_listener_is_judged_as_a_change() {
        assert_eq!(
            LiveCaptureBackend::late_rate_notification(44_100.0, 44_100.0),
            None
        );
        let selector = LiveCaptureBackend::late_rate_notification(44_100.0, 48_000.0);
        assert_eq!(selector, Some(kAudioDevicePropertyNominalSampleRate));

        let (watcher, thread, judgements) = watching(None);
        watcher.notify(selector.unwrap());
        let judged = judgements.recv_timeout(WAIT).unwrap();
        assert_eq!(
            judged,
            Judged::Notification(kAudioDevicePropertyNominalSampleRate)
        );
        stop(&watcher, thread);

        let baseline = DeviceSnapshot {
            output_uid: Some("speakers".into()),
            default_output_uid: Some("speakers".into()),
            input_uid: Some("built-in".into()),
            output_alive: true,
            input_alive: true,
            sample_rate: 44_100.0,
        };
        let relocked = DeviceSnapshot {
            sample_rate: 48_000.0,
            ..baseline.clone()
        };
        assert_eq!(
            LiveCaptureBackend::judgement(judged, &relocked, &baseline),
            Some(DeviceChangeReason::SampleRateChanged)
        );
    }

    /// A system output that switched while the aggregate was built, before
    /// its listener was registered, is judged as a system output
    /// notification and reported, so the session rebuilds on the new clock
    /// master; the same device, read again, raises nothing.
    #[test]
    fn a_system_output_that_switched_before_its_listener_is_judged_as_a_change() {
        assert_eq!(
            LiveCaptureBackend::late_output_notification("speakers", Some("speakers")),
            None
        );
        let selector = LiveCaptureBackend::late_output_notification("speakers", Some("headset"));
        assert_eq!(
            selector,
            Some(kAudioHardwarePropertyDefaultSystemOutputDevice)
        );
        assert_eq!(
            LiveCaptureBackend::late_output_notification("speakers", None),
            selector,
            "none resolving is judged too"
        );

        let (watcher, thread, judgements) = watching(None);
        watcher.notify(selector.unwrap());
        let judged = judgements.recv_timeout(WAIT).unwrap();
        assert_eq!(
            judged,
            Judged::Notification(kAudioHardwarePropertyDefaultSystemOutputDevice)
        );
        stop(&watcher, thread);

        let baseline = DeviceSnapshot {
            output_uid: Some("speakers".into()),
            default_output_uid: Some("speakers".into()),
            input_uid: Some("built-in".into()),
            output_alive: true,
            input_alive: true,
            sample_rate: 48_000.0,
        };
        let switched = DeviceSnapshot {
            output_uid: Some("headset".into()),
            ..baseline.clone()
        };
        assert_eq!(
            LiveCaptureBackend::judgement(judged, &switched, &baseline),
            Some(DeviceChangeReason::DefaultOutputChanged)
        );
        assert_eq!(
            LiveCaptureBackend::judgement(judged, &baseline, &baseline),
            None
        );
    }

    /// A 48 kHz built-in microphone beside a headset at 24 kHz: its
    /// latency is halved into the stream's frames, never doubled, while a
    /// microphone on the clock master keeps its own count.
    #[test]
    fn a_microphone_on_its_own_clock_has_its_latency_rescaled() {
        assert_eq!(
            LiveCaptureBackend::mic_latency_frames(481, false, 48_000.0, 24_000.0),
            240
        );
        assert_eq!(
            LiveCaptureBackend::mic_latency_frames(481, true, 48_000.0, 24_000.0),
            481
        );
        assert_eq!(
            LiveCaptureBackend::mic_latency_frames(481, false, 0.0, 24_000.0),
            481
        );
    }

    /// The silent output IOProc zeroes the output list it is handed and
    /// returns 0. The buffer cases (a null `mData`, a size of 0, the last
    /// buffer) are `tests/realtime.rs`' `the_silent_output_allocates_nothing`.
    #[test]
    fn the_silent_output_writes_only_zeros() {
        use objc2_core_audio_types::AudioBuffer;

        let mut samples = vec![0.75f32; 1_024];
        let list = |data: *mut f32, floats: u32| AudioBufferList {
            mNumberBuffers: 1,
            mBuffers: [AudioBuffer {
                mNumberChannels: 2,
                mDataByteSize: floats * 4,
                mData: data.cast(),
            }],
        };
        let mut output = list(samples.as_mut_ptr(), 1_024);
        let mut input = list(std::ptr::null_mut(), 0);
        // SAFETY: an all-zero `AudioTimeStamp` is a valid, unset stamp.
        let mut time: AudioTimeStamp = unsafe { std::mem::zeroed() };
        let time = NonNull::from(&mut time);
        // SAFETY: the output's one buffer points at a vector of
        // `mDataByteSize` bytes that outlives the call; the input has none.
        let status = unsafe {
            silent_io_proc(
                0,
                time,
                NonNull::from(&mut input),
                time,
                NonNull::from(&mut output),
                time,
                std::ptr::null_mut(),
            )
        };
        assert_eq!(status, 0);
        assert!(samples.iter().all(|s| s.to_bits() == 0), "zeroed in full");
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

    /// A notification reports the first difference; a re-check only
    /// another microphone, so a bad read of the outputs, or a microphone
    /// that did not resolve, costs no rebuild. A restart of the audio
    /// service is a change with the devices as they were.
    #[test]
    fn a_recheck_reports_another_microphone_alone() {
        let baseline = DeviceSnapshot {
            output_uid: Some("speakers".into()),
            default_output_uid: Some("speakers".into()),
            input_uid: Some("built-in".into()),
            output_alive: true,
            input_alive: true,
            sample_rate: 48_000.0,
        };
        let back = DeviceSnapshot {
            input_uid: Some("usb-microphone".into()),
            ..baseline.clone()
        };
        let misread = DeviceSnapshot {
            default_output_uid: None,
            sample_rate: 0.0,
            ..baseline.clone()
        };
        let unresolved = DeviceSnapshot {
            input_uid: None,
            ..baseline.clone()
        };
        let notified = Judged::Notification(kAudioHardwarePropertyDevices);
        let table = [
            (notified, &baseline, None),
            (
                notified,
                &back,
                Some(DeviceChangeReason::DefaultInputChanged),
            ),
            (
                notified,
                &misread,
                Some(DeviceChangeReason::DefaultOutputChanged),
            ),
            (Judged::Recheck, &baseline, None),
            (
                Judged::Recheck,
                &back,
                Some(DeviceChangeReason::DefaultInputChanged),
            ),
            (Judged::Recheck, &misread, None),
            (Judged::Recheck, &unresolved, None),
            (
                Judged::Notification(kAudioHardwarePropertyServiceRestarted),
                &baseline,
                Some(DeviceChangeReason::AudioServiceRestarted),
            ),
        ];
        for (judged, resolved, reported) in table {
            assert_eq!(
                LiveCaptureBackend::judgement(judged, resolved, &baseline),
                reported,
                "{judged:?} of {resolved:?}"
            );
        }
    }
}
