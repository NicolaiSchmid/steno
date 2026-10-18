//! The live capture backend, one per platform behind the one name
//! `LiveCaptureBackend`: a CoreAudio process tap and aggregate device on
//! macOS (`backend`), one PipeWire stream on Linux (`pipewire`), WASAPI on
//! Windows (`wasapi`), and a stub that fails at `start` elsewhere. Beside
//! each, `AudioDevices` lists the platform's inputs for the picker.

#[cfg(target_os = "macos")]
pub mod backend;
#[cfg(target_os = "macos")]
pub mod devices;
#[cfg(target_os = "macos")]
pub(crate) mod hal;

#[cfg(target_os = "macos")]
pub use backend::LiveCaptureBackend;
#[cfg(target_os = "macos")]
pub use devices::AudioDevices;
/// The error [`AudioDevices`] returns; the HAL binding behind it is
/// crate-private.
#[cfg(target_os = "macos")]
pub use hal::CoreAudioError;

#[cfg(target_os = "linux")]
pub mod pipewire;
#[cfg(target_os = "linux")]
pub use pipewire::{AudioDevices, LiveCaptureBackend};

#[cfg(windows)]
pub mod wasapi;
#[cfg(windows)]
pub use wasapi::{AudioDevices, LiveCaptureBackend};

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
pub use stub::LiveCaptureBackend;

/// One device as the app's input picker (and `steno dev audio-devices`)
/// sees it. `uid` is the stable identifier `Settings.input_device_uid`
/// stores.
#[derive(Debug, Clone, PartialEq)]
pub struct AudioDeviceInfo {
    /// The `AudioObjectID`; on Windows the index in the enumeration, on
    /// Linux the node's global id.
    pub id: u32,
    pub uid: String,
    pub name: String,
    pub input_channels: usize,
    pub output_channels: usize,
    pub nominal_sample_rate: f64,
    /// Built-in, USB, Bluetooth, aggregate, virtual and so on, as text.
    pub transport_type: String,
    /// Some process has I/O running on it.
    pub is_running_somewhere: bool,
    /// The system's default input.
    pub is_default_input: bool,
    /// The system's default output, where calls play.
    pub is_default_output: bool,
    /// The system's default output for alerts: the aggregate's clock
    /// master. On Windows the `eConsole` render endpoint, the same one as
    /// `is_default_output`.
    pub is_default_system_output: bool,
}

impl AudioDeviceInfo {
    #[must_use]
    pub fn is_input(&self) -> bool {
        self.input_channels > 0
    }

    #[must_use]
    pub fn is_output(&self) -> bool {
        self.output_channels > 0
    }
}

/// The microphone a capture asked for `uid` records now: what `chosen`
/// finds while it is `usable`, else what `default` finds, with `true`
/// beside it when that is the fallback; `None` when neither is usable.
#[cfg(any(target_os = "macos", windows, test))]
pub(crate) fn chosen_or_default<D>(
    uid: Option<&str>,
    chosen: impl FnOnce(&str) -> Option<D>,
    default: impl FnOnce() -> Option<D>,
    usable: impl Fn(&D) -> bool,
) -> Option<(D, bool)> {
    match uid.map(|uid| chosen(uid).filter(&usable)) {
        Some(Some(device)) => Some((device, false)),
        asked => default()
            .filter(&usable)
            .map(|device| (device, asked.is_some())),
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
mod stub {
    use std::sync::Arc;

    use steno_core::AudioLane;

    use crate::capture::{CaptureBackend, CaptureError, CaptureStream};
    use crate::realtime::LaneFrameSink;

    /// On platforms without Core Audio, PipeWire or WASAPI the live
    /// backend exists so callers compile, and fails at `start`.
    #[derive(Debug, Default)]
    pub struct LiveCaptureBackend;

    impl LiveCaptureBackend {
        #[must_use]
        pub fn new() -> Self {
            Self
        }
    }

    impl CaptureBackend for LiveCaptureBackend {
        fn start(
            &self,
            _lanes: &[AudioLane],
            _input_device_uid: Option<&str>,
            _sink: Arc<LaneFrameSink>,
        ) -> Result<CaptureStream, CaptureError> {
            Err(CaptureError::BackendFailed(
                "live capture needs macOS, Linux or Windows in this build".into(),
            ))
        }

        fn stop(&self) {}
    }
}

#[cfg(test)]
mod tests {
    use super::chosen_or_default;

    #[test]
    fn the_default_stands_in_for_a_missing_chosen_device() {
        let connected = ["usb", "built-in", "speakers"];
        let pick = |uid, default| {
            chosen_or_default(
                uid,
                |uid| connected.iter().copied().find(|&d| d == uid),
                || Some(default),
                |&device| device != "speakers",
            )
        };
        assert_eq!(pick(Some("usb"), "built-in"), Some(("usb", false)));
        assert_eq!(pick(Some("gone"), "built-in"), Some(("built-in", true)));
        assert_eq!(
            pick(Some("speakers"), "built-in"),
            Some(("built-in", true)),
            "a chosen device it cannot record counts as missing"
        );
        assert_eq!(pick(None, "built-in"), Some(("built-in", false)));
        assert_eq!(pick(Some("gone"), "speakers"), None);
        assert_eq!(pick(None, "speakers"), None);
    }
}
