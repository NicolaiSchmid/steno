//! Device enumeration, UID lookup and default-device reads over the HAL.
//! Swift: `Sources/StenoAudio/Capture/AudioDevices.swift`.

use objc2_core_audio::{
    AudioObjectPropertySelector, kAudioDevicePropertyDeviceIsRunningSomewhere,
    kAudioDevicePropertyNominalSampleRate, kAudioDevicePropertyTransportType,
    kAudioDeviceTransportTypeAggregate, kAudioDeviceTransportTypeAirPlay,
    kAudioDeviceTransportTypeBluetooth, kAudioDeviceTransportTypeBluetoothLE,
    kAudioDeviceTransportTypeBuiltIn, kAudioDeviceTransportTypeContinuityCaptureWired,
    kAudioDeviceTransportTypeContinuityCaptureWireless, kAudioDeviceTransportTypeDisplayPort,
    kAudioDeviceTransportTypeHDMI, kAudioDeviceTransportTypeThunderbolt,
    kAudioDeviceTransportTypeUSB, kAudioDeviceTransportTypeVirtual,
    kAudioHardwarePropertyDefaultInputDevice, kAudioHardwarePropertyDefaultOutputDevice,
    kAudioHardwarePropertyDefaultSystemOutputDevice, kAudioHardwarePropertyDevices,
    kAudioObjectPropertyScopeGlobal, kAudioObjectPropertyScopeInput,
    kAudioObjectPropertyScopeOutput,
};

use super::AudioDeviceInfo;
use super::hal::{self, AGGREGATE_UID_PREFIX, CoreAudioError, Id, SYSTEM, UNKNOWN};

/// The HAL's device list and default devices, read fresh on every call
/// (nothing is cached; a device can come and go between two calls).
pub struct AudioDevices;

impl AudioDevices {
    /// Every device the HAL lists, in HAL order. Private aggregates Steno
    /// creates are included while they exist.
    pub fn all() -> Result<Vec<AudioDeviceInfo>, CoreAudioError> {
        let ids: Vec<Id> = hal::read_array(
            SYSTEM,
            kAudioHardwarePropertyDevices,
            kAudioObjectPropertyScopeGlobal,
        )?;
        let default_input = Self::default_device(kAudioHardwarePropertyDefaultInputDevice).ok();
        let default_output = Self::default_device(kAudioHardwarePropertyDefaultOutputDevice).ok();
        let default_system =
            Self::default_device(kAudioHardwarePropertyDefaultSystemOutputDevice).ok();
        Ok(ids
            .into_iter()
            .filter_map(|id| {
                let mut device = Self::info(id).ok()?;
                device.is_default_input = Some(id) == default_input;
                device.is_default_output = Some(id) == default_output;
                device.is_default_system_output = Some(id) == default_system;
                Some(device)
            })
            .collect())
    }

    /// The app's input picker: every device with input channels but the
    /// private aggregates Steno's own captures create, which a recording
    /// would otherwise list as a choice while it runs. Rust only: Swift
    /// lists them.
    pub fn inputs() -> Result<Vec<AudioDeviceInfo>, CoreAudioError> {
        Ok(Self::all()?
            .into_iter()
            .filter(|device| device.is_input() && !device.uid.starts_with(AGGREGATE_UID_PREFIX))
            .collect())
    }

    /// The device with `uid`, or `None` when it is not connected.
    pub fn device(uid: &str) -> Result<Option<AudioDeviceInfo>, CoreAudioError> {
        Ok(Self::all()?.into_iter().find(|d| d.uid == uid))
    }

    /// The system's default input device: what the microphone lane records
    /// when `Settings` names no input.
    pub fn default_input() -> Result<AudioDeviceInfo, CoreAudioError> {
        Self::info(Self::default_device(
            kAudioHardwarePropertyDefaultInputDevice,
        )?)
    }

    /// The device alerts and system sounds play through; the tap
    /// aggregate's clock master.
    pub fn default_system_output() -> Result<AudioDeviceInfo, CoreAudioError> {
        Self::info(Self::default_device(
            kAudioHardwarePropertyDefaultSystemOutputDevice,
        )?)
    }

    /// The default output device's UID, `None` when none resolves.
    #[must_use]
    pub fn default_output_uid() -> Option<String> {
        hal::uid(Self::default_device(kAudioHardwarePropertyDefaultOutputDevice).ok()?).ok()
    }

    /// The device a default-device `selector` names; an error when none is
    /// set.
    fn default_device(selector: AudioObjectPropertySelector) -> Result<Id, CoreAudioError> {
        let id: Id = hal::read_pod(SYSTEM, selector, kAudioObjectPropertyScopeGlobal, None)?;
        if id == UNKNOWN {
            return Err(CoreAudioError {
                operation: "default device".into(),
                object: SYSTEM,
                status: -1,
            });
        }
        Ok(id)
    }

    /// One device's description; the default flags are left `false` for
    /// [`Self::all`] to fill.
    fn info(id: Id) -> Result<AudioDeviceInfo, CoreAudioError> {
        let transport = hal::read_u32(
            id,
            kAudioDevicePropertyTransportType,
            kAudioObjectPropertyScopeGlobal,
        )
        .unwrap_or(0);
        Ok(AudioDeviceInfo {
            id,
            uid: hal::uid(id)?,
            name: hal::name(id),
            input_channels: hal::channel_counts(id, kAudioObjectPropertyScopeInput)
                .iter()
                .sum(),
            output_channels: hal::channel_counts(id, kAudioObjectPropertyScopeOutput)
                .iter()
                .sum(),
            nominal_sample_rate: hal::read_f64(id, kAudioDevicePropertyNominalSampleRate)
                .unwrap_or(0.0),
            transport_type: Self::transport_name(transport),
            is_running_somewhere: hal::read_bool(id, kAudioDevicePropertyDeviceIsRunningSomewhere),
            is_default_input: false,
            is_default_output: false,
            is_default_system_output: false,
        })
    }

    /// A transport type code as the text [`AudioDeviceInfo::transport_type`]
    /// carries.
    #[must_use]
    pub fn transport_name(kind: u32) -> String {
        match kind {
            0 => "unknown".into(),
            k if k == kAudioDeviceTransportTypeBuiltIn => "built-in".into(),
            k if k == kAudioDeviceTransportTypeAggregate => "aggregate".into(),
            k if k == kAudioDeviceTransportTypeVirtual => "virtual".into(),
            k if k == kAudioDeviceTransportTypeUSB => "usb".into(),
            k if k == kAudioDeviceTransportTypeBluetooth
                || k == kAudioDeviceTransportTypeBluetoothLE =>
            {
                "bluetooth".into()
            }
            k if k == kAudioDeviceTransportTypeHDMI => "hdmi".into(),
            k if k == kAudioDeviceTransportTypeDisplayPort => "displayport".into(),
            k if k == kAudioDeviceTransportTypeAirPlay => "airplay".into(),
            k if k == kAudioDeviceTransportTypeThunderbolt => "thunderbolt".into(),
            k if k == kAudioDeviceTransportTypeContinuityCaptureWired
                || k == kAudioDeviceTransportTypeContinuityCaptureWireless =>
            {
                "continuity-capture".into()
            }
            other => hal::selector_name(other),
        }
    }
}
