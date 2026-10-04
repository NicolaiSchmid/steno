//! The live capture backend, one per platform behind the one name
//! `LiveCaptureBackend`, each a [`CaptureBackend`](super::CaptureBackend):
//! on macOS a CoreAudio process tap plus the microphone in a private
//! aggregate device with one IOProc (`backend`); on Linux one PipeWire
//! capture stream linked to the microphone and the default sink's monitor
//! (`pipewire`, WP5b of `.plans/2026-10-02-rust-core-and-tauri-shell.md`);
//! on Windows WASAPI process loopback and the capture endpoint as two
//! streams on their own threads (`wasapi`, WP10a, not run on hardware;
//! see its module doc). All three coalesce device changes and report them for
//! the session's rebuild. Other targets get the stub, failing at `start`.
//! Swift: `Sources/StenoAudio/Capture/LiveCaptureBackend.swift`.

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
mod pipewire;
#[cfg(target_os = "linux")]
pub use pipewire::LiveCaptureBackend;

#[cfg(windows)]
pub mod wasapi;
#[cfg(windows)]
pub use wasapi::{AudioDevices, LiveCaptureBackend};

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
pub use stub::LiveCaptureBackend;

/// One device as the app's input picker (and, on the Mac,
/// `steno dev audio-devices`) sees it. `uid` is the stable identifier
/// `Settings.input_device_uid` stores. Filled in by Core Audio on macOS and
/// by WASAPI on Windows; Linux has no device list yet.
#[derive(Debug, Clone, PartialEq)]
pub struct AudioDeviceInfo {
    /// The `AudioObjectID`, valid until the device goes away; on Windows
    /// the index in the enumeration (WASAPI has no numeric ids).
    pub id: u32,
    /// The stable UID `Settings` stores.
    pub uid: String,
    /// The device's name.
    pub name: String,
    /// Input channels across its streams.
    pub input_channels: usize,
    /// Output channels across its streams.
    pub output_channels: usize,
    /// Hertz.
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
    /// Has input channels.
    #[must_use]
    pub fn is_input(&self) -> bool {
        self.input_channels > 0
    }

    /// Has output channels.
    #[must_use]
    pub fn is_output(&self) -> bool {
        self.output_channels > 0
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
        /// The stub; `start` fails.
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
