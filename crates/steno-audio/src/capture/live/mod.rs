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
pub use devices::{AudioDeviceInfo, AudioDevices};

#[cfg(not(target_os = "macos"))]
pub use stub::{AudioDeviceInfo, LiveCaptureBackend};

#[cfg(not(target_os = "macos"))]
mod stub {
    use std::sync::Arc;

    use steno_core::AudioLane;

    use crate::capture::{CaptureBackend, CaptureError, CaptureStream};
    use crate::realtime::LaneFrameSink;

    /// One audio device as the input picker sees it; filled in by the
    /// PipeWire (WP5b) and WASAPI (WP10) backends.
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
