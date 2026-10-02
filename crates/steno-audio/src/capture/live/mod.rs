//! The live capture backend: on macOS a CoreAudio process tap plus the
//! microphone in a private aggregate device with one IOProc, device-change
//! listeners, coalescing and the rebuild report. Elsewhere a stub that
//! fails at `start`: PipeWire is WP5b, WASAPI is WP10; both implement
//! [`CaptureBackend`](super::CaptureBackend) behind this same name.
//! Swift: `Sources/StenoAudio/Capture/LiveCaptureBackend.swift`.

#[cfg(target_os = "macos")]
pub mod backend;
#[cfg(target_os = "macos")]
pub mod devices;
#[cfg(target_os = "macos")]
pub mod hal;

#[cfg(target_os = "macos")]
pub use backend::LiveCaptureBackend;
#[cfg(target_os = "macos")]
pub use devices::AudioDevices;

#[cfg(not(target_os = "macos"))]
pub use stub::LiveCaptureBackend;

/// One device as `steno dev audio-devices` and the app's input picker see
/// it. `uid` is the stable identifier `Settings.input_device_uid` stores.
/// Filled in by Core Audio here, by PipeWire (WP5b) and WASAPI (WP10) on
/// the other platforms.
#[derive(Debug, Clone, PartialEq)]
pub struct AudioDeviceInfo {
    pub id: u32,
    pub uid: String,
    pub name: String,
    pub input_channels: usize,
    pub output_channels: usize,
    pub nominal_sample_rate: f64,
    pub transport_type: String,
    pub is_running_somewhere: bool,
    pub is_default_input: bool,
    pub is_default_output: bool,
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

#[cfg(not(target_os = "macos"))]
mod stub {
    use std::sync::Arc;

    use steno_core::AudioLane;

    use crate::capture::{CaptureBackend, CaptureError, CaptureStream};
    use crate::realtime::LaneFrameSink;

    /// On platforms without Core Audio the live backend exists so callers
    /// compile, and fails at `start`.
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
                "live capture needs macOS (Core Audio); PipeWire arrives in WP5b, WASAPI in WP10"
                    .into(),
            ))
        }

        fn stop(&self) {}
    }
}
