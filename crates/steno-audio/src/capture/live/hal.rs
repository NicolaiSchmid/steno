//! The CoreAudio HAL calls behind the capture: property reads and writes,
//! default devices, the process tap
//! (`AudioHardwareCreateProcessTap` with a `CATapDescription`), the private
//! aggregate device, one IOProc and property listeners.
//! Swift: `Sources/StenoAudio/Capture/AudioObjectProperties.swift`,
//! `ProcessTap.swift`, `AggregateDevice.swift`, the create/start/stop of
//! `RealTime/IOProcRunner.swift`.
//!
//! Everything here runs off the audio thread except the IOProc callback
//! the caller supplies to [`IoProc::start`]. The invariants each `unsafe`
//! block relies on are stated beside it; the HAL's own contract is that a
//! property buffer of the size it reported is written in full, that an
//! `AudioObjectID` stays valid until the object is destroyed or the HAL
//! reports it gone, and that a listener or IOProc is never called after
//! its removal returns.

use std::ffi::{CStr, c_void};
use std::panic::AssertUnwindSafe;
use std::ptr::NonNull;

use objc2::AnyThread;
use objc2::rc::Retained;
use objc2_core_audio::{
    AudioConvertHostTimeToNanos, AudioDeviceCreateIOProcID, AudioDeviceDestroyIOProcID,
    AudioDeviceIOProc, AudioDeviceIOProcID, AudioDeviceStart, AudioDeviceStop,
    AudioGetCurrentHostTime, AudioHardwareCreateAggregateDevice, AudioHardwareCreateProcessTap,
    AudioHardwareDestroyAggregateDevice, AudioHardwareDestroyProcessTap,
    AudioHardwareIOProcStreamUsage, AudioObjectAddPropertyListener, AudioObjectGetPropertyData,
    AudioObjectGetPropertyDataSize, AudioObjectID, AudioObjectPropertyAddress,
    AudioObjectPropertyScope, AudioObjectPropertySelector, AudioObjectRemovePropertyListener,
    AudioObjectSetPropertyData, CATapDescription, CATapMuteBehavior,
    kAudioAggregateDeviceIsPrivateKey, kAudioAggregateDeviceIsStackedKey,
    kAudioAggregateDeviceMainSubDeviceKey, kAudioAggregateDeviceNameKey,
    kAudioAggregateDevicePropertyActiveSubDeviceList, kAudioAggregateDevicePropertyMainSubDevice,
    kAudioAggregateDeviceSubDeviceListKey, kAudioAggregateDeviceTapAutoStartKey,
    kAudioAggregateDeviceTapListKey, kAudioAggregateDeviceUIDKey,
    kAudioDevicePropertyDeviceIsAlive, kAudioDevicePropertyDeviceUID,
    kAudioDevicePropertyIOProcStreamUsage, kAudioDevicePropertyLatency,
    kAudioDevicePropertyNominalSampleRate, kAudioDevicePropertySafetyOffset,
    kAudioDevicePropertyStreamConfiguration, kAudioDevicePropertyStreams,
    kAudioObjectPropertyElementMain, kAudioObjectPropertyName, kAudioObjectPropertyScopeGlobal,
    kAudioObjectPropertyScopeInput, kAudioObjectSystemObject, kAudioObjectUnknown,
    kAudioSubDeviceDriftCompensationKey, kAudioSubDeviceUIDKey, kAudioSubTapDriftCompensationKey,
    kAudioSubTapUIDKey, kAudioTapPropertyFormat,
};
use objc2_core_audio_types::{
    AudioBufferList, AudioStreamBasicDescription, kAudioFormatFlagIsNonInterleaved,
};
use objc2_core_foundation::{CFDictionary, CFRetained, CFString};
use objc2_foundation::{NSArray, NSDictionary, NSNumber, NSObject, NSString};

use crate::capture::CaptureError;
use crate::capture::configuration::four_char_code;
use crate::capture::start_log::start_log;

pub type Id = AudioObjectID;
pub type OSStatus = i32;
pub const SYSTEM: Id = kAudioObjectSystemObject as Id;
pub const UNKNOWN: Id = kAudioObjectUnknown;

/// A failed Core Audio call: which property or function, on which object,
/// with which `OSStatus`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{operation} on audio object {object} failed: {status} ({})", four_char_code(*status))]
pub struct CoreAudioError {
    /// The property or function that failed.
    pub operation: String,
    /// The audio object it was called on.
    pub object: Id,
    /// The `OSStatus` it returned.
    pub status: OSStatus,
}

impl From<CoreAudioError> for CaptureError {
    fn from(error: CoreAudioError) -> Self {
        CaptureError::CoreAudio {
            operation: error.operation,
            status: error.status,
        }
    }
}

/// The four-character spelling of a selector, for errors and logs.
#[must_use]
pub fn selector_name(selector: AudioObjectPropertySelector) -> String {
    // Selectors are four ASCII bytes packed in a u32.
    #[allow(clippy::cast_possible_wrap)]
    four_char_code(selector as i32)
}

fn address(
    selector: AudioObjectPropertySelector,
    scope: AudioObjectPropertyScope,
) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress {
        mSelector: selector,
        mScope: scope,
        mElement: kAudioObjectPropertyElementMain,
    }
}

fn failed(operation: &str, object: Id, status: OSStatus) -> CoreAudioError {
    CoreAudioError {
        operation: operation.to_owned(),
        object,
        status,
    }
}

/// One plain-old-data property.
pub fn read_pod<T: Copy>(
    id: Id,
    selector: AudioObjectPropertySelector,
    scope: AudioObjectPropertyScope,
) -> Result<T, CoreAudioError> {
    let mut addr = address(selector, scope);
    // SAFETY: `T` is a plain-old-data HAL value (integer, float, object id,
    // struct of those, or a CF reference the caller takes ownership of);
    // the all-zero pattern is valid for each, and the HAL overwrites it in
    // full on success.
    let mut value: T = unsafe { std::mem::zeroed() };
    let mut size = u32::try_from(std::mem::size_of::<T>()).unwrap_or(u32::MAX);
    // SAFETY: every pointer refers to a live local of the size passed; no
    // qualifier (size 0, null).
    let status = unsafe {
        AudioObjectGetPropertyData(
            id,
            NonNull::from(&mut addr),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
            NonNull::from(&mut value).cast::<c_void>(),
        )
    };
    if status != 0 {
        return Err(failed(
            &format!("read {}", selector_name(selector)),
            id,
            status,
        ));
    }
    Ok(value)
}

/// A `CFString` property as a Rust string.
pub fn read_string(
    id: Id,
    selector: AudioObjectPropertySelector,
    scope: AudioObjectPropertyScope,
) -> Result<String, CoreAudioError> {
    let ptr: *mut CFString = read_pod(id, selector, scope)?;
    let ptr = NonNull::new(ptr).ok_or_else(|| failed("read string", id, -1))?;
    // SAFETY: the HAL returns a +1 retained CFString for string properties;
    // `from_raw` takes that reference and releases it on drop.
    let string: CFRetained<CFString> = unsafe { CFRetained::from_raw(ptr) };
    Ok(string.to_string())
}

pub fn read_u32(
    id: Id,
    selector: AudioObjectPropertySelector,
    scope: AudioObjectPropertyScope,
) -> Result<u32, CoreAudioError> {
    read_pod(id, selector, scope)
}

/// `false` when the property reads zero and when it cannot be read at all.
#[must_use]
pub fn read_bool(id: Id, selector: AudioObjectPropertySelector) -> bool {
    read_u32(id, selector, kAudioObjectPropertyScopeGlobal).unwrap_or(0) != 0
}

pub fn read_f64(id: Id, selector: AudioObjectPropertySelector) -> Result<f64, CoreAudioError> {
    read_pod(id, selector, kAudioObjectPropertyScopeGlobal)
}

/// An array of plain-old-data values (`[AudioObjectID]`).
pub fn read_array<T: Copy>(
    id: Id,
    selector: AudioObjectPropertySelector,
    scope: AudioObjectPropertyScope,
) -> Result<Vec<T>, CoreAudioError> {
    let mut addr = address(selector, scope);
    let mut size: u32 = 0;
    // SAFETY: valid pointers to locals.
    let status = unsafe {
        AudioObjectGetPropertyDataSize(
            id,
            NonNull::from(&mut addr),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
        )
    };
    if status != 0 {
        return Err(failed(
            &format!("size of {}", selector_name(selector)),
            id,
            status,
        ));
    }
    let stride = std::mem::size_of::<T>().max(1);
    let capacity = size as usize / stride;
    if capacity == 0 {
        return Ok(Vec::new());
    }
    // SAFETY: as in `read_pod`, zeroed PODs the HAL overwrites.
    let mut values: Vec<T> = vec![unsafe { std::mem::zeroed() }; capacity];
    // SAFETY: `values` holds `size` bytes; the HAL writes at most that many
    // and reports how many it wrote in `size`.
    let status = unsafe {
        AudioObjectGetPropertyData(
            id,
            NonNull::from(&mut addr),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
            NonNull::new(values.as_mut_ptr().cast::<c_void>()).expect("non-null vec"),
        )
    };
    if status != 0 {
        return Err(failed(
            &format!("read {}", selector_name(selector)),
            id,
            status,
        ));
    }
    values.truncate(size as usize / stride);
    Ok(values)
}

pub fn write_f64(
    id: Id,
    selector: AudioObjectPropertySelector,
    mut value: f64,
) -> Result<(), CoreAudioError> {
    let mut addr = address(selector, kAudioObjectPropertyScopeGlobal);
    // SAFETY: valid pointers to locals of the size passed.
    let status = unsafe {
        AudioObjectSetPropertyData(
            id,
            NonNull::from(&mut addr),
            0,
            std::ptr::null(),
            u32::try_from(std::mem::size_of::<f64>()).unwrap_or(8),
            NonNull::from(&mut value).cast::<c_void>(),
        )
    };
    if status != 0 {
        return Err(failed(
            &format!("write {}", selector_name(selector)),
            id,
            status,
        ));
    }
    Ok(())
}

/// Channel count per buffer of `kAudioDevicePropertyStreamConfiguration`
/// in `scope`; empty when the device has no streams there or cannot be
/// read.
#[must_use]
pub fn channel_counts(id: Id, scope: AudioObjectPropertyScope) -> Vec<usize> {
    let mut addr = address(kAudioDevicePropertyStreamConfiguration, scope);
    let mut size: u32 = 0;
    // SAFETY: valid pointers to locals.
    let status = unsafe {
        AudioObjectGetPropertyDataSize(
            id,
            NonNull::from(&mut addr),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
        )
    };
    if status != 0 || size == 0 {
        return Vec::new();
    }
    // u64 storage keeps the `AudioBufferList` alignment.
    let mut raw = vec![0u64; (size as usize).div_ceil(8)];
    // SAFETY: `raw` holds `size` bytes, aligned for the list.
    let status = unsafe {
        AudioObjectGetPropertyData(
            id,
            NonNull::from(&mut addr),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
            NonNull::new(raw.as_mut_ptr().cast::<c_void>()).expect("non-null vec"),
        )
    };
    if status != 0 {
        return Vec::new();
    }
    let list = raw.as_ptr().cast::<AudioBufferList>();
    // SAFETY: the HAL filled a variable-length `AudioBufferList` whose
    // `mBuffers` array has `mNumberBuffers` entries within `size` bytes.
    unsafe {
        let count = (*list).mNumberBuffers as usize;
        let buffers = (*list).mBuffers.as_ptr();
        (0..count)
            .map(|i| (*buffers.add(i)).mNumberChannels as usize)
            .collect()
    }
}

/// `kAudioDevicePropertyLatency` plus `kAudioDevicePropertySafetyOffset` of
/// one device in `scope`, in frames.
#[must_use]
pub fn latency_frames(id: Id, scope: AudioObjectPropertyScope) -> usize {
    let latency = read_u32(id, kAudioDevicePropertyLatency, scope).unwrap_or(0);
    let safety = read_u32(id, kAudioDevicePropertySafetyOffset, scope).unwrap_or(0);
    latency as usize + safety as usize
}

#[must_use]
pub fn nominal_sample_rate(id: Id) -> f64 {
    read_f64(id, kAudioDevicePropertyNominalSampleRate).unwrap_or(0.0)
}

pub fn uid(id: Id) -> Result<String, CoreAudioError> {
    read_string(
        id,
        kAudioDevicePropertyDeviceUID,
        kAudioObjectPropertyScopeGlobal,
    )
}

#[must_use]
pub fn name(id: Id) -> String {
    read_string(
        id,
        kAudioObjectPropertyName,
        kAudioObjectPropertyScopeGlobal,
    )
    .unwrap_or_default()
}

/// `kAudioDevicePropertyDeviceIsAlive`: false once the HAL has dropped the
/// device, and when the object cannot be read at all.
#[must_use]
pub fn is_alive(id: Id) -> bool {
    read_bool(id, kAudioDevicePropertyDeviceIsAlive)
}

/// A private, unmuted global process tap of everything the Mac plays,
/// Steno's own process included: the call capture's silent output IOProc
/// on the aggregate's clock master is what keeps the tap aggregate
/// running, and the HAL counts only the output of a process the tap
/// includes (A10 of `.plans/2026-10-07-stable-promotion.md`). So nothing
/// else of Steno's may play while it records, which
/// [`crate::playback::Playback`] enforces. Destroyed on drop.
pub struct ProcessTap {
    pub id: Id,
    /// The tap's UID as `kAudioSubTapUIDKey` wants it.
    pub uid: String,
    pub format: AudioStreamBasicDescription,
}

impl std::fmt::Debug for ProcessTap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProcessTap")
            .field("id", &self.id)
            .field("uid", &self.uid)
            .finish_non_exhaustive()
    }
}

impl ProcessTap {
    pub fn new(name: &str) -> Result<Self, CaptureError> {
        // Excluding no process: the global tap of everything.
        let array: Retained<NSArray<NSNumber>> = NSArray::new();
        // SAFETY: Objective-C initialiser and setters on a freshly allocated
        // description; the array outlives the call.
        let description = unsafe {
            let description = CATapDescription::initStereoGlobalTapButExcludeProcesses(
                CATapDescription::alloc(),
                &array,
            );
            description.setName(&NSString::from_str(name));
            description.setPrivate(true);
            description.setMuteBehavior(CATapMuteBehavior::Unmuted);
            description
        };
        let mut id: Id = UNKNOWN;
        // SAFETY: valid description and out-pointer.
        let status = unsafe { AudioHardwareCreateProcessTap(Some(&description), &raw mut id) };
        if status != 0 {
            return Err(CaptureError::CoreAudio {
                operation: "AudioHardwareCreateProcessTap".into(),
                status,
            });
        }
        if id == UNKNOWN {
            return Err(CaptureError::BackendFailed(
                "AudioHardwareCreateProcessTap returned no tap object".into(),
            ));
        }
        // SAFETY: the description's UUID is set by the initialiser.
        let uid = unsafe { description.UUID().UUIDString() }.to_string();
        let format: AudioStreamBasicDescription =
            read_pod(id, kAudioTapPropertyFormat, kAudioObjectPropertyScopeGlobal)
                // SAFETY: an all-zero ASBD is the "unknown format" value.
                .unwrap_or(unsafe { std::mem::zeroed() });
        Ok(Self { id, uid, format })
    }

    #[must_use]
    pub fn channel_count(&self) -> usize {
        self.format.mChannelsPerFrame as usize
    }

    /// The tap's buffers as the aggregate exposes them: one interleaved
    /// buffer, or one buffer per channel when the format is non-interleaved.
    #[must_use]
    pub fn buffer_channel_counts(&self) -> Vec<usize> {
        let channels = self.channel_count().max(1);
        if self.format.mFormatFlags & kAudioFormatFlagIsNonInterleaved != 0 {
            vec![1; channels]
        } else {
            vec![channels]
        }
    }
}

impl Drop for ProcessTap {
    fn drop(&mut self) {
        // SAFETY: created in `new`, destroyed exactly once here.
        unsafe { AudioHardwareDestroyProcessTap(self.id) };
    }
}

fn key(c: &CStr) -> Retained<NSString> {
    NSString::from_str(c.to_str().unwrap_or_default())
}
fn ns_bool(b: bool) -> Retained<NSObject> {
    Retained::into_super(Retained::into_super(NSNumber::numberWithBool(b)))
}
fn ns_str(s: &str) -> Retained<NSObject> {
    Retained::into_super(NSString::from_str(s))
}
fn ns_dict(
    keys: &[Retained<NSString>],
    values: &[Retained<NSObject>],
) -> Retained<NSDictionary<NSString, NSObject>> {
    let refs: Vec<&NSString> = keys.iter().map(|k| &**k).collect();
    NSDictionary::from_retained_objects(&refs, values)
}

/// A private aggregate device: the output device as clock master
/// (`kAudioAggregateDeviceMainSubDeviceKey`), the microphone as a
/// drift-compensated sub-device and the process tap as a drift-compensated
/// sub-tap with `TapAutoStart`. One IOProc on it delivers every lane in one
/// callback, sample-aligned by the HAL. Destroyed on drop.
pub struct AggregateDevice {
    pub id: Id,
    pub uid: String,
}

impl std::fmt::Debug for AggregateDevice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AggregateDevice")
            .field("id", &self.id)
            .field("uid", &self.uid)
            .finish()
    }
}

/// How the UID of every private aggregate Steno creates begins.
pub const AGGREGATE_UID_PREFIX: &str = "uno.schmid.steno.aggregate.";

impl AggregateDevice {
    pub fn new(
        name: &str,
        main_sub_device_uid: &str,
        sub_device_uids: &[String],
        tap_uids: &[String],
    ) -> Result<Self, CaptureError> {
        let uid = format!(
            "{AGGREGATE_UID_PREFIX}{}",
            steno_core::json::uuid_string(uuid::Uuid::new_v4())
        );
        let sub_devices: Vec<Retained<NSObject>> = sub_device_uids
            .iter()
            .map(|device_uid| {
                Retained::into_super(ns_dict(
                    &[
                        key(kAudioSubDeviceUIDKey),
                        key(kAudioSubDeviceDriftCompensationKey),
                    ],
                    &[
                        ns_str(device_uid),
                        ns_bool(device_uid != main_sub_device_uid),
                    ],
                ))
            })
            .collect();
        let taps: Vec<Retained<NSObject>> = tap_uids
            .iter()
            .map(|tap_uid| {
                Retained::into_super(ns_dict(
                    &[
                        key(kAudioSubTapUIDKey),
                        key(kAudioSubTapDriftCompensationKey),
                    ],
                    &[ns_str(tap_uid), ns_bool(true)],
                ))
            })
            .collect();
        let description = ns_dict(
            &[
                key(kAudioAggregateDeviceNameKey),
                key(kAudioAggregateDeviceUIDKey),
                key(kAudioAggregateDeviceMainSubDeviceKey),
                key(kAudioAggregateDeviceIsPrivateKey),
                key(kAudioAggregateDeviceIsStackedKey),
                key(kAudioAggregateDeviceTapAutoStartKey),
                key(kAudioAggregateDeviceSubDeviceListKey),
                key(kAudioAggregateDeviceTapListKey),
            ],
            &[
                ns_str(name),
                ns_str(&uid),
                ns_str(main_sub_device_uid),
                ns_bool(true),
                ns_bool(false),
                ns_bool(true),
                Retained::into_super(NSArray::from_retained_slice(&sub_devices)),
                Retained::into_super(NSArray::from_retained_slice(&taps)),
            ],
        );
        // SAFETY: NSDictionary is toll-free bridged to CFDictionary; the
        // description lives for the call.
        let cf: &CFDictionary = unsafe { &*Retained::as_ptr(&description).cast::<CFDictionary>() };
        let mut id: Id = UNKNOWN;
        // SAFETY: valid dictionary and out-pointer.
        let status = unsafe { AudioHardwareCreateAggregateDevice(cf, NonNull::from(&mut id)) };
        if status != 0 {
            return Err(CaptureError::CoreAudio {
                operation: "AudioHardwareCreateAggregateDevice".into(),
                status,
            });
        }
        if id == UNKNOWN {
            return Err(CaptureError::BackendFailed(
                "AudioHardwareCreateAggregateDevice returned no device object".into(),
            ));
        }
        Ok(Self { id, uid })
    }

    #[must_use]
    pub fn nominal_sample_rate(&self) -> f64 {
        nominal_sample_rate(self.id)
    }

    pub fn set_nominal_sample_rate(&self, rate: f64) -> Result<(), CoreAudioError> {
        write_f64(self.id, kAudioDevicePropertyNominalSampleRate, rate)
    }

    /// Channel count per input buffer, the shape the IOProc receives.
    #[must_use]
    pub fn input_channel_counts(&self) -> Vec<usize> {
        channel_counts(self.id, kAudioObjectPropertyScopeInput)
    }

    /// The aggregate's clock master as the HAL reports it: the active
    /// sub-device whose UID is its main sub-device
    /// (`kAudioAggregateDevicePropertyMainSubDevice`). Only ids and UIDs
    /// are read.
    pub fn main_sub_device(&self) -> Result<Id, CoreAudioError> {
        let main = read_string(
            self.id,
            kAudioAggregateDevicePropertyMainSubDevice,
            kAudioObjectPropertyScopeGlobal,
        )?;
        let active: Vec<Id> = read_array(
            self.id,
            kAudioAggregateDevicePropertyActiveSubDeviceList,
            kAudioObjectPropertyScopeGlobal,
        )?;
        active
            .into_iter()
            .find(|&device| uid(device).is_ok_and(|uid| uid == main))
            .ok_or_else(|| failed("find the main sub-device", self.id, -1))
    }
}

impl Drop for AggregateDevice {
    fn drop(&mut self) {
        // SAFETY: created in `new`, destroyed exactly once here.
        unsafe { AudioHardwareDestroyAggregateDevice(self.id) };
    }
}

/// One started IOProc; `AudioDeviceStop` then `AudioDeviceDestroyIOProcID`
/// on drop.
pub struct IoProc {
    device: Id,
    proc_id: AudioDeviceIOProcID,
}

impl std::fmt::Debug for IoProc {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IoProc")
            .field("device", &self.device)
            .finish_non_exhaustive()
    }
}

impl IoProc {
    /// Starts `callback` on `device` over every stream it has, as the HAL
    /// does by default.
    ///
    /// # Safety
    ///
    /// `client` must stay valid, and whatever it points at must not move,
    /// until the returned `IoProc` is dropped; the HAL passes it to
    /// `callback` on its real-time thread.
    pub unsafe fn start(
        device: Id,
        callback: AudioDeviceIOProc,
        client: *mut c_void,
    ) -> Result<Self, CaptureError> {
        // SAFETY: the caller's guarantee, passed on.
        unsafe { Self::start_with(device, callback, client, false) }
    }

    /// As [`Self::start`], for an output client that reads nothing: its
    /// input streams are set off for its IOProc (read back as off), through
    /// `kAudioDevicePropertyIOProcStreamUsage` before the start. A usage the
    /// HAL refuses is logged and the IOProc starts over every stream.
    ///
    /// # Safety
    ///
    /// As for [`Self::start`].
    pub unsafe fn start_output_only(
        device: Id,
        callback: AudioDeviceIOProc,
        client: *mut c_void,
    ) -> Result<Self, CaptureError> {
        // SAFETY: the caller's guarantee, passed on.
        unsafe { Self::start_with(device, callback, client, true) }
    }

    /// # Safety
    ///
    /// As for [`Self::start`].
    unsafe fn start_with(
        device: Id,
        callback: AudioDeviceIOProc,
        client: *mut c_void,
        output_only: bool,
    ) -> Result<Self, CaptureError> {
        let mut proc_id: AudioDeviceIOProcID = None;
        // SAFETY: valid device id, callback and out-pointer; `client` by the
        // caller's guarantee.
        let status = unsafe {
            AudioDeviceCreateIOProcID(device, callback, client, NonNull::from(&mut proc_id))
        };
        if status != 0 {
            return Err(CaptureError::CoreAudio {
                operation: "AudioDeviceCreateIOProcID".into(),
                status,
            });
        }
        if output_only && let Err(error) = input_stream_usage_off(device, proc_id) {
            start_log!(
                warn,
                "the input streams stay on for an output-only IOProc ({error})"
            );
        }
        // SAFETY: the proc id was just created on this device.
        let status = unsafe { AudioDeviceStart(device, proc_id) };
        if status != 0 {
            // SAFETY: destroying what was created above.
            unsafe { AudioDeviceDestroyIOProcID(device, proc_id) };
            return Err(CaptureError::CoreAudio {
                operation: "AudioDeviceStart".into(),
                status,
            });
        }
        Ok(Self { device, proc_id })
    }

    /// Whether each of the device's input streams is on for this IOProc.
    #[cfg(test)]
    fn input_stream_usage(&self) -> Result<Vec<u32>, CoreAudioError> {
        let streams = input_stream_count(self.device)?;
        let (mut usage, mut size) = stream_usage(self.proc_id, streams);
        let mut addr = address(
            kAudioDevicePropertyIOProcStreamUsage,
            kAudioObjectPropertyScopeInput,
        );
        // SAFETY: `usage` holds `size` bytes, aligned for the struct, with
        // `mIOProc` naming the proc whose usage the HAL fills in.
        let status = unsafe {
            AudioObjectGetPropertyData(
                self.device,
                NonNull::from(&mut addr),
                0,
                std::ptr::null(),
                NonNull::from(&mut size),
                NonNull::new(usage.as_mut_ptr().cast::<c_void>()).expect("non-null vec"),
            )
        };
        if status != 0 {
            return Err(failed(
                "read the IOProc's stream usage",
                self.device,
                status,
            ));
        }
        Ok(streams_on(&usage, streams))
    }
}

/// How many input streams `device` has.
fn input_stream_count(device: Id) -> Result<usize, CoreAudioError> {
    Ok(read_array::<Id>(
        device,
        kAudioDevicePropertyStreams,
        kAudioObjectPropertyScopeInput,
    )?
    .len())
}

/// An `AudioHardwareIOProcStreamUsage` for `proc_id` over `streams`
/// streams, every one off, and its size in bytes. The struct ends in a
/// variable-length array; `u64` storage keeps its pointer aligned.
fn stream_usage(proc_id: AudioDeviceIOProcID, streams: usize) -> (Vec<u64>, u32) {
    let size = std::mem::offset_of!(AudioHardwareIOProcStreamUsage, mStreamIsOn)
        + streams * std::mem::size_of::<u32>();
    let bytes = size.max(std::mem::size_of::<AudioHardwareIOProcStreamUsage>());
    let mut usage = vec![0u64; bytes.div_ceil(8)];
    let header = usage.as_mut_ptr().cast::<AudioHardwareIOProcStreamUsage>();
    let proc_ptr = proc_id.map_or(std::ptr::null_mut(), |f| f as *mut c_void);
    // SAFETY: `usage` is zeroed, aligned for the struct and at least its
    // size; the pointer field is written as a raw pointer, since the zeroed
    // storage is not yet a valid `NonNull`.
    unsafe {
        (&raw mut (*header).mIOProc)
            .cast::<*mut c_void>()
            .write(proc_ptr);
        (&raw mut (*header).mNumberStreams).write(u32::try_from(streams).unwrap_or(u32::MAX));
    }
    (usage, u32::try_from(size).unwrap_or(u32::MAX))
}

/// The `mStreamIsOn` entries of a usage from [`stream_usage`].
#[cfg(test)]
fn streams_on(usage: &[u64], streams: usize) -> Vec<u32> {
    let on = std::mem::offset_of!(AudioHardwareIOProcStreamUsage, mStreamIsOn);
    let bytes: Vec<u8> = usage.iter().flat_map(|word| word.to_ne_bytes()).collect();
    let (entries, _) = bytes[on..].as_chunks::<4>();
    entries
        .iter()
        .take(streams)
        .map(|entry| u32::from_ne_bytes(*entry))
        .collect()
}

/// Turns every input stream of `device` off for `proc_id`, before its
/// start. A device without input streams has nothing to turn off.
fn input_stream_usage_off(device: Id, proc_id: AudioDeviceIOProcID) -> Result<(), CoreAudioError> {
    let streams = input_stream_count(device)?;
    if streams == 0 {
        return Ok(());
    }
    let (mut usage, size) = stream_usage(proc_id, streams);
    let mut addr = address(
        kAudioDevicePropertyIOProcStreamUsage,
        kAudioObjectPropertyScopeInput,
    );
    // SAFETY: `usage` holds `size` bytes, aligned for the struct, naming a
    // proc created on `device`.
    let status = unsafe {
        AudioObjectSetPropertyData(
            device,
            NonNull::from(&mut addr),
            0,
            std::ptr::null(),
            size,
            NonNull::new(usage.as_mut_ptr().cast::<c_void>()).expect("non-null vec"),
        )
    };
    if status != 0 {
        return Err(failed("write the IOProc's stream usage", device, status));
    }
    Ok(())
}

impl Drop for IoProc {
    fn drop(&mut self) {
        // SAFETY: stopping and destroying the proc created in `start`; the
        // HAL makes no further callbacks once `AudioDeviceStop` returns.
        unsafe {
            AudioDeviceStop(self.device, self.proc_id);
            AudioDeviceDestroyIOProcID(self.device, self.proc_id);
        }
    }
}

/// What a listener runs with: the selector that fired.
pub type ListenerHandler = Box<dyn Fn(AudioObjectPropertySelector) + Send + Sync>;

/// One registered property listener (`AudioObjectAddPropertyListener` with
/// a C callback and a boxed handler as client data); removed on drop. The
/// handler runs on the HAL's notification thread, never on the IO thread,
/// and must not block for long.
pub struct PropertyListener {
    object: Id,
    address: AudioObjectPropertyAddress,
    handler: *mut ListenerHandler,
}

// SAFETY: the handler box is only touched by the HAL callback (which is
// `Send + Sync`) and freed in `drop` after the listener is removed.
unsafe impl Send for PropertyListener {}
unsafe impl Sync for PropertyListener {}

impl std::fmt::Debug for PropertyListener {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PropertyListener")
            .field("object", &self.object)
            .finish_non_exhaustive()
    }
}

/// The HAL's host clock now, in its ticks: the clock an IOProc's
/// timestamps (`mHostTime`) count in.
#[must_use]
pub fn host_time_now() -> u64 {
    // SAFETY: no arguments and no preconditions; it reads the host clock.
    unsafe { AudioGetCurrentHostTime() }
}

/// `ticks` of the host clock in nanoseconds.
#[must_use]
pub fn host_time_to_nanos(ticks: u64) -> u64 {
    // SAFETY: a pure conversion by the host clock's timebase; any value is
    // valid input.
    unsafe { AudioConvertHostTimeToNanos(ticks) }
}

/// The body of a callback the HAL makes into us (the IOProc, a property
/// listener). A panic must not unwind into the HAL, so it aborts the
/// process instead; the barrier costs nothing on the path that does not
/// panic.
#[inline(always)]
pub fn abort_on_panic(body: impl FnOnce()) {
    if std::panic::catch_unwind(AssertUnwindSafe(body)).is_err() {
        std::process::abort();
    }
}

/// The C listener: runs the boxed handler once per address, behind
/// [`abort_on_panic`].
unsafe extern "C-unwind" fn listener_proc(
    _object: Id,
    count: u32,
    addresses: NonNull<AudioObjectPropertyAddress>,
    client: *mut c_void,
) -> OSStatus {
    abort_on_panic(|| {
        // SAFETY: `client` is the `*mut ListenerHandler` registered in
        // `add`, alive until `remove` has returned (the HAL calls no
        // listener after its removal); `addresses` has `count` entries.
        unsafe {
            let handler = &*client.cast::<ListenerHandler>();
            for index in 0..count as usize {
                let address = *addresses.as_ptr().add(index);
                handler(address.mSelector);
            }
        }
    });
    0
}

impl PropertyListener {
    pub fn add(
        object: Id,
        selector: AudioObjectPropertySelector,
        scope: AudioObjectPropertyScope,
        handler: ListenerHandler,
    ) -> Result<Self, CoreAudioError> {
        let mut addr = address(selector, scope);
        let handler = Box::into_raw(Box::new(handler));
        // SAFETY: valid object, address and callback; `handler` is a live
        // heap pointer owned by the returned listener.
        let status = unsafe {
            AudioObjectAddPropertyListener(
                object,
                NonNull::from(&mut addr),
                Some(listener_proc),
                handler.cast::<c_void>(),
            )
        };
        if status != 0 {
            // SAFETY: the box was never registered; reclaim it.
            drop(unsafe { Box::from_raw(handler) });
            return Err(failed(
                &format!("listen {}", selector_name(selector)),
                object,
                status,
            ));
        }
        Ok(Self {
            object,
            address: addr,
            handler,
        })
    }
}

impl Drop for PropertyListener {
    fn drop(&mut self) {
        // SAFETY: removing the exact registration from `add`; after this
        // returns the HAL makes no further calls with `handler`, so the box
        // is freed last.
        unsafe {
            AudioObjectRemovePropertyListener(
                self.object,
                NonNull::from(&mut self.address),
                Some(listener_proc),
                self.handler.cast::<c_void>(),
            );
            drop(Box::from_raw(self.handler));
        }
    }
}

#[cfg(test)]
mod tests {
    use objc2_core_audio::kAudioHardwarePropertyDefaultInputDevice;
    use objc2_core_audio_types::AudioTimeStamp;

    use super::*;

    unsafe extern "C-unwind" fn nothing(
        _device: Id,
        _now: NonNull<AudioTimeStamp>,
        _input: NonNull<AudioBufferList>,
        _input_time: NonNull<AudioTimeStamp>,
        _output: NonNull<AudioBufferList>,
        _output_time: NonNull<AudioTimeStamp>,
        _client: *mut c_void,
    ) -> OSStatus {
        0
    }

    /// The usage the silent output writes: its own proc, the stream count,
    /// every stream off, sized to the array.
    #[test]
    fn a_stream_usage_names_its_proc_with_every_stream_off() {
        let proc_id: AudioDeviceIOProcID = Some(nothing);
        let (usage, size) = stream_usage(proc_id, 3);
        let on = std::mem::offset_of!(AudioHardwareIOProcStreamUsage, mStreamIsOn);
        assert_eq!(size as usize, on + 12);
        assert!(usage.len() * 8 >= size as usize);
        assert_eq!(streams_on(&usage, 3), [0, 0, 0]);
        let header = usage.as_ptr().cast::<AudioHardwareIOProcStreamUsage>();
        // SAFETY: `stream_usage` wrote both fields of the header.
        let (proc_ptr, streams) = unsafe {
            (
                (&raw const (*header).mIOProc).cast::<*mut c_void>().read(),
                (*header).mNumberStreams,
            )
        };
        assert_eq!(
            proc_ptr,
            proc_id.map_or(std::ptr::null_mut(), |f| f as *mut c_void)
        );
        assert_eq!(streams, 3);
    }

    /// On the default input, an IOProc started as the HAL does uses every
    /// input stream; one started output-only uses none, read back from the
    /// HAL.
    #[test]
    #[ignore = "needs a Mac with a microphone; run with -- --ignored"]
    fn an_output_only_io_proc_leaves_every_input_stream_off() {
        let microphone: Id = read_pod(
            SYSTEM,
            kAudioHardwarePropertyDefaultInputDevice,
            kAudioObjectPropertyScopeGlobal,
        )
        .expect("a default input");
        // SAFETY: `nothing` reads no client data.
        let every = unsafe { IoProc::start(microphone, Some(nothing), std::ptr::null_mut()) }
            .expect("an IOProc on the microphone");
        let usage = every.input_stream_usage().expect("the usage reads");
        assert!(!usage.is_empty(), "the microphone has input streams");
        assert!(usage.iter().all(|on| *on != 0), "on by default: {usage:?}");
        drop(every);
        // SAFETY: as above.
        let output_only =
            unsafe { IoProc::start_output_only(microphone, Some(nothing), std::ptr::null_mut()) }
                .expect("an output-only IOProc on the microphone");
        let usage = output_only.input_stream_usage().expect("the usage reads");
        println!("output-only input stream usage: {usage:?}");
        assert!(
            usage.iter().all(|on| *on == 0),
            "every input off: {usage:?}"
        );
    }
}
