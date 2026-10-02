//! Hand-written FFI for the vendored SpeexDSP echo canceller and
//! preprocessor, and a `SpeexEchoCanceller` that mirrors
//! `Sources/StenoAudio/AEC/SpeexEchoCanceller.swift`: 48 kHz, 10 ms frames
//! (480), 200 ms tail, MDF filter then residual echo suppression at
//! -40 dB / -15 dB, denoise, AGC and VAD off. `process` allocates nothing.
use std::ffi::c_void;
use std::os::raw::c_int;

#[repr(C)]
pub struct SpeexEchoState {
    _private: [u8; 0],
}
#[repr(C)]
pub struct SpeexPreprocessState {
    _private: [u8; 0],
}

pub const SPEEX_ECHO_SET_SAMPLING_RATE: c_int = 24;
pub const SPEEX_PREPROCESS_SET_DENOISE: c_int = 0;
pub const SPEEX_PREPROCESS_SET_AGC: c_int = 2;
pub const SPEEX_PREPROCESS_SET_VAD: c_int = 4;
pub const SPEEX_PREPROCESS_SET_ECHO_SUPPRESS: c_int = 20;
pub const SPEEX_PREPROCESS_SET_ECHO_SUPPRESS_ACTIVE: c_int = 22;
pub const SPEEX_PREPROCESS_SET_ECHO_STATE: c_int = 24;

extern "C" {
    pub fn speex_echo_state_init(frame_size: c_int, filter_length: c_int) -> *mut SpeexEchoState;
    pub fn speex_echo_state_destroy(st: *mut SpeexEchoState);
    pub fn speex_echo_state_reset(st: *mut SpeexEchoState);
    pub fn speex_echo_cancellation(
        st: *mut SpeexEchoState,
        rec: *const i16,
        play: *const i16,
        out: *mut i16,
    );
    pub fn speex_echo_ctl(st: *mut SpeexEchoState, request: c_int, ptr: *mut c_void) -> c_int;
    pub fn speex_preprocess_state_init(
        frame_size: c_int,
        sampling_rate: c_int,
    ) -> *mut SpeexPreprocessState;
    pub fn speex_preprocess_state_destroy(st: *mut SpeexPreprocessState);
    pub fn speex_preprocess_run(st: *mut SpeexPreprocessState, x: *mut i16) -> c_int;
    pub fn speex_preprocess_ctl(
        st: *mut SpeexPreprocessState,
        request: c_int,
        ptr: *mut c_void,
    ) -> c_int;
}

pub struct SpeexEchoCanceller {
    pub frame_size: usize,
    pub tail_length: usize,
    echo: *mut SpeexEchoState,
    preprocess: *mut SpeexPreprocessState,
    near: Vec<i16>,
    far: Vec<i16>,
    out: Vec<i16>,
}

unsafe impl Send for SpeexEchoCanceller {}

impl SpeexEchoCanceller {
    pub fn new(sample_rate: u32, frame_size: usize, tail_length: usize) -> Result<Self, String> {
        if frame_size == 0 || tail_length < frame_size {
            return Err(format!("frame {frame_size} and tail {tail_length} invalid"));
        }
        unsafe {
            let echo = speex_echo_state_init(frame_size as c_int, tail_length as c_int);
            if echo.is_null() {
                return Err("speex_echo_state_init returned null".into());
            }
            let mut rate = sample_rate as c_int;
            speex_echo_ctl(echo, SPEEX_ECHO_SET_SAMPLING_RATE, &mut rate as *mut _ as *mut c_void);
            let preprocess = speex_preprocess_state_init(frame_size as c_int, sample_rate as c_int);
            if preprocess.is_null() {
                speex_echo_state_destroy(echo);
                return Err("speex_preprocess_state_init returned null".into());
            }
            speex_preprocess_ctl(preprocess, SPEEX_PREPROCESS_SET_ECHO_STATE, echo as *mut c_void);
            let mut off: c_int = 0;
            for request in [
                SPEEX_PREPROCESS_SET_DENOISE,
                SPEEX_PREPROCESS_SET_AGC,
                SPEEX_PREPROCESS_SET_VAD,
            ] {
                speex_preprocess_ctl(preprocess, request, &mut off as *mut _ as *mut c_void);
            }
            let mut suppress: c_int = -40;
            speex_preprocess_ctl(
                preprocess,
                SPEEX_PREPROCESS_SET_ECHO_SUPPRESS,
                &mut suppress as *mut _ as *mut c_void,
            );
            let mut suppress_active: c_int = -15;
            speex_preprocess_ctl(
                preprocess,
                SPEEX_PREPROCESS_SET_ECHO_SUPPRESS_ACTIVE,
                &mut suppress_active as *mut _ as *mut c_void,
            );
            Ok(Self {
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

    /// One frame. Slices shorter than `frame_size` are zero padded, longer
    /// ones truncated, as in the Swift implementation.
    pub fn process(&mut self, near_end: &[f32], far_end: &[f32], out: &mut [f32]) {
        to_i16(near_end, &mut self.near);
        to_i16(far_end, &mut self.far);
        unsafe {
            speex_echo_cancellation(
                self.echo,
                self.near.as_ptr(),
                self.far.as_ptr(),
                self.out.as_mut_ptr(),
            );
            speex_preprocess_run(self.preprocess, self.out.as_mut_ptr());
        }
        for (dst, &src) in out.iter_mut().zip(self.out.iter()) {
            *dst = src as f32 / 32768.0;
        }
    }

    #[allow(dead_code)]
    pub fn reset(&mut self) {
        unsafe { speex_echo_state_reset(self.echo) }
    }
}

impl Drop for SpeexEchoCanceller {
    fn drop(&mut self) {
        unsafe {
            speex_preprocess_state_destroy(self.preprocess);
            speex_echo_state_destroy(self.echo);
        }
    }
}

#[inline(always)]
fn to_i16(source: &[f32], destination: &mut [i16]) {
    let available = source.len().min(destination.len());
    for i in 0..available {
        let scaled = (source[i] * 32767.0).round();
        destination[i] = scaled.clamp(-32768.0, 32767.0) as i16;
    }
    for sample in &mut destination[available..] {
        *sample = 0;
    }
}
