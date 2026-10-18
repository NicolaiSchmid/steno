//! The devices a capture runs on, as resolved at one moment: pure, so the
//! comparison the live backends make after a notification burst is
//! unit-tested without a HAL.

use super::configuration::DeviceChangeReason;

/// The devices a capture runs on at one moment.
#[derive(Debug, Clone, PartialEq)]
pub struct DeviceSnapshot {
    /// The default system output: the aggregate's clock master. On Linux
    /// the default sink's node name; on Windows the `eConsole` default
    /// render endpoint, which both loopbacks follow.
    pub output_uid: Option<String>,
    /// The default output, which the tap mirrors. Always `None` on Linux
    /// (the default sink stands for both outputs) and on Windows (no
    /// stream opens the `eCommunications` default).
    pub default_output_uid: Option<String>,
    /// The microphone the capture would record now, the chosen one or
    /// else the default (`chosen_or_default` on the Mac and Windows,
    /// `Graph::followed_source` on Linux); `None` without a microphone
    /// lane. On Linux the source node's name; on Windows the capture
    /// endpoint's id.
    pub input_uid: Option<String>,
    /// The output device the capture started on is still alive (on Linux
    /// its node and ports keep their `object.serial`; on Windows it is
    /// still `DEVICE_STATE_ACTIVE`).
    pub output_alive: bool,
    /// The input device the capture started on is still alive, as above.
    pub input_alive: bool,
    /// The aggregate's nominal rate; 0 once it is gone. Always 48 kHz on
    /// Linux and Windows, which resample.
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

    /// [`Self::difference`] in the microphone alone, for the re-check on
    /// the fallback: the other fields have notifications of their own, and
    /// a bad read of them would cost a rebuild every re-check.
    #[must_use]
    pub fn input_difference(&self, baseline: &DeviceSnapshot) -> Option<DeviceChangeReason> {
        (self.input_uid.is_some() && self.input_uid != baseline.input_uid)
            .then_some(DeviceChangeReason::DefaultInputChanged)
    }
}
