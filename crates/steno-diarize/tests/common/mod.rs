//! Shared helpers for the model-free tests: axis-coded embeddings and
//! audio whose samples name the speaker.

#![allow(dead_code)]

use steno_core::{AudioBuffer16k, ClusterChunk, Embedding, SpeakerCluster, SpeakerTurn, TimeRange};

pub fn range(lower: f64, upper: f64) -> TimeRange {
    TimeRange { lower, upper }
}

pub fn vector(axis: usize, scale: f32) -> Vec<f32> {
    let mut values = vec![0.0f32; Embedding::DIMENSION];
    values[axis] = scale;
    values
}

pub fn chunk(
    label: &str,
    start: f64,
    end: f64,
    axis: usize,
    quality: f32,
    scale: f32,
) -> ClusterChunk {
    ClusterChunk {
        speaker_label: label.to_owned(),
        start,
        end,
        embedding: vector(axis, scale),
        quality,
    }
}

pub fn turn(label: &str, start: f64, end: f64, quality: f32) -> SpeakerTurn {
    SpeakerTurn {
        speaker_label: label.to_owned(),
        start,
        end,
        quality,
    }
}

pub fn cluster(
    label: &str,
    ranges: Vec<TimeRange>,
    axis: usize,
    confidence: f32,
) -> SpeakerCluster {
    SpeakerCluster {
        label: label.to_owned(),
        ranges,
        embedding: Some(Embedding(vector(axis, 1.0))),
        cluster_confidence: confidence,
        sample_clip_range: None,
    }
}

/// The sample index at `seconds`.
pub fn sample_index(seconds: f64) -> usize {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let index = (seconds * AudioBuffer16k::SAMPLE_RATE) as usize;
    index
}

/// Audio where `layout` says which speaker constant fills each range; the
/// rest is zero (silence).
pub fn audio(layout: &[(f32, TimeRange)], duration: f64) -> AudioBuffer16k {
    let mut samples = vec![0.0f32; sample_index(duration)];
    for (speaker, range) in layout {
        let end = sample_index(range.upper).min(samples.len());
        for sample in samples.iter_mut().take(end).skip(sample_index(range.lower)) {
            *sample = *speaker;
        }
    }
    AudioBuffer16k::new(samples)
}

/// `SplitMix64`, so the generated-input tests are reproducible.
pub struct SplitMix64(pub u64);

impl SplitMix64 {
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in `0..n`.
    pub fn below(&mut self, n: u64) -> u64 {
        self.next_u64() % n
    }

    /// Uniform in `lower..=upper`.
    pub fn int(&mut self, lower: u64, upper: u64) -> u64 {
        lower + self.below(upper - lower + 1)
    }

    /// Uniform in `0.0..1.0`.
    pub fn unit(&mut self) -> f64 {
        #[allow(clippy::cast_precision_loss)]
        let value = (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64;
        value
    }

    pub fn float(&mut self, lower: f64, upper: f64) -> f64 {
        lower + self.unit() * (upper - lower)
    }

    pub fn bool(&mut self) -> bool {
        self.next_u64() & 1 == 1
    }
}
