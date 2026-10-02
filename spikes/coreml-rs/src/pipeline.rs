//! Port of the parts of FluidAudio's ChunkProcessor that Steno's call path
//! exercises on v3 (`melChunkContext = false`, no warmup prefix, no dual
//! decode arbitration). Simplifications are listed in the spike report:
//! regular (not silence-aligned) chunk starts, LCS-only overlap merge, no
//! seam-gap repair pass, no empty-decode recovery.

use crate::coreml::{output, provider, Array, Model};
use crate::decoder::{decode_window, encoder_view_from, DecodeStats, DecoderBuffers, Token};
use std::collections::{HashMap, HashSet};
use std::time::Instant;

pub const SAMPLE_RATE: usize = 16_000;
pub const FRAME: usize = 1280; // 80 ms
pub const MAX_MODEL_SAMPLES: usize = 240_000;
pub const MEL_HOP: usize = 160;
pub const OVERLAP_SECONDS: f64 = 2.0;
pub const FRAME_SECONDS: f64 = 0.08;
pub const SPEECH_RMS_FLOOR: f32 = 0.0005;
pub const WORD_BOUNDARY: &str = "\u{2581}";

pub struct Models {
    pub pre: Model,
    pub enc: Model,
    pub dec: Model,
    pub joint: Model,
}

impl Models {
    pub fn load(dir: &str, encoder_units: objc2_core_ml::MLComputeUnits) -> Result<(Self, f64), String> {
        use objc2_core_ml::MLComputeUnits;
        let t0 = Instant::now();
        // FluidAudio: preprocessor .cpuOnly; encoder/decoder/joint config.computeUnits (.all)
        let pre = Model::load(&format!("{dir}/Preprocessor.mlmodelc"), MLComputeUnits::CPUOnly)?;
        let enc = Model::load(&format!("{dir}/Encoder.mlmodelc"), encoder_units)?;
        let dec = Model::load(&format!("{dir}/Decoder.mlmodelc"), MLComputeUnits::All)?;
        let joint = Model::load(&format!("{dir}/JointDecisionv3.mlmodelc"), MLComputeUnits::All)?;
        Ok((Self { pre, enc, dec, joint }, t0.elapsed().as_secs_f64()))
    }
}

pub struct Vocab {
    pub pieces: HashMap<usize, String>,
    pub splice_safe: HashSet<usize>,
}

impl Vocab {
    pub fn load(path: &str) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
        let v: serde_json::Value = serde_json::from_str(&text).map_err(|e| format!("{path}: {e}"))?;
        let mut pieces = HashMap::new();
        for (k, val) in v.as_object().ok_or("vocab is not an object")? {
            if let (Ok(id), Some(s)) = (k.parse::<usize>(), val.as_str()) {
                pieces.insert(id, s.to_string());
            }
        }
        let splice_safe = pieces
            .iter()
            .filter(|(_, p)| starts_word(p) || is_punctuation_piece(p))
            .map(|(id, _)| *id)
            .collect();
        Ok(Self { pieces, splice_safe })
    }
    fn piece(&self, id: usize) -> &str {
        self.pieces.get(&id).map(String::as_str).unwrap_or("")
    }
}

/// The v3 vocabulary file marks word starts with a leading space; the
/// SentencePiece export uses "\u{2581}". FluidAudio accepts both.
fn starts_word(p: &str) -> bool {
    p.starts_with(WORD_BOUNDARY) || p.starts_with(' ')
}

fn strip_boundary(p: &str) -> &str {
    p.strip_prefix(WORD_BOUNDARY).or_else(|| p.strip_prefix(' ')).unwrap_or(p)
}

fn is_punctuation_piece(p: &str) -> bool {
    let core = strip_boundary(p).trim();
    !core.is_empty() && core.chars().all(|c| c.is_ascii_punctuation() || matches!(c, '。' | '？' | '！' | '，' | '、' | '…' | '’' | '“' | '”'))
}

pub struct Layout {
    pub chunk: usize,
    pub overlap: usize,
    pub stride: usize,
}

pub fn layout() -> Layout {
    let max_actual = MAX_MODEL_SAMPLES; // no mel context reserved
    let raw = (max_actual - MEL_HOP).max(FRAME);
    let chunk = raw / FRAME * FRAME; // 239 360 = 14.96 s
    let requested = (OVERLAP_SECONDS * SAMPLE_RATE as f64) as usize;
    let overlap = requested.min(chunk / 2) / FRAME * FRAME; // 32 000 = 2.0 s
    let stride = (chunk - overlap).max(FRAME) / FRAME * FRAME; // 207 360 = 12.96 s
    Layout { chunk, overlap, stride }
}

fn speech_end_samples(audio: &[f32]) -> usize {
    let mut end = audio.len();
    while end > 0 {
        let start = end.saturating_sub(FRAME);
        let s = &audio[start..end];
        let sum: f32 = s.iter().map(|x| x * x).sum();
        if (sum / s.len() as f32).sqrt() >= SPEECH_RMS_FLOOR {
            return end;
        }
        end = start;
    }
    audio.len()
}

fn last_chunk_warmup_samples(chunk_start: usize, default_warmup: usize, chunk: usize, total: usize, speech_end: usize) -> usize {
    let default_visible = (chunk.saturating_sub(default_warmup)).max(FRAME);
    let is_last = chunk_start + default_visible >= total;
    let remaining = speech_end.min(total) as isize - chunk_start as isize;
    if !is_last || remaining <= 0 || chunk_start == 0 {
        return default_warmup;
    }
    let fill = (chunk as isize - remaining) as usize / FRAME * FRAME;
    if fill == 0 {
        return default_warmup;
    }
    let available = chunk_start / FRAME * FRAME;
    default_warmup.max(available.min(fill))
}

pub struct ChunkTiming {
    pub pre_s: f64,
    pub enc_s: f64,
    pub dec_s: f64,
}

pub struct Transcript {
    pub tokens: Vec<Token>,
    pub chunks: usize,
    pub timing: ChunkTiming,
    pub stats: DecodeStats,
}

/// FluidAudio's empty-decode recovery (#909): a window with speech energy
/// that decodes to nothing is retried with alternative length declarations.
#[derive(Clone, Copy, PartialEq)]
struct LengthPolicy {
    encoder_full: bool,
    preprocessor_full: bool,
    trimmed_tail: bool,
}

const ACTUAL: LengthPolicy = LengthPolicy { encoder_full: false, preprocessor_full: false, trimmed_tail: false };
const RECOVERY_POLICIES: [LengthPolicy; 5] = [
    LengthPolicy { encoder_full: true, preprocessor_full: false, trimmed_tail: false },
    LengthPolicy { encoder_full: false, preprocessor_full: true, trimmed_tail: false },
    LengthPolicy { encoder_full: false, preprocessor_full: false, trimmed_tail: true },
    LengthPolicy { encoder_full: true, preprocessor_full: false, trimmed_tail: true },
    LengthPolicy { encoder_full: false, preprocessor_full: true, trimmed_tail: true },
];
const RECOVERY_MIN_SAMPLES: usize = 2 * SAMPLE_RATE;
const RECOVERY_MIN_RMS: f64 = 0.003;
const RECOVERY_MIN_CONF: f32 = 0.7;
const RECOVERY_MIN_TOKENS: usize = 2;
const TRIMMED_TAIL_SAMPLES: usize = SAMPLE_RATE / 5;

fn run_inference(
    models: &Models,
    bufs: &mut DecoderBuffers,
    audio_in: &Array,
    audio_len: &Array,
    samples: &[f32],
    policy: LengthPolicy,
    chunk_start_offset: usize,
    is_last: bool,
    emit_after_frame: Option<usize>,
    timing: &mut ChunkTiming,
    stats: &mut DecodeStats,
) -> Result<Vec<Token>, String> {
    let full_len = samples.len();
    let trimmed = RECOVERY_MIN_SAMPLES.max(full_len.saturating_sub(TRIMMED_TAIL_SAMPLES)) / FRAME * FRAME;
    let effective = if policy.trimmed_tail { trimmed } else { full_len };
    let buf = audio_in.f32_mut();
    buf[..full_len].copy_from_slice(samples);
    buf[full_len..].fill(0.0);
    if policy.trimmed_tail && effective < full_len {
        buf[effective..full_len].fill(0.0);
    }
    audio_len.i32_mut()[0] = if policy.preprocessor_full { MAX_MODEL_SAMPLES as i32 } else { effective as i32 };
    let actual_frames = (effective + FRAME - 1) / FRAME;
    let global_offset = chunk_start_offset / FRAME;

    let t0 = Instant::now();
    let pre_in = provider(&[("audio_signal", audio_in), ("audio_length", audio_len)])?;
    let pre_out = models.pre.predict(&pre_in)?;
    let mel = output(&pre_out, "mel")?;
    let mel_len = output(&pre_out, "mel_length")?;
    if policy.encoder_full {
        mel_len.i32_mut()[0] = mel.shape[2] as i32;
    }
    let t1 = Instant::now();
    let enc_in = provider(&[("mel", &mel), ("mel_length", &mel_len)])?;
    let enc_out = models.enc.predict(&enc_in)?;
    let view = encoder_view_from(&enc_out)?;
    let t2 = Instant::now();
    let tokens = decode_window(bufs, &models.dec, &models.joint, &view, actual_frames, global_offset, emit_after_frame, is_last, stats)?;
    let t3 = Instant::now();
    timing.pre_s += (t1 - t0).as_secs_f64();
    timing.enc_s += (t2 - t1).as_secs_f64();
    timing.dec_s += (t3 - t2).as_secs_f64();
    Ok(tokens)
}

fn transcribe_chunk(
    models: &Models,
    bufs: &mut DecoderBuffers,
    audio_in: &Array,
    audio_len: &Array,
    samples: &[f32],
    chunk_start_offset: usize,
    is_last: bool,
    emit_after_frame: Option<usize>,
    timing: &mut ChunkTiming,
    stats: &mut DecodeStats,
) -> Result<Vec<Token>, String> {
    let tokens = run_inference(models, bufs, audio_in, audio_len, samples, ACTUAL, chunk_start_offset, is_last, emit_after_frame, timing, stats)?;
    if !tokens.is_empty() {
        return Ok(tokens);
    }
    let energy: f64 = samples.iter().map(|x| (*x as f64) * (*x as f64)).sum();
    let recoverable = samples.len() >= RECOVERY_MIN_SAMPLES && (energy / samples.len() as f64).sqrt() >= RECOVERY_MIN_RMS;
    if !recoverable {
        return Ok(tokens);
    }
    for policy in RECOVERY_POLICIES {
        let retry = run_inference(models, bufs, audio_in, audio_len, samples, policy, chunk_start_offset, is_last, emit_after_frame, timing, stats)?;
        stats.recoveries_tried += 1;
        if std::env::var_os("COREML_RS_DEBUG").is_some() {
            let mean = if retry.is_empty() { 0.0 } else { retry.iter().map(|t| t.conf).sum::<f32>() / retry.len() as f32 };
            eprintln!("  recovery at {:.2}s policy enc_full={} pre_full={} trim={} -> {} tokens mean conf {:.2}", chunk_start_offset as f64 / SAMPLE_RATE as f64, policy.encoder_full, policy.preprocessor_full, policy.trimmed_tail, retry.len(), mean);
        }
        if retry.len() >= RECOVERY_MIN_TOKENS {
            let mean = retry.iter().map(|t| t.conf).sum::<f32>() / retry.len() as f32;
            if mean >= RECOVERY_MIN_CONF {
                stats.recoveries_accepted += 1;
                return Ok(retry);
            }
        }
    }
    Ok(tokens)
}

pub fn transcribe(models: &Models, vocab: &Vocab, audio: &[f32]) -> Result<Transcript, String> {
    let lay = layout();
    let total = audio.len();
    let speech_end = speech_end_samples(audio);
    let mut bufs = DecoderBuffers::new()?;
    let audio_in = Array::zeros_f32(&[1, MAX_MODEL_SAMPLES])?;
    let audio_len = Array::zeros_i32(&[1])?;
    let mut timing = ChunkTiming { pre_s: 0.0, enc_s: 0.0, dec_s: 0.0 };
    let mut stats = DecodeStats { decoder_calls: 0, joint_calls: 0, recoveries_tried: 0, recoveries_accepted: 0 };

    let mut windows: Vec<Vec<Token>> = Vec::new();
    let mut chunk_start = 0usize;
    while chunk_start < total {
        let warmup = last_chunk_warmup_samples(chunk_start, 0, lay.chunk, total, speech_end);
        let visible = (lay.chunk - warmup).max(FRAME);
        let candidate_end = chunk_start + visible;
        let is_last = candidate_end >= total;
        let chunk_end = if is_last { total } else { candidate_end };
        if chunk_end <= chunk_start {
            break;
        }
        let audio_end = if is_last { chunk_end.min(speech_end) } else { chunk_end };
        if audio_end <= chunk_start {
            break;
        }
        let context_start = chunk_start - warmup;
        let samples = &audio[context_start..audio_end];
        let emit_after = if warmup > 0 { Some(chunk_start / FRAME) } else { None };
        let offset = if warmup > 0 { context_start } else { chunk_start };
        let toks = transcribe_chunk(models, &mut bufs, &audio_in, &audio_len, samples, offset, is_last, emit_after, &mut timing, &mut stats)?;
        if std::env::var_os("COREML_RS_DEBUG").is_some() {
            eprintln!("window {:>3} start {:>8.2}s end {:>8.2}s warmup {:>5} frames tokens {:>4}", windows.len(), context_start as f64 / SAMPLE_RATE as f64, audio_end as f64 / SAMPLE_RATE as f64, warmup / FRAME, toks.len());
        }
        windows.push(toks);
        if is_last {
            break;
        }
        chunk_start += lay.stride;
    }

    let mut merged: Vec<Token> = windows.first().cloned().unwrap_or_default();
    for w in windows.iter().skip(1) {
        merged = merge_windows(&merged, w, &vocab.splice_safe);
        merged = enforce_monotonic(merged);
    }
    Ok(Transcript { tokens: merged, chunks: windows.len(), timing, stats })
}

fn enforce_monotonic(mut tokens: Vec<Token>) -> Vec<Token> {
    let mut max = 0usize;
    for t in tokens.iter_mut() {
        if t.frame < max {
            t.frame = max;
        }
        max = t.frame;
    }
    tokens
}

/// mergeChunks: time-tolerant LCS over the overlap region, else midpoint cut.
fn merge_windows(left: &[Token], right: &[Token], safe: &HashSet<usize>) -> Vec<Token> {
    if left.is_empty() {
        return right.to_vec();
    }
    if right.is_empty() {
        return left.to_vec();
    }
    let start = |t: &Token| t.frame as f64 * FRAME_SECONDS;
    let left_end = start(left.last().unwrap()) + FRAME_SECONDS;
    let right_start = start(&right[0]);
    if left_end <= right_start {
        return [left, right].concat();
    }
    let ol: Vec<usize> = (0..left.len()).filter(|&i| start(&left[i]) + FRAME_SECONDS > right_start - OVERLAP_SECONDS).collect();
    let or: Vec<usize> = (0..right.len()).filter(|&i| start(&right[i]) < left_end + OVERLAP_SECONDS).collect();
    if ol.len() < 2 || or.len() < 2 {
        return merge_by_midpoint(left, right, left_end, right_start, safe);
    }
    let tol = OVERLAP_SECONDS / 2.0;
    let matches = |a: usize, b: usize| left[a].id == right[b].id && (start(&left[a]) - start(&right[b])).abs() < tol;
    // LCS
    let (n, m) = (ol.len(), or.len());
    let mut dp = vec![vec![0u16; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[i][j] = if matches(ol[i], or[j]) { dp[i + 1][j + 1] + 1 } else { dp[i + 1][j].max(dp[i][j + 1]) };
        }
    }
    let mut pairs = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if matches(ol[i], or[j]) {
            pairs.push((ol[i], or[j]));
            i += 1;
            j += 1;
        } else if dp[i + 1][j] >= dp[i][j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    if pairs.is_empty() {
        return merge_by_midpoint(left, right, left_end, right_start, safe);
    }
    // mergeUsingMatches
    let mut out: Vec<Token> = Vec::new();
    out.extend_from_slice(&left[..pairs[0].0]);
    for k in 0..pairs.len() {
        let (li, ri) = pairs[k];
        out.push(left[li]);
        if k + 1 < pairs.len() {
            let (nl, nr) = pairs[k + 1];
            let gap_l = &left[li + 1..nl];
            let gap_r = &right[ri + 1..nr];
            out.extend_from_slice(if gap_r.len() > gap_l.len() { gap_r } else { gap_l });
        }
    }
    let (last_l, last_r) = *pairs.last().unwrap();
    let tail = &right[last_r + 1..];
    if let Some(first) = tail.first() {
        if !safe.contains(&first.id) {
            // left owns the seam word; resume right at the next word start
            let mut cursor = last_l + 1;
            while cursor < left.len() && !safe.contains(&left[cursor].id) {
                out.push(left[cursor]);
                cursor += 1;
            }
            match tail.iter().position(|t| safe.contains(&t.id)) {
                Some(p) => out.extend_from_slice(&tail[p..]),
                None => out.extend_from_slice(tail),
            }
        } else {
            out.extend_from_slice(tail);
        }
    }
    out
}

fn merge_by_midpoint(left: &[Token], right: &[Token], left_end: f64, right_start: f64, safe: &HashSet<usize>) -> Vec<Token> {
    let cutoff = (left_end + right_start) / 2.0;
    let sec = |t: &Token| t.frame as f64 * FRAME_SECONDS;
    let mut le = left.iter().position(|t| sec(t) >= cutoff).unwrap_or(left.len());
    let mut rs = right.iter().position(|t| sec(t) >= cutoff).unwrap_or(right.len());
    if le > 0 {
        while le < left.len() && !safe.contains(&left[le].id) {
            le += 1;
        }
    }
    let mut scan = rs;
    while scan < right.len() && !safe.contains(&right[scan].id) {
        scan += 1;
    }
    if scan < right.len() {
        rs = scan;
    }
    [&left[..le], &right[rs..]].concat()
}

pub struct Word {
    pub word: String,
    pub start: f64,
    pub end: f64,
}

/// Tokens to text and word timings (TDT emission delay of one frame as in
/// FluidAudio's createTokenTimings).
pub fn render(tokens: &[Token], vocab: &Vocab) -> (String, Vec<Word>) {
    let mut text = String::new();
    let mut words: Vec<Word> = Vec::new();
    for t in tokens {
        let piece = vocab.piece(t.id);
        let start = (t.frame.saturating_sub(1)) as f64 * FRAME_SECONDS;
        let end = start + (t.dur as f64 * FRAME_SECONDS).max(FRAME_SECONDS);
        let starts_word = starts_word(piece);
        let body = strip_boundary(piece);
        if starts_word || words.is_empty() {
            if starts_word && !text.is_empty() {
                text.push(' ');
            }
            text.push_str(body);
            words.push(Word { word: body.to_string(), start, end });
        } else {
            text.push_str(body);
            if let Some(w) = words.last_mut() {
                w.word.push_str(body);
                w.end = end.max(w.end);
            }
        }
    }
    (text.trim().to_string(), words)
}
