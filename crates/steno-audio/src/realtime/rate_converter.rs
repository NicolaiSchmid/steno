//! One lane from the device's rate (a hands-free headset runs at 8 to
//! 24 kHz) to 48 kHz, streamed on the processing thread through
//! [`SincResampler`]'s polyphase table. The input position is kept as an
//! exact integer and remainder, so lanes never drift; output 0 is input 0.
//! `process` allocates nothing.

use crate::codec::sinc::SincResampler;

/// A streaming converter for one lane.
///
/// ```
/// use steno_audio::realtime::RateConverter;
///
/// let mut converter = RateConverter::new(24_000.0, 48_000.0, 240);
/// let mut output = vec![0.0f32; converter.max_output()];
/// let mut written = 0;
/// for _ in 0..100 {
///     written += converter.process(&[0.25; 240], &mut output);
/// }
/// assert_eq!(written, 2 * (24_000 - 32), "one second less half a window");
/// ```
#[derive(Debug, Clone)]
pub struct RateConverter {
    input_rate: u64,
    output_rate: u64,
    max_input: usize,
    table: Vec<f32>,
    /// Input not yet consumed by a window, from `history[0]`.
    history: Vec<f32>,
    filled: usize,
    /// The next output's whole input position in `history`.
    index: usize,
    /// Its fraction, in units of `1 / output_rate`.
    remainder: u64,
}

impl RateConverter {
    /// The lowest device rate the converter accepts: narrowband hands-free.
    pub const MIN_RATE: f64 = 8_000.0;
    /// The highest: four input samples per output. Content just above
    /// 24 kHz then aliases into the top of the band (a 28 kHz tone lands at
    /// 20 kHz, at -49 dB).
    pub const MAX_RATE: f64 = 192_000.0;
    /// The zeros the history starts with, so output 0 centres on input 0.
    const LEADING_ZEROS: usize = SincResampler::TAPS / 2 - 1;

    #[must_use]
    pub fn supports(rate: f64) -> bool {
        (Self::MIN_RATE..=Self::MAX_RATE).contains(&rate.round())
    }

    /// `input_rate` to `output_rate` hertz (rounded, both
    /// [supported](Self::supports)), for calls of at most `max_input`
    /// samples.
    #[must_use]
    pub fn new(input_rate: f64, output_rate: f64, max_input: usize) -> Self {
        debug_assert!(Self::supports(input_rate) && Self::supports(output_rate));
        let (input_rate, output_rate) = (input_rate.round(), output_rate.round());
        Self {
            input_rate: input_rate as u64,
            output_rate: output_rate as u64,
            max_input,
            table: SincResampler::table(input_rate, output_rate),
            // A window never holds more than `TAPS - 1` samples it has not
            // consumed, so that plus one call's input always fits.
            history: vec![0.0; SincResampler::TAPS - 1 + max_input],
            filled: Self::LEADING_ZEROS,
            index: 0,
            remainder: 0,
        }
    }

    /// The most input samples one `process` call takes.
    #[must_use]
    pub fn max_input(&self) -> usize {
        self.max_input
    }

    /// The most output samples one `process` call writes.
    #[must_use]
    pub fn max_output(&self) -> usize {
        (self.max_input as u64 * self.output_rate).div_ceil(self.input_rate) as usize + 1
    }

    /// Back to the start of a signal: zeros before input 0, position 0.
    pub fn reset(&mut self) {
        self.history.fill(0.0);
        self.filled = Self::LEADING_ZEROS;
        self.index = 0;
        self.remainder = 0;
    }

    /// Converts `input` into the front of `output` and returns how many
    /// samples it wrote.
    #[inline(always)]
    pub fn process(&mut self, input: &[f32], output: &mut [f32]) -> usize {
        let taps = SincResampler::TAPS;
        let phases = SincResampler::PHASES as u64;
        self.history[self.filled..self.filled + input.len()].copy_from_slice(input);
        self.filled += input.len();
        let mut written = 0;
        while self.index + taps <= self.filled {
            let scaled = self.remainder * phases;
            let phase = (scaled / self.output_rate) as usize;
            let blend = (scaled % self.output_rate) as f32 / self.output_rate as f32;
            let low = &self.table[phase * taps..(phase + 1) * taps];
            let high = &self.table[(phase + 1) * taps..(phase + 2) * taps];
            let window = &self.history[self.index..self.index + taps];
            let mut accumulator = 0.0f32;
            for k in 0..taps {
                accumulator += (low[k] + (high[k] - low[k]) * blend) * window[k];
            }
            output[written] = accumulator;
            written += 1;
            self.remainder += self.input_rate;
            self.index += (self.remainder / self.output_rate) as usize;
            self.remainder %= self.output_rate;
        }
        // Keep what the next window starts on.
        let consumed = self.index.min(self.filled);
        self.history.copy_within(consumed..self.filled, 0);
        self.filled -= consumed;
        self.index -= consumed;
        written
    }
}
