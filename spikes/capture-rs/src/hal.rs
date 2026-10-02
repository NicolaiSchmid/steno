//! The CoreAudio HAL calls behind the capture: property reads, default
//! devices, the process tap (`AudioHardwareCreateProcessTap` with a
//! `CATapDescription`), the private aggregate device and one IOProc.
//! Ports of `ProcessTap.swift`, `AggregateDevice.swift`, `AudioDevices.swift`
//! and `AudioObjectProperties.swift`. All of this runs off the audio thread.
use std::ffi::{c_void, CStr};
use std::ptr::NonNull;

use objc2::rc::Retained;
use objc2::AnyThread;
use objc2_core_audio::*;
use objc2_core_audio_types::AudioStreamBasicDescription;
use objc2_core_foundation::{CFDictionary, CFRetained, CFString};
use objc2_foundation::{NSArray, NSDictionary, NSNumber, NSObject, NSString};

pub type Id = AudioObjectID;
pub type OSStatus = i32;
pub const SYSTEM: Id = kAudioObjectSystemObject as Id;

fn fourcc(code: u32) -> String {
    let bytes = code.to_be_bytes();
    if bytes.iter().all(|b| b.is_ascii_graphic() || *b == b' ') {
        String::from_utf8_lossy(&bytes).into_owned()
    } else {
        format!("{code}")
    }
}

pub fn status_error(operation: &str, status: OSStatus) -> String {
    format!("{operation} failed: OSStatus {status} ('{}')", fourcc(status as u32))
}

fn address(selector: AudioObjectPropertySelector, scope: AudioObjectPropertyScope) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress { mSelector: selector, mScope: scope, mElement: kAudioObjectPropertyElementMain }
}

/// One plain-old-data property, optionally with a qualifier.
pub fn read_pod<T: Copy>(
    id: Id,
    selector: AudioObjectPropertySelector,
    scope: AudioObjectPropertyScope,
    qualifier: Option<&[u8]>,
) -> Result<T, String> {
    let mut addr = address(selector, scope);
    let mut value: T = unsafe { std::mem::zeroed() };
    let mut size = std::mem::size_of::<T>() as u32;
    let (qsize, qptr) = match qualifier {
        Some(q) => (q.len() as u32, q.as_ptr() as *const c_void),
        None => (0, std::ptr::null()),
    };
    let status = unsafe {
        AudioObjectGetPropertyData(
            id,
            NonNull::from(&mut addr),
            qsize,
            qptr,
            NonNull::from(&mut size),
            NonNull::new(&mut value as *mut T as *mut c_void).unwrap(),
        )
    };
    if status != 0 {
        return Err(status_error(&format!("read '{}' on {id}", fourcc(selector)), status));
    }
    Ok(value)
}

pub fn read_string(id: Id, selector: AudioObjectPropertySelector, scope: AudioObjectPropertyScope) -> Result<String, String> {
    let ptr: *mut CFString = read_pod(id, selector, scope, None)?;
    let ptr = NonNull::new(ptr).ok_or("null CFString")?;
    let string: CFRetained<CFString> = unsafe { CFRetained::from_raw(ptr) };
    Ok(string.to_string())
}

pub fn write_f64(id: Id, selector: AudioObjectPropertySelector, mut value: f64) -> Result<(), String> {
    let mut addr = address(selector, kAudioObjectPropertyScopeGlobal);
    let status = unsafe {
        AudioObjectSetPropertyData(
            id,
            NonNull::from(&mut addr),
            0,
            std::ptr::null(),
            std::mem::size_of::<f64>() as u32,
            NonNull::new(&mut value as *mut f64 as *mut c_void).unwrap(),
        )
    };
    if status != 0 {
        return Err(status_error(&format!("write '{}'", fourcc(selector)), status));
    }
    Ok(())
}

/// Channel count per buffer of `kAudioDevicePropertyStreamConfiguration`.
pub fn channel_counts(id: Id, scope: AudioObjectPropertyScope) -> Vec<usize> {
    let mut addr = address(kAudioDevicePropertyStreamConfiguration, scope);
    let mut size: u32 = 0;
    let status = unsafe {
        AudioObjectGetPropertyDataSize(id, NonNull::from(&mut addr), 0, std::ptr::null(), NonNull::from(&mut size))
    };
    if status != 0 || size == 0 {
        return Vec::new();
    }
    let mut raw = vec![0u64; (size as usize).div_ceil(8)];
    let status = unsafe {
        AudioObjectGetPropertyData(
            id,
            NonNull::from(&mut addr),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
            NonNull::new(raw.as_mut_ptr() as *mut c_void).unwrap(),
        )
    };
    if status != 0 {
        return Vec::new();
    }
    let list = raw.as_ptr() as *const objc2_core_audio_types::AudioBufferList;
    unsafe {
        let count = (*list).mNumberBuffers as usize;
        let buffers = (*list).mBuffers.as_ptr();
        (0..count).map(|i| (*buffers.add(i)).mNumberChannels as usize).collect()
    }
}

pub fn default_device(selector: AudioObjectPropertySelector) -> Result<Id, String> {
    let id: Id = read_pod(SYSTEM, selector, kAudioObjectPropertyScopeGlobal, None)?;
    if id == kAudioObjectUnknown {
        return Err(format!("no default device for '{}'", fourcc(selector)));
    }
    Ok(id)
}

pub fn uid(id: Id) -> Result<String, String> {
    read_string(id, kAudioDevicePropertyDeviceUID, kAudioObjectPropertyScopeGlobal)
}

pub fn name(id: Id) -> String {
    read_string(id, kAudioObjectPropertyName, kAudioObjectPropertyScopeGlobal).unwrap_or_default()
}

/// `kAudioDevicePropertyLatency` plus `kAudioDevicePropertySafetyOffset` in `scope`.
pub fn latency_frames(id: Id, scope: AudioObjectPropertyScope) -> u32 {
    let latency: u32 = read_pod(id, kAudioDevicePropertyLatency, scope, None).unwrap_or(0);
    let safety: u32 = read_pod(id, kAudioDevicePropertySafetyOffset, scope, None).unwrap_or(0);
    latency + safety
}

pub fn nominal_sample_rate(id: Id) -> f64 {
    read_pod(id, kAudioDevicePropertyNominalSampleRate, kAudioObjectPropertyScopeGlobal, None).unwrap_or(0.0)
}

pub fn buffer_frame_size(id: Id) -> u32 {
    read_pod(id, kAudioDevicePropertyBufferFrameSize, kAudioObjectPropertyScopeGlobal, None).unwrap_or(0)
}

/// `kAudioHardwarePropertyTranslatePIDToProcessObject` for our own pid.
pub fn own_process_object() -> Result<Id, String> {
    let pid: i32 = std::process::id() as i32;
    let id: Id = read_pod(
        SYSTEM,
        kAudioHardwarePropertyTranslatePIDToProcessObject,
        kAudioObjectPropertyScopeGlobal,
        Some(&pid.to_ne_bytes()),
    )?;
    if id == kAudioObjectUnknown {
        return Err(format!("no process object for pid {pid}"));
    }
    Ok(id)
}

/// A private, unmuted stereo global tap excluding `exclude`.
pub struct ProcessTap {
    pub id: Id,
    pub uid: String,
    pub format: AudioStreamBasicDescription,
}

impl ProcessTap {
    pub fn new(exclude: &[Id]) -> Result<Self, String> {
        let numbers: Vec<Retained<NSNumber>> = exclude.iter().map(|&o| NSNumber::numberWithUnsignedInt(o)).collect();
        let array = NSArray::from_retained_slice(&numbers);
        let description = unsafe { CATapDescription::initStereoGlobalTapButExcludeProcesses(CATapDescription::alloc(), &array) };
        unsafe {
            description.setName(&NSString::from_str("Steno system lane (rust spike)"));
            description.setPrivate(true);
            description.setMuteBehavior(CATapMuteBehavior::Unmuted);
        }
        let mut id: Id = kAudioObjectUnknown;
        let status = unsafe { AudioHardwareCreateProcessTap(Some(&description), &mut id) };
        if status != 0 {
            return Err(status_error("AudioHardwareCreateProcessTap", status));
        }
        if id == kAudioObjectUnknown {
            return Err("AudioHardwareCreateProcessTap returned no tap object".into());
        }
        let uid = unsafe { description.UUID().UUIDString() }.to_string();
        let format: AudioStreamBasicDescription =
            read_pod(id, kAudioTapPropertyFormat, kAudioObjectPropertyScopeGlobal, None).unwrap_or(unsafe { std::mem::zeroed() });
        Ok(Self { id, uid, format })
    }

    /// `[2]` for interleaved stereo, `[1, 1]` for non-interleaved.
    pub fn buffer_channel_counts(&self) -> Vec<usize> {
        let channels = (self.format.mChannelsPerFrame as usize).max(1);
        if self.format.mFormatFlags & objc2_core_audio_types::kAudioFormatFlagIsNonInterleaved != 0 {
            vec![1; channels]
        } else {
            vec![channels]
        }
    }
}

impl Drop for ProcessTap {
    fn drop(&mut self) {
        unsafe { AudioHardwareDestroyProcessTap(self.id) };
    }
}

fn key(c: &CStr) -> Retained<NSString> {
    NSString::from_str(c.to_str().unwrap())
}
fn ns_bool(b: bool) -> Retained<NSObject> {
    Retained::into_super(Retained::into_super(NSNumber::numberWithBool(b)))
}
fn ns_str(s: &str) -> Retained<NSObject> {
    Retained::into_super(NSString::from_str(s))
}
fn ns_dict(keys: &[Retained<NSString>], values: Vec<Retained<NSObject>>) -> Retained<NSDictionary<NSString, NSObject>> {
    let refs: Vec<&NSString> = keys.iter().map(|k| &**k).collect();
    NSDictionary::from_retained_objects(&refs, &values)
}

/// The private aggregate: output device as clock master, microphone as a
/// drift-compensated sub-device, the tap as a drift-compensated sub-tap
/// with `TapAutoStart`.
pub struct AggregateDevice {
    pub id: Id,
    pub uid: String,
}

impl AggregateDevice {
    pub fn new(name: &str, main_sub_device_uid: &str, sub_device_uids: &[String], tap_uids: &[String]) -> Result<Self, String> {
        let uid = format!("uno.schmid.steno.spike.aggregate.{}", std::process::id());
        let sub_devices: Vec<Retained<NSObject>> = sub_device_uids
            .iter()
            .map(|device_uid| {
                Retained::into_super(ns_dict(
                    &[key(kAudioSubDeviceUIDKey), key(kAudioSubDeviceDriftCompensationKey)],
                    vec![ns_str(device_uid), ns_bool(device_uid != main_sub_device_uid)],
                ))
            })
            .collect();
        let taps: Vec<Retained<NSObject>> = tap_uids
            .iter()
            .map(|tap_uid| {
                Retained::into_super(ns_dict(
                    &[key(kAudioSubTapUIDKey), key(kAudioSubTapDriftCompensationKey)],
                    vec![ns_str(tap_uid), ns_bool(true)],
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
            vec![
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
        // Toll-free bridge: NSDictionary is a CFDictionary.
        let cf: &CFDictionary = unsafe { &*(Retained::as_ptr(&description) as *const CFDictionary) };
        let mut id: Id = kAudioObjectUnknown;
        let status = unsafe { AudioHardwareCreateAggregateDevice(cf, NonNull::from(&mut id)) };
        if status != 0 {
            return Err(status_error("AudioHardwareCreateAggregateDevice", status));
        }
        if id == kAudioObjectUnknown {
            return Err("AudioHardwareCreateAggregateDevice returned no device".into());
        }
        Ok(Self { id, uid })
    }

    pub fn nominal_sample_rate(&self) -> f64 {
        nominal_sample_rate(self.id)
    }

    pub fn set_nominal_sample_rate(&self, rate: f64) -> Result<(), String> {
        write_f64(self.id, kAudioDevicePropertyNominalSampleRate, rate)
    }

    pub fn input_channel_counts(&self) -> Vec<usize> {
        channel_counts(self.id, kAudioObjectPropertyScopeInput)
    }
}

impl Drop for AggregateDevice {
    fn drop(&mut self) {
        unsafe { AudioHardwareDestroyAggregateDevice(self.id) };
    }
}

/// One started IOProc; `AudioDeviceStop` then `AudioDeviceDestroyIOProcID` on drop.
pub struct IoProc {
    device: Id,
    proc_id: AudioDeviceIOProcID,
}

impl IoProc {
    /// # Safety
    /// `client` must outlive the IOProc and be valid for the callback.
    pub unsafe fn start(device: Id, callback: AudioDeviceIOProc, client: *mut c_void) -> Result<Self, String> {
        let mut proc_id: AudioDeviceIOProcID = None;
        let status = AudioDeviceCreateIOProcID(device, callback, client, NonNull::from(&mut proc_id));
        if status != 0 {
            return Err(status_error("AudioDeviceCreateIOProcID", status));
        }
        let status = AudioDeviceStart(device, proc_id);
        if status != 0 {
            AudioDeviceDestroyIOProcID(device, proc_id);
            return Err(status_error("AudioDeviceStart", status));
        }
        Ok(Self { device, proc_id })
    }
}

impl Drop for IoProc {
    fn drop(&mut self) {
        unsafe {
            AudioDeviceStop(self.device, self.proc_id);
            AudioDeviceDestroyIOProcID(self.device, self.proc_id);
        }
    }
}
