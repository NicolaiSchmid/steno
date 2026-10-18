//! The HAL seam under [`CaptureSession`](super::CaptureSession).

use std::sync::Arc;

use steno_core::AudioLane;

use super::configuration::CaptureError;
use super::layout::StreamLayout;
use crate::SAMPLE_RATE;
use crate::realtime::LaneFrameSink;

/// A backend delivers frames for every lane into the [`LaneFrameSink`] from
/// its own real-time context and reports device changes through the sink.
/// The session rebuilds after a change by calling `stop()` and `start`
/// again on the same backend and sink, so a backend must be restartable.
pub trait CaptureBackend: Send + Sync {
    /// Starts delivering `lanes` (in this order) and describes the stream,
    /// at [`SAMPLE_RATE`] or the device's own rate. `input_device_uid`
    /// `None` selects the default input. A UID that names no connected input
    /// records the default instead, says so in [`CaptureStream::input`],
    /// and reports a change once the chosen device is back. A chosen device
    /// that is connected but fails to open fails the start
    /// ([`CaptureError::DidNotRun`] when linked but silent).
    fn start(
        &self,
        lanes: &[AudioLane],
        input_device_uid: Option<&str>,
        sink: Arc<LaneFrameSink>,
    ) -> Result<CaptureStream, CaptureError>;

    /// Stops delivering; idempotent. No frame arrives after it returns.
    fn stop(&self);

    /// Whether a capture of `lanes` delivers callbacks whatever it hears,
    /// so that one that stops for longer than
    /// [`CaptureSession::STALL_TIMEOUT`] has stalled; only such a backend is
    /// watched. `false` by default, so a test backend whose audio ends is
    /// not taken for stalled; endpoint loopback alone is not continuous.
    ///
    /// [`CaptureSession::STALL_TIMEOUT`]: super::CaptureSession::STALL_TIMEOUT
    fn delivers_continuously(&self, lanes: &[AudioLane]) -> bool {
        let _ = lanes;
        false
    }

    /// Whether a stall of a capture of `lanes` may be the microphone's, so
    /// trying the default input in its place may help. `false` for a Mac
    /// call capture, which runs on the output's clock. The recorder's
    /// `microphone_is_the_master` (steno-services) is the strict version of
    /// this rule; a change to either goes to the other.
    fn stall_may_be_the_microphone(&self, lanes: &[AudioLane]) -> bool {
        let _ = lanes;
        true
    }

    /// Whether [`Self::probe_input`] can ask a microphone without touching
    /// the capture that records; only then does the session ask for a
    /// replaced chosen microphone on a timer. `true` on Linux only.
    fn probes_inputs(&self) -> bool {
        false
    }

    /// Whether the input `uid` names delivers audio now, asked on a stream
    /// of its own, never through the recording capture. Blocks up to the
    /// backend's start deadline; never called with the session's lock held.
    fn probe_input(&self, uid: &str) -> bool {
        let _ = uid;
        false
    }
}

/// What one started capture delivers: the device's rate, the latencies the
/// far-end delay is built from, and where each lane sits in the buffers.
#[derive(Debug, Clone, PartialEq)]
pub struct CaptureStream {
    /// The rate the lanes arrive at: [`SAMPLE_RATE`], except where the
    /// clock master will not run at it (a hands-free headset).
    pub sample_rate: f64,
    /// Latency plus safety offset of the microphone's input path, in frames
    /// at `sample_rate`.
    pub input_latency_frames: usize,
    /// Latency plus safety offset of the output path, in frames at
    /// `sample_rate`.
    pub output_latency_frames: usize,
    /// `None` for a backend without HAL buffers (synthetic).
    pub layout: Option<StreamLayout>,
    /// The microphone the capture records; `None` without a microphone lane
    /// or devices.
    pub input: Option<CaptureInput>,
}

/// The microphone a started capture records, so the app can name it while
/// it is not the one the user chose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureInput {
    /// The UID `Settings.input_device_uid` stores: the Core Audio UID, the
    /// PipeWire `node.name`, the WASAPI endpoint id.
    pub uid: String,
    /// Its name as the input picker lists it; `None` when it has none.
    pub name: Option<String>,
    /// The default input recorded in place of a chosen microphone that is
    /// missing or did not open.
    pub is_fallback: bool,
}

impl CaptureStream {
    /// 48 kHz, no latency, no HAL layout.
    pub const SYNTHETIC: CaptureStream = CaptureStream {
        sample_rate: SAMPLE_RATE,
        input_latency_frames: 0,
        output_latency_frames: 0,
        layout: None,
        input: None,
    };

    /// `samples` at the stream's rate as samples at [`SAMPLE_RATE`],
    /// rounded down ([`Self::rescaled`]).
    #[must_use]
    pub fn at_output_rate(&self, samples: usize) -> usize {
        Self::rescaled(samples, self.sample_rate, SAMPLE_RATE)
    }

    /// `frames` at `from` hertz as frames at `to` hertz, rounded down (an
    /// over-delayed far end is unrecoverable). Unchanged when the rates are
    /// equal or either is not positive.
    #[must_use]
    pub fn rescaled(frames: usize, from: f64, to: f64) -> usize {
        if from == to || !(from > 0.0 && to > 0.0) {
            return frames;
        }
        (frames as f64 * to / from).floor() as usize
    }
}
