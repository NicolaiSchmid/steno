//! Language identification: spoken (sherpa-onnx whisper SLID) for the lane
//! prior, text (whatlang) for segment-level voting.

use sherpa_rs_sys as ffi;
use std::collections::BTreeMap;
use std::ffi::{CStr, CString};
use std::path::Path;

pub struct Slid {
    ptr: *const ffi::SherpaOnnxSpokenLanguageIdentification,
    _keep: Vec<CString>,
}

impl Slid {
    pub fn new(dir: &Path, threads: i32) -> Self {
        let enc = CString::new(dir.join("tiny-encoder.int8.onnx").to_str().unwrap()).unwrap();
        let dec = CString::new(dir.join("tiny-decoder.int8.onnx").to_str().unwrap()).unwrap();
        let prov = CString::new("cpu").unwrap();
        let cfg = ffi::SherpaOnnxSpokenLanguageIdentificationConfig {
            whisper: ffi::SherpaOnnxSpokenLanguageIdentificationWhisperConfig { encoder: enc.as_ptr(), decoder: dec.as_ptr(), tail_paddings: -1 },
            num_threads: threads,
            debug: 0,
            provider: prov.as_ptr(),
        };
        let ptr = unsafe { ffi::SherpaOnnxCreateSpokenLanguageIdentification(&cfg) };
        assert!(!ptr.is_null(), "SherpaOnnxCreateSpokenLanguageIdentification failed");
        Self { ptr, _keep: vec![enc, dec, prov] }
    }

    pub fn detect(&self, samples: &[f32]) -> String {
        unsafe {
            let stream = ffi::SherpaOnnxSpokenLanguageIdentificationCreateOfflineStream(self.ptr);
            ffi::SherpaOnnxAcceptWaveformOffline(stream, 16000, samples.as_ptr(), samples.len() as i32);
            let res = ffi::SherpaOnnxSpokenLanguageIdentificationCompute(self.ptr, stream);
            let lang = CStr::from_ptr((*res).lang).to_string_lossy().to_string();
            ffi::SherpaOnnxDestroySpokenLanguageIdentificationResult(res);
            ffi::SherpaOnnxDestroyOfflineStream(stream);
            lang
        }
    }
}

impl Drop for Slid {
    fn drop(&mut self) {
        unsafe { ffi::SherpaOnnxDestroySpokenLanguageIdentification(self.ptr) }
    }
}

/// `count` windows of `window_s` spread evenly over speech time (not file time).
pub fn sample_windows(speech: &[(usize, usize)], total: usize, count: usize, window_s: f32) -> Vec<(usize, usize)> {
    let sr = 16_000usize;
    let win = (window_s * sr as f32) as usize;
    let speech_total: usize = speech.iter().map(|(a, b)| b - a).sum();
    if speech_total == 0 {
        return vec![];
    }
    let mut out = Vec::new();
    for k in 0..count {
        let target = (k as f64 + 0.5) / count as f64 * speech_total as f64;
        let mut acc = 0usize;
        for &(a, b) in speech {
            if acc + (b - a) >= target as usize {
                let pos = a + (target as usize - acc);
                let start = pos.saturating_sub(win / 4).min(total.saturating_sub(win));
                out.push((start, (start + win).min(total)));
                break;
            }
            acc += b - a;
        }
    }
    out
}

/// Lane language set: languages with at least `min_share` of the votes (and at least 2 votes).
pub fn vote(langs: &[String], min_share: f32) -> (Vec<String>, BTreeMap<String, usize>) {
    let mut hist: BTreeMap<String, usize> = BTreeMap::new();
    for l in langs {
        *hist.entry(l.clone()).or_default() += 1;
    }
    let n = langs.len().max(1) as f32;
    let mut set: Vec<String> = hist.iter().filter(|(_, &c)| c >= 2 && c as f32 / n >= min_share).map(|(l, _)| l.clone()).collect();
    if set.is_empty() {
        if let Some((l, _)) = hist.iter().max_by_key(|(_, &c)| c) {
            set.push(l.clone());
        }
    }
    (set, hist)
}

/// Text language as an ISO 639-1 code where whatlang knows it, else the 639-3 code.
pub fn text_lang(text: &str) -> Option<(String, f64, bool)> {
    let info = whatlang::detect(text)?;
    let code = match info.lang() {
        whatlang::Lang::Deu => "de".to_string(),
        whatlang::Lang::Eng => "en".to_string(),
        l => l.code().to_string(),
    };
    Some((code, info.confidence(), info.is_reliable()))
}
