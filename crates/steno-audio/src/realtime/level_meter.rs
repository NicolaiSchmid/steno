//! RMS and peak over a metering window, in dBFS, and the atomic slot the
//! processing thread publishes them through.
//! Swift: `Sources/StenoAudio/RealTime/LevelMeter.swift`.

use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

use crate::capture::{LaneLevel, LaneLevels};

/// A value type the processing thread mutates in place; `flush()` reads
/// and resets it.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct LevelMeter {
    sum_squares: f64,
    peak: f32,
    count: usize,
}

impl LevelMeter {
    /// Empty: silence until samples arrive.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds `samples` to the window.
    #[inline(always)]
    pub fn accumulate(&mut self, samples: &[f32]) {
        let mut squares = 0.0f64;
        let mut peak = self.peak;
        for &value in samples {
            squares += f64::from(value * value);
            let magnitude = value.abs();
            if magnitude > peak {
                peak = magnitude;
            }
        }
        self.sum_squares += squares;
        self.peak = peak;
        self.count += samples.len();
    }

    /// The loudest magnitude of the window so far, linear.
    #[must_use]
    pub fn linear_peak(&self) -> f32 {
        self.peak
    }

    /// The window so far, without resetting.
    #[must_use]
    pub fn current(&self) -> LaneLevel {
        if self.count == 0 {
            return LaneLevel::SILENCE;
        }
        // The count is exact in f64 far beyond any window.
        let mean = self.sum_squares / self.count as f64;
        // f32 holds the magnitude range a meter needs.
        let rms = mean.sqrt() as f32;
        LaneLevel {
            rms: Self::decibels(rms),
            peak: Self::decibels(self.peak),
        }
    }

    /// The window's level, then an empty window.
    pub fn flush(&mut self) -> LaneLevel {
        let level = self.current();
        *self = Self::default();
        level
    }

    /// dBFS of a linear magnitude; `-160` for digital silence.
    #[must_use]
    pub fn decibels(linear: f32) -> f32 {
        if linear <= 1e-8 {
            return -160.0;
        }
        (20.0 * linear.log10()).max(-160.0)
    }
}

/// The latest [`LaneLevels`] written by the processing thread as atomic bit
/// patterns, read by whoever publishes them (the session's writer thread).
/// The reader may observe a torn pair across a publish; levels feed a
/// meter, nothing else, so no lock is worth it.
#[derive(Debug)]
pub struct LevelSlot {
    mic_rms: AtomicU32,
    mic_peak: AtomicU32,
    system_rms: AtomicU32,
    system_peak: AtomicU32,
    has_system: bool,
    generation: AtomicUsize,
}

impl LevelSlot {
    /// Silence on every lane; `has_system` says whether a system lane is
    /// published at all.
    #[must_use]
    pub fn new(has_system: bool) -> Self {
        let silence = LaneLevel::SILENCE;
        Self {
            mic_rms: AtomicU32::new(silence.rms.to_bits()),
            mic_peak: AtomicU32::new(silence.peak.to_bits()),
            system_rms: AtomicU32::new(silence.rms.to_bits()),
            system_peak: AtomicU32::new(silence.peak.to_bits()),
            has_system,
            generation: AtomicUsize::new(0),
        }
    }

    /// The processing thread's levels for the next reader.
    #[inline(always)]
    pub fn publish(&self, mic: LaneLevel, system: Option<LaneLevel>) {
        self.mic_rms.store(mic.rms.to_bits(), Ordering::Relaxed);
        self.mic_peak.store(mic.peak.to_bits(), Ordering::Relaxed);
        if let Some(system) = system {
            self.system_rms
                .store(system.rms.to_bits(), Ordering::Relaxed);
            self.system_peak
                .store(system.peak.to_bits(), Ordering::Relaxed);
        }
        self.generation.fetch_add(1, Ordering::Release);
    }

    /// The publish count so far; a reader republishes when it changed.
    #[must_use]
    pub fn current_generation(&self) -> usize {
        self.generation.load(Ordering::Acquire)
    }

    /// The last published levels.
    #[must_use]
    pub fn levels(&self) -> LaneLevels {
        LaneLevels {
            mic: LaneLevel {
                rms: f32::from_bits(self.mic_rms.load(Ordering::Relaxed)),
                peak: f32::from_bits(self.mic_peak.load(Ordering::Relaxed)),
            },
            system: self.has_system.then(|| LaneLevel {
                rms: f32::from_bits(self.system_rms.load(Ordering::Relaxed)),
                peak: f32::from_bits(self.system_peak.load(Ordering::Relaxed)),
            }),
        }
    }
}
