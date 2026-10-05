//! The ring writer handed to the backend: [`LaneRings`] with lane names, a
//! wake per callback and the device-change signal.
//! Swift: `Sources/StenoAudio/RealTime/LaneFrameSink.swift`.
//!
//! Runs on the HAL's IOProc thread (or the synthetic backend's producer
//! thread). Producer protocol (real-time safe, one producer at a time):
//! `begin_callback(frames)` reserves the whole callback on every ring (or
//! counts it as dropped for every lane and returns `false`), then one
//! `write`/`write_mixed`/`write_silence` per lane, then `end_callback()`.
//! Nothing in that path allocates or locks.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use steno_core::AudioLane;

use super::ring::LaneRingBuffer;
use super::rings::LaneRings;
use super::wake::Wake;
use crate::SAMPLE_RATE;
use crate::capture::DeviceChangeReason;

/// Runs on the backend's listener thread with what changed.
pub type DeviceChangeHandler = Box<dyn Fn(DeviceChangeReason) + Send + Sync>;

/// The producer's view of the rings; see the module doc.
pub struct LaneFrameSink {
    lanes: Vec<AudioLane>,
    rings: LaneRings,
    device_change_reported: AtomicBool,
    device_change_handler: DeviceChangeHandler,
    /// Producer-only scratch for the callback in flight. An atomic only so
    /// the sink is `Sync`; the single producer is the one writer.
    pending_frames: AtomicUsize,
}

impl std::fmt::Debug for LaneFrameSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LaneFrameSink")
            .field("lanes", &self.lanes)
            .field("rings", &self.rings)
            .finish_non_exhaustive()
    }
}

impl LaneFrameSink {
    /// Two seconds of headroom per lane at 48 kHz, no device-change handler.
    #[must_use]
    pub fn new(lanes: &[AudioLane]) -> Self {
        Self::with_handler(lanes, SAMPLE_RATE, 2.0, Box::new(|_| {}))
    }

    /// `ring_seconds` of headroom per lane absorbs a stalled consumer;
    /// `on_device_change` runs on the backend's listener thread. It must
    /// not stop or start the backend that reports to it, nor drop the last
    /// owner of that backend: the backend's `stop()` may wait for the
    /// report in flight (on Linux it does), so it would wait on itself.
    #[must_use]
    pub fn with_handler(
        lanes: &[AudioLane],
        sample_rate: f64,
        ring_seconds: f64,
        on_device_change: DeviceChangeHandler,
    ) -> Self {
        // Whole samples; the product is small and positive.
        let capacity = (sample_rate * ring_seconds) as usize;
        Self {
            lanes: lanes.to_vec(),
            rings: LaneRings::new(lanes.len(), capacity),
            device_change_reported: AtomicBool::new(false),
            device_change_handler: on_device_change,
            pending_frames: AtomicUsize::new(0),
        }
    }

    /// The lanes, in ring order.
    #[must_use]
    pub fn lanes(&self) -> &[AudioLane] {
        &self.lanes
    }

    /// The rings underneath.
    #[must_use]
    pub fn rings(&self) -> &LaneRings {
        &self.rings
    }

    // Producer (real-time)

    /// Reserves `frames` on every ring, or counts the drop on every lane and
    /// returns `false`.
    #[inline(always)]
    pub fn begin_callback(&self, frames: usize) -> bool {
        if !self.rings.reserve(frames) {
            return false;
        }
        self.pending_frames.store(frames, Ordering::Relaxed);
        true
    }

    /// One lane from a raw pointer with a stride.
    ///
    /// # Safety
    ///
    /// `source` must be valid for the callback's frames at `stride`, see
    /// [`LaneRingBuffer::write`].
    #[inline(always)]
    pub unsafe fn write(&self, lane: usize, source: *const f32, stride: usize) {
        let count = self.pending_frames.load(Ordering::Relaxed);
        // SAFETY: forwarded from the caller's guarantee.
        unsafe { self.rings.ring(lane).write(source, count, stride) };
    }

    /// One lane from a slice holding at least the callback's frames.
    #[inline(always)]
    pub fn write_slice(&self, lane: usize, source: &[f32]) {
        let count = self.pending_frames.load(Ordering::Relaxed);
        self.rings.ring(lane).write_slice(&source[..count]);
    }

    /// Two channels folded to one lane.
    ///
    /// # Safety
    ///
    /// As [`LaneRingBuffer::write_mixed`].
    #[inline(always)]
    pub unsafe fn write_mixed(
        &self,
        lane: usize,
        left: *const f32,
        right: *const f32,
        left_stride: usize,
        right_stride: usize,
    ) {
        let count = self.pending_frames.load(Ordering::Relaxed);
        // SAFETY: forwarded from the caller's guarantee.
        unsafe {
            self.rings
                .ring(lane)
                .write_mixed(left, right, count, left_stride, right_stride)
        };
    }

    /// Zeros for the callback's frames on `lane`.
    #[inline(always)]
    pub fn write_silence(&self, lane: usize) {
        let count = self.pending_frames.load(Ordering::Relaxed);
        self.rings.ring(lane).write_zeros(count);
    }

    /// Publishes the callback's frames and wakes the consumer.
    #[inline(always)]
    pub fn end_callback(&self) {
        self.rings.commit();
    }

    // Backend (any thread)

    /// The backend's listener calls this once it has resolved what changed;
    /// the first call runs the handler, later calls are ignored until
    /// [`Self::rearm_device_change`].
    pub fn report_device_change(&self, reason: DeviceChangeReason) {
        if self
            .device_change_reported
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            (self.device_change_handler)(reason);
        }
    }

    /// Lets the next `report_device_change` through again. The session
    /// calls it once the old backend is stopped and before the restart,
    /// never a producer.
    pub fn rearm_device_change(&self) {
        self.device_change_reported.store(false, Ordering::Release);
    }

    // Consumer (processing thread)

    /// Signalled once per completed callback.
    #[must_use]
    pub fn wake(&self) -> &Wake {
        self.rings.wake()
    }

    /// Ring `lane`.
    #[must_use]
    pub fn ring(&self, lane: usize) -> &LaneRingBuffer {
        self.rings.ring(lane)
    }

    /// Samples every lane has queued right now (the minimum over lanes).
    #[must_use]
    pub fn available_to_read(&self) -> usize {
        self.rings.available_to_read()
    }

    /// Ring overruns per lane, in samples; lanes without drops are absent.
    #[must_use]
    pub fn dropped_samples(&self) -> BTreeMap<AudioLane, usize> {
        self.lanes
            .iter()
            .zip(self.rings.dropped_samples())
            .filter(|(_, dropped)| *dropped > 0)
            .map(|(lane, dropped)| (*lane, dropped))
            .collect()
    }

    /// Zeroes every ring so a restart never replays stale frames. Only
    /// while no producer runs.
    pub fn clear(&self) {
        self.rings.clear();
    }
}
