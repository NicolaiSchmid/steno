//! Acoustic echo cancellation on the capture thread.
//! Swift: `Sources/StenoCore/Protocols/EchoCanceller.swift`.

/// Acoustic echo cancellation on the mic lane with the tap as far-end
/// reference. `far_end` is the far-end signal captured at the same instant
/// as `near_end`; implementations own delay handling.
///
/// The one synchronous boundary, because it runs on the audio thread:
/// `process` **must not allocate and must not take a lock** (plan
/// invariant 5, proven by the counting allocator in `steno-audio`'s
/// tests). Everything it needs (filter state, far-end history, scratch
/// buffers) is allocated by the constructor, which is the backend's own
/// `new(sample_rate, frame_size)` rather than a trait method so the trait
/// stays dyn-compatible. The three slices have the same length, one frame.
///
/// `Send` without `Sync`: the capture session owns one canceller and calls
/// it from one thread; the methods take `&mut self` because the filter
/// adapts on every frame.
pub trait EchoCanceller: Send {
    fn process(&mut self, near_end: &[f32], far_end: &[f32], out: &mut [f32]);

    /// Forgets every adapted state (the filter, the far-end history). The
    /// capture session calls it on every start so a new recording, possibly
    /// on other devices, never begins with the previous meeting's echo path.
    /// Stateless cancellers keep the empty default.
    fn reset(&mut self) {}
}
