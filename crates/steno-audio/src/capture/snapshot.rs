//! The devices a capture runs on, as resolved at one moment.
//! Swift: `DeviceSnapshot` in `Sources/StenoAudio/Capture/LiveCaptureBackend.swift`.
//!
//! The default system output's UID (the aggregate's clock master), the
//! default output's UID (where the call plays, which the tap mirrors), the
//! microphone's UID (`None` when no microphone lane is recorded, or none
//! resolves), whether the two devices the capture started on are still
//! alive, and the aggregate's rate. Pure, so the comparison the live
//! backend makes after a notification burst is unit-tested without a HAL.

use super::configuration::DeviceChangeReason;

/// The devices a capture runs on at one moment; see the module doc.
#[derive(Debug, Clone, PartialEq)]
pub struct DeviceSnapshot {
    /// The default system output: the aggregate's clock master.
    pub output_uid: Option<String>,
    /// The default output, which the tap mirrors.
    pub default_output_uid: Option<String>,
    /// The microphone; `None` without a microphone lane.
    pub input_uid: Option<String>,
    /// The output device the capture started on still answers `DeviceIsAlive`.
    pub output_alive: bool,
    /// The input device the capture started on still answers `DeviceIsAlive`.
    pub input_alive: bool,
    /// The aggregate's nominal rate; 0 once it is gone.
    pub sample_rate: f64,
}

impl DeviceSnapshot {
    /// The first thing that differs from `baseline`, or `None` when the
    /// devices are the same, alive and at the same rate. Loss comes before
    /// movement: a dead device is why a default moved.
    #[must_use]
    pub fn difference(&self, baseline: &DeviceSnapshot) -> Option<DeviceChangeReason> {
        if baseline.output_alive && !self.output_alive {
            return Some(DeviceChangeReason::OutputDeviceGone);
        }
        if baseline.input_alive && !self.input_alive {
            return Some(DeviceChangeReason::InputDeviceGone);
        }
        if self.output_uid != baseline.output_uid {
            return Some(DeviceChangeReason::DefaultOutputChanged);
        }
        if self.default_output_uid != baseline.default_output_uid {
            return Some(DeviceChangeReason::DefaultOutputChanged);
        }
        if self.input_uid != baseline.input_uid {
            return Some(DeviceChangeReason::DefaultInputChanged);
        }
        if self.sample_rate != baseline.sample_rate {
            return Some(DeviceChangeReason::SampleRateChanged);
        }
        None
    }
}
