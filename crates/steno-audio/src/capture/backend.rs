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
/// `LiveCaptureBackend` is the tap + aggregate + IOProc on macOS and one
/// self-linked PipeWire stream on Linux;
/// [`SyntheticCaptureBackend`](crate::testing::SyntheticCaptureBackend)
/// generates deterministic tones. The session orchestrates a rebuild after
/// a change by calling `stop()` and `start` again on the same backend and
/// the same sink, so a backend must be restartable. The WASAPI backend
/// (WP10 of `.plans/2026-10-02-rust-core-and-tauri-shell.md`) will
/// implement this trait too (see `capture::live`).
pub trait CaptureBackend: Send + Sync {
    /// Starts delivering `lanes` (in this order) at [`SAMPLE_RATE`] and
    /// describes the stream it opened. `input_device_uid` `None` selects
    /// the default input device.
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
    /// The confirmed rate; [`SAMPLE_RATE`] for every backend that started
    /// (the live one fails otherwise).
    pub sample_rate: f64,
    /// Latency plus safety offset of the microphone's input path, in frames.
    pub input_latency_frames: usize,
    /// Latency plus safety offset of the loudspeaker's output path, in
    /// frames: the tap sees a sample this long before the room hears it.
    pub output_latency_frames: usize,
    /// `None` for a backend without HAL buffers (synthetic).
    pub layout: Option<StreamLayout>,
}

impl CaptureStream {
    /// 48 kHz, no latency, no HAL layout.
    pub const SYNTHETIC: CaptureStream = CaptureStream {
        sample_rate: SAMPLE_RATE,
        input_latency_frames: 0,
        output_latency_frames: 0,
        layout: None,
    };
}
