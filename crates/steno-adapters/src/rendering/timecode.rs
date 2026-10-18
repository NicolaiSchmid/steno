//! In-recording offsets as text.

/// Integer arithmetic over milliseconds, independent of locale.
pub struct Timecode;

impl Timecode {
    /// `"00:12:34"`: whole seconds, floored. Negative offsets clamp to zero.
    #[must_use]
    pub fn clock(offset: f64) -> String {
        let seconds = Self::milliseconds(offset) / 1000;
        format!(
            "{}:{}:{}",
            Self::pad(seconds / 3600, 2),
            Self::pad(seconds / 60 % 60, 2),
            Self::pad(seconds % 60, 2)
        )
    }

    /// `"00:12:34.567"`: the `WebVTT` timestamp, milliseconds rounded.
    #[must_use]
    pub fn vtt(offset: f64) -> String {
        let total = Self::milliseconds(offset);
        let seconds = total / 1000;
        format!(
            "{}:{}:{}.{}",
            Self::pad(seconds / 3600, 2),
            Self::pad(seconds / 60 % 60, 2),
            Self::pad(seconds % 60, 2),
            Self::pad(total % 1000, 3)
        )
    }

    /// Whole milliseconds, rounded half away from zero; zero unless finite
    /// and positive.
    #[must_use]
    pub fn milliseconds(offset: f64) -> i64 {
        if !offset.is_finite() || offset <= 0.0 {
            return 0;
        }
        // Finite, positive and already rounded: the cast is exact below 2^53.
        #[allow(clippy::cast_possible_truncation)]
        let total = (offset * 1000.0).round() as i64;
        total
    }

    #[must_use]
    pub fn pad(value: i64, width: usize) -> String {
        format!("{value:0width$}")
    }
}
