//! Single-producer, single-consumer rings of f32 samples, one per lane,
//! mirroring `Sources/StenoAudio/RealTime/LaneRingBuffer.swift` and
//! `LaneRings.swift`. Indices are monotonically increasing sample counts in
//! atomics; the producer publishes with a Release store, the consumer
//! observes with an Acquire load. No locks, no allocation after `new`.
//!
//! The callback reserves the whole callback on every lane or refuses it
//! for every lane (so lanes stay sample-aligned) and counts the drop.
use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicUsize, Ordering};

pub struct LaneRing {
    capacity: usize,
    mask: usize,
    storage: Box<[UnsafeCell<f32>]>,
    write_index: AtomicUsize,
    read_index: AtomicUsize,
    dropped: AtomicUsize,
}

unsafe impl Sync for LaneRing {}
unsafe impl Send for LaneRing {}

impl LaneRing {
    pub fn new(capacity: usize) -> Self {
        let mut size = 2;
        while size < capacity {
            size <<= 1;
        }
        let storage: Vec<UnsafeCell<f32>> = (0..size).map(|_| UnsafeCell::new(0.0)).collect();
        Self {
            capacity: size,
            mask: size - 1,
            storage: storage.into_boxed_slice(),
            write_index: AtomicUsize::new(0),
            read_index: AtomicUsize::new(0),
            dropped: AtomicUsize::new(0),
        }
    }

    #[inline(always)]
    pub fn available_to_read(&self) -> usize {
        self.write_index.load(Ordering::Acquire) - self.read_index.load(Ordering::Acquire)
    }

    pub fn dropped_samples(&self) -> usize {
        self.dropped.load(Ordering::Relaxed)
    }

    /// Producer side. Whether `count` more samples fit right now.
    #[inline(always)]
    pub fn has_room(&self, count: usize) -> bool {
        let write = self.write_index.load(Ordering::Relaxed);
        let read = self.read_index.load(Ordering::Acquire);
        self.capacity - (write - read) >= count
    }

    #[inline(always)]
    pub fn record_drop(&self, count: usize) {
        self.dropped.fetch_add(count, Ordering::Relaxed);
    }

    /// Producer side. `count` samples from `source` every `stride` floats.
    /// The caller has checked `has_room`.
    ///
    /// # Safety
    /// `source` must be valid for `count * stride` floats (or `count` zeros
    /// when `stride == 0`), and only one producer may run at a time.
    #[inline(always)]
    pub unsafe fn write(&self, source: *const f32, count: usize, stride: usize) {
        let write = self.write_index.load(Ordering::Relaxed);
        let mut position = write & self.mask;
        for i in 0..count {
            *self.storage[position].get() = *source.add(i * stride);
            position = (position + 1) & self.mask;
        }
        self.write_index.store(write + count, Ordering::Release);
    }

    /// Producer side. The average of two channels (stereo tap folded to the
    /// mono system lane), each with its own stride.
    ///
    /// # Safety
    /// As `write`, for both pointers.
    #[inline(always)]
    pub unsafe fn write_mixed(
        &self,
        left: *const f32,
        right: *const f32,
        count: usize,
        left_stride: usize,
        right_stride: usize,
    ) {
        let write = self.write_index.load(Ordering::Relaxed);
        let mut position = write & self.mask;
        for i in 0..count {
            *self.storage[position].get() =
                (*left.add(i * left_stride) + *right.add(i * right_stride)) * 0.5;
            position = (position + 1) & self.mask;
        }
        self.write_index.store(write + count, Ordering::Release);
    }

    #[inline(always)]
    pub fn write_zeros(&self, count: usize) {
        let zero: f32 = 0.0;
        unsafe { self.write(&zero, count, 0) }
    }

    /// Consumer side. Exactly `count` samples into `destination`, or nothing
    /// and `false` when fewer are queued.
    pub fn read(&self, destination: &mut [f32]) -> bool {
        let count = destination.len();
        let read = self.read_index.load(Ordering::Relaxed);
        let write = self.write_index.load(Ordering::Acquire);
        if write - read < count {
            return false;
        }
        let mut position = read & self.mask;
        for sample in destination.iter_mut() {
            *sample = unsafe { *self.storage[position].get() };
            position = (position + 1) & self.mask;
        }
        self.read_index.store(read + count, Ordering::Release);
        true
    }
}

/// All lanes of one capture, reserved and committed together.
pub struct LaneRings {
    pub rings: Vec<LaneRing>,
}

impl LaneRings {
    pub fn new(lanes: usize, capacity: usize) -> Self {
        Self {
            rings: (0..lanes).map(|_| LaneRing::new(capacity)).collect(),
        }
    }

    /// Producer side. True when every lane has room for `count`; otherwise
    /// every lane counts the drop and nothing is written.
    #[inline(always)]
    pub fn reserve(&self, count: usize) -> bool {
        if self.rings.iter().all(|r| r.has_room(count)) {
            true
        } else {
            for ring in &self.rings {
                ring.record_drop(count);
            }
            false
        }
    }

    pub fn available_to_read(&self) -> usize {
        self.rings
            .iter()
            .map(|r| r.available_to_read())
            .min()
            .unwrap_or(0)
    }
}
