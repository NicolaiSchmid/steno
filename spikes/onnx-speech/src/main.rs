//! Steno spike: transcribe and diarize 16 kHz mono WAV files with sherpa-onnx
//! (Parakeet TDT 0.6b v3, stock int8 or an own export via `--asr-dir`/`--asr-files`;
//! pyannote segmentation 3.0 + speaker embedding)
//! and record wall time, RTFx, peak RSS and load.
//!
//! Uses `sherpa-rs-sys` directly because the safe `sherpa-rs` wrappers drop
//! token timestamps and hard-code one thread for diarization.

mod chunker;
mod lang;
mod merge;
mod vad;

use merge::Tok;
use serde::Serialize;
use sherpa_rs_sys as ffi;
use std::collections::BTreeMap;
use std::ffi::{CStr, CString};
use std::path::{Path, PathBuf};
use std::time::Instant;

struct Args {
    models: PathBuf,
    corpus: PathBuf,
    out: PathBuf,
    threads: i32,
    provider: String,
    asr: bool,
    diar: bool,
    embedding: String,
    thresholds: Vec<f32>,
    files: Vec<String>,
    tag: String,
    chunk_seconds: f32,
    // Stage 1+: VAD-driven pause-aligned chunker.
    chunker: String,
    target_seconds: f32,
    search_seconds: f32,
    overlap_seconds: f32,
    long_pause_seconds: f32,
    vad_min_silence: f32,
    vad_model: PathBuf,
    // Stage 2: lane-level spoken language identification.
    slid_dir: Option<PathBuf>,
    slid_windows: usize,
    slid_window_seconds: f32,
    lane_langs: Option<Vec<String>>,
    // Stage 3: segment-level voting.
    vote: bool,
    vote_shift_seconds: f32,
    /// Debug: decode the given "start,end;start,end" second ranges of each file and print word counts.
    probe: Option<Vec<(f32, f32)>>,
    probe_gain: f32,
    /// Spike E: directory of an own Parakeet export (default: the stock int8 under `--models`)
    /// and which file set it holds ("int8" = `*.int8.onnx`, "fp32" = `*.onnx` + `encoder.weights`).
    asr_dir: Option<PathBuf>,
    asr_files: String,
}

fn parse_args() -> Args {
    let mut a = Args {
        models: PathBuf::from("models"),
        corpus: PathBuf::from("corpus"),
        out: PathBuf::from("out"),
        threads: 4,
        provider: "cpu".into(),
        asr: true,
        diar: true,
        embedding: "3dspeaker_speech_eres2net_sv_en_voxceleb_16k.onnx".into(),
        thresholds: vec![0.3, 0.4, 0.5, 0.6, 0.7, 0.8, 0.9],
        files: vec![],
        tag: "run".into(),
        chunk_seconds: 60.0,
        chunker: "quiet".into(),
        target_seconds: 25.0,
        search_seconds: 4.0,
        overlap_seconds: 1.5,
        long_pause_seconds: 3.0,
        vad_min_silence: 0.25,
        vad_model: PathBuf::from("models/silero_vad.onnx"),
        slid_dir: None,
        slid_windows: 24,
        slid_window_seconds: 9.0,
        lane_langs: None,
        vote: false,
        vote_shift_seconds: 2.0,
        probe: None,
        probe_gain: 1.0,
        asr_dir: None,
        asr_files: "int8".into(),
    };
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < argv.len() {
        let next = |i: &mut usize| -> String {
            *i += 1;
            argv.get(*i).cloned().unwrap_or_else(|| panic!("missing value for {}", argv[*i - 1]))
        };
        match argv[i].as_str() {
            "--models" => a.models = PathBuf::from(next(&mut i)),
            "--corpus" => a.corpus = PathBuf::from(next(&mut i)),
            "--out" => a.out = PathBuf::from(next(&mut i)),
            "--threads" => a.threads = next(&mut i).parse().unwrap(),
            "--provider" => a.provider = next(&mut i),
            "--skip-asr" => a.asr = false,
            "--skip-diar" => a.diar = false,
            "--embedding" => a.embedding = next(&mut i),
            "--thresholds" => {
                a.thresholds = next(&mut i).split(',').map(|s| s.parse().unwrap()).collect()
            }
            "--files" => a.files = next(&mut i).split(',').map(String::from).collect(),
            "--tag" => a.tag = next(&mut i),
            "--chunk-seconds" => a.chunk_seconds = next(&mut i).parse().unwrap(),
            "--chunker" => a.chunker = next(&mut i),
            "--target-seconds" => a.target_seconds = next(&mut i).parse().unwrap(),
            "--search-seconds" => a.search_seconds = next(&mut i).parse().unwrap(),
            "--overlap-seconds" => a.overlap_seconds = next(&mut i).parse().unwrap(),
            "--long-pause-seconds" => a.long_pause_seconds = next(&mut i).parse().unwrap(),
            "--vad-min-silence" => a.vad_min_silence = next(&mut i).parse().unwrap(),
            "--vad-model" => a.vad_model = PathBuf::from(next(&mut i)),
            "--slid-dir" => a.slid_dir = Some(PathBuf::from(next(&mut i))),
            "--slid-windows" => a.slid_windows = next(&mut i).parse().unwrap(),
            "--slid-window-seconds" => a.slid_window_seconds = next(&mut i).parse().unwrap(),
            "--lane-langs" => a.lane_langs = Some(next(&mut i).split(',').map(String::from).collect()),
            "--vote" => a.vote = true,
            "--vote-shift-seconds" => a.vote_shift_seconds = next(&mut i).parse().unwrap(),
            "--probe" => {
                a.probe = Some(
                    next(&mut i)
                        .split(';')
                        .map(|r| {
                            let (x, y) = r.split_once(',').unwrap();
                            (x.parse().unwrap(), y.parse().unwrap())
                        })
                        .collect(),
                )
            }
            "--probe-gain" => a.probe_gain = next(&mut i).parse().unwrap(),
            "--asr-dir" => a.asr_dir = Some(PathBuf::from(next(&mut i))),
            "--asr-files" => a.asr_files = next(&mut i),
            other => panic!("unknown arg {other}"),
        }
        i += 1;
    }
    a
}

fn cstr(s: &str) -> CString {
    CString::new(s).unwrap()
}

fn peak_rss_mb() -> f64 {
    let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
    unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut ru) };
    // Linux reports KiB, macOS reports bytes.
    if cfg!(target_os = "macos") {
        ru.ru_maxrss as f64 / 1_048_576.0
    } else {
        ru.ru_maxrss as f64 / 1024.0
    }
}

fn load_1min() -> f64 {
    let mut l = [0f64; 3];
    unsafe { libc::getloadavg(l.as_mut_ptr(), 3) };
    l[0]
}

fn read_wav(path: &Path) -> (Vec<f32>, u32) {
    let mut r = hound::WavReader::open(path).expect("open wav");
    let spec = r.spec();
    assert_eq!(spec.channels, 1, "mono only");
    let samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Int => r
            .samples::<i16>()
            .map(|s| s.unwrap() as f32 / 32768.0)
            .collect(),
        hound::SampleFormat::Float => r.samples::<f32>().map(|s| s.unwrap()).collect(),
    };
    (samples, spec.sample_rate)
}

#[derive(Serialize)]
struct Word {
    word: String,
    start: f32,
    end: Option<f32>,
}

#[derive(Serialize)]
struct AsrResult {
    model: String,
    wall_s: f64,
    load_wall_s: f64,
    rtfx: f64,
    threads: i32,
    provider: String,
    chunk_seconds: f32,
    load_1min_before: f64,
    peak_rss_mb_after: f64,
    text: String,
    words: Vec<Word>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stages: Option<Stages>,
}

#[derive(Serialize, Default)]
struct SegmentInfo {
    start: f32,
    end: f32,
    cut: String,
    words: usize,
    lang: Option<String>,
    flagged: bool,
    changed: bool,
    /// "lang" (text language outside the lane set) or "empty" (near-empty decode on speech).
    #[serde(skip_serializing_if = "Option::is_none")]
    flag: Option<String>,
    speech_s: f32,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    candidate_langs: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    candidate_words: Vec<usize>,
}

#[derive(Serialize, Default)]
struct Stages {
    chunker: String,
    target_s: f32,
    overlap_s: f32,
    search_s: f32,
    long_pause_s: f32,
    vad_wall_s: f64,
    speech_regions: usize,
    speech_s: f64,
    segments: usize,
    decoded_s: f64,
    decode_wall_s: f64,
    cuts: BTreeMap<String, usize>,
    slid_wall_s: f64,
    slid_votes: BTreeMap<String, usize>,
    lane_langs: Vec<String>,
    lane_langs_source: String,
    vote_wall_s: f64,
    flagged: usize,
    flagged_lang: usize,
    flagged_empty: usize,
    changed: usize,
    changed_lang: usize,
    changed_empty: usize,
    extra_decodes: usize,
    segment_info: Vec<SegmentInfo>,
}

#[derive(Serialize)]
struct DiarSegment {
    start: f32,
    end: f32,
    speaker: i32,
}

#[derive(Serialize)]
struct DiarRun {
    threshold: f32,
    wall_s: f64,
    rtfx: f64,
    num_speakers: i32,
    num_segments: usize,
    load_1min_before: f64,
    segments: Vec<DiarSegment>,
}

#[derive(Serialize)]
struct DiarResult {
    segmentation_model: String,
    embedding_model: String,
    threads: i32,
    provider: String,
    load_wall_s: f64,
    runs: Vec<DiarRun>,
    peak_rss_mb_after: f64,
}

#[derive(Serialize)]
struct FileReport {
    file: String,
    tag: String,
    host: String,
    os: String,
    arch: String,
    audio_s: f64,
    asr: Option<AsrResult>,
    diarization: Option<DiarResult>,
}

struct Recognizer {
    ptr: *const ffi::SherpaOnnxOfflineRecognizer,
    _keep: Vec<CString>,
}

impl Recognizer {
    /// `files` is "int8" (stock layout, `*.int8.onnx`) or "fp32" (`*.onnx`; the fp32 encoder
    /// is over protobuf's 2 GB limit and keeps its weights in `encoder.weights` next to it).
    /// sherpa-onnx reads each model into memory and creates the ORT session from the buffer,
    /// so ORT resolves the external-data path against the process CWD, not the model file;
    /// we therefore chdir into `dir` while the sessions are created and restore the CWD after.
    fn new(dir: &Path, threads: i32, provider: &str, files: &str) -> Self {
        let dir = std::fs::canonicalize(dir).unwrap_or_else(|e| panic!("asr dir {}: {e}", dir.display()));
        let suffix = match files {
            "int8" => ".int8.onnx",
            "fp32" => ".onnx",
            other => panic!("--asr-files must be int8 or fp32, got {other}"),
        };
        let enc = cstr(dir.join(format!("encoder{suffix}")).to_str().unwrap());
        let dec = cstr(dir.join(format!("decoder{suffix}")).to_str().unwrap());
        let joi = cstr(dir.join(format!("joiner{suffix}")).to_str().unwrap());
        let tok = cstr(dir.join("tokens.txt").to_str().unwrap());
        let model_type = cstr("nemo_transducer");
        let prov = cstr(provider);
        let decoding = cstr("greedy_search");
        let empty = cstr("");
        let mut model_config: ffi::SherpaOnnxOfflineModelConfig = unsafe { std::mem::zeroed() };
        model_config.transducer = ffi::SherpaOnnxOfflineTransducerModelConfig {
            encoder: enc.as_ptr(),
            decoder: dec.as_ptr(),
            joiner: joi.as_ptr(),
        };
        model_config.tokens = tok.as_ptr();
        model_config.num_threads = threads;
        model_config.debug = 0;
        model_config.provider = prov.as_ptr();
        model_config.model_type = model_type.as_ptr();
        model_config.modeling_unit = empty.as_ptr();
        model_config.bpe_vocab = empty.as_ptr();
        let mut cfg: ffi::SherpaOnnxOfflineRecognizerConfig = unsafe { std::mem::zeroed() };
        cfg.model_config = model_config;
        cfg.feat_config = ffi::SherpaOnnxFeatureConfig { sample_rate: 16000, feature_dim: 80 };
        cfg.decoding_method = decoding.as_ptr();
        cfg.hotwords_file = empty.as_ptr();
        cfg.rule_fsts = empty.as_ptr();
        cfg.rule_fars = empty.as_ptr();
        cfg.max_active_paths = 4;
        let cwd = std::env::current_dir().unwrap();
        std::env::set_current_dir(&dir).unwrap();
        let ptr = unsafe { ffi::SherpaOnnxCreateOfflineRecognizer(&cfg) };
        std::env::set_current_dir(&cwd).unwrap();
        assert!(!ptr.is_null(), "SherpaOnnxCreateOfflineRecognizer failed (provider={provider})");
        Self { ptr, _keep: vec![enc, dec, joi, tok, model_type, prov, decoding, empty] }
    }

    /// Pieces with absolute timestamps for one chunk.
    fn decode_toks(&self, samples: &[f32], offset: f32) -> Vec<Tok> {
        unsafe {
            let stream = ffi::SherpaOnnxCreateOfflineStream(self.ptr);
            ffi::SherpaOnnxAcceptWaveformOffline(stream, 16000, samples.as_ptr(), samples.len() as i32);
            ffi::SherpaOnnxDecodeOfflineStream(self.ptr, stream);
            let res = ffi::SherpaOnnxGetOfflineStreamResult(stream);
            let r = &*res;
            let mut toks = Vec::with_capacity(r.count as usize);
            for k in 0..r.count as usize {
                let piece = CStr::from_ptr(*r.tokens_arr.add(k)).to_string_lossy().to_string();
                let ts = if r.timestamps.is_null() { 0.0 } else { *r.timestamps.add(k) } + offset;
                toks.push(Tok { piece, t: ts });
            }
            ffi::SherpaOnnxDestroyOfflineRecognizerResult(res);
            ffi::SherpaOnnxDestroyOfflineStream(stream);
            toks
        }
    }

    /// Returns (text, words) for one chunk; `offset` shifts timestamps.
    fn decode(&self, samples: &[f32], offset: f32) -> (String, Vec<Word>) {
        unsafe {
            let stream = ffi::SherpaOnnxCreateOfflineStream(self.ptr);
            ffi::SherpaOnnxAcceptWaveformOffline(stream, 16000, samples.as_ptr(), samples.len() as i32);
            ffi::SherpaOnnxDecodeOfflineStream(self.ptr, stream);
            let res = ffi::SherpaOnnxGetOfflineStreamResult(stream);
            let r = &*res;
            let text = CStr::from_ptr(r.text).to_string_lossy().trim().to_string();
            let mut words: Vec<Word> = Vec::new();
            for k in 0..r.count as usize {
                let tok = CStr::from_ptr(*r.tokens_arr.add(k)).to_string_lossy().to_string();
                let ts = if r.timestamps.is_null() { 0.0 } else { *r.timestamps.add(k) } + offset;
                // sherpa-onnx 1.12.9 (what sherpa-rs-sys 0.6.8 ships) has no `durations` field.
                let dur: Option<f32> = None;
                if let Some(piece) = tok.strip_prefix('\u{2581}').or_else(|| tok.strip_prefix(' ')) {
                    words.push(Word { word: piece.to_string(), start: ts, end: dur.map(|d| ts + d) });
                } else if let Some(w) = words.last_mut() {
                    w.word.push_str(&tok);
                    w.end = dur.map(|d| ts + d);
                } else {
                    words.push(Word { word: tok, start: ts, end: dur.map(|d| ts + d) });
                }
            }
            ffi::SherpaOnnxDestroyOfflineRecognizerResult(res);
            ffi::SherpaOnnxDestroyOfflineStream(stream);
            (text, words)
        }
    }
}

impl Drop for Recognizer {
    fn drop(&mut self) {
        unsafe { ffi::SherpaOnnxDestroyOfflineRecognizer(self.ptr) }
    }
}

struct Diarizer {
    ptr: *const ffi::SherpaOnnxOfflineSpeakerDiarization,
    cfg: ffi::SherpaOnnxOfflineSpeakerDiarizationConfig,
    _keep: Vec<CString>,
}

impl Diarizer {
    fn new(seg_model: &Path, emb_model: &Path, threads: i32, provider: &str) -> Self {
        let seg = cstr(seg_model.to_str().unwrap());
        let emb = cstr(emb_model.to_str().unwrap());
        let prov = cstr(provider);
        let cfg = ffi::SherpaOnnxOfflineSpeakerDiarizationConfig {
            segmentation: ffi::SherpaOnnxOfflineSpeakerSegmentationModelConfig {
                pyannote: ffi::SherpaOnnxOfflineSpeakerSegmentationPyannoteModelConfig { model: seg.as_ptr() },
                num_threads: threads,
                debug: 0,
                provider: prov.as_ptr(),
            },
            embedding: ffi::SherpaOnnxSpeakerEmbeddingExtractorConfig {
                model: emb.as_ptr(),
                num_threads: threads,
                debug: 0,
                provider: prov.as_ptr(),
            },
            clustering: ffi::SherpaOnnxFastClusteringConfig { num_clusters: -1, threshold: 0.5 },
            min_duration_on: 0.3,
            min_duration_off: 0.5,
        };
        let ptr = unsafe { ffi::SherpaOnnxCreateOfflineSpeakerDiarization(&cfg) };
        assert!(!ptr.is_null(), "SherpaOnnxCreateOfflineSpeakerDiarization failed");
        Self { ptr, cfg, _keep: vec![seg, emb, prov] }
    }

    fn run(&mut self, samples: &[f32], threshold: f32) -> (i32, Vec<DiarSegment>) {
        unsafe {
            self.cfg.clustering.threshold = threshold;
            self.cfg.clustering.num_clusters = -1;
            ffi::SherpaOnnxOfflineSpeakerDiarizationSetConfig(self.ptr, &self.cfg);
            let res = ffi::SherpaOnnxOfflineSpeakerDiarizationProcess(self.ptr, samples.as_ptr(), samples.len() as i32);
            let n_spk = ffi::SherpaOnnxOfflineSpeakerDiarizationResultGetNumSpeakers(res);
            let n_seg = ffi::SherpaOnnxOfflineSpeakerDiarizationResultGetNumSegments(res);
            let segs = ffi::SherpaOnnxOfflineSpeakerDiarizationResultSortByStartTime(res);
            let mut out = Vec::with_capacity(n_seg as usize);
            if !segs.is_null() {
                for k in 0..n_seg as usize {
                    let s = &*segs.add(k);
                    out.push(DiarSegment { start: s.start, end: s.end, speaker: s.speaker });
                }
                ffi::SherpaOnnxOfflineSpeakerDiarizationDestroySegment(segs);
            }
            ffi::SherpaOnnxOfflineSpeakerDiarizationDestroyResult(res);
            (n_spk, out)
        }
    }
}

impl Drop for Diarizer {
    fn drop(&mut self) {
        unsafe { ffi::SherpaOnnxDestroyOfflineSpeakerDiarization(self.ptr) }
    }
}

/// Split into chunks of roughly `target_s` seconds, cutting at the quietest
/// 100 ms frame within +-5 s of each target boundary. The sherpa-onnx Parakeet
/// export caps the encoder at 2500 frames (~200 s), so long files must be split.
fn split_at_quiet_points(samples: &[f32], target_s: f32) -> Vec<(usize, &[f32])> {
    let sr = 16000usize;
    let target = (target_s * sr as f32) as usize;
    let search = 5 * sr;
    let frame = sr / 10;
    let mut out = Vec::new();
    let mut start = 0usize;
    while start < samples.len() {
        let want = start + target;
        if want + search >= samples.len() {
            out.push((start, &samples[start..]));
            break;
        }
        let lo = want.saturating_sub(search).max(start + frame);
        let hi = want + search;
        let mut best = (f32::MAX, want);
        let mut p = lo;
        while p + frame <= hi {
            let e: f32 = samples[p..p + frame].iter().map(|x| x * x).sum();
            if e < best.0 { best = (e, p + frame / 2); }
            p += frame;
        }
        out.push((start, &samples[start..best.1]));
        start = best.1;
    }
    out
}

struct Candidate {
    toks: Vec<Tok>,
    words: usize,
    lang: Option<String>,
    conf: f64,
}

/// Seconds of VAD speech inside [start, end).
fn speech_inside(speech: &[(usize, usize)], start: usize, end: usize) -> f32 {
    speech.iter().map(|&(a, b)| b.min(end).saturating_sub(a.max(start))).sum::<usize>() as f32 / 16000.0
}

/// Two half-chunk decodes concatenated (split at the longest pause near the middle, else the midpoint).
fn candidate_split(rec: &Recognizer, samples: &[f32], speech: &[(usize, usize)], start: usize, end: usize) -> Candidate {
    let mid = (start + end) / 2;
    let window = 3 * 16000;
    let pauses = chunker::pauses(speech, samples.len());
    let cut = pauses
        .iter()
        .filter(|p| p.1 > mid.saturating_sub(window) && p.0 < mid + window && p.0 > start + 16000 && p.1 < end - 16000)
        .max_by_key(|p| p.1 - p.0)
        .map(|p| (p.0 + p.1) / 2)
        .unwrap_or(mid);
    let a = candidate(rec, samples, start, cut);
    let b = candidate(rec, samples, cut, end);
    let toks: Vec<Tok> = a.toks.into_iter().chain(b.toks).collect();
    let (text, words) = merge::render(&toks);
    let (lang, conf) = if words.len() >= 4 { lang::text_lang(&text).map(|(l, c, _)| (Some(l), c)).unwrap_or((None, 0.0)) } else { (None, 0.0) };
    Candidate { toks, words: words.len(), lang, conf }
}

fn candidate(rec: &Recognizer, samples: &[f32], start: usize, end: usize) -> Candidate {
    let toks = rec.decode_toks(&samples[start..end], start as f32 / 16000.0);
    let (text, words) = merge::render(&toks);
    let (lang, conf) = if words.len() >= 4 { lang::text_lang(&text).map(|(l, c, _)| (Some(l), c)).unwrap_or((None, 0.0)) } else { (None, 0.0) };
    Candidate { toks, words: words.len(), lang, conf }
}

/// Stages 1 to 3: VAD regions, pause-aligned layout, decode, optional lane
/// prior and segment voting, LCS merge. Returns (text, words, stages).
fn transcribe_vad(
    rec: &Recognizer,
    vad: &vad::Vad,
    slid: Option<&lang::Slid>,
    samples: &[f32],
    args: &Args,
) -> (String, Vec<Word>, Stages) {
    let mut st = Stages { chunker: "vad".into(), target_s: args.target_seconds, overlap_s: args.overlap_seconds, search_s: args.search_seconds, long_pause_s: args.long_pause_seconds, ..Default::default() };
    let t = Instant::now();
    let speech = vad.speech_regions(samples);
    st.vad_wall_s = t.elapsed().as_secs_f64();
    st.speech_regions = speech.len();
    st.speech_s = speech.iter().map(|(a, b)| (b - a) as f64).sum::<f64>() / 16000.0;

    let cfg = chunker::ChunkerConfig {
        target_s: args.target_seconds,
        search_s: args.search_seconds,
        overlap_s: args.overlap_seconds,
        long_pause_s: args.long_pause_seconds,
        max_s: 190.0,
        pad_s: 0.25,
    };
    let chunks = chunker::layout(samples, &speech, &cfg);
    st.segments = chunks.len();
    st.decoded_s = chunks.iter().map(|c| (c.end - c.start) as f64).sum::<f64>() / 16000.0;
    for c in &chunks {
        *st.cuts.entry(c.cut.to_string()).or_default() += 1;
    }

    // Stage 2: lane language set.
    let t = Instant::now();
    let lane: Option<Vec<String>> = if let Some(l) = &args.lane_langs {
        st.lane_langs_source = "given".into();
        Some(l.clone())
    } else if let Some(slid) = slid {
        let mut votes = Vec::new();
        for (a, b) in lang::sample_windows(&speech, samples.len(), args.slid_windows, args.slid_window_seconds) {
            votes.push(slid.detect(&samples[a..b]));
        }
        let (set, hist) = lang::vote(&votes, 0.12);
        st.slid_votes = hist;
        st.lane_langs_source = "slid".into();
        Some(set)
    } else {
        None
    };
    st.slid_wall_s = t.elapsed().as_secs_f64();
    if let Some(l) = &lane {
        st.lane_langs = l.clone();
    }

    // Decode every chunk.
    let t = Instant::now();
    let mut cands: Vec<Candidate> = chunks.iter().map(|c| candidate(rec, samples, c.start, c.end)).collect();
    st.decode_wall_s = t.elapsed().as_secs_f64();

    // Stage 3: segment-level voting on flagged chunks.
    let t = Instant::now();
    let shift = (args.vote_shift_seconds * 16000.0) as usize;
    let mut infos = Vec::with_capacity(cands.len());
    for (k, c) in chunks.iter().enumerate() {
        let speech_s = speech_inside(&speech, c.start, c.end);
        let mut info = SegmentInfo { start: c.start as f32 / 16000.0, end: c.end as f32 / 16000.0, cut: c.cut.to_string(), words: cands[k].words, lang: cands[k].lang.clone(), speech_s, ..Default::default() };
        if args.vote {
            let outside = match (&lane, &cands[k].lang) {
                (Some(set), Some(l)) => !set.contains(l),
                _ => false,
            };
            // Near-empty: under 0.4 words per second of VAD speech on at least 3 s of speech.
            let empty = speech_s >= 3.0 && (cands[k].words as f32) < 0.4 * speech_s;
            if outside || empty {
                let flag = if empty { "empty" } else { "lang" };
                info.flagged = true;
                info.flag = Some(flag.into());
                st.flagged += 1;
                if empty { st.flagged_empty += 1 } else { st.flagged_lang += 1 }
                let (s0, e0) = (c.start, c.end);
                let total = samples.len();
                let ms = s0.saturating_sub(shift);
                let minus = candidate(rec, samples, ms, e0.saturating_sub(shift).max(ms + 16000).min(total));
                let ps = (s0 + shift).min(total.saturating_sub(16000));
                let plus = candidate(rec, samples, ps, (e0 + shift).min(total).max(ps + 16000));
                st.extra_decodes += 2;
                let orig = std::mem::replace(&mut cands[k], Candidate { toks: vec![], words: 0, lang: None, conf: 0.0 });
                let mut all: Vec<Candidate> = vec![orig, minus, plus];
                if empty && e0 - s0 >= 6 * 16000 {
                    all.push(candidate_split(rec, samples, &speech, s0, e0));
                    st.extra_decodes += 2;
                }
                info.candidate_langs = all.iter().map(|c| c.lang.clone().unwrap_or_else(|| "-".into())).collect();
                info.candidate_words = all.iter().map(|c| c.words).collect();
                let n = all.len();
                let in_set = |c: &Candidate| match (&lane, &c.lang) {
                    (Some(set), Some(l)) => set.contains(l),
                    (None, _) => true,
                    (Some(_), None) => c.words < 4, // too short to judge: not held against it
                };
                let mut best = 0usize;
                if empty {
                    // Most words among language-acceptable candidates.
                    let mut best_key = (false, 0usize);
                    for (i, c) in all.iter().enumerate() {
                        let key = (in_set(c), c.words);
                        if i == 0 || key > best_key {
                            best_key = key;
                            best = i;
                        }
                    }
                } else {
                    // Language in set first, then agreement with the other candidates, then text-language confidence.
                    let mut best_key = (false, 0f32, 0f64);
                    for i in 0..n {
                        let mut agree = 0f32;
                        for j in 0..n {
                            if i != j {
                                agree += merge::similarity(&all[i].toks, &all[j].toks);
                            }
                        }
                        let key = (in_set(&all[i]) && all[i].lang.is_some(), agree, all[i].conf);
                        if i == 0 || key > best_key {
                            best_key = key;
                            best = i;
                        }
                    }
                    if !best_key.0 {
                        best = 0; // nothing in set: keep the original
                    }
                }
                if best != 0 {
                    info.changed = true;
                    st.changed += 1;
                    if empty { st.changed_empty += 1 } else { st.changed_lang += 1 }
                }
                let chosen = all.swap_remove(best);
                info.lang = chosen.lang.clone();
                info.words = chosen.words;
                cands[k] = chosen;
            }
        }
        infos.push(info);
    }
    st.vote_wall_s = t.elapsed().as_secs_f64();
    st.segment_info = infos;

    let windows: Vec<Vec<Tok>> = cands.iter().map(|c| c.toks.clone()).collect();
    let merged = merge::merge_all(&windows, args.overlap_seconds);
    let (text, words) = merge::render(&merged);
    let words = words.into_iter().map(|(w, t)| Word { word: w, start: t, end: None }).collect();
    (text, words, st)
}

fn main() {
    let args = parse_args();
    std::fs::create_dir_all(&args.out).unwrap();
    let host = std::env::var("HOSTNAME")
        .ok()
        .or_else(|| std::process::Command::new("hostname").output().ok().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()))
        .unwrap_or_default();

    let mut wavs: Vec<PathBuf> = std::fs::read_dir(&args.corpus)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().map(|x| x == "wav").unwrap_or(false))
        .filter(|p| args.files.is_empty() || args.files.iter().any(|f| p.file_name().unwrap().to_string_lossy().starts_with(f)))
        .collect();
    wavs.sort();
    eprintln!("files: {}", wavs.len());

    let asr_dir = args.asr_dir.clone().unwrap_or_else(|| args.models.join("sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8"));
    let asr_label = format!(
        "parakeet-tdt-0.6b-v3 {} ({})",
        args.asr_files,
        asr_dir.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
    );
    let seg_model = args.models.join("sherpa-onnx-pyannote-segmentation-3-0/model.onnx");
    let emb_model = args.models.join(&args.embedding);

    let mut recognizer = None;
    let mut asr_load_s = 0.0;
    if args.asr {
        let t = Instant::now();
        recognizer = Some(Recognizer::new(&asr_dir, args.threads, &args.provider, &args.asr_files));
        asr_load_s = t.elapsed().as_secs_f64();
        eprintln!("asr model {asr_label} loaded in {asr_load_s:.2}s, rss {:.0} MB", peak_rss_mb());
    }
    let vad = if args.chunker == "vad" { Some(vad::Vad::new(&args.vad_model, args.threads, args.vad_min_silence, 120.0)) } else { None };
    let slid = args.slid_dir.as_ref().map(|d| lang::Slid::new(d, args.threads));
    if vad.is_some() {
        eprintln!("vad loaded; slid {}", if slid.is_some() { "loaded" } else { "off" });
    }
    let mut diarizer = None;
    let mut diar_load_s = 0.0;
    if args.diar {
        let t = Instant::now();
        diarizer = Some(Diarizer::new(&seg_model, &emb_model, args.threads, &args.provider));
        diar_load_s = t.elapsed().as_secs_f64();
        eprintln!("diarization models loaded in {diar_load_s:.2}s");
    }

    for wav in &wavs {
        let stem = wav.file_stem().unwrap().to_string_lossy().to_string();
        let (samples, sr) = read_wav(wav);
        assert_eq!(sr, 16000);
        let audio_s = samples.len() as f64 / 16000.0;
        let mut report = FileReport {
            file: wav.file_name().unwrap().to_string_lossy().to_string(),
            tag: args.tag.clone(),
            host: host.clone(),
            os: std::env::consts::OS.into(),
            arch: std::env::consts::ARCH.into(),
            audio_s,
            asr: None,
            diarization: None,
        };

        if let (Some(rec), Some(ranges)) = (&recognizer, &args.probe) {
            for &(x, y) in ranges {
                let (a, b) = ((x * 16000.0) as usize, ((y * 16000.0) as usize).min(samples.len()));
                let seg: Vec<f32> = samples[a..b].iter().map(|v| (v * args.probe_gain).clamp(-1.0, 1.0)).collect();
                let rms = (seg.iter().map(|v| v * v).sum::<f32>() / seg.len() as f32).sqrt();
                let peak = seg.iter().fold(0f32, |m, v| m.max(v.abs()));
                let toks = rec.decode_toks(&seg, x);
                let (text, words) = merge::render(&toks);
                let first = toks.first().map(|t| t.t).unwrap_or(0.0);
                eprintln!("{stem} probe {x:.1}-{y:.1}s gain {:.2}: rms {rms:.4} peak {peak:.3} tokens {} words {} first-token {first:.2}s chars {}", args.probe_gain, toks.len(), words.len(), text.chars().count());
            }
            continue;
        }

        if let Some(rec) = &recognizer {
            let load = load_1min();
            let t = Instant::now();
            let mut stages = None;
            let (text, words) = if let Some(v) = &vad {
                let (text, words, st) = transcribe_vad(rec, v, slid.as_ref(), &samples, &args);
                eprintln!(
                    "{stem} vad {:.2}s regions {} speech {:.1}s | segments {} decoded {:.1}s cuts {:?} | slid {:.2}s votes {:?} lane {:?} | flagged {} (lang {} empty {}) changed {} (lang {} empty {}) extra {}",
                    st.vad_wall_s, st.speech_regions, st.speech_s, st.segments, st.decoded_s, st.cuts, st.slid_wall_s, st.slid_votes, st.lane_langs, st.flagged, st.flagged_lang, st.flagged_empty, st.changed, st.changed_lang, st.changed_empty, st.extra_decodes
                );
                stages = Some(st);
                (text, words)
            } else if args.chunk_seconds > 0.0 {
                let mut text = String::new();
                let mut words = Vec::new();
                for (start, chunk) in split_at_quiet_points(&samples, args.chunk_seconds) {
                    let (t2, w2) = rec.decode(chunk, start as f32 / 16000.0);
                    if !text.is_empty() { text.push(' '); }
                    text.push_str(&t2);
                    words.extend(w2);
                }
                (text, words)
            } else {
                rec.decode(&samples, 0.0)
            };
            let wall = t.elapsed().as_secs_f64();
            let rss = peak_rss_mb();
            eprintln!("{stem} asr: {wall:.2}s  RTFx {:.1}  threads {}  load {load:.2}  peakRSS {rss:.0} MB  words {}", audio_s / wall, args.threads, words.len());
            report.asr = Some(AsrResult {
                model: asr_label.clone(),
                wall_s: wall,
                load_wall_s: asr_load_s,
                rtfx: audio_s / wall,
                threads: args.threads,
                provider: args.provider.clone(),
                chunk_seconds: args.chunk_seconds,
                load_1min_before: load,
                peak_rss_mb_after: rss,
                text,
                words,
                stages,
            });
        }

        if let Some(d) = &mut diarizer {
            let mut runs = Vec::new();
            for &th in &args.thresholds {
                let load = load_1min();
                let t = Instant::now();
                let (n_spk, segs) = d.run(&samples, th);
                let wall = t.elapsed().as_secs_f64();
                eprintln!("{stem} diar th={th:.2}: {wall:.2}s RTFx {:.1} speakers {n_spk} segments {} load {load:.2}", audio_s / wall, segs.len());
                runs.push(DiarRun { threshold: th, wall_s: wall, rtfx: audio_s / wall, num_speakers: n_spk, num_segments: segs.len(), load_1min_before: load, segments: segs });
            }
            report.diarization = Some(DiarResult {
                segmentation_model: "pyannote-segmentation-3.0".into(),
                embedding_model: args.embedding.clone(),
                threads: args.threads,
                provider: args.provider.clone(),
                load_wall_s: diar_load_s,
                runs,
                peak_rss_mb_after: peak_rss_mb(),
            });
        }

        let out = args.out.join(format!("{stem}.{}.json", args.tag));
        std::fs::write(&out, serde_json::to_string_pretty(&report).unwrap()).unwrap();
    }
    eprintln!("done; final peak RSS {:.0} MB", peak_rss_mb());
}
