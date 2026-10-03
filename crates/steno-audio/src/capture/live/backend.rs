//! The real backend on macOS: process tap + private aggregate device + one
//! IOProc, with device-change listeners, coalescing and the report that
//! makes the session rebuild.
//! Swift: `Sources/StenoAudio/Capture/LiveCaptureBackend.swift`.
//!
//! `System` comes from the tap, `Mic` and `Mixed` from the first channel of
//! the selected input device, which the aggregate resamples to the output
//! device's 48 kHz clock.
//!
//! Device notifications (a default device moving, a sub-device dying, the
//! aggregate leaving 48 kHz) arrive on the HAL's notification thread and
//! are coalesced for [`LiveCaptureBackend::COALESCE_DELAY`] on a watcher
//! thread, then the devices are resolved again and compared with what the
//! capture started on ([`DeviceSnapshot::difference`]). Nothing changed
//! means the burst is logged and ignored; otherwise the sink gets one
//! [`DeviceChangeReason`] and the session rebuilds by calling `stop()` and
//! `start` again. Nothing here runs on the IO thread except
//! [`io_proc`], which only calls [`deliver`].
//!
//! Teardown order: watcher thread, `AudioDeviceStop`,
//! `AudioDeviceDestroyIOProcID`, listeners, `AudioHardwareDestroyAggregateDevice`,
//! `AudioHardwareDestroyProcessTap`.

use std::ffi::c_void;
use std::panic::AssertUnwindSafe;
use std::ptr::NonNull;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use objc2_core_audio::{
    AudioObjectPropertySelector, kAudioDevicePropertyDeviceIsAlive,
    kAudioDevicePropertyNominalSampleRate, kAudioHardwarePropertyDefaultInputDevice,
    kAudioHardwarePropertyDefaultOutputDevice, kAudioHardwarePropertyDefaultSystemOutputDevice,
    kAudioObjectPropertyScopeGlobal, kAudioObjectPropertyScopeInput,
    kAudioObjectPropertyScopeOutput,
};
use objc2_core_audio_types::{AudioBufferList, AudioTimeStamp};
use steno_core::AudioLane;

use super::hal::{
    self, AggregateDevice, Id, IoProc, OSStatus, ProcessTap, PropertyListener, SYSTEM,
};
use super::{AudioDeviceInfo, AudioDevices};
use crate::SAMPLE_RATE;
use crate::capture::{
    CaptureBackend, CaptureError, CaptureStream, DeviceSnapshot, LaneSource, NominalSampleRate,
    StreamLayout,
};
use crate::realtime::{BufferView, LaneFrameSink, deliver};

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
/// syscall. A panic inside must not unwind into the HAL, so the body runs
/// under `catch_unwind` and a panic aborts the process; the barrier costs
/// nothing on the path that does not panic.
unsafe extern "C-unwind" fn io_proc(
    _device: Id,
    _now: NonNull<AudioTimeStamp>,
    input: NonNull<AudioBufferList>,
    _input_time: NonNull<AudioTimeStamp>,
    _output: NonNull<AudioBufferList>,
    _output_time: NonNull<AudioTimeStamp>,
    client: *mut c_void,
) -> OSStatus {
    let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| {
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
    }));
    if outcome.is_err() {
        std::process::abort();
    }
    0
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
    /// The devices as they are now: the defaults resolved again (or the
    /// explicit input by UID), the started devices' `DeviceIsAlive`, the
    /// aggregate's rate (0 once it is gone).
    fn resolve(&self) -> DeviceSnapshot {
        let output = AudioDevices::default_system_output().ok();
        let input: Option<AudioDeviceInfo> = if self.mic_id.is_some() {
            match &self.input_device_uid {
                Some(uid) => AudioDevices::device(uid).ok().flatten(),
                None => AudioDevices::default_input().ok(),
            }
        } else {
            None
        };
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
}

/// What one started capture holds.
struct Active {
    _tap: Option<ProcessTap>,
    _aggregate: AggregateDevice,
    _io_proc: IoProc,
    _context: Box<CallbackContext>,
    _listeners: Vec<PropertyListener>,
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
    /// for `COALESCE_DELAY` after the last one, then resolves the devices
    /// and reports the first difference. It belongs to one `start`:
    /// `stop()` raises `WatchState::stop` and joins it before the backend
    /// can start again, so a report never reaches a later capture (Swift
    /// compared a generation counter instead; the join makes that
    /// unnecessary here).
    fn watch(
        watcher: &Watcher,
        probe: &DeviceProbe,
        baseline: &DeviceSnapshot,
        sink: &LaneFrameSink,
    ) {
        let mut state = watcher.lock();
        loop {
            if state.stop {
                return;
            }
            let Some((selector, last)) = state.pending else {
                state = watcher
                    .condvar
                    .wait(state)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
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
            // The HAL reads run outside the lock.
            let snapshot = probe.resolve();
            let name = hal::selector_name(selector);
            match snapshot.difference(baseline) {
                None => tracing::info!("ignored device notification {name}"),
                Some(reason) => {
                    tracing::info!("device notification {name} reported {reason:?}");
                    sink.report_device_change(reason);
                }
            }
            state = watcher.lock();
        }
    }
}

impl CaptureBackend for LiveCaptureBackend {
    /// Returns the stream it opened: the confirmed 48 kHz rate, both device
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
        let mut mic: Option<AudioDeviceInfo> = None;
        if needs_mic {
            let resolved = match input_device_uid {
                Some(uid) => AudioDevices::device(uid).ok().flatten(),
                None => AudioDevices::default_input().ok(),
            };
            match resolved {
                Some(device) if device.is_input() => mic = Some(device),
                _ => return Err(CaptureError::InputDeviceUnavailable),
            }
        }

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
        // then read it back: the HAL applies the change asynchronously and a
        // device that cannot run at 48 kHz keeps its own, which would leave
        // a pitch-shifted master labelled 48 kHz. Fail loud instead.
        if aggregate.nominal_sample_rate() != SAMPLE_RATE {
            let _ = aggregate.set_nominal_sample_rate(SAMPLE_RATE);
        }
        let sample_rate = NominalSampleRate::settle(
            SAMPLE_RATE,
            NominalSampleRate::ATTEMPTS,
            || aggregate.nominal_sample_rate(),
            || std::thread::sleep(NominalSampleRate::INTERVAL),
        );
        if sample_rate != SAMPLE_RATE {
            // Whole hertz.
            return Err(CaptureError::SampleRateMismatch {
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
        // freed by field order below.
        let io = unsafe {
            IoProc::start(
                aggregate.id,
                Some(io_proc),
                context_ptr.cast_mut().cast::<c_void>(),
            )?
        };

        // Device changes: the default devices moving, a sub-device dying,
        // the aggregate leaving 48 kHz. The tap mirrors the default output
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
        if let Some(mic) = &mic {
            selectors.push((mic.id, kAudioDevicePropertyDeviceIsAlive));
            if input_device_uid.is_none() {
                selectors.push((SYSTEM, kAudioHardwarePropertyDefaultInputDevice));
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
                    Box::new(move |selector| {
                        watcher.lock().pending = Some((selector, Instant::now()));
                        watcher.condvar.notify_all();
                    }),
                )
                .ok()
            })
            .collect();

        // The far-end delay: the microphone's input path plus the
        // loudspeaker's output path, each latency plus safety offset, read
        // on the devices themselves rather than the aggregate.
        let input_latency = mic.as_ref().map_or(0, |m| {
            hal::latency_frames(m.id, kAudioObjectPropertyScopeInput)
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
                    LiveCaptureBackend::watch(&watcher, &probe, &baseline, &sink);
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
        drop(listeners);
        drop(aggregate);
        drop(tap);
        drop(context);
    }
}

impl Drop for LiveCaptureBackend {
    /// A backend dropped without `stop()` still tears the HAL objects down
    /// in order instead of leaving it to field destruction order.
    fn drop(&mut self) {
        self.stop();
    }
}
