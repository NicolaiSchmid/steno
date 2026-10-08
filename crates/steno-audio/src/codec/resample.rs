//! One lane to 16 kHz as its samples arrive: the copy at 16 kHz, the
//! writer's exact 3:1 FIR with its group delay dropped at 48 kHz, the
//! windowed sinc ([`SincStream`]) at every other rate, and the exact-length
//! rule. However the input is cut, the output is what one pass over the
//! whole lane gives. No Swift counterpart (the AVFoundation codec handed
//! `AVAudioConverter` 32 768-frame chunks).

use steno_core::AudioBuffer16k;

use super::sinc::{SincResampler, SincStream};
use crate::writer::Resampler48kTo16k;
use crate::{FRAME_SIZE, SAMPLE_RATE};

/// Resamples one lane to 16 kHz; see the module doc.
///
/// ```
/// use steno_audio::codec::LaneResampler;
///
/// let mut resampler = LaneResampler::new(44_100);
/// let mut lane = Vec::new();
/// for piece in [vec![0.25f32; 1_000], vec![0.5f32; 3_410]] {
///     resampler.push(piece, &mut lane);
/// }
/// resampler.finish(&mut lane);
/// assert_eq!(lane.len(), 1_600, "4 410 samples at 44.1 kHz are 100 ms");
/// ```
#[derive(Debug, Clone)]
pub struct LaneResampler {
    rate: u32,
    filter: Filter,
    /// Input samples pushed so far.
    received: usize,
    /// Output samples appended so far.
    produced: usize,
}

#[derive(Debug, Clone)]
enum Filter {
    Copy,
    Decimate(Box<Decimator>),
    Sinc(Box<SincStream>),
}

impl LaneResampler {
    /// A lane at `rate` hertz.
    ///
    /// # Panics
    ///
    /// When `rate` is zero.
    #[must_use]
    pub fn new(rate: u32) -> Self {
        assert!(rate > 0, "a lane needs a sample rate");
        let filter = if f64::from(rate) == AudioBuffer16k::SAMPLE_RATE {
            Filter::Copy
        } else if f64::from(rate) == SAMPLE_RATE {
            Filter::Decimate(Box::default())
        } else {
            Filter::Sinc(Box::new(SincStream::new(SincResampler::new(
                f64::from(rate),
                AudioBuffer16k::SAMPLE_RATE,
            ))))
        };
        Self {
            rate,
            filter,
            received: 0,
            produced: 0,
        }
    }

    /// Takes `input` and appends the 16 kHz samples it completes.
    pub fn push(&mut self, input: impl IntoIterator<Item = f32>, output: &mut Vec<f32>) {
        let before = output.len();
        let mut counted = input.into_iter().inspect(|_| self.received += 1);
        match &mut self.filter {
            Filter::Copy => output.extend(counted),
            Filter::Decimate(decimator) => decimator.push(&mut counted, output),
            Filter::Sinc(stream) => stream.push(counted, output),
        }
        self.produced += output.len() - before;
    }

    /// The lane's 16 kHz length so far: `round(len * 16000 / rate)`, so
    /// the 16 kHz lane lasts exactly as long as the master and segment
    /// counts stay stable.
    #[must_use]
    pub fn expected_len(&self) -> usize {
        length_at_16k(self.received, self.rate)
    }

    /// How many of the samples appended so far the finished lane keeps
    /// whatever follows; a caller may hand those on (and drop them from
    /// `output`) before `finish`, never more.
    #[must_use]
    pub fn settled(&self) -> usize {
        self.produced.min(self.expected_len())
    }

    /// Flushes the filter and trims or zero-pads the end of `output` so the
    /// lane is `expected_len()` long, counting the samples already handed on.
    pub fn finish(mut self, output: &mut Vec<f32>) {
        let before = output.len();
        match &mut self.filter {
            Filter::Copy => {}
            Filter::Decimate(decimator) => decimator.finish(output),
            Filter::Sinc(stream) => stream.finish(output),
        }
        self.produced += output.len() - before;
        let expected = self.expected_len();
        if self.produced > expected {
            output.truncate(output.len() - (self.produced - expected));
        } else {
            output.resize(output.len() + (expected - self.produced), 0.0);
        }
    }
}

/// `frames` at `rate` as a 16 kHz length: `round(frames * 16000 / rate)`.
pub(super) fn length_at_16k(frames: usize, rate: u32) -> usize {
    // Lengths are exact in f64; the result is a small positive count.
    (frames as f64 * AudioBuffer16k::SAMPLE_RATE / f64::from(rate.max(1))).round() as usize
}

/// The 48 kHz path: the sidecar writer's filter, frame by frame, compensated
/// for its 95.5-sample group delay so the lane aligns with the master like
/// a zero-phase conversion would. The live sidecar is the same filter run
/// causally, so it lags this decode by 32 samples (2 ms); Swift's
/// `AVAudioConverter` decode and causal sidecar writer had the same
/// relationship.
#[derive(Debug, Clone)]
struct Decimator {
    filter: Resampler48kTo16k,
    /// The frame being filled, `FRAME_SIZE` long once full.
    frame: Vec<f32>,
    out_frame: Vec<i16>,
    /// Outputs still to drop at the start: the group delay.
    delay: usize,
}

impl Default for Decimator {
    fn default() -> Self {
        Self {
            filter: Resampler48kTo16k::new(FRAME_SIZE),
            frame: Vec::with_capacity(FRAME_SIZE),
            out_frame: vec![0i16; FRAME_SIZE / Resampler48kTo16k::FACTOR],
            // 95.5 input samples is 31.83 output samples, so the onset
            // lands within a sample of where the master has it.
            delay: Self::DELAY_IN.div_ceil(Resampler48kTo16k::FACTOR),
        }
    }
}

impl Decimator {
    /// The filter's group delay in input samples, rounded down.
    const DELAY_IN: usize = (Resampler48kTo16k::TAPS - 1) / 2;

    fn push(&mut self, input: &mut impl Iterator<Item = f32>, output: &mut Vec<f32>) {
        loop {
            let room = FRAME_SIZE - self.frame.len();
            self.frame.extend(input.by_ref().take(room));
            if self.frame.len() < FRAME_SIZE {
                return;
            }
            self.process(output);
        }
    }

    fn process(&mut self, output: &mut Vec<f32>) {
        self.filter.process(&self.frame, &mut self.out_frame);
        self.frame.clear();
        let skip = self.delay.min(self.out_frame.len());
        self.delay -= skip;
        output.extend(
            self.out_frame[skip..]
                .iter()
                .map(|&s| f32::from(s) / 32767.0),
        );
    }

    /// The tail: the group delay (dropped from the front of the output)
    /// plus a frame of zeros, then zeros up to a whole frame.
    fn finish(&mut self, output: &mut Vec<f32>) {
        self.push(
            &mut std::iter::repeat_n(0.0f32, Self::DELAY_IN + FRAME_SIZE),
            output,
        );
        if !self.frame.is_empty() {
            self.frame.resize(FRAME_SIZE, 0.0);
            self.process(output);
        }
    }
}
