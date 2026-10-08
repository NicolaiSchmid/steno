//! The real backend on macOS: process tap + private aggregate device + one
//! IOProc, with device-change listeners, coalescing and the report that
//! makes the session rebuild.
//! Swift: `Sources/StenoAudio/Capture/LiveCaptureBackend.swift`.
//!
//! `System` comes from the tap, `Mic` and `Mixed` from the first channel of
//! the chosen input device, which the aggregate resamples to the output
//! device's clock. That clock is 48 kHz where the output device accepts it;
//! a Bluetooth headset in the hands-free profile keeps 24 or 16 kHz, and
//! the stream then reports that rate for the processing thread to convert.
//! A chosen device that is not connected records the default input
//! instead ([`chosen_or_default_input`]), and the device list is watched so
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
//! ([`DeviceSnapshot::input_difference`]). Nothing changed means the burst
//! is logged and ignored; otherwise the sink gets one
//! [`DeviceChangeReason`] and the session rebuilds by calling `stop()` and
//! `start` again. Nothing here runs on the IO thread except [`io_proc`],
//! which only calls [`deliver`].
//!
//! Teardown order: watcher thread, `AudioDeviceStop`,
//! `AudioDeviceDestroyIOProcID`, the callback context, listeners,
//! `AudioHardwareDestroyAggregateDevice`, `AudioHardwareDestroyProcessTap`.
//!
//! Call mode needs an output client. The aggregate's clock master is the
//! system output device; where the capture permission is missing (a
//! session without a GUI, over SSH) the HAL runs the IOProc only while
//! another client has that output open, so the capture delivers no
//! callbacks at all until something plays (measured in
//! `.plans/spikes/2026-10-01-spike-rust-capture.md`: the first callback
//! arrived when `afplay` opened the speakers, and a run with nothing
//! playing got none). In-person mode has no tap and runs on the
//! microphone's clock.

use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use objc2_core_audio::{
    AudioObjectPropertySelector, kAudioDevicePropertyDeviceIsAlive,
    kAudioDevicePropertyNominalSampleRate, kAudioHardwarePropertyDefaultInputDevice,
    kAudioHardwarePropertyDefaultOutputDevice, kAudioHardwarePropertyDefaultSystemOutputDevice,
    kAudioHardwarePropertyDevices, kAudioObjectPropertyScopeGlobal, kAudioObjectPropertyScopeInput,
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
use crate::realtime::{BufferView, LaneFrameSink, RateConverter, deliver};

/// Shared with the IOProc through a raw pointer; boxed so it never moves,
/// alive until the `IoProc` is dropped.
struct CallbackContext {
    sink: Arc<LaneFrameSink>,
    sources: Vec<LaneSource>,
}

/// The most input buffers an aggregate of ours produces: the output device's
/// inputs (usually none), the microphone's and the tap's. Sixteen leaves
/// room for a many-channel interface.
const MAX_BUFFERS: usize = 16;

/// The IOProc. Builds a stack array of [`BufferView`]s from the HAL's
/// buffer list and hands it to [`deliver`]: no allocation, no lock, no
/// syscall; behind [`hal::abort_on_panic`].
unsafe extern "C-unwind" fn io_proc(
    _device: Id,
    _now: NonNull<AudioTimeStamp>,
    input: NonNull<AudioBufferList>,
    _input_time: NonNull<AudioTimeStamp>,
    _output: NonNull<AudioBufferList>,
    _output_time: NonNull<AudioTimeStamp>,
    client: *mut c_void,
) -> OSStatus {
    hal::abort_on_panic(|| {
        // SAFETY: `client` is the boxed `CallbackContext` registered in
        // `start`, alive until the IOProc is destroyed (which happens before
        // the box is dropped); `input` is the HAL's list, valid for the call.
        unsafe {
            let ctx = &*client.cast::<CallbackContext>();
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
    /// The last notification's selector and arrival, `None` once judged.
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
    /// judged once the burst settles.
    fn notify(&self, selector: AudioObjectPropertySelector) {
        self.lock().pending = Some((selector, Instant::now()));
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
    watcher: Arc<Watcher>,
    watcher_thread: Option<JoinHandle<()>>,
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

    /// The watcher's re-check interval: [`Self::FALLBACK_RECHECK`] for a
    /// capture on the fallback, none otherwise.
    fn recheck_for(is_fallback: bool) -> Option<Duration> {
        is_fallback.then_some(Self::FALLBACK_RECHECK)
    }

    /// What a judgement reports of the devices `resolved` now: after a
    /// notification their first difference from `baseline`, on a re-check
    /// only another microphone ([`DeviceSnapshot::input_difference`]).
    fn judgement(
        judged: Judged,
        resolved: &DeviceSnapshot,
        baseline: &DeviceSnapshot,
    ) -> Option<DeviceChangeReason> {
        match judged {
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

        let tap = if needs_tap {
            let own = hal::own_process_object()?;
            Some(ProcessTap::new(&[own], "Steno system lane")?)
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

        let context = Box::new(CallbackContext {
            sink: Arc::clone(&sink),
            sources: layout.sources.clone(),
        });
        let context_ptr: *const CallbackContext = &raw const *context;
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
        // after the burst settles.
        let mut selectors: Vec<(Id, AudioObjectPropertySelector)> = vec![
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
                .ok()
            })
            .collect();
        // Any change from here on notifies; one before the rate listener was
        // in place is judged as if it had.
        if let Some(selector) =
            Self::late_rate_notification(sample_rate, aggregate.nominal_sample_rate())
        {
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
            let frames = hal::latency_frames(m.id, kAudioObjectPropertyScopeInput);
            if mic_sub_device == Some(0) {
                frames
            } else {
                CaptureStream::rescaled(frames, hal::nominal_sample_rate(m.id), sample_rate)
            }
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
            _tap: tap,
            _aggregate: aggregate,
            _io_proc: io,
            _context: context,
            _listeners: listeners,
            watcher,
            watcher_thread: Some(watcher_thread),
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
        // reads, then listeners, aggregate, tap.
        let Active {
            _io_proc: io_proc,
            _context: context,
            _listeners: listeners,
            _aggregate: aggregate,
            _tap: tap,
            ..
        } = active;
        drop(io_proc);
        drop(context);
        drop(listeners);
        drop(aggregate);
        drop(tap);
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
    /// that did not resolve, costs no rebuild.
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
