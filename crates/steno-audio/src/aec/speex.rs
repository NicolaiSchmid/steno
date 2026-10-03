//! SpeexDSP's MDF adaptive filter followed by its preprocessor for residual
//! echo suppression, behind [`steno_core::EchoCanceller`].
//! Swift: `Sources/StenoAudio/AEC/SpeexEchoCanceller.swift`.
//!
//! 48 kHz on 10 ms frames with a 200 ms tail by default. Float in and out;
//! `i16` scratch buffers for the C API are allocated once in `new`.
//! `process` allocates nothing and takes no locks: Speex works inside the
//! state it allocated up front. One frame of far-end per frame of near-end,
//! captured at the same instant; the processing thread owns any extra
//! delay.
//!
//! The FFI surface is declared by hand (the spike's `speex.rs`); the C
//! sources are vendored under `vendor/speexdsp` and compiled by `build.rs`.

use std::ffi::c_void;
use std::os::raw::c_int;

use steno_core::EchoCanceller;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EchoCancellerError {
    #[error("echo canceller could not start: {0}")]
    InitialisationFailed(String),
}

#[repr(C)]
struct SpeexEchoState {
    _private: [u8; 0],
}
#[repr(C)]
struct SpeexPreprocessState {
    _private: [u8; 0],
}

const SPEEX_ECHO_SET_SAMPLING_RATE: c_int = 24;
const SPEEX_PREPROCESS_SET_DENOISE: c_int = 0;
const SPEEX_PREPROCESS_SET_AGC: c_int = 2;
const SPEEX_PREPROCESS_SET_VAD: c_int = 4;
const SPEEX_PREPROCESS_SET_ECHO_SUPPRESS: c_int = 20;
const SPEEX_PREPROCESS_SET_ECHO_SUPPRESS_ACTIVE: c_int = 22;
const SPEEX_PREPROCESS_SET_ECHO_STATE: c_int = 24;

unsafe extern "C" {
    fn speex_echo_state_init(frame_size: c_int, filter_length: c_int) -> *mut SpeexEchoState;
    fn speex_echo_state_destroy(st: *mut SpeexEchoState);
    fn speex_echo_state_reset(st: *mut SpeexEchoState);
    fn speex_echo_cancellation(
        st: *mut SpeexEchoState,
        rec: *const i16,
        play: *const i16,
        out: *mut i16,
    );
    fn speex_echo_ctl(st: *mut SpeexEchoState, request: c_int, ptr: *mut c_void) -> c_int;
    fn speex_preprocess_state_init(
        frame_size: c_int,
        sampling_rate: c_int,
    ) -> *mut SpeexPreprocessState;
    fn speex_preprocess_state_destroy(st: *mut SpeexPreprocessState);
    fn speex_preprocess_run(st: *mut SpeexPreprocessState, x: *mut i16) -> c_int;
    fn speex_preprocess_ctl(
        st: *mut SpeexPreprocessState,
        request: c_int,
        ptr: *mut c_void,
    ) -> c_int;
}

pub struct SpeexEchoCanceller {
    sample_rate: f64,
    frame_size: usize,
    tail_length: usize,
    echo: *mut SpeexEchoState,
    preprocess: *mut SpeexPreprocessState,
    near: Vec<i16>,
    far: Vec<i16>,
    out: Vec<i16>,
}

// SAFETY: the Speex states are plain heap memory touched only through
// `&mut self`; moving the owner to another thread is sound.
unsafe impl Send for SpeexEchoCanceller {}

impl std::fmt::Debug for SpeexEchoCanceller {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SpeexEchoCanceller")
            .field("sample_rate", &self.sample_rate)
            .field("frame_size", &self.frame_size)
            .field("tail_length", &self.tail_length)
            .finish_non_exhaustive()
    }
}

impl SpeexEchoCanceller {
    /// Tail 200 ms of `sample_rate`, Speex's default suppression.
    pub fn new(sample_rate: f64, frame_size: usize) -> Result<Self, EchoCancellerError> {
        let tail = (sample_rate * 0.2) as usize;
        Self::with_tail(sample_rate, frame_size, tail, -40, -15)
    }

    /// `residual_suppression` is the preprocessor's echo suppression in dB
    /// (negative), `residual_suppression_active` the same while near-end
    /// speech is present; Speex's defaults are -40 and -15.
    pub fn with_tail(
        sample_rate: f64,
        frame_size: usize,
        tail_length: usize,
        residual_suppression: i32,
        residual_suppression_active: i32,
    ) -> Result<Self, EchoCancellerError> {
        if frame_size == 0 || tail_length < frame_size {
            return Err(EchoCancellerError::InitialisationFailed(format!(
                "frame {frame_size} and tail {tail_length} must be positive with tail >= frame"
            )));
        }
        let frame = c_int::try_from(frame_size).map_err(|_| {
            EchoCancellerError::InitialisationFailed(format!("frame {frame_size} too large"))
        })?;
        let tail = c_int::try_from(tail_length).map_err(|_| {
            EchoCancellerError::InitialisationFailed(format!("tail {tail_length} too large"))
        })?;
        // Whole hertz of a sane rate.
        let mut rate = sample_rate as c_int;
        // SAFETY: plain C calls with valid arguments; null returns are
        // checked before use, and `ctl` pointers point at live locals.
        unsafe {
            let echo = speex_echo_state_init(frame, tail);
            if echo.is_null() {
                return Err(EchoCancellerError::InitialisationFailed(
                    "speex_echo_state_init returned null".into(),
                ));
            }
            speex_echo_ctl(
                echo,
                SPEEX_ECHO_SET_SAMPLING_RATE,
                (&raw mut rate).cast::<c_void>(),
            );
            let preprocess = speex_preprocess_state_init(frame, rate);
            if preprocess.is_null() {
                speex_echo_state_destroy(echo);
                return Err(EchoCancellerError::InitialisationFailed(
                    "speex_preprocess_state_init returned null".into(),
                ));
            }
            speex_preprocess_ctl(preprocess, SPEEX_PREPROCESS_SET_ECHO_STATE, echo.cast());
            let mut off: c_int = 0;
            for request in [
                SPEEX_PREPROCESS_SET_DENOISE,
                SPEEX_PREPROCESS_SET_AGC,
                SPEEX_PREPROCESS_SET_VAD,
            ] {
                speex_preprocess_ctl(preprocess, request, (&raw mut off).cast::<c_void>());
            }
            let mut suppress: c_int = residual_suppression;
            speex_preprocess_ctl(
                preprocess,
                SPEEX_PREPROCESS_SET_ECHO_SUPPRESS,
                (&raw mut suppress).cast::<c_void>(),
            );
            let mut suppress_active: c_int = residual_suppression_active;
            speex_preprocess_ctl(
                preprocess,
                SPEEX_PREPROCESS_SET_ECHO_SUPPRESS_ACTIVE,
                (&raw mut suppress_active).cast::<c_void>(),
            );
            Ok(Self {
                sample_rate,
                frame_size,
                tail_length,
                echo,
                preprocess,
                near: vec![0; frame_size],
                far: vec![0; frame_size],
                out: vec![0; frame_size],
            })
        }
    }

    #[must_use]
    pub fn sample_rate(&self) -> f64 {
        self.sample_rate
    }

    #[must_use]
    pub fn frame_size(&self) -> usize {
        self.frame_size
    }

    #[must_use]
    pub fn tail_length(&self) -> usize {
        self.tail_length
    }
}

impl EchoCanceller for SpeexEchoCanceller {
    /// `near_end`, `far_end` and `out` should hold `frame_size` samples;
    /// shorter buffers are zero-padded, longer ones truncated.
    fn process(&mut self, near_end: &[f32], far_end: &[f32], out: &mut [f32]) {
        to_i16(near_end, &mut self.near);
        to_i16(far_end, &mut self.far);
        // SAFETY: the three scratch buffers hold `frame_size` samples, the
        // size both states were created for; the states are live until drop.
        unsafe {
            speex_echo_cancellation(
                self.echo,
                self.near.as_ptr(),
                self.far.as_ptr(),
                self.out.as_mut_ptr(),
            );
            speex_preprocess_run(self.preprocess, self.out.as_mut_ptr());
        }
        for (dst, &src) in out.iter_mut().zip(&self.out) {
            *dst = f32::from(src) / 32768.0;
        }
    }

    /// Forgets the adaptive filter (a device change, a new recording).
    fn reset(&mut self) {
        // SAFETY: the state is live until drop.
        unsafe { speex_echo_state_reset(self.echo) }
    }
}

impl Drop for SpeexEchoCanceller {
    fn drop(&mut self) {
        // SAFETY: both states were created in `with_tail` and are destroyed
        // exactly once here.
        unsafe {
            speex_preprocess_state_destroy(self.preprocess);
            speex_echo_state_destroy(self.echo);
        }
    }
}

/// Float to the C API's `i16`, zero-padding a short source.
#[inline(always)]
fn to_i16(source: &[f32], destination: &mut [i16]) {
    let available = source.len().min(destination.len());
    for (dst, &src) in destination[..available].iter_mut().zip(source) {
        // Clamped to the i16 range first, so the cast is exact.
        *dst = (src * 32767.0).round().clamp(-32768.0, 32767.0) as i16;
    }
    for sample in &mut destination[available..] {
        *sample = 0;
    }
}
