//! Arbitrary-ratio resampling through a Kaiser-windowed sinc with a
//! polyphase table: the pure-Rust stand-in for `AVAudioConverter` at
//! maximum quality on rates the exact 3:1 path does not cover (the phone's
//! 44.1 kHz). No Swift equivalent.
//!
//! For every output sample the input position `n * ratio` is split into an
//! integer index and a fraction; the fraction selects two adjacent
//! sub-filters of the table (`PHASES` per input sample) which are linearly
//! interpolated. 64 taps per phase at the output rate's Nyquist (cutoff
//! 0.45 of the lower rate), beta 9. Measured from 44.1 kHz in
//! `tests/codec.rs`: within 0.3 dB to 6 kHz, -1.3 dB at 6.5 kHz, 12 kHz
//! aliases below -50 dB (the design stopband is about -69 dB); plenty for
//! speech. The exact 3:1 FIR the 48 kHz path uses is the flat one.
//!
//! [`SincStream`] runs the same filter over a signal that arrives in
//! pieces; [`SincResampler::resample`] is one piece. The table also serves
//! the processing thread's [`RateConverter`], which converts a device that
//! will not run at 48 kHz; its module doc says why it is a second filter.
//!
//! [`RateConverter`]: crate::realtime::RateConverter

use crate::writer::Resampler48kTo16k;

/// One resampler for one pair of rates; see the module doc.
#[derive(Debug, Clone)]
pub struct SincResampler {
    ratio: f64,
    /// `PHASES + 1` sub-filters of `TAPS` coefficients, the last one equal
    /// to the first shifted by a sample so interpolation never reads past
    /// the table.
    table: Vec<f32>,
    taps: usize,
}

impl SincResampler {
    /// Sub-filters per input sample.
    pub const PHASES: usize = 128;
    /// Coefficients per sub-filter.
    pub const TAPS: usize = 64;

    /// `input_rate` to `output_rate`, both in hertz.
    #[must_use]
    pub fn new(input_rate: f64, output_rate: f64) -> Self {
        Self {
            ratio: input_rate / output_rate,
            table: Self::table(input_rate, output_rate),
            taps: Self::TAPS,
        }
    }

    /// The `PHASES + 1` sub-filters of `TAPS` coefficients for one pair of
    /// rates, each normalised to unit gain. Shared with the real-time
    /// [`RateConverter`](crate::realtime::RateConverter).
    #[must_use]
    pub fn table(input_rate: f64, output_rate: f64) -> Vec<f32> {
        // The low-pass sits below the lower Nyquist, normalised to the
        // input rate; when upsampling the input's own band is the limit.
        let cutoff = 0.45 * output_rate.min(input_rate) / input_rate;
        let taps = Self::TAPS;
        let mut table = Vec::with_capacity((Self::PHASES + 1) * taps);
        for phase in 0..=Self::PHASES {
            // Phase counts are tiny.
            let fraction = phase as f64 / Self::PHASES as f64;
            let mut coefficients = Vec::with_capacity(taps);
            let centre = (taps / 2) as f64;
            let denominator = Resampler48kTo16k::bessel_i0(9.0);
            let mut sum = 0.0f64;
            for k in 0..taps {
                let x = k as f64 - centre + 1.0 - fraction;
                let sinc = if x == 0.0 {
                    2.0 * cutoff
                } else {
                    (2.0 * std::f64::consts::PI * cutoff * x).sin() / (std::f64::consts::PI * x)
                };
                let ratio_k = x / centre;
                let window = if ratio_k.abs() >= 1.0 {
                    0.0
                } else {
                    Resampler48kTo16k::bessel_i0(9.0 * (1.0 - ratio_k * ratio_k).sqrt())
                        / denominator
                };
                coefficients.push(sinc * window);
                sum += sinc * window;
            }
            // The coefficients are small; the sum is near 2 * cutoff.
            table.extend(coefficients.iter().map(|c| (c / sum) as f32));
        }
        table
    }

    /// The whole signal; the output has `ceil(len / ratio)` samples, which
    /// the caller trims to the exact expected length. The same samples as
    /// [`SincStream`] fed `input` in any number of pieces.
    #[must_use]
    pub fn resample(&self, input: &[f32]) -> Vec<f32> {
        let mut stream = SincStream::new(self.clone());
        let mut output = Vec::with_capacity((input.len() as f64 / self.ratio).ceil() as usize);
        stream.push(input.iter().copied(), &mut output);
        stream.finish(&mut output);
        output
    }

    /// Output sample `n` from the input samples `input`, the first of which
    /// is input sample `base`; taps at or past `end`, the input's length,
    /// read nothing, as do taps before the start.
    fn output_at(&self, n: usize, input: &[f32], base: usize, end: usize) -> f32 {
        let half = self.taps / 2;
        let position = n as f64 * self.ratio;
        let index = position.floor();
        let fraction = position - index;
        let index = index as usize;
        let phase = (fraction * Self::PHASES as f64) as usize;
        let blend = (fraction * Self::PHASES as f64 - phase as f64) as f32;
        let low = &self.table[phase * self.taps..(phase + 1) * self.taps];
        let high = &self.table[(phase + 1) * self.taps..(phase + 2) * self.taps];
        let mut accumulator = 0.0f32;
        for k in 0..self.taps {
            // Tap k reads the input sample at index - half + 1 + k.
            let Some(at) = (index + 1 + k).checked_sub(half) else {
                continue;
            };
            if at >= end {
                continue;
            }
            let coefficient = low[k] + (high[k] - low[k]) * blend;
            accumulator += coefficient * input[at - base];
        }
        accumulator
    }

    /// The input sample output `n`'s window is centred on.
    fn centre(&self, n: usize) -> usize {
        (n as f64 * self.ratio).floor() as usize
    }
}

/// [`SincResampler`] over a signal that arrives in pieces: an output
/// sample is computed once every input sample its taps read has arrived,
/// and the input before the earliest tap still needed is let go, so the
/// state is the taps' span plus one piece. The output is the whole-signal
/// [`SincResampler::resample`]'s, sample for sample, however the input is
/// cut.
#[derive(Debug, Clone)]
pub struct SincStream {
    resampler: SincResampler,
    /// Input samples from input sample `base` on.
    window: Vec<f32>,
    base: usize,
    /// Input samples pushed so far.
    received: usize,
    /// The next output sample.
    next: usize,
}

impl SincStream {
    /// Input samples taken into the window between computing outputs.
    const PIECE: usize = 4_096;

    /// A stream through `resampler`.
    #[must_use]
    pub fn new(resampler: SincResampler) -> Self {
        Self {
            resampler,
            window: Vec::new(),
            base: 0,
            received: 0,
            next: 0,
        }
    }

    /// Takes `input` and appends every output sample it completes.
    pub fn push(&mut self, input: impl IntoIterator<Item = f32>, output: &mut Vec<f32>) {
        let mut input = input.into_iter();
        let half = self.resampler.taps / 2;
        loop {
            let before = self.window.len();
            self.window.extend(input.by_ref().take(Self::PIECE));
            let taken = self.window.len() - before;
            if taken == 0 {
                return;
            }
            self.received += taken;
            // The last tap of output n reads input sample centre + half.
            while self.resampler.centre(self.next) + half < self.received {
                self.emit(output);
            }
            // The first tap of the next output reads centre + 1 - half.
            let keep = (self.resampler.centre(self.next) + 1).saturating_sub(half);
            if keep > self.base {
                self.window.drain(..keep - self.base);
                self.base = keep;
            }
        }
    }

    /// Appends the outputs the end of the input completes: the whole
    /// signal's `ceil(len / ratio)` in all.
    pub fn finish(&mut self, output: &mut Vec<f32>) {
        let count = (self.received as f64 / self.resampler.ratio).ceil() as usize;
        while self.next < count {
            self.emit(output);
        }
    }

    /// Appends the next output sample from the input received so far.
    fn emit(&mut self, output: &mut Vec<f32>) {
        output.push(
            self.resampler
                .output_at(self.next, &self.window, self.base, self.received),
        );
        self.next += 1;
    }
}
