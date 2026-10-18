//! The one audio format the speech boundary accepts.

use serde::{Deserialize, Serialize};

use super::TimeRange;

/// Mono `f32` audio at 16 kHz, the only format speech engines and diarizers
/// accept. Decoded one lane at a time so at most one buffer is alive.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct AudioBuffer16k {
    pub samples: Vec<f32>,
    /// What the decoder could not read and replaced by silence of its length, so
    /// later samples keep their time. None for a clean decode.
    pub damage: AudioDamage,
}

/// The parts of a recording a decoder replaced by silence, and how long that
/// silence lasts in all.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct AudioDamage {
    /// The packets that did not decode, less those the encoder priming trims.
    pub parts: u32,
    /// The silence they became, in seconds.
    pub seconds: f64,
    /// Whether the recording had no index and was read without one (a phone
    /// recorder killed mid-recording). Left out of the JSON when false.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub salvaged: bool,
}

impl AudioDamage {
    /// No part damaged and nothing salvaged: a clean decode.
    #[must_use]
    pub fn is_none(self) -> bool {
        self.parts == 0 && !self.salvaged
    }

    /// The larger count and longer silence of the two, salvaged if either was.
    #[must_use]
    pub fn max(self, other: AudioDamage) -> AudioDamage {
        AudioDamage {
            parts: self.parts.max(other.parts),
            seconds: self.seconds.max(other.seconds),
            salvaged: self.salvaged || other.salvaged,
        }
    }
}

impl AudioBuffer16k {
    /// Samples per second.
    pub const SAMPLE_RATE: f64 = 16_000.0;

    #[must_use]
    pub fn new(samples: Vec<f32>) -> Self {
        AudioBuffer16k {
            samples,
            damage: AudioDamage::default(),
        }
    }

    /// `seconds` of silence, rounded down to whole samples.
    #[must_use]
    pub fn silence(seconds: f64) -> Self {
        AudioBuffer16k::new(vec![0.0; Self::sample_index(seconds, false)])
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.samples.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    #[must_use]
    pub fn duration(&self) -> f64 {
        // Exact up to 2^53 samples, far beyond any recording.
        #[allow(clippy::cast_precision_loss)]
        let count = self.samples.len() as f64;
        count / Self::SAMPLE_RATE
    }

    /// The samples covering `range`, clamped to the buffer: the lower bound rounds
    /// down, the upper up.
    #[must_use]
    pub fn slice(&self, range: TimeRange) -> AudioBuffer16k {
        let lower = Self::sample_index(range.lower, false);
        let upper = Self::sample_index(range.upper, true).min(self.samples.len());
        if lower >= upper {
            return AudioBuffer16k::default();
        }
        AudioBuffer16k::new(self.samples[lower..upper].to_vec())
    }

    /// The sample index of `seconds`, floored or ceiled, never below zero; NaN is zero.
    fn sample_index(seconds: f64, round_up: bool) -> usize {
        // Clamped to `0..=2^53`, so the cast neither truncates nor loses the sign.
        const MAX_INDEX: f64 = 9_007_199_254_740_992.0;
        let scaled = seconds * Self::SAMPLE_RATE;
        let rounded = if round_up {
            scaled.ceil()
        } else {
            scaled.floor()
        };
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let index = rounded.clamp(0.0, MAX_INDEX) as usize;
        index
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ramp(count: usize) -> AudioBuffer16k {
        // Exact: test buffers are tiny.
        #[allow(clippy::cast_precision_loss)]
        AudioBuffer16k::new((0..count).map(|i| i as f32).collect())
    }

    /// A salvaged recording counts as damaged, the flag survives `max`, and an
    /// unsalvaged record's JSON is unchanged.
    #[test]
    fn a_salvage_is_damage_and_its_flag_is_left_out_when_false() {
        let salvaged = AudioDamage {
            salvaged: true,
            ..AudioDamage::default()
        };
        assert!(AudioDamage::default().is_none());
        assert!(!salvaged.is_none());
        let parts = AudioDamage {
            parts: 2,
            seconds: 0.05,
            salvaged: false,
        };
        assert_eq!(
            parts.max(salvaged),
            AudioDamage {
                salvaged: true,
                ..parts
            }
        );
        assert_eq!(salvaged.max(AudioDamage::default()), salvaged);

        let old = r#"{"parts":2,"seconds":0.05}"#;
        assert_eq!(serde_json::to_string(&parts).unwrap(), old);
        assert_eq!(serde_json::from_str::<AudioDamage>(old).unwrap(), parts);
        let json = serde_json::to_string(&salvaged).unwrap();
        assert_eq!(json, r#"{"parts":0,"seconds":0.0,"salvaged":true}"#);
        assert_eq!(
            serde_json::from_str::<AudioDamage>(&json).unwrap(),
            salvaged
        );
    }

    #[test]
    fn duration_is_samples_over_the_rate() {
        assert_eq!(AudioBuffer16k::default().duration(), 0.0);
        assert_eq!(ramp(16_000).duration(), 1.0);
        assert_eq!(ramp(8_000).duration(), 0.5);
    }

    #[test]
    fn silence_rounds_down_to_whole_samples() {
        assert_eq!(AudioBuffer16k::silence(0.25).len(), 4_000);
        // 0.8 of a sample is no sample; 1.6 samples is one.
        assert_eq!(AudioBuffer16k::silence(0.000_05).len(), 0);
        assert_eq!(AudioBuffer16k::silence(0.000_1).len(), 1);
        assert_eq!(AudioBuffer16k::silence(-1.0).len(), 0);
    }

    #[test]
    fn slice_floors_the_start_and_ceils_the_end() {
        let buffer = ramp(32_000);
        let slice = buffer.slice(TimeRange {
            lower: 0.5,
            upper: 1.0,
        });
        assert_eq!(slice.len(), 8_000);
        assert_eq!(slice.samples[0], 8_000.0);
        // 0.00005 s is 0.8 samples: the start floors to 0, the end ceils to 1.
        let partial = buffer.slice(TimeRange {
            lower: 0.000_05,
            upper: 0.000_05,
        });
        assert_eq!(partial.samples, vec![0.0]);
    }

    #[test]
    fn slice_clamps_to_the_buffer_and_rejects_empty_ranges() {
        let buffer = ramp(16_000);
        assert_eq!(
            buffer
                .slice(TimeRange {
                    lower: -1.0,
                    upper: 5.0
                })
                .len(),
            16_000
        );
        assert!(
            buffer
                .slice(TimeRange {
                    lower: 2.0,
                    upper: 3.0
                })
                .is_empty()
        );
        assert!(
            buffer
                .slice(TimeRange {
                    lower: 0.5,
                    upper: 0.25
                })
                .is_empty()
        );
        assert!(
            buffer
                .slice(TimeRange {
                    lower: f64::NAN,
                    upper: f64::NAN
                })
                .is_empty()
        );
    }
}
