//! A single-producer, single-consumer ring of `f32` samples for one lane.
//! Swift: `Sources/StenoAudio/RealTime/LaneRingBuffer.swift`.
//!
//! The IOProc writes, the processing thread reads. Indices are monotonically
//! increasing sample counts in atomics; the producer publishes with a
//! `Release` store, the consumer observes with an `Acquire` load. No locks,
//! no allocation after `new`.
//!
//! A write that does not fit is refused as a whole (the caller keeps lanes
//! aligned by refusing every lane of that callback) and counted in
//! `dropped_samples`.
//!
//! # Safety model
//!
//! The storage is a `Box<[UnsafeCell<f32>]>`. The one invariant that makes
//! the unsynchronised reads and writes sound is *disjoint ownership of
//! positions*: the producer only touches positions in
//! `[write_index, write_index + room)` and the consumer only positions in
//! `[read_index, write_index)`, with `room = capacity - (write - read)`.
//! Both indices only ever move forward, each is moved by exactly one side,
//! and every move is published with `Release` after the samples are in
//! place and observed with `Acquire` before they are read. The two ranges
//! never overlap, so no sample is read while it is written. `clear` breaks
//! the rule and therefore requires that no producer or consumer runs.

use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicUsize, Ordering};

/// The single-producer, single-consumer ring; see the module doc.
pub struct LaneRingBuffer {
    capacity: usize,
    mask: usize,
    storage: Box<[UnsafeCell<f32>]>,
    write_index: AtomicUsize,
    read_index: AtomicUsize,
    dropped: AtomicUsize,
}

// SAFETY: see the module doc. Shared access from exactly one producer and
// one consumer thread is sound because the two only touch disjoint ranges
// of `storage`, published through the Release/Acquire index pairs.
unsafe impl Sync for LaneRingBuffer {}
// SAFETY: moving the ring between threads moves plain data and atomics.
unsafe impl Send for LaneRingBuffer {}

impl std::fmt::Debug for LaneRingBuffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LaneRingBuffer")
            .field("capacity", &self.capacity)
            .field("available_to_read", &self.available_to_read())
            .field("dropped", &self.dropped_samples())
            .finish_non_exhaustive()
    }
}

impl LaneRingBuffer {
    /// `capacity` is rounded up to a power of two, at least 2.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        let size = capacity.max(2).next_power_of_two();
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

    /// Samples the ring holds; a power of two.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Samples the consumer may read right now.
    #[inline(always)]
    pub fn available_to_read(&self) -> usize {
        self.write_index.load(Ordering::Acquire) - self.read_index.load(Ordering::Acquire)
    }

    /// Samples the producer may write right now.
    #[inline(always)]
    pub fn available_to_write(&self) -> usize {
        self.capacity - self.available_to_read()
    }

    /// Samples refused because the ring was full.
    pub fn dropped_samples(&self) -> usize {
        self.dropped.load(Ordering::Relaxed)
    }

    /// Producer side. Whether `count` samples fit right now.
    #[inline(always)]
    pub fn has_room(&self, count: usize) -> bool {
        let write = self.write_index.load(Ordering::Relaxed);
        let read = self.read_index.load(Ordering::Acquire);
        self.capacity - (write - read) >= count
    }

    /// Producer side. Records `count` refused samples without writing.
    #[inline(always)]
    pub fn record_drop(&self, count: usize) {
        self.dropped.fetch_add(count, Ordering::Relaxed);
    }

    /// Producer side. The write index when `count` more samples fit; `None`,
    /// with the drop counted, when they do not.
    #[inline(always)]
    fn reserve(&self, count: usize) -> Option<usize> {
        let write = self.write_index.load(Ordering::Relaxed);
        let read = self.read_index.load(Ordering::Acquire);
        if self.capacity - (write - read) >= count {
            Some(write)
        } else {
            self.dropped.fetch_add(count, Ordering::Relaxed);
            None
        }
    }

    /// Producer side. Copies `count` samples read from `source` every
    /// `stride` floats (1 for a non-interleaved channel, the channel count
    /// for an interleaved buffer, 0 to repeat one value). Returns `false`
    /// and counts the drop when the samples do not fit.
    ///
    /// # Safety
    ///
    /// `source` must be valid for reads of `(count - 1) * stride + 1` floats
    /// (one float when `stride == 0`), and only one producer may run at a
    /// time.
    #[inline(always)]
    pub unsafe fn write(&self, source: *const f32, count: usize, stride: usize) -> bool {
        if count == 0 {
            return true;
        }
        let Some(write) = self.reserve(count) else {
            return false;
        };
        let mut position = write & self.mask;
        for index in 0..count {
            // SAFETY: `position` is inside `[write, write + count)`, which
            // `reserve` showed is free (not between read and write), so the
            // consumer does not read it; `source.add(index * stride)` is
            // within the caller's guarantee.
            unsafe {
                *self.storage[position].get() = *source.add(index * stride);
            }
            position = (position + 1) & self.mask;
        }
        self.write_index.store(write + count, Ordering::Release);
        true
    }

    /// Producer side. Writes the average of two channels (a stereo tap
    /// folded to the mono system lane); each channel has its own stride
    /// because the HAL may deliver them in different buffers.
    ///
    /// # Safety
    ///
    /// As [`Self::write`], for both pointers and strides.
    #[inline(always)]
    pub unsafe fn write_mixed(
        &self,
        left: *const f32,
        right: *const f32,
        count: usize,
        left_stride: usize,
        right_stride: usize,
    ) -> bool {
        if count == 0 {
            return true;
        }
        let Some(write) = self.reserve(count) else {
            return false;
        };
        let mut position = write & self.mask;
        for index in 0..count {
            // SAFETY: as in `write`.
            unsafe {
                *self.storage[position].get() = f32::midpoint(
                    *left.add(index * left_stride),
                    *right.add(index * right_stride),
                );
            }
            position = (position + 1) & self.mask;
        }
        self.write_index.store(write + count, Ordering::Release);
        true
    }

    /// Producer side. Copies a slice; the safe form of [`Self::write`] for
    /// producers that own their buffers (the synthetic backend, the relay,
    /// the far-end delay line).
    #[inline(always)]
    pub fn write_slice(&self, source: &[f32]) -> bool {
        // SAFETY: a slice is valid for `len()` reads at stride 1; the caller
        // is the single producer by the type's contract.
        unsafe { self.write(source.as_ptr(), source.len(), 1) }
    }

    /// Producer side. Writes `count` zeros (a buffer the HAL delivered
    /// without data keeps the lane aligned; the far-end delay line is primed
    /// with them).
    #[inline(always)]
    pub fn write_zeros(&self, count: usize) -> bool {
        let zero: f32 = 0.0;
        // SAFETY: stride 0 reads the one local float `count` times.
        unsafe { self.write(&raw const zero, count, 0) }
    }

    /// Consumer side. Copies exactly `destination.len()` samples or, when
    /// fewer are available, copies nothing and returns `false`.
    #[inline(always)]
    pub fn read(&self, destination: &mut [f32]) -> bool {
        let count = destination.len();
        if count == 0 {
            return true;
        }
        let read = self.read_index.load(Ordering::Relaxed);
        let write = self.write_index.load(Ordering::Acquire);
        if write - read < count {
            return false;
        }
        let mut position = read & self.mask;
        for sample in destination.iter_mut() {
            // SAFETY: `position` is inside `[read, write)`, which the
            // producer published with Release and will not write again
            // until `read_index` has moved past it.
            *sample = unsafe { *self.storage[position].get() };
            position = (position + 1) & self.mask;
        }
        self.read_index.store(read + count, Ordering::Release);
        true
    }

    /// Consumer side, not real-time: takes everything queued (the
    /// permission probe inspects what the tap delivered).
    pub fn drain_all(&self) -> Vec<f32> {
        let count = self.available_to_read();
        let mut samples = vec![0.0; count];
        if count > 0 {
            self.read(&mut samples);
        }
        samples
    }

    /// Empties the ring and zeroes its storage so a restart never replays
    /// stale frames. Only while no producer and no consumer runs; the
    /// `&mut` borrow is not required because the session holds the ring in
    /// an `Arc`, so the contract is documented rather than typed.
    pub fn clear(&self) {
        for cell in &self.storage {
            // SAFETY: by contract nobody else touches the storage now.
            unsafe { *cell.get() = 0.0 };
        }
        self.read_index.store(0, Ordering::SeqCst);
        self.write_index.store(0, Ordering::SeqCst);
        self.dropped.store(0, Ordering::SeqCst);
    }
}
