//! The CoreAudio HAL calls behind the capture, and the one place in the
//! crate besides the ring where `unsafe` lives: property reads and writes,
//! default devices, process objects, the process tap
//! (`AudioHardwareCreateProcessTap` with a `CATapDescription`), the private
//! aggregate device, one IOProc and property listeners.
//! Swift: `Sources/StenoAudio/Capture/AudioObjectProperties.swift`,
//! `ProcessTap.swift`, `AggregateDevice.swift`, the create/start/stop of
//! `RealTime/IOProcRunner.swift`. Built on `spikes/capture-rs/src/hal.rs`.
//!
//! Everything here runs off the audio thread except the IOProc callback
//! the caller supplies to [`IoProc::start`]. The invariants each `unsafe`
//! block relies on are stated beside it; the HAL's own contract is that a
//! property buffer of the size it reported is written in full, that an
//! `AudioObjectID` stays valid until the object is destroyed or the HAL
//! reports it gone, and that a listener or IOProc is never called after
//! its removal returns.

use std::ffi::{CStr, c_void};
use std::ptr::NonNull;

use objc2::AnyThread;
use objc2::rc::Retained;
use objc2_core_audio::{
    AudioDeviceCreateIOProcID, AudioDeviceDestroyIOProcID, AudioDeviceIOProc, AudioDeviceIOProcID,
    AudioDeviceStart, AudioDeviceStop, AudioHardwareCreateAggregateDevice,
    AudioHardwareCreateProcessTap, AudioHardwareDestroyAggregateDevice,
    AudioHardwareDestroyProcessTap, AudioObjectAddPropertyListener, AudioObjectGetPropertyData,
    AudioObjectGetPropertyDataSize, AudioObjectID, AudioObjectPropertyAddress,
    AudioObjectPropertyScope, AudioObjectPropertySelector, AudioObjectRemovePropertyListener,
    AudioObjectSetPropertyData, CATapDescription, CATapMuteBehavior,
    kAudioAggregateDeviceIsPrivateKey, kAudioAggregateDeviceIsStackedKey,
    kAudioAggregateDeviceMainSubDeviceKey, kAudioAggregateDeviceNameKey,
    kAudioAggregateDeviceSubDeviceListKey, kAudioAggregateDeviceTapAutoStartKey,
    kAudioAggregateDeviceTapListKey, kAudioAggregateDeviceUIDKey,
    kAudioDevicePropertyBufferFrameSize, kAudioDevicePropertyDeviceIsAlive,
    kAudioDevicePropertyDeviceUID, kAudioDevicePropertyLatency,
    kAudioDevicePropertyNominalSampleRate, kAudioDevicePropertySafetyOffset,
    kAudioDevicePropertyStreamConfiguration, kAudioHardwarePropertyTranslatePIDToProcessObject,
    kAudioObjectPropertyElementMain, kAudioObjectPropertyName, kAudioObjectPropertyScopeGlobal,
    kAudioObjectSystemObject, kAudioObjectUnknown, kAudioSubDeviceDriftCompensationKey,
    kAudioSubDeviceUIDKey, kAudioSubTapDriftCompensationKey, kAudioSubTapUIDKey,
    kAudioTapPropertyFormat,
};
use objc2_core_audio_types::{
    AudioBufferList, AudioStreamBasicDescription, kAudioFormatFlagIsNonInterleaved,
};
use objc2_core_foundation::{CFDictionary, CFRetained, CFString};
use objc2_foundation::{NSArray, NSDictionary, NSNumber, NSObject, NSString};

use crate::capture::CaptureError;

pub type Id = AudioObjectID;
pub type OSStatus = i32;
pub const SYSTEM: Id = kAudioObjectSystemObject as Id;
pub const UNKNOWN: Id = kAudioObjectUnknown;

/// A failed Core Audio call: which property or function, on which object,
/// with which `OSStatus`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{operation} on audio object {object} failed: {status} ({})", crate::capture::four_char_code(*status))]
pub struct CoreAudioError {
    pub operation: String,
    pub object: Id,
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
    crate::capture::four_char_code(selector as i32)
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

/// One plain-old-data property, optionally with a qualifier (a PID).
pub fn read_pod<T: Copy>(
    id: Id,
    selector: AudioObjectPropertySelector,
    scope: AudioObjectPropertyScope,
    qualifier: Option<&[u8]>,
) -> Result<T, CoreAudioError> {
    let mut addr = address(selector, scope);
    // SAFETY: `T` is a plain-old-data HAL value (integer, float, object id,
    // struct of those, or a CF reference the caller takes ownership of);
    // the all-zero pattern is valid for each, and the HAL overwrites it in
    // full on success.
    let mut value: T = unsafe { std::mem::zeroed() };
    let mut size = u32::try_from(std::mem::size_of::<T>()).unwrap_or(u32::MAX);
    let (qsize, qptr) = match qualifier {
        Some(q) => (
            u32::try_from(q.len()).unwrap_or(0),
            q.as_ptr().cast::<c_void>(),
        ),
        None => (0, std::ptr::null()),
    };
    // SAFETY: every pointer refers to a live local of the size passed.
    let status = unsafe {
        AudioObjectGetPropertyData(
            id,
            NonNull::from(&mut addr),
            qsize,
            qptr,
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
    let ptr: *mut CFString = read_pod(id, selector, scope, None)?;
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
    read_pod(id, selector, scope, None)
}

/// `false` when the property reads zero and when it cannot be read at all.
#[must_use]
pub fn read_bool(id: Id, selector: AudioObjectPropertySelector) -> bool {
    read_u32(id, selector, kAudioObjectPropertyScopeGlobal).unwrap_or(0) != 0
}

pub fn read_f64(id: Id, selector: AudioObjectPropertySelector) -> Result<f64, CoreAudioError> {
    read_pod(id, selector, kAudioObjectPropertyScopeGlobal, None)
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

#[must_use]
pub fn buffer_frame_size(id: Id) -> u32 {
    read_u32(
        id,
        kAudioDevicePropertyBufferFrameSize,
        kAudioObjectPropertyScopeGlobal,
    )
    .unwrap_or(0)
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

/// The HAL's process object for `pid`.
pub fn process_object(pid: i32) -> Result<Id, CoreAudioError> {
    read_pod(
        SYSTEM,
        kAudioHardwarePropertyTranslatePIDToProcessObject,
        kAudioObjectPropertyScopeGlobal,
        Some(&pid.to_ne_bytes()),
    )
}

/// The HAL's process object for this process. Fails rather than returning
/// `kAudioObjectUnknown`: a tap that "excludes" object 0 excludes nothing
/// and would record Steno's own playback.
pub fn own_process_object() -> Result<Id, CaptureError> {
    // A pid fits i32 on macOS.
    #[allow(clippy::cast_possible_wrap)]
    let pid = std::process::id() as i32;
    let object = process_object(pid)?;
    if object == UNKNOWN {
        return Err(CaptureError::BackendFailed(format!(
            "no process object for pid {pid}"
        )));
    }
    Ok(object)
}

/// A private, unmuted global process tap of everything the Mac plays
/// except the excluded processes (Steno itself). Destroyed on drop.
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
    pub fn new(exclude: &[Id], name: &str) -> Result<Self, CaptureError> {
        let numbers: Vec<Retained<NSNumber>> = exclude
            .iter()
            .map(|&o| NSNumber::numberWithUnsignedInt(o))
            .collect();
        let array = NSArray::from_retained_slice(&numbers);
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
        let format: AudioStreamBasicDescription = read_pod(
            id,
            kAudioTapPropertyFormat,
            kAudioObjectPropertyScopeGlobal,
            None,
        )
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

impl AggregateDevice {
    pub fn new(
        name: &str,
        main_sub_device_uid: &str,
        sub_device_uids: &[String],
        tap_uids: &[String],
    ) -> Result<Self, CaptureError> {
        let uid = format!(
            "uno.schmid.steno.aggregate.{}",
            uuid::Uuid::new_v4().hyphenated().to_string().to_uppercase()
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
        channel_counts(self.id, objc2_core_audio::kAudioObjectPropertyScopeInput)
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

unsafe extern "C-unwind" fn listener_proc(
    _object: Id,
    count: u32,
    addresses: NonNull<AudioObjectPropertyAddress>,
    client: *mut c_void,
) -> OSStatus {
    // SAFETY: `client` is the `*mut ListenerHandler` registered in `add`,
    // alive until `remove` has returned (the HAL calls no listener after
    // its removal); `addresses` has `count` entries.
    unsafe {
        let handler = &*client.cast::<ListenerHandler>();
        for index in 0..count as usize {
            let address = *addresses.as_ptr().add(index);
            handler(address.mSelector);
        }
    }
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
