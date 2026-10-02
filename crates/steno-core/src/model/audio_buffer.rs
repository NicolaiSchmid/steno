//! The one audio format the speech boundary accepts.
//! Swift: `Sources/StenoCore/Model/Transcript.swift` (`AudioBuffer16k`).

use super::TimeRange;

/// Mono `f32` audio at 16 kHz, the only format speech engines and diarizers
/// accept. Decoded one lane at a time so at most one buffer is alive.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct AudioBuffer16k {
    pub samples: Vec<f32>,
}

impl AudioBuffer16k {
    /// Samples per second; the type's name is the promise.
    pub const SAMPLE_RATE: f64 = 16_000.0;

    #[must_use]
    pub fn new(samples: Vec<f32>) -> Self {
        AudioBuffer16k { samples }
    }

    /// `seconds` of silence, rounded down to whole samples.
    #[must_use]
    pub fn silence(seconds: f64) -> Self {
        AudioBuffer16k {
            samples: vec![0.0; Self::sample_index(seconds, false)],
        }
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.samples.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    /// Length in seconds.
    #[must_use]
    pub fn duration(&self) -> f64 {
        // Exact up to 2^53 samples, far beyond any recording.
        #[allow(clippy::cast_precision_loss)]
        let count = self.samples.len() as f64;
        count / Self::SAMPLE_RATE
    }

    /// The samples covering `range`, clamped to the buffer: the lower bound
    /// rounds down, the upper bound rounds up, as in Swift.
    #[must_use]
    pub fn slice(&self, range: TimeRange) -> AudioBuffer16k {
        let lower = Self::sample_index(range.lower, false);
        let upper = Self::sample_index(range.upper, true).min(self.samples.len());
        if lower >= upper {
            return AudioBuffer16k::default();
        }
        AudioBuffer16k {
            samples: self.samples[lower..upper].to_vec(),
        }
    }

    /// The sample index of `seconds`, floored or ceiled, never below zero.
    /// NaN reads as zero.
    fn sample_index(seconds: f64, round_up: bool) -> usize {
        // Clamped to `0..=2^53` below (a buffer that long cannot exist), so
        // the cast neither truncates nor loses the sign.
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

    #[test]
    fn duration_is_samples_over_the_rate() {
        assert_eq!(AudioBuffer16k::default().duration(), 0.0);
        assert_eq!(ramp(16_000).duration(), 1.0);
        assert_eq!(ramp(8_000).duration(), 0.5);
        assert_eq!(AudioBuffer16k::silence(0.25).len(), 4_000);
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
