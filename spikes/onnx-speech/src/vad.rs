//! Silero VAD through the sherpa-onnx C API: one pass over the whole file,
//! returning speech regions in samples.

use sherpa_rs_sys as ffi;
use std::ffi::CString;
use std::path::Path;

pub struct Vad {
    ptr: *const ffi::SherpaOnnxVoiceActivityDetector,
    window: usize,
    _keep: Vec<CString>,
}

impl Vad {
    /// `max_speech_s` forces a segment end (threshold raised to 0.9) after that
    /// much continuous speech; the circular buffer must be larger than it.
    pub fn new(model: &Path, threads: i32, min_silence_s: f32, max_speech_s: f32) -> Self {
        let model = CString::new(model.to_str().unwrap()).unwrap();
        let prov = CString::new("cpu").unwrap();
        let empty = CString::new("").unwrap();
        let mut cfg: ffi::SherpaOnnxVadModelConfig = unsafe { std::mem::zeroed() };
        cfg.silero_vad = ffi::SherpaOnnxSileroVadModelConfig {
            model: model.as_ptr(),
            threshold: 0.5,
            min_silence_duration: min_silence_s,
            min_speech_duration: 0.1,
            window_size: 512,
            max_speech_duration: max_speech_s,
        };
        cfg.ten_vad.model = empty.as_ptr();
        cfg.sample_rate = 16000;
        cfg.num_threads = threads;
        cfg.provider = prov.as_ptr();
        cfg.debug = 0;
        let ptr = unsafe { ffi::SherpaOnnxCreateVoiceActivityDetector(&cfg, max_speech_s + 60.0) };
        assert!(!ptr.is_null(), "SherpaOnnxCreateVoiceActivityDetector failed");
        Self { ptr, window: 512, _keep: vec![model, prov, empty] }
    }

    /// Speech regions as (start, end) sample indices, sorted and non-overlapping.
    pub fn speech_regions(&self, samples: &[f32]) -> Vec<(usize, usize)> {
        let mut out = Vec::new();
        unsafe {
            ffi::SherpaOnnxVoiceActivityDetectorReset(self.ptr);
            let mut p = 0usize;
            while p < samples.len() {
                let n = self.window.min(samples.len() - p);
                ffi::SherpaOnnxVoiceActivityDetectorAcceptWaveform(self.ptr, samples[p..].as_ptr(), n as i32);
                p += n;
                self.drain(&mut out);
            }
            ffi::SherpaOnnxVoiceActivityDetectorFlush(self.ptr);
            self.drain(&mut out);
        }
        out.sort();
        // Merge touching or overlapping regions.
        let mut merged: Vec<(usize, usize)> = Vec::new();
        for r in out {
            if let Some(last) = merged.last_mut() {
                if r.0 <= last.1 {
                    last.1 = last.1.max(r.1);
                    continue;
                }
            }
            merged.push(r);
        }
        merged
    }

    unsafe fn drain(&self, out: &mut Vec<(usize, usize)>) {
        while ffi::SherpaOnnxVoiceActivityDetectorEmpty(self.ptr) == 0 {
            let seg = ffi::SherpaOnnxVoiceActivityDetectorFront(self.ptr);
            let s = &*seg;
            out.push((s.start as usize, s.start as usize + s.n as usize));
            ffi::SherpaOnnxDestroySpeechSegment(seg);
            ffi::SherpaOnnxVoiceActivityDetectorPop(self.ptr);
        }
    }
}

impl Drop for Vad {
    fn drop(&mut self) {
        unsafe { ffi::SherpaOnnxDestroyVoiceActivityDetector(self.ptr) }
    }
}
