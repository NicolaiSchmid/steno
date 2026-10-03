//! The HAL applies `kAudioDevicePropertyNominalSampleRate` asynchronously.
//! Swift: `Sources/StenoAudio/Capture/NominalSampleRate.swift`.
//!
//! A successful write can still read back the old rate for a few cycles,
//! and a device that cannot run at the requested rate keeps its own for
//! good. `settle` reads until the rate matches or the attempts run out, so
//! the backend fails loudly instead of labelling a 44.1 kHz master 48 kHz.

use std::time::Duration;

/// Namespace for `settle`.
pub struct NominalSampleRate;

impl NominalSampleRate {
    /// Ten reads, 20 ms apart: 200 ms at most.
    pub const ATTEMPTS: usize = 10;
    /// Between reads.
    pub const INTERVAL: Duration = Duration::from_millis(20);

    /// Returns the first rate `read()` reports that equals `target`, or the
    /// last rate read once `attempts` are used up; `wait()` runs between
    /// attempts.
    pub fn settle(
        target: f64,
        attempts: usize,
        mut read: impl FnMut() -> f64,
        mut wait: impl FnMut(),
    ) -> f64 {
        let mut rate = read();
        let mut attempt = 1;
        while rate != target && attempt < attempts {
            wait();
            rate = read();
            attempt += 1;
        }
        rate
    }
}
