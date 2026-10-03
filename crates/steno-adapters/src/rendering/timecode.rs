//! In-recording offsets as text.
//! Swift: `Sources/StenoAdapters/Rendering/Timecode.swift`.

/// Integer arithmetic over milliseconds, so the output never depends on a
/// locale or a platform's number formatting.
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

    /// Whole milliseconds, rounded half away from zero as Swift's
    /// `rounded()`; zero for anything not finite and positive.
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

    /// `value` zero-padded on the left to at least `width` digits.
    #[must_use]
    pub fn pad(value: i64, width: usize) -> String {
        format!("{value:0width$}")
    }
}
