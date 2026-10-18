//! The ring writer handed to the backend: [`LaneRings`] with lane names, a
//! wake per callback and the device-change signal. One producer at a time:
//! `begin_callback` reserves the whole callback on every ring (or counts a
//! drop), one write per lane, then `end_callback`; nothing in that path
//! allocates or locks.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use steno_core::AudioLane;

use super::missed::Missed;
use super::ring::LaneRingBuffer;
use super::rings::LaneRings;
use super::wake::Wake;
use crate::SAMPLE_RATE;
use crate::capture::DeviceChangeReason;

/// Runs on the backend's listener thread with what changed.
pub type DeviceChangeHandler = Box<dyn Fn(DeviceChangeReason) + Send + Sync>;

/// The producer's view of the rings.
pub struct LaneFrameSink {
    lanes: Vec<AudioLane>,
    rings: LaneRings,
    device_change_reported: AtomicBool,
    device_change_handler: DeviceChangeHandler,
    /// Producer-only scratch for the callback in flight. An atomic only so
    /// the sink is `Sync`; the single producer is the one writer.
    pending_frames: AtomicUsize,
    /// Frames of every callback begun, refused ones included, for the
    /// session's stall watchdog.
    frames_offered: AtomicUsize,
    /// A backend start succeeded on this sink ([`Self::first_start`]).
    started: AtomicBool,
    before_first: Box<[AtomicUsize]>,
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

    /// `ring_seconds` of headroom per lane absorbs a stalled consumer.
    /// `on_device_change` runs on the backend's listener thread and must
    /// not stop, start or drop the backend reporting to it: its `stop()`
    /// may wait for the report in flight, so it would wait on itself.
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
            frames_offered: AtomicUsize::new(0),
            started: AtomicBool::new(false),
            before_first: lanes.iter().map(|_| AtomicUsize::new(0)).collect(),
        }
    }

    #[must_use]
    pub fn lanes(&self) -> &[AudioLane] {
        &self.lanes
    }

    #[must_use]
    pub fn rings(&self) -> &LaneRings {
        &self.rings
    }

    // Producer (real-time)

    /// Reserves `frames` on every ring, or counts the drop on every lane and
    /// returns `false`.
    #[inline(always)]
    pub fn begin_callback(&self, frames: usize) -> bool {
        // A load and a store, not a read-modify-write: there is one producer.
        let offered = self.frames_offered.load(Ordering::Relaxed);
        self.frames_offered
            .store(offered.wrapping_add(frames), Ordering::Relaxed);
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

    /// `frames` of zeros on every lane for missed audio, as one callback of
    /// their own; whether they were written (refused and counted as dropped
    /// when a ring is full). The caller bounds `frames`.
    #[inline(always)]
    pub fn fill_missed(&self, frames: usize) -> bool {
        if frames == 0 || !self.begin_callback(frames) {
            return false;
        }
        for lane in 0..self.lanes.len() {
            self.write_silence(lane);
            self.ring(lane).record_filled(frames);
        }
        self.end_callback();
        true
    }

    /// What a clock found missed before a callback: the fill through
    /// [`Self::fill_missed`], the unfilled counted. From the producer, or
    /// the backend's stop once no producer runs.
    #[inline(always)]
    pub fn write_missed(&self, missed: Missed) {
        self.fill_missed(missed.fill);
        if missed.unfilled > 0 {
            for lane in 0..self.lanes.len() {
                self.ring(lane).record_unfilled(missed.unfilled);
            }
        }
    }

    // Backend (any thread)

    /// Frames of every callback begun, refused ones included; only whether
    /// it moved means anything.
    #[must_use]
    pub fn frames_offered(&self) -> usize {
        self.frames_offered.load(Ordering::Relaxed)
    }

    /// The first call runs the handler; later calls are ignored until
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

    /// Whether this is the first backend start to succeed on this sink,
    /// marking that one has; `false` for every rebuild's restart. Missed
    /// audio before the first frame counts only on the first.
    #[must_use]
    pub fn first_start(&self) -> bool {
        !self.started.swap(true, Ordering::AcqRel)
    }

    /// Counts the audio each stream missed before its first frame,
    /// unfilled: `master` frames on every lane but the follower's, and the
    /// follower's own `(lane, frames)`. Kept apart from
    /// [`Self::unfilled_samples`], so a rebuild's gap never takes it back
    /// (see [`realtime::missed`](super::missed)).
    pub fn count_before_first(&self, master: usize, follower: Option<(usize, usize)>) {
        for (lane, count) in self.before_first.iter().enumerate() {
            let frames = match follower {
                Some((index, frames)) if index == lane => frames,
                _ => master,
            };
            count.fetch_add(frames, Ordering::Relaxed);
        }
    }

    /// Whether a device change was reported and not let through again: a
    /// backend stopped for the rebuild reads no missed audio at its stop.
    #[must_use]
    pub fn device_change_reported(&self) -> bool {
        self.device_change_reported.load(Ordering::Acquire)
    }

    /// Lets the next `report_device_change` through. Called by the session
    /// between the old backend's stop and the restart, never a producer.
    pub fn rearm_device_change(&self) {
        self.device_change_reported.store(false, Ordering::Release);
    }

    // Consumer (processing thread)

    /// Signalled once per completed callback.
    #[must_use]
    pub fn wake(&self) -> &Wake {
        self.rings.wake()
    }

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
        self.by_lane(self.rings.dropped_samples())
    }

    /// Zeros written per lane for missed audio, in samples at the rings'
    /// rate, across restarts until [`Self::clear`]; lanes without are
    /// absent.
    #[must_use]
    pub fn filled_samples(&self) -> BTreeMap<AudioLane, usize> {
        self.by_lane(self.rings.filled_samples())
    }

    /// Samples per lane of missed audio not written; as
    /// [`Self::filled_samples`].
    #[must_use]
    pub fn unfilled_samples(&self) -> BTreeMap<AudioLane, usize> {
        self.by_lane(self.rings.unfilled_samples())
    }

    /// Samples per lane missed before a stream's first frame; as
    /// [`Self::filled_samples`].
    #[must_use]
    pub fn before_first_samples(&self) -> BTreeMap<AudioLane, usize> {
        self.by_lane(
            self.before_first
                .iter()
                .map(|count| count.load(Ordering::Relaxed))
                .collect(),
        )
    }

    fn by_lane(&self, counts: Vec<usize>) -> BTreeMap<AudioLane, usize> {
        self.lanes
            .iter()
            .zip(counts)
            .filter(|(_, count)| *count > 0)
            .map(|(lane, count)| (*lane, count))
            .collect()
    }

    /// Zeroes every ring and its counts so a restart never replays stale
    /// frames. Only while no producer runs.
    pub fn clear(&self) {
        self.rings.clear();
        for count in &self.before_first {
            count.store(0, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fill_writes_zeros_on_every_lane_and_counts_them_as_missed() {
        let lanes = [AudioLane::Mic, AudioLane::System];
        let sink = LaneFrameSink::with_handler(&lanes, 1_000.0, 1.0, Box::new(|_| {}));
        assert!(sink.begin_callback(10));
        sink.write_slice(0, &[1.0; 10]);
        sink.write_slice(1, &[2.0; 10]);
        sink.end_callback();
        assert!(sink.fill_missed(6));
        assert!(!sink.fill_missed(0), "nothing to fill");
        assert_eq!(sink.available_to_read(), 16);
        let (mut mic, mut system) = ([9.0f32; 16], [9.0f32; 16]);
        assert!(sink.ring(0).read(&mut mic) && sink.ring(1).read(&mut system));
        assert_eq!(&mic[10..], [0.0; 6]);
        assert_eq!(&system[10..], [0.0; 6]);
        assert_eq!(
            sink.filled_samples(),
            BTreeMap::from([(AudioLane::Mic, 6), (AudioLane::System, 6)])
        );
        assert_eq!(sink.frames_offered(), 16, "a fill is delivery");
        assert!(sink.dropped_samples().is_empty());
        sink.ring(1).record_filled(4);
        assert_eq!(sink.filled_samples()[&AudioLane::System], 10);
        sink.write_missed(Missed {
            fill: 2,
            unfilled: 9,
        });
        assert_eq!(sink.available_to_read(), 2, "the fill, not the unfilled");
        assert_eq!(sink.filled_samples()[&AudioLane::Mic], 8);
        assert_eq!(
            sink.unfilled_samples(),
            BTreeMap::from([(AudioLane::Mic, 9), (AudioLane::System, 9)])
        );
        sink.clear();
        assert!(
            sink.filled_samples().is_empty() && sink.unfilled_samples().is_empty(),
            "cleared with the rings"
        );
    }

    /// The loss before the first frames lands on the lanes each stream
    /// feeds, apart from the running counts.
    #[test]
    fn the_loss_before_the_first_frames_is_counted_apart_on_each_streams_lanes() {
        let sink = LaneFrameSink::new(&[AudioLane::Mic, AudioLane::System]);
        sink.count_before_first(4_800, Some((1, 9_600)));
        sink.count_before_first(0, Some((1, 0)));
        assert_eq!(
            sink.before_first_samples(),
            BTreeMap::from([(AudioLane::Mic, 4_800), (AudioLane::System, 9_600)])
        );
        assert!(sink.unfilled_samples().is_empty() && sink.filled_samples().is_empty());
        let mixed = LaneFrameSink::new(&[AudioLane::Mic, AudioLane::System]);
        mixed.count_before_first(480, None);
        assert_eq!(
            mixed.before_first_samples(),
            BTreeMap::from([(AudioLane::Mic, 480), (AudioLane::System, 480)])
        );
        sink.clear();
        assert!(sink.before_first_samples().is_empty());
    }

    #[test]
    fn only_the_first_start_on_a_sink_is_the_first() {
        let sink = LaneFrameSink::new(&[AudioLane::Mic]);
        assert!(sink.first_start(), "the recording's start");
        assert!(!sink.first_start(), "a rebuild's restart");
        sink.clear();
        assert!(!sink.first_start(), "a restart after a clear");
    }

    #[test]
    fn a_fill_the_rings_cannot_take_is_dropped_not_missed() {
        let lanes = [AudioLane::Mixed];
        // 1 000 samples of ring, rounded up to 1 024.
        let sink = LaneFrameSink::with_handler(&lanes, 1_000.0, 1.0, Box::new(|_| {}));
        assert!(!sink.fill_missed(1_025));
        assert!(sink.filled_samples().is_empty());
        assert_eq!(
            sink.dropped_samples(),
            BTreeMap::from([(AudioLane::Mixed, 1_025)])
        );
        assert_eq!(sink.available_to_read(), 0);
    }
}
