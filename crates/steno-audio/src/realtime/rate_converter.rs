//! One lane from the device's rate to 48 kHz, converted as it streams on
//! the processing thread. On the Mac the aggregate runs at whatever its
//! clock master accepts, and a Bluetooth headset in the hands-free profile
//! accepts only 24 or 16 kHz (`.plans/2026-10-05-device-sample-rate.md`).
//! Swift: `Sources/StenoAudio/RealTime/RateConverter.swift`.
//!
//! The filter is [`SincResampler`]'s polyphase table. Output `n` sits at
//! input position `n * input_rate / output_rate`, kept as a whole index
//! and a remainder in units of `1 / output_rate`, so the position is exact
//! over any length and never drifts. The window is centred on that
//! position and the history starts with `TAPS / 2 - 1` zeros, so output 0
//! is input 0 and the lane keeps its timing; the last `TAPS / 2` input
//! samples wait for the next call. Converters with the same rates fed the
//! same input counts write the same output counts, which keeps the lanes
//! in step.
//!
//! `new` allocates everything; `process` allocates nothing.
//!
//! Two streaming filters share that table. [`SincStream`] serves the
//! decoder: it allocates as it goes and keeps a float position, and its
//! output must stay what it is, sample for sample. This one runs on the
//! processing thread, so it needs fixed buffers, and an exact position so
//! the lanes never drift over hours.
//!
//! [`SincStream`]: crate::codec::sinc::SincStream

use crate::codec::sinc::SincResampler;

/// A streaming converter for one lane; see the module doc.
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
    /// The highest: four input samples per output at 48 kHz. The 64 taps
    /// then leave a transition band about 9 kHz wide, so content just above
    /// 24 kHz aliases into the top of the band (a 28 kHz tone lands at
    /// 20 kHz, at -49 dB); below 8 kHz nothing aliases above -97 dB.
    pub const MAX_RATE: f64 = 192_000.0;
    /// The zeros the history starts with, so output 0 centres on input 0.
    const LEADING_ZEROS: usize = SincResampler::TAPS / 2 - 1;

    /// Whether a device at `rate` hertz can be converted.
    #[must_use]
    pub fn supports(rate: f64) -> bool {
        (Self::MIN_RATE..=Self::MAX_RATE).contains(&rate.round())
    }

    /// `input_rate` to `output_rate` hertz (rounded to whole hertz, both
    /// [supported](Self::supports): the session checks the device's rate
    /// before it builds one), for calls of at most `max_input` samples.
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

    /// Converts `input` (at most [`max_input`](Self::max_input) samples)
    /// into the front of `output` (at least
    /// [`max_output`](Self::max_output) long) and returns how many samples
    /// it wrote.
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
