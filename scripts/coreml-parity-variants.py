#!/usr/bin/env python3
"""Measurement only, never committed to a build: puts environment switches
into a scratch checkout of Steno for the variant runs of
`scripts/coreml-parity-forge.sh --variants` (A2 of
`.plans/2026-10-07-stable-promotion.md`). The CoreML crate's own merge,
seam-word collapse and seam-gap repair come back from the last commit that
had them (BASE below) as `steno_speech_coreml::old`.

usage: coreml-parity-variants.py <scratch checkout>

Switches (unset means the shipped behaviour):
  STENO_A2_TARGET, _SEARCH, _OVERLAP, _LONG_PAUSE, _PAD   chunker seconds
  STENO_A2_VAD=energy|all     the fixed 0.01 RMS detector, or all speech
  STENO_A2_SYMBOLS=n          symbols a frame
  STENO_A2_CASE=1             the merge matches ids case-insensitively
  STENO_A2_BACKWARD=1         the merge walks the LCS back from the end
  STENO_A2_COREML_MERGE=1     the CoreML crate's merge instead of the shared one
  STENO_A2_COLLAPSE=1         the CoreML crate's seam-word collapse after the merge
  STENO_A2_REPAIR=1           the CoreML crate's seam-gap repair after the merge
"""
import subprocess, sys, os

BASE = "c6cc64a98"
root = sys.argv[1]
cm = root + "/crates/steno-speech-coreml/src"

def show(path):
    return subprocess.run(["git", "-C", root, "show", f"{BASE}:{path}"], check=True, capture_output=True, text=True).stdout

os.makedirs(cm + "/old", exist_ok=True)
for name in ["chunking", "merge", "vocab"]:
    text = show(f"crates/steno-speech-coreml/src/{name}.rs")
    text = text[: text.index("#[cfg(test)]")] if "#[cfg(test)]" in text else text
    for a, b in [("use crate::Token;", "use super::Token;"), ("use crate::chunking::", "use super::chunking::"),
                 ("use crate::segments::is_swift_whitespace;", "use super::is_swift_whitespace;"), ("use crate::vocab::", "use super::vocab::")]:
        text = text.replace(a, b)
    open(f"{cm}/old/{name}.rs", "w").write(text)
open(cm + "/old/mod.rs", "w").write(r"""//! Measurement only: the CoreML crate's merge, seam collapse and seam-gap
//! repair as they were on main, over the shared tokens.
#![allow(dead_code, clippy::all, clippy::pedantic)]
pub mod chunking;
pub mod merge;
pub mod vocab;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Token { pub id: usize, pub frame: usize, pub confidence: f32, pub duration: usize }

impl From<steno_speech::Token> for Token {
    fn from(t: steno_speech::Token) -> Self { Token { id: t.id as usize, frame: t.frame, confidence: t.confidence, duration: t.duration } }
}
impl From<Token> for steno_speech::Token {
    fn from(t: Token) -> Self { steno_speech::Token { id: t.id as u32, frame: t.frame, confidence: t.confidence, duration: t.duration } }
}

pub(crate) fn is_swift_whitespace(c: char) -> bool {
    matches!(c, '\t' | ' ' | '\u{A0}' | '\u{1680}' | '\u{2000}'..='\u{200A}' | '\u{202F}' | '\u{205F}' | '\u{3000}')
}

/// The old merge sequence: fold `merge_chunks`, clamp, optional collapse.
pub fn coreml_merge(windows: &[Vec<steno_speech::Token>], vocab: &vocab::Vocab, merge: bool, collapse: bool, shared: &dyn Fn() -> Vec<steno_speech::Token>) -> Vec<steno_speech::Token> {
    let mut tokens: Vec<Token> = if merge {
        let mut it = windows.iter().map(|w| w.iter().map(|t| Token::from(*t)).collect::<Vec<_>>());
        let first = it.next().unwrap_or_default();
        merge::enforce_monotonic(it.fold(first, |m, w| merge::merge_chunks(&m, &w, vocab)))
    } else {
        shared().into_iter().map(Token::from).collect()
    };
    if collapse && windows.len() > 1 {
        tokens = merge::collapse_seam_word_duplicates(&tokens, vocab);
    }
    tokens.into_iter().map(Into::into).collect()
}

/// The old seam-gap repair over merged tokens, probing with `decode`
/// (a fresh window of one chunk at the gap start, then gap-centred).
pub fn repair(audio: &[f32], tokens: Vec<steno_speech::Token>, vocab: &vocab::Vocab, decode: &mut dyn FnMut(std::ops::Range<usize>) -> Vec<steno_speech::Token>) -> (Vec<steno_speech::Token>, usize, usize) {
    use chunking::*;
    let total = audio.len();
    let threshold = adaptive_speech_rms_threshold(audio);
    let min_gap_frames = ((1.5 / FRAME_SECONDS) as usize).max(2);
    let window_samples = Layout::v3().chunk_samples;
    let mut working: Vec<Token> = tokens.into_iter().map(Token::from).collect();
    let (mut probes, mut repaired) = (0usize, 0usize);
    let mut probed = std::collections::HashSet::new();
    for _ in 0..3 {
        let mut inserts: Vec<Token> = Vec::new();
        for index in 0..working.len().saturating_sub(1) {
            if probes >= 32 { break; }
            let current = working[index];
            let next = working[index + 1];
            let gap_start_frame = current.frame + current.duration.max(1);
            let gap_end_frame = next.frame;
            if gap_end_frame < gap_start_frame + min_gap_frames || probed.contains(&gap_start_frame) { continue; }
            let gs = gap_start_frame * FRAME_SAMPLES;
            let ge = (gap_end_frame * FRAME_SAMPLES).min(total);
            if ge <= gs { continue; }
            if speech_like_seconds(audio, gs, ge, threshold) < 0.5 { continue; }
            probed.insert(gap_start_frame);
            probes += 1;
            let center = usize::midpoint(gs, ge);
            let lead = merge::word_neighbor(&working, index, -1, vocab);
            let tail = merge::word_neighbor(&working, index + 1, 1, vocab);
            for placement in [gs, center.saturating_sub(window_samples / 2)] {
                let ws = placement.min(total.saturating_sub(window_samples)) / FRAME_SAMPLES * FRAME_SAMPLES;
                let we = (ws + window_samples).min(total);
                if we <= ws { continue; }
                let window: Vec<Token> = decode(ws..we).into_iter().map(Token::from).collect();
                let candidate = merge::splice_candidate(&window, gap_start_frame, gap_end_frame, lead, tail, vocab);
                if candidate.is_empty() { continue; }
                repaired += candidate.len();
                inserts.extend(candidate);
                break;
            }
        }
        if inserts.is_empty() { break; }
        working.extend(inserts);
        working.sort_by_key(|t| t.frame);
    }
    (working.into_iter().map(Into::into).collect(), probes, repaired)
}
""")
p=root+"/crates/steno-speech-coreml/src/engine.rs"
s=open(p).read()
old='''pub fn pipeline_config() -> PipelineConfig {'''
new='''pub fn pipeline_config() -> PipelineConfig {
    let env = |k: &str| std::env::var(k).ok();
    let num = |k: &str, d: f32| env(k).and_then(|v| v.parse().ok()).unwrap_or(d);
    let mut config = shipped_pipeline_config();
    config.chunker.target_seconds = num("STENO_A2_TARGET", config.chunker.target_seconds);
    config.chunker.search_seconds = num("STENO_A2_SEARCH", config.chunker.search_seconds);
    config.chunker.overlap_seconds = num("STENO_A2_OVERLAP", config.chunker.overlap_seconds);
    config.chunker.long_pause_seconds = num("STENO_A2_LONG_PAUSE", config.chunker.long_pause_seconds);
    config.chunker.pad_seconds = num("STENO_A2_PAD", config.chunker.pad_seconds);
    if let Some(v) = env("STENO_A2_SYMBOLS") { config.decoder.max_symbols_per_frame = v.parse().unwrap(); }
    config
}

fn shipped_pipeline_config() -> PipelineConfig {'''
assert old in s; s=s.replace(old,new)
old='''                    Box::new(AdaptiveEnergyVad {
                        config: VadConfig::default(),
                    }),'''
new='''                    match std::env::var("STENO_A2_VAD").as_deref() {
                        Ok("energy") => Box::new(steno_speech::EnergyVad::default()) as Box<dyn steno_speech::VoiceActivityDetector>,
                        Ok("all") => Box::new(steno_speech::EnergyVad { rms_threshold: 0.0, config: VadConfig::default() }),
                        _ => Box::new(AdaptiveEnergyVad {
                            config: VadConfig::default(),
                        }),
                    },'''
assert old in s; s=s.replace(old,new)
open(p,"w").write(s)

s=open(cm+'/lib.rs').read()
s=s.replace("pub mod parity;", "pub mod parity;\n#[cfg(target_os = \"macos\")]\npub mod old;", 1)
open(cm+'/lib.rs','w').write(s)
# shared pipeline: a merge hook
p=root+'/crates/steno-speech/src/pipeline.rs'
s=open(p).read()
s=s.replace('''/// When a chunk with speech decodes''','''/// Measurement only: replaces the merge.
pub type MergeHook = Box<dyn Fn(&[Vec<Token>], &dyn Fn() -> Vec<Token>) -> Vec<Token> + Send + Sync>;
pub static MERGE_HOOK: std::sync::OnceLock<MergeHook> = std::sync::OnceLock::new();

/// When a chunk with speech decodes''',1)
old='''        let tokens = merge_all(
            &windows,
            f64::from(self.config.chunker.overlap_seconds),
            &self.vocab,
        );'''
new='''        let shared = || merge_all(
            &windows,
            f64::from(self.config.chunker.overlap_seconds),
            &self.vocab,
        );
        let tokens = match MERGE_HOOK.get() { Some(hook) => hook(&windows, &shared), None => shared() };'''
assert old in s; s=s.replace(old,new); open(p,'w').write(s)
# shared merge: case-insensitive ids and the backward walk
p=root+'/crates/steno-speech/src/merge.rs'
s=open(p).read()
old='''    let matches = |a: usize, b: usize| {
        left[a].id == right[b].id && (seconds(&left[a]) - seconds(&right[b])).abs() < tolerance
    };'''
new='''    let case = std::env::var_os("STENO_A2_CASE").is_some();
    let ids = |a: u32, b: u32| a == b || (case && vocab.piece(a).to_lowercase() == vocab.piece(b).to_lowercase());
    let matches = |a: usize, b: usize| {
        ids(left[a].id, right[b].id) && (seconds(&left[a]) - seconds(&right[b])).abs() < tolerance
    };'''
assert old in s; s=s.replace(old,new)
old='''    let mut pairs = Vec::new();
    let (mut row, mut col) = (0, 0);
    while row < rows && col < cols {'''
new='''    let mut pairs = Vec::new();
    let backward = std::env::var_os("STENO_A2_BACKWARD").is_some();
    if backward {
        let mut dp = vec![vec![0u32; cols + 1]; rows + 1];
        for i in 1..=rows { for j in 1..=cols {
            dp[i][j] = if matches(overlap_left[i - 1], overlap_right[j - 1]) { dp[i - 1][j - 1] + 1 } else { dp[i - 1][j].max(dp[i][j - 1]) };
        } }
        let (mut i, mut j) = (rows, cols);
        while i > 0 && j > 0 {
            if matches(overlap_left[i - 1], overlap_right[j - 1]) { pairs.push((overlap_left[i - 1], overlap_right[j - 1])); i -= 1; j -= 1; }
            else if dp[i - 1][j] > dp[i][j - 1] { i -= 1 } else { j -= 1 }
        }
        pairs.reverse();
    }
    let (mut row, mut col) = (0, 0);
    while !backward && row < rows && col < cols {'''
assert old in s; s=s.replace(old,new)
open(p,'w').write(s)
# engine: install the hook and expose decode_range
p=cm+'/engine.rs'
s=open(p).read()
old='''            let models = Arc::new(Models::load(&self.model_directory)?);'''
new='''            let merge = std::env::var_os("STENO_A2_COREML_MERGE").is_some();
            let collapse = std::env::var_os("STENO_A2_COLLAPSE").is_some();
            if merge || collapse {
                let old = crate::old::vocab::Vocab::load(&self.model_directory.join("parakeet_vocab.json"))?;
                let _ = steno_speech::pipeline::MERGE_HOOK.set(Box::new(move |windows, shared| crate::old::coreml_merge(windows, &old, merge, collapse, shared)));
            }
            let models = Arc::new(Models::load(&self.model_directory)?);'''
assert old in s; s=s.replace(old,new)
old='''    /// `transcribe` without the trait: the segments for `audio`.'''
new='''    /// Measurement only.
    pub fn decode_range(&self, samples: &[f32], range: std::ops::Range<usize>) -> Vec<steno_speech::Token> {
        self.with_transcriber(|t| Ok(t.decode_range(samples, range, &mut steno_speech::DecodeStats::default())?)).unwrap()
    }

    /// Measurement only.
    pub fn render(&self, tokens: &[steno_speech::Token]) -> String {
        self.with_transcriber(|t| Ok(t.render(tokens))).unwrap()
    }

    /// `transcribe` without the trait: the segments for `audio`.'''
assert old in s; s=s.replace(old,new)
open(p,'w').write(s)
# parity: the repair pass
p=cm+'/parity.rs'
s=open(p).read()
old='''        let transcript = engine.transcribe_samples(&audio, None)?;'''
new='''        let mut transcript = engine.transcribe_samples(&audio, None)?;
        if std::env::var_os("STENO_A2_REPAIR").is_some() && transcript.chunks.len() > 1 {
            let old = crate::old::vocab::Vocab::load(&models.join("parakeet_vocab.json"))?;
            let (tokens, probes, repaired) = crate::old::repair(&audio, transcript.tokens.clone(), &old, &mut |range| engine.decode_range(&audio, range));
            eprintln!("repair: {probes} probes, {repaired} tokens");
            transcript.segments = vec![RawSegment { start: 0.0, end: audio_seconds, text: engine.render(&tokens), language: None, word_timings: None }];
            transcript.tokens = tokens;
        }'''
assert old in s; s=s.replace(old,new)
open(p,'w').write(s)
