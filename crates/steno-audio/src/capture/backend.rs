//! The HAL seam under [`CaptureSession`](super::CaptureSession).
//! Swift: `Sources/StenoAudio/Capture/CaptureBackend.swift`.

use std::sync::Arc;

use steno_core::AudioLane;

use super::configuration::CaptureError;
use super::layout::StreamLayout;
use crate::SAMPLE_RATE;
use crate::realtime::LaneFrameSink;

/// A backend delivers frames for every lane into the [`LaneFrameSink`] from
/// its own real-time context and reports device changes through the sink.
/// `LiveCaptureBackend` is the tap + aggregate + IOProc on macOS, one
/// self-linked PipeWire stream on Linux and two WASAPI streams on Windows
/// (see `capture::live`);
/// [`SyntheticCaptureBackend`](crate::testing::SyntheticCaptureBackend)
/// generates deterministic tones. The session orchestrates a rebuild after
/// a change by calling `stop()` and `start` again on the same backend and
/// the same sink, so a backend must be restartable.
pub trait CaptureBackend: Send + Sync {
    /// Starts delivering `lanes` (in this order) and describes the stream
    /// it opened, at [`SAMPLE_RATE`] or, where the device will not run at
    /// it, at the device's own rate, which the processing thread converts.
    /// `input_device_uid` `None` selects the default input device. A live
    /// backend given a UID that names no connected input records the
    /// default input instead (the fallback), says so in
    /// [`CaptureStream::input`], and reports a change once the chosen
    /// device is back, so the rebuild's `start` returns to it; the
    /// recording never fails or ends because the chosen microphone is
    /// missing while the default input can be opened. A chosen device that
    /// is connected but fails to open fails this `start` (with
    /// [`CaptureError::DidNotRun`] when it was linked but the graph
    /// delivered nothing); the session then starts again without a UID (see
    /// `CaptureSession`).
    fn start(
        &self,
        lanes: &[AudioLane],
        input_device_uid: Option<&str>,
        sink: Arc<LaneFrameSink>,
    ) -> Result<CaptureStream, CaptureError>;

    /// Stops delivering; idempotent. No frame arrives after it returns.
    fn stop(&self);
}

/// What one started capture delivers, as the backend found it: the rate
/// the device runs at, the device latencies the far-end delay is built
/// from, and where each lane sits in the HAL's buffers.
#[derive(Debug, Clone, PartialEq)]
pub struct CaptureStream {
    /// The confirmed rate the lanes arrive at: [`SAMPLE_RATE`], except on
    /// the Mac when the clock master will not run at it (a Bluetooth
    /// headset in the hands-free profile runs at 24 or 16 kHz).
    pub sample_rate: f64,
    /// Latency plus safety offset of the microphone's input path, in frames
    /// at `sample_rate`.
    pub input_latency_frames: usize,
    /// Latency plus safety offset of the loudspeaker's output path, in
    /// frames at `sample_rate`: the tap sees a sample this long before the
    /// room hears it.
    pub output_latency_frames: usize,
    /// `None` for a backend without HAL buffers (synthetic).
    pub layout: Option<StreamLayout>,
    /// The microphone the capture records; `None` without a microphone
    /// lane, or for a backend without devices (synthetic).
    pub input: Option<CaptureInput>,
}

/// The microphone a started capture records, so the app can name it while
/// it is not the one the user chose. Rust only: Swift fails the start when
/// the chosen microphone is missing, so its stream needs no such field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureInput {
    /// The UID `Settings.input_device_uid` would store for it: the Core
    /// Audio UID, the PipeWire `node.name`, the WASAPI endpoint id.
    pub uid: String,
    /// Its name, as the input picker lists it; `None` when the system
    /// gives it none (an ID is no name to show).
    pub name: Option<String>,
    /// The fallback: the chosen microphone is not connected or did not
    /// open, so this is the default input recorded in its place.
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
    /// rounded down.
    #[must_use]
    pub fn resampled(&self, samples: usize) -> usize {
        if self.sample_rate == SAMPLE_RATE {
            return samples;
        }
        (samples as f64 * SAMPLE_RATE / self.sample_rate).floor() as usize
    }
}
