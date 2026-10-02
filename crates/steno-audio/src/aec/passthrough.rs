//! Copies the near-end to the output unchanged.
//! Swift: `Sources/StenoAudio/AEC/PassthroughEchoCanceller.swift`.

use steno_core::EchoCanceller;

/// For tests, in-person sessions and `aec-bench --engine passthrough`.
#[derive(Debug, Clone, Copy)]
pub struct PassthroughEchoCanceller {
    pub sample_rate: f64,
    pub frame_size: usize,
}

impl PassthroughEchoCanceller {
    #[must_use]
    pub fn new(sample_rate: f64, frame_size: usize) -> Self {
        Self {
            sample_rate,
            frame_size,
        }
    }
}

impl EchoCanceller for PassthroughEchoCanceller {
    fn process(&mut self, near_end: &[f32], _far_end: &[f32], out: &mut [f32]) {
        let count = near_end.len().min(out.len());
        out[..count].copy_from_slice(&near_end[..count]);
    }
}
