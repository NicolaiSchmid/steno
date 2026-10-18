//! Acoustic echo cancellation on the capture thread.

/// Acoustic echo cancellation on the mic lane with the tap as far-end
/// reference captured at the same instant; implementations own delay handling.
///
/// The one synchronous boundary, because it runs on the audio thread:
/// `process` **must not allocate and must not take a lock**. Everything it
/// needs is allocated by the backend's own constructor. The three slices are
/// one frame of the same length; an implementation processes the shortest
/// common length rather than panic on the audio thread.
pub trait EchoCanceller: Send {
    fn process(&mut self, near_end: &[f32], far_end: &[f32], out: &mut [f32]);

    /// Forgets every adapted state, so a new recording never begins with the
    /// previous meeting's echo path.
    fn reset(&mut self) {}
}
