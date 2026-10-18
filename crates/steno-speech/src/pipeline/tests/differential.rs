//! The differential test: the partition's merge against a frozen copy of
//! the merge before it (`chunker::gap_windows`, `splice_gaps`, `Seam::fit`,
//! `keep_chunk_and_overlap` and the recovery that scored on the chunk's
//! frames, as of 2fbab03dd; its gap windows read the tail-only setting of
//! [`Gaps::Tail`] since). The copy is not reused code, so the partition
//! can change under it; it gives the same tokens (id and sample) as that
//! commit on the jitter-free cases. It checks clause 5 of the ownership
//! invariant (A16a in `.plans/2026-10-07-stable-promotion.md`): every word
//! the old merge keeps, this one keeps, with its pieces and its boundaries,
//! on the layouts and fakes it runs ([`ref_losses`]).

use super::*;
use std::collections::{HashMap, HashSet};

fn ref_split(range: Range<usize>, max: usize, overlap: usize, out: &mut Vec<Range<usize>>) {
    let stride = range.len().saturating_sub(overlap);
    let count = stride.div_ceil(max - overlap).max(1);
    for i in 0..count {
        let start = range.start + i * stride / count;
        let end = range.start + (i + 1) * stride / count + overlap;
        out.push(start..end.min(range.end));
    }
}

fn ref_gap_windows(
    audio: &[f32],
    chunks: &[crate::chunker::Chunk],
    config: &ChunkerConfig,
) -> Vec<Range<usize>> {
    if config.decode_gaps == Gaps::None {
        return Vec::new();
    }
    let total = audio.len();
    let max = sample_count(config.max_seconds).max(1);
    let overlap = sample_count(config.overlap_seconds).min(max - 1);
    let shortest = sample_count(crate::chunker::MIN_GAP_SECONDS);
    let mut windows = Vec::new();
    let mut covered = 0;
    for (i, next) in chunks
        .iter()
        .map(|c| c.range.clone())
        .chain(std::iter::once(total..total))
        .enumerate()
    {
        let tail = i == chunks.len() && i > 0;
        if next.start >= covered + shortest && (config.decode_gaps == Gaps::All || tail) {
            ref_split(covered..next.start, max, overlap, &mut windows);
        }
        covered = covered.max(next.end);
    }
    windows.retain(|window| audio[window.clone()].iter().any(|&x| x != 0.0));
    windows
}

fn ref_starts_word(vocab: &Vocab, token: &Token) -> bool {
    starts_word(vocab.piece(token.id))
}

fn ref_repeats(earlier: &Token, later: &Token) -> bool {
    earlier.id == later.id && later.sample().abs_diff(earlier.sample()) <= 2 * FRAME_SAMPLES
}

fn ref_fit(vocab: &Vocab, mut part: Vec<Token>, tokens: &[Token], at: usize) -> Vec<Token> {
    if let Some(next) = tokens.get(at) {
        part.retain(|t| t.sample() < next.sample());
        let last = if ref_starts_word(vocab, next) {
            part.iter()
                .rposition(|t| ref_starts_word(vocab, t))
                .filter(|&i| ref_repeats(&part[i], next))
        } else {
            part.len().checked_sub(1).filter(|&i| {
                part[i].id == next.id && next.sample() - part[i].sample() < SAMPLE_RATE
            })
        };
        if let Some(last) = last {
            part.truncate(last);
        }
    }
    if at > 0 {
        let rest = part
            .iter()
            .take_while(|t| !ref_starts_word(vocab, t))
            .count();
        part.drain(..rest);
        let chunk_last = tokens[..at]
            .iter()
            .rev()
            .find(|t| ref_starts_word(vocab, t));
        if let (Some(first), Some(last)) = (part.first(), chunk_last)
            && ref_repeats(last, first)
        {
            let word = 1 + part[1..]
                .iter()
                .take_while(|t| !ref_starts_word(vocab, t))
                .count();
            part.drain(..word);
        }
    }
    part
}

fn ref_splice_gaps(
    tokens: &[Token],
    windows: &[Range<usize>],
    decoded: &[Vec<Token>],
    overlap_seconds: f32,
    vocab: &Vocab,
) -> Vec<Token> {
    let mut inserts: Vec<(usize, Vec<Token>)> = Vec::new();
    let mut first = 0;
    while first < windows.len() {
        let mut end = first + 1;
        while end < windows.len() && windows[end].start < windows[end - 1].end {
            end += 1;
        }
        let gap = merge_all(&decoded[first..end], f64::from(overlap_seconds), vocab);
        let span = windows[first].start..windows[end - 1].end;
        let at = tokens.partition_point(|t| t.sample() < span.start);
        let past = tokens.partition_point(|t| t.sample() < span.end);
        let kept_to = (span.start + sample_count(overlap_seconds)) / FRAME_SAMPLES;
        let later = tokens[at..past]
            .iter()
            .position(|t| t.frame > kept_to && ref_starts_word(vocab, t));
        let mid = later.map_or(past, |n| at + n);
        let sides = [at..mid, mid..past]
            .into_iter()
            .filter(|side| !side.is_empty());
        let mut covered_to = None;
        for side in sides.chain(std::iter::once(past..past)) {
            let uncovered = gap
                .iter()
                .filter(|t| covered_to.is_none_or(|to| t.sample() > to));
            inserts.push((
                side.start,
                ref_fit(vocab, uncovered.copied().collect(), tokens, side.start),
            ));
            covered_to = tokens[side].last().map(Token::sample);
        }
        first = end;
    }
    let mut out = Vec::new();
    let mut done = 0;
    for (at, window_tokens) in inserts {
        out.extend_from_slice(&tokens[done..at]);
        out.extend(window_tokens);
        done = at;
    }
    out.extend_from_slice(&tokens[done..]);
    out
}

fn ref_keep_chunk_and_overlap(
    tokens: &mut Vec<Token>,
    range: &Range<usize>,
    overlap: usize,
    vocab: &Vocab,
) {
    let frames =
        range.start.saturating_sub(overlap) / FRAME_SAMPLES..=(range.end + overlap) / FRAME_SAMPLES;
    let inside = |t: &Token| frames.contains(&t.frame);
    let (Some(first), Some(last)) = (
        tokens.iter().position(inside),
        tokens.iter().rposition(inside),
    ) else {
        tokens.clear();
        return;
    };
    let splice_safe = |t: &Token| vocab.is_splice_safe(t.id);
    let start = tokens[..=first].iter().rposition(splice_safe).unwrap_or(0);
    let end = tokens[last + 1..]
        .iter()
        .position(splice_safe)
        .map_or(tokens.len(), |n| last + 1 + n);
    tokens.truncate(end);
    tokens.drain(..start);
}

fn ref_words_inside(vocab: &Vocab, tokens: &[Token], range: &Range<usize>) -> usize {
    let frames = range.start / FRAME_SAMPLES..=range.end / FRAME_SAMPLES;
    tokens
        .iter()
        .filter(|t| frames.contains(&t.frame) && ref_starts_word(vocab, t))
        .count()
}

/// The old merge's transcription of `samples` down to the tokens, on
/// `t`'s first backend and detector.
fn ref_transcribe(t: &mut Transcriber<FrameTokenBackend>, samples: &[f32]) -> Vec<Token> {
    let speech = t.vad.speech_regions(samples).unwrap();
    let config = t.config.clone();
    let vocab = t.vocab.clone();
    let chunks = layout(samples, &speech, &config.chunker);
    let gaps = ref_gap_windows(samples, &chunks, &config.chunker);
    let mut stats = DecodeStats::default();
    let max = sample_count(config.chunker.max_seconds);
    let overlap = sample_count(config.chunker.overlap_seconds);
    let mut decoded = Vec::new();
    for chunk in &chunks {
        let range = &chunk.range;
        let original = t.decode_range(samples, range.clone(), &mut stats).unwrap();
        let speech_seconds = speech_inside(&speech, range);
        let words = ref_words_inside(&vocab, &original, range) as f32;
        let empty = speech_seconds >= config.recovery.min_speech_seconds
            && words < config.recovery.min_words_per_speech_second * speech_seconds;
        if !empty {
            decoded.push(original);
            continue;
        }
        let original_words = ref_words_inside(&vocab, &original, range);
        let (mut best, mut best_words) = (original, original_words);
        for &(before, after) in &config.recovery.extensions_seconds {
            let start = range.start.saturating_sub(sample_count(before));
            let end = (range.end + sample_count(after)).min(samples.len());
            if end - start > max || (start == range.start && end == range.end) {
                continue;
            }
            let candidate = t.decode_range(samples, start..end, &mut stats).unwrap();
            let words = ref_words_inside(&vocab, &candidate, range);
            if words > best_words {
                best = candidate;
                best_words = words;
            }
        }
        if best_words > original_words {
            ref_keep_chunk_and_overlap(&mut best, range, overlap, &vocab);
        }
        decoded.push(best);
    }
    let extra: Vec<Vec<Token>> = gaps
        .iter()
        .map(|w| t.decode_range(samples, w.clone(), &mut stats).unwrap())
        .collect();
    let overlap_seconds = config.chunker.overlap_seconds;
    let merged = merge_all(&decoded, f64::from(overlap_seconds), &vocab);
    ref_splice_gaps(&merged, &gaps, &extra, overlap_seconds, &vocab)
}

/// Every token the final windows heard that their span keeps, taken from
/// the window the transcript records ([`Transcript::windows`]) and decoded
/// again: a plain chunk's whole window (`.0`), and the reach of a gap's
/// window (the window) or of an accepted recovery (`.0` and `.1`). A plain
/// chunk's decode goes whole to its run's LCS merge, which A16a leaves as
/// it was. And the pieces each accepted recovery's keep (`own` over its
/// reach) begins with that continue a word (`.2`).
fn ref_heard(
    t: &mut Transcriber<FrameTokenBackend>,
    samples: &[f32],
    tr: &Transcript,
) -> (Vec<Token>, Vec<Token>, Vec<Token>) {
    let mut stats = DecodeStats::default();
    let mut heard = (Vec::new(), Vec::new(), Vec::new());
    for (span, window) in tr.spans.iter().zip(&tr.windows) {
        let plain = span.chunk && *window == span.window;
        let bound = if plain { &span.window } else { &span.reach };
        let tokens = t.decode_range(samples, window.clone(), &mut stats).unwrap();
        if span.chunk && !plain {
            let kept = own(&tokens, &span.reach, &t.vocab);
            let vocab = &t.vocab;
            heard
                .2
                .extend(kept.iter().take_while(|p| !ref_starts_word(vocab, p)));
        }
        let kept: Vec<Token> = tokens
            .into_iter()
            .filter(|t| bound.contains(&t.sample()))
            .collect();
        if !plain {
            heard.1.extend_from_slice(&kept);
        }
        heard.0.extend(kept);
    }
    heard
}

/// A placed word at `seconds`.
fn ref_placed(pieces: Vec<u32>, seconds: f32, frames: usize) -> Placed {
    Placed {
        pieces,
        frame: sample_count(seconds) / FRAME_SAMPLES,
        frames,
    }
}

fn ref_tenths(rng: &mut Rng, from: f32, count: usize) -> f32 {
    from + rng.below(count.max(1)) as f32 / 10.0
}

/// A differential case: the samples, the words placed in them, and fixed
/// speech regions or the detector's.
struct RefCase {
    samples: Vec<f32>,
    words: Vec<Placed>,
    regions: Option<Vec<Range<usize>>>,
}

/// `words` put into `samples`, over the fixed speech `regions`.
fn ref_case(mut samples: Vec<f32>, words: Vec<Placed>, regions: Vec<Range<usize>>) -> RefCase {
    for word in &words {
        word.put(&mut samples);
    }
    RefCase {
        samples,
        words,
        regions: Some(regions),
    }
}

/// Words placed at encoder frames: their pieces, the first piece's frame
/// and each piece's frames.
fn ref_at(words: &[(&[u32], usize, usize)]) -> Vec<Placed> {
    let word = |&(pieces, frame, frames): &(&[u32], usize, usize)| Placed {
        pieces: pieces.to_vec(),
        frame,
        frames,
    };
    words.iter().map(word).collect()
}

/// The old merge's tests' random layout: the 20 words, loud and quiet, one or two
/// pieces, a quarter said twice in a row, some after loud sound the
/// fake decodes to nothing.
fn ref_random_layout(rng: &mut Rng, case: usize) -> RefCase {
    let total = (20 + rng.below(50)) * SAMPLE_RATE;
    let mut samples = vec![0.0f32; total];
    if case.is_multiple_of(2) {
        for (i, x) in samples.iter_mut().enumerate() {
            *x = if i % 2 == 0 { 3e-5 } else { -3e-5 };
        }
    }
    let mut ids: Vec<u32> = (1..=20).collect();
    for i in (1..ids.len()).rev() {
        ids.swap(i, rng.below(i + 1));
    }
    let mut words: Vec<Placed> = Vec::new();
    let mut frame = rng.below(4 * SAMPLE_RATE) / FRAME_SAMPLES;
    'outer: for id in ids {
        let frames = 3 + rng.below(6);
        let pieces = if case % 3 == 1 {
            vec![id, 20 + id]
        } else {
            vec![id]
        };
        for _ in 0..=usize::from(rng.below(4) == 0) {
            let word = Placed {
                pieces: pieces.clone(),
                frame,
                frames,
            };
            if word.span().end + FRAME_SAMPLES >= total {
                break 'outer;
            }
            if id > 10 && rng.below(3) == 0 {
                let from = frame.saturating_sub(10 + rng.below(60)) * FRAME_SAMPLES;
                let from = from.max(words.last().map_or(0, |w| w.span().end));
                samples[from..word.start(0)].fill(0.05);
            }
            word.put(&mut samples);
            frame += pieces.len() * frames + 2 + rng.below(4);
            words.push(word);
        }
        let pause = if rng.below(3) == 0 { 12 } else { 3 };
        frame += rng.below(pause * SAMPLE_RATE) / FRAME_SAMPLES;
    }
    RefCase {
        samples,
        words,
        regions: None,
    }
}

/// The old merge's tests' two-sided layout: chunk A, a gap of 2.6 to 20 s, chunk B,
/// each plain, empty and recovered into the gap, or absent; quiet
/// words in the gap, in either recovery's reach and across it.
fn ref_two_sided_layout(rng: &mut Rng) -> RefCase {
    let (mode_a, mut mode_b) = (rng.below(3), rng.below(3));
    if mode_a == 2 && mode_b == 2 {
        mode_b = 0;
    }
    let gap = if rng.below(2) == 0 {
        ref_tenths(rng, 2.6, 45)
    } else {
        ref_tenths(rng, 7.0, 130)
    };
    let lengths = |rng: &mut Rng, mode| match mode {
        0 => ref_tenths(rng, 6.0, 25),
        1 => ref_tenths(rng, 10.8, 20),
        _ => 0.0,
    };
    let a = if mode_a == 2 {
        0.0
    } else {
        ref_tenths(rng, 1.0, 20)
    };
    let a_end = a + lengths(rng, mode_a);
    let b = a_end + gap;
    let total = b + lengths(rng, mode_b);
    let mut samples = vec![0.0f32; sample_count(total)];
    if rng.below(2) == 0 {
        for (i, x) in samples.iter_mut().enumerate() {
            *x = if i % 2 == 0 { 3e-5 } else { -3e-5 };
        }
    }
    let (mut words, mut regions) = (Vec::new(), Vec::new());
    for (mode, from, to, ids) in [
        (mode_a, a, a_end, [11, 13, 14, 15, 16]),
        (mode_b, b, total, [12, 17, 18, 19, 20]),
    ] {
        let region = sample_count(from)..sample_count(to);
        samples[region.clone()].fill(0.05);
        regions.extend((!region.is_empty()).then_some(region));
        let count = [1, 5, 0][mode];
        for (i, id) in ids.into_iter().take(count).enumerate() {
            words.push(ref_placed(vec![id], from + 0.5 + 2.0 * i as f32, 5));
        }
    }
    let quiet = |rng: &mut Rng, id: u32, seconds: f32| {
        let pieces = [id, 20 + id, 30 + id][..=rng.below(3)].to_vec();
        let frames = 2 + rng.below(6);
        let length = (pieces.len() * frames) as f32 * 0.08;
        (ref_placed(pieces, seconds, frames), length)
    };
    let mut gap_words = Vec::new();
    let straddle = rng.below(3);
    if mode_a != 2 && straddle == 1 {
        let at = ref_tenths(rng, a_end + 1.6, 4);
        let long = ref_placed(vec![2, 22, 32], at, 6 + rng.below(2));
        let past = long.span().end as f32 / SAMPLE_RATE as f32 + ref_tenths(rng, 0.08, 3);
        gap_words.push(long);
        gap_words.push(ref_placed(vec![7], past, 2 + rng.below(2)));
    } else if mode_a != 2 {
        let at = ref_tenths(rng, a_end + 0.3, 17);
        gap_words.push(quiet(rng, 2, at).0);
    }
    if mode_b != 2 && straddle == 2 {
        let frames = 6 + rng.below(2);
        let start = ref_tenths(rng, b - 1.9, 4) - 0.24 * frames as f32;
        gap_words.push(ref_placed(vec![8, 28, 38], start, frames));
        let before = start - ref_tenths(rng, 0.32, 3);
        gap_words.push(ref_placed(vec![9], before, 3));
    } else if mode_b != 2 {
        let (mut word, length) = quiet(rng, 4, 0.0);
        word.frame = sample_count(b - ref_tenths(rng, 0.3, 16) - length) / FRAME_SAMPLES;
        gap_words.push(word);
    }
    for id in [3, 5, 6, 10] {
        if rng.below(3) > 0 {
            let at = ref_tenths(rng, a_end + 0.1, ((gap - 0.2) * 10.0) as usize);
            gap_words.push(quiet(rng, id, at).0);
        }
    }
    let (lo, hi) = (sample_count(a_end + 0.1), sample_count(b - 0.1));
    let margin = 2 * FRAME_SAMPLES;
    for word in gap_words {
        let inside = lo <= word.start(0) && word.span().end <= hi;
        let clear = words.iter().all(|o: &Placed| {
            o.start(0) >= word.span().end + margin || word.start(0) >= o.span().end + margin
        });
        if inside && clear {
            words.push(word);
        }
    }
    for word in &words {
        word.put(&mut samples);
    }
    RefCase {
        samples,
        words,
        regions: Some(regions),
    }
}

/// Two chunks around a pause no window decodes, one plain and dense with
/// words to its edge, the other loud sound the fake decodes to nothing,
/// with one word, which a recovery hears; and quiet words in the pause
/// near the plain chunk that reuse its ids, which a recovery's LCS with
/// the plain chunk can take for the plain chunk's own words.
fn ref_collision_layout(rng: &mut Rng) -> RefCase {
    let backward = rng.below(2) == 0;
    let gap = ref_tenths(rng, 3.1, 100);
    let plain = ref_tenths(rng, 6.0, 60);
    let recovered = ref_tenths(rng, 4.0, 40);
    let (a_len, b_len) = if backward {
        (plain, recovered)
    } else {
        (recovered, plain)
    };
    let (a, a_end) = (1.0, 1.0 + a_len);
    let (b, b_end) = (a_end + gap, a_end + gap + b_len);
    let mut samples = vec![0.0f32; sample_count(b_end + 1.0)];
    let regions = vec![
        sample_count(a)..sample_count(a_end),
        sample_count(b)..sample_count(b_end),
    ];
    let loud = regions[usize::from(backward)].clone();
    samples[loud.clone()].fill(0.05);
    let mut words = vec![Placed {
        pieces: vec![if backward { 17 } else { 18 }],
        frame: loud.start.midpoint(loud.end) / FRAME_SAMPLES,
        frames: 5,
    }];
    let (from, to) = if backward { (a, a_end) } else { (b, b_end) };
    let mut at = from + 0.3;
    while at < to - 0.3 {
        let frames = 3 + rng.below(3);
        let word = ref_placed(vec![11 + rng.below(6) as u32], at, frames);
        if word.span().end + FRAME_SAMPLES > sample_count(to) {
            break;
        }
        at += frames as f32 * 0.08 + 0.08 + ref_tenths(rng, 0.0, 3);
        words.push(word);
    }
    let (lo, hi) = (sample_count(a_end + 0.3), sample_count(b - 0.3));
    for _ in 0..=rng.below(3) {
        let off = ref_tenths(rng, 0.35, 12);
        let frames = 2 + rng.below(3);
        let at = if backward {
            a_end + off
        } else {
            b - off - frames as f32 * 0.08
        };
        let word = ref_placed(vec![11 + rng.below(6) as u32], at, frames);
        let margin = 2 * FRAME_SAMPLES;
        let clear = words.iter().all(|o: &Placed| {
            o.start(0) >= word.span().end + margin || word.start(0) >= o.span().end + margin
        });
        if clear && lo < word.start(0) && word.span().end < hi {
            words.push(word);
        }
    }
    for word in &words {
        word.put(&mut samples);
    }
    words.sort_by_key(|w| w.frame);
    RefCase {
        samples,
        words,
        regions: Some(regions),
    }
}

/// The general layout's regions in seconds, whether each is the loud
/// sound the fake decodes to nothing, and the recording's length.
fn ref_general_regions(rng: &mut Rng) -> (Vec<(f32, f32, bool)>, f32) {
    let count = 1 + rng.below(4);
    let mut t = if rng.below(3) == 0 {
        0.0
    } else {
        ref_tenths(rng, 0.2, 60)
    };
    let mut regions = Vec::new();
    for i in 0..count {
        let empty = rng.below(2) == 0;
        let length = if empty {
            ref_tenths(rng, 3.0, 70)
        } else {
            ref_tenths(rng, 1.5, 200)
        };
        regions.push((t, t + length, empty));
        t += length;
        if i + 1 < count {
            t += match rng.below(4) {
                0 => ref_tenths(rng, 0.3, 25),
                1 => ref_tenths(rng, 2.5, 60),
                2 => ref_tenths(rng, 8.0, 120),
                _ => ref_tenths(rng, 20.0, 260),
            };
        }
    }
    let tail = if rng.below(3) == 0 {
        0.0
    } else {
        ref_tenths(rng, 0.2, 200)
    };
    (regions, t + tail)
}

/// One to four speech regions, each plain (words of any length to its
/// edges) or loud sound the fake decodes to nothing with a few words in
/// it, pauses of 0.3 to 46 s between them, so some gaps take several
/// windows, and a lead-in and a tail of up to 20 s, the first region now
/// and then at the recording's start and the last at its end; quiet words
/// of any id in the pauses, so the chunks' ids too, dense near the
/// regions' edges; a quarter of the words ending in a full stop; and now
/// and then a word on the recording's first frames or its last.
fn ref_general_layout(rng: &mut Rng) -> RefCase {
    let (regions, total) = ref_general_regions(rng);
    let end = sample_count(total);
    let mut samples = vec![0.0f32; end];
    if rng.below(3) != 0 {
        for (i, x) in samples.iter_mut().enumerate() {
            *x = if i % 2 == 0 { 3e-5 } else { -3e-5 };
        }
    }
    let mut words: Vec<Placed> = Vec::new();
    let try_put = |words: &mut Vec<Placed>, word: Placed, until: usize| {
        let margin = 4 * FRAME_SAMPLES;
        let clear = words.iter().all(|o: &Placed| {
            o.start(0) >= word.span().end + margin || word.start(0) >= o.span().end + margin
        });
        if clear && word.span().end <= until && word.span().end + FRAME_SAMPLES <= end {
            words.push(word);
        }
    };
    let word = |rng: &mut Rng, first: u32, seconds: f32| {
        let id = first + rng.below((21 - first) as usize) as u32;
        let mut pieces = vec![id];
        for _ in 0..rng.below(3) {
            pieces.push(20 + id);
        }
        if rng.below(4) == 0 {
            pieces.push(41);
        }
        ref_placed(pieces, seconds.max(0.0), 2 + rng.below(5))
    };
    for &(from, to, empty) in &regions {
        let region = sample_count(from)..sample_count(to);
        if empty {
            samples[region.clone()].fill(0.05);
            for _ in 0..=rng.below(4) {
                let room = ((to - from - 0.6) * 10.0).max(1.0) as usize;
                let at = from + ref_tenths(rng, 0.2, room);
                try_put(&mut words, word(rng, 11, at), region.end);
            }
        } else {
            let mut at = from + ref_tenths(rng, 0.0, 4);
            while at < to {
                let next = word(rng, 11, at);
                at += next.span().len() as f32 / SAMPLE_RATE as f32 + ref_tenths(rng, 0.1, 10);
                try_put(&mut words, next, region.end);
            }
        }
    }
    let mut pauses = Vec::new();
    let mut cursor = 0.0f32;
    for &(from, to, _) in &regions {
        if from > cursor {
            pauses.push((cursor, from));
        }
        cursor = to;
    }
    if total > cursor {
        pauses.push((cursor, total));
    }
    for (from, to) in pauses {
        let length = to - from;
        for _ in 0..=(length / 1.5) as usize {
            let at = match rng.below(4) {
                0 => from + ref_tenths(rng, 0.0, 25),
                1 => to - ref_tenths(rng, 0.3, 25),
                _ => from + ref_tenths(rng, 0.0, (length * 10.0) as usize),
            };
            if at >= from && at < to {
                try_put(&mut words, word(rng, 1, at), sample_count(to));
            }
        }
    }
    if rng.below(4) == 0 {
        try_put(&mut words, word(rng, 1, 0.0), end);
    }
    if rng.below(4) == 0 {
        try_put(&mut words, word(rng, 1, total - 0.5), end);
    }
    for word in &words {
        word.put(&mut samples);
    }
    words.sort_by_key(|w| w.frame);
    let regions = regions
        .iter()
        .map(|&(from, to, _)| sample_count(from)..sample_count(to));
    RefCase {
        samples,
        words,
        regions: Some(regions.collect()),
    }
}

/// The general layout and quiet words of any id within 2.4 s of every
/// owned boundary and window edge of its partition under `config`, a third
/// of them orphans: a word whose start the recording cuts off (it begins
/// with a continuation) or a lone full stop.
fn ref_seam_layout(rng: &mut Rng, config: &PipelineConfig) -> RefCase {
    let mut case = ref_general_layout(rng);
    let total = case.samples.len();
    let regions = case.regions.as_deref().unwrap_or_default();
    let spans = partition(&case.samples, regions, &config.chunker);
    let mut edges: Vec<usize> = spans
        .iter()
        .flat_map(|s| [s.owned.start, s.window.start, s.window.end])
        .filter(|&x| x > 0 && x < total)
        .collect();
    edges.sort_unstable();
    edges.dedup();
    let mut words = std::mem::take(&mut case.words);
    let margin = 4 * FRAME_SAMPLES;
    for edge in edges {
        for _ in 0..3 {
            let Some(frame) =
                (edge / FRAME_SAMPLES).checked_add_signed(rng.below(61) as isize - 30)
            else {
                continue;
            };
            let id = 1 + rng.below(20) as u32;
            let mut pieces = vec![id];
            for _ in 0..rng.below(3) {
                pieces.push(20 + id);
            }
            if rng.below(4) == 0 {
                pieces.push(41);
            }
            if rng.below(3) == 0 {
                pieces[0] = if rng.below(3) == 0 { 41 } else { 20 + id };
            }
            let word = Placed {
                pieces,
                frame,
                frames: 2 + rng.below(5),
            };
            let clear = words.iter().all(|o: &Placed| {
                o.start(0) >= word.span().end + margin || word.start(0) >= o.span().end + margin
            });
            if clear && word.span().end + FRAME_SAMPLES <= total {
                words.push(word);
            }
        }
    }
    for word in &words {
        word.put(&mut case.samples);
    }
    words.sort_by_key(|w| w.frame);
    case.words = words;
    case
}

/// The placed word and piece `token` is: the nearest piece of its id
/// within `slack` frames.
fn ref_place(token: &Token, words: &[Placed], slack: usize) -> Option<(usize, usize)> {
    words
        .iter()
        .enumerate()
        .flat_map(|(w, word)| (0..word.pieces.len()).map(move |i| (w, i, word)))
        .filter(|&(_, i, word)| word.piece_of(token, slack) == Some(i))
        .min_by_key(|&(_, i, word)| word.start(i).abs_diff(token.sample()))
        .map(|(w, i, _)| (w, i))
}

/// Per word, how often its first piece came out and which pieces did,
/// each token at its place ([`ref_place`]).
fn ref_tally(tokens: &[Token], words: &[Placed], slack: usize) -> Vec<(usize, u8)> {
    let mut per = vec![(0, 0u8); words.len()];
    for (w, i) in tokens.iter().filter_map(|t| ref_place(t, words, slack)) {
        per[w].0 += usize::from(i == 0);
        per[w].1 |= 1 << i;
    }
    per
}

/// A token of a merge's text: its place ([`ref_place`]) and where it
/// starts.
type RefPiece = (Option<(usize, usize)>, usize);

/// The words a merge's tokens make in its text, grouped as the transcript
/// groups them: each from a word start up to the next, a continuation or a
/// full stop joining the word before it.
fn ref_joins(
    tokens: &[Token],
    words: &[Placed],
    vocab: &Vocab,
    slack: usize,
) -> Vec<Vec<RefPiece>> {
    let place = |t: &Token| (ref_place(t, words, slack), t.sample());
    tokens
        .chunk_by(|_, next| !ref_starts_word(vocab, next))
        .map(|word| word.iter().map(place).collect())
        .collect()
}

/// One differential run: the old merge's tokens, the transcript, what its
/// final windows heard, what its gap windows and accepted recoveries
/// heard in their reach, and the orphans its accepted recoveries begin
/// with ([`ref_heard`]).
struct RefRun {
    old: Vec<Token>,
    new: Transcript,
    heard: Vec<Token>,
    reached: Vec<Token>,
    /// The orphans accepted recoveries begin with.
    leads: Vec<Token>,
}

/// How the fake hears one case: the jitter in frames and the deaf edges
/// (`FrameTokenBackend::jitter`, `FrameTokenBackend::deaf`).
#[derive(Clone, Copy)]
struct Hearing {
    jitter: usize,
    deaf: (usize, usize, usize),
}

impl Hearing {
    /// A fake `jitter` frames off that hears every frame.
    fn every_frame(jitter: usize) -> Self {
        Self {
            jitter,
            deaf: (0, 0, 1),
        }
    }

    /// A quiet transcriber under `config` whose fake hears this way and
    /// decodes a window under `min_frames` frames that holds loud sound to
    /// nothing, with speech in `regions` if given.
    fn transcriber(
        self,
        config: &PipelineConfig,
        min_frames: usize,
        regions: Option<&[Range<usize>]>,
    ) -> Transcriber<FrameTokenBackend> {
        let mut t = quiet_transcriber(config.clone(), min_frames);
        t.workers[0].jitter = self.jitter;
        t.workers[0].deaf = self.deaf;
        if let Some(regions) = regions {
            t.vad = Box::new(FixedVad(regions.to_vec()));
        }
        t
    }
}

/// The deaf edges the differential test runs, beside hearing every frame:
/// a window's first or last 30 or 10 frames on every window, both edges on
/// every second window, and 15 frames either side on every third.
const DEAF: [(usize, usize, usize); 6] = [
    (30, 0, 1),
    (0, 30, 1),
    (10, 0, 1),
    (0, 10, 1),
    (30, 30, 2),
    (15, 15, 3),
];

fn ref_run(case: &RefCase, config: &PipelineConfig, min_frames: usize, hearing: Hearing) -> RefRun {
    let samples = &case.samples;
    let make = || hearing.transcriber(config, min_frames, case.regions.as_deref());
    let old = ref_transcribe(&mut make(), samples);
    let mut t = make();
    let new = t.transcribe(samples, None).unwrap();
    let (heard, reached, leads) = ref_heard(&mut t, samples, &new);
    RefRun {
        old,
        new,
        heard,
        reached,
        leads,
    }
}

/// What one case lost against the old merge, by placed word. `lost`,
/// `pieces` and `glued` are the gate; `unheard` flags a case to look at;
/// `glues`, `led` and `doubled` are counted.
#[derive(Debug, Default)]
struct Losses {
    /// Words the old merge keeps and the transcript loses.
    lost: Vec<usize>,
    /// Words the transcript keeps without a piece the old merge gave them.
    pieces: Vec<usize>,
    /// Words of the old merge's text that the transcript's text holds
    /// nowhere with their pieces and their boundaries.
    glued: Vec<usize>,
    /// Words a gap's window or an accepted recovery heard in its reach
    /// that the transcript loses.
    unheard: Vec<usize>,
    /// Words of the transcript's text that hold a word of the old merge's
    /// with more pieces, extra pieces the boundaries allow: counted.
    glues: usize,
    /// Words of the old merge's text whose last pieces come out only as
    /// the orphan an accepted recovery begins with (limit E): counted.
    led: usize,
    /// Words of the old merge's text the boundaries flag, held once a
    /// later piece of a placed word may cross chunks' owned boundaries
    /// that lie in no decoded window, with no decoded window wholly
    /// between the two pieces and no piece the old merge kept of another
    /// word between ([`RefTexts::kept_between`]): the named limit of a
    /// recovery that missed a word's start gluing its rest onto a word
    /// across an undecoded gap.
    /// Counted, not passed: the gate caps it at its value on the CI seed,
    /// 0.
    across_gap: usize,
    /// Words of the old merge's text the boundaries flag, held once such a
    /// piece, of a place the old merge did not keep and in an accepted
    /// recovery's owned span, may cross that span's owned start alone,
    /// which the recovery's window holds, with no piece the old merge kept
    /// of another word between: limit E across its own window. Counted and
    /// capped as `across_gap`, at 0.
    across_own_window: usize,
    /// Words of the old merge's text the boundaries flag, held once a
    /// piece of an orphan word the old merge dropped may cross as
    /// `across_own_window`'s piece does: limit (ii) across its own window.
    /// Counted and capped as `across_gap`, at 0.
    orphan_across_own_window: usize,
    /// Words of the old merge's text the boundaries flag, held once a
    /// piece of an orphan word the old merge dropped may lie inside the
    /// old merge's own glue after the same word (before the last piece of
    /// another word it glued there), with no piece the old merge kept of
    /// another word between: limit (ii) inside the old merge's glue.
    /// Counted and capped as `across_gap`, at 0.
    inside_glue: usize,
    /// Words of the old merge's text the boundaries flag, held once the
    /// rest of a word the old merge kept nothing of, which an accepted
    /// recovery begins with, may lie inside the old merge's own glue after
    /// the same word, across only owned boundaries that a gap's span shares
    /// and a window decodes, with no piece the old merge kept of another
    /// word between: limit E inside the old merge's glue. Counted and
    /// capped as `across_gap`, at 0.
    rest_inside_glue: usize,
    /// The same as `rest_inside_glue`, for the old merge's text-leading
    /// orphan ([`RefTexts::lead_glued`]): limit E inside the old merge's
    /// leading glue. Counted and capped as `across_gap`, at 0.
    rest_inside_lead_glue: usize,
    /// The old merge's text-leading orphan, an accepted recovery's leading
    /// orphan, held once copies of pieces of its own word in the old
    /// merge's text may come before it, with no piece the old merge kept of
    /// another word between: a copy before the text-leading orphan.
    /// Counted and capped as `across_gap`, at 0.
    copy_before_lead: usize,
    /// Flags in `glued` that an unplaced piece makes: the word would be
    /// held if a piece no placed word lies near could be an extra one.
    /// Counted beside the flags.
    unplaced: usize,
    /// Words that came out more often than from the old merge.
    doubled: usize,
}

impl Losses {
    /// Whether the case passes the gate: no word lost, no piece lost, no
    /// word glued beyond what the boundaries allow.
    fn passes(&self) -> bool {
        self.lost.is_empty() && self.pieces.is_empty() && self.glued.is_empty()
    }

    /// Counts a flagged word that `class` holds, from the old merge's
    /// text-leading orphan if `lead`.
    fn count(&mut self, class: Extras, lead: bool) {
        *match class {
            Extras::AcrossGap => &mut self.across_gap,
            Extras::AcrossOwnWindow => &mut self.across_own_window,
            Extras::OrphanAcrossOwnWindow => &mut self.orphan_across_own_window,
            Extras::InsideGlue => &mut self.inside_glue,
            Extras::RestInsideGlue if lead => &mut self.rest_inside_lead_glue,
            Extras::RestInsideGlue => &mut self.rest_inside_glue,
            Extras::CopyBeforeLead => &mut self.copy_before_lead,
            Extras::Check | Extras::Unplaced => unreachable!("no counted class"),
        } += 1;
    }
}

/// Where `run` loses against the old merge ([`Losses`]). `unheard` also
/// fires on a word both merges drop, as a run's LCS can (between two
/// recoveries, say), so it flags a case to look at, and the comparison
/// with the old merge decides whether it is a loss.
///
/// A sanity rule comes first: identical token sequences pass, so the check
/// never fails the old merge's text against itself. The jitter can put a
/// piece beyond the slack of every placed word, which the boundaries then
/// cannot place (case 158 of seed 0xa09c6d2c7ddf7445, at jitter 3,
/// `the_boundary_check_never_fails_the_old_merges_text_against_itself`);
/// a flag such a piece makes on a text that differs is counted apart
/// ([`Losses::unplaced`]).
///
/// The boundaries: every word of the old merge's text (its tokens from a
/// word start to the next) comes out in the transcript's text with the
/// same pieces at the same places, at least once, as a word of its own or
/// as the start of a longer word. A longer word passes only if each of its
/// extra pieces is one the old merge glued after the same word, or one with
/// no chunk's owned span starting or ending between it and the word's
/// first piece: a copy of a piece the old merge kept in another of its
/// words (which then has to come out on its own), or a piece of a word the
/// old merge dropped. A word that grows across a chunk, a copy or not,
/// fails; so do two of the old merge's words joined, and a full stop moved
/// onto another word.
///
/// An orphan never counts in `unheard`: after a chunk's token both merges
/// drop the pieces a window begins with that continue a word.
///
/// A word whose first piece repeats the last piece of the word before it,
/// or that repeats that word, counts only where a final window heard it,
/// never in `unheard`: the fake never gives one piece twice in a row, so a
/// window that heard the word before swallows it, which a model does not,
/// and a run's LCS may take two words said alike for one.
///
/// # The boundaries' exemptions
///
/// The table is closed. A text-only glue shape found later becomes a
/// named limit whose count the gate reports and caps at its value on the
/// CI seed; it never joins this table. A lost word or a lost piece always
/// fails. The limits counted outside the table so far, each 0 on the CI
/// seed:
///
/// - A recovery that missed a word's start can glue its rest onto a word
///   across an undecoded gap ([`Losses::across_gap`];
///   `a_recovery_that_missed_a_words_start_can_glue_its_rest_onto_a_word_across_an_undecoded_gap`).
///   The same glue across a chunk whose decoded window heard nothing is
///   no such limit, and fails
///   (`a_recovery_that_missed_a_words_start_can_glue_its_rest_across_a_chunk_that_heard_nothing`).
/// - Limit E across its own window: the same, across the owned start of
///   the recovered chunk that kept the rest, which that recovery's window
///   holds ([`Losses::across_own_window`];
///   `a_recovery_that_missed_a_words_start_can_glue_its_rest_across_a_boundary_its_window_decodes`).
/// - Limit (ii) inside the old merge's glue: an orphan word the old merge
///   dropped comes out inside the old merge's own glue after the same
///   word, across a chunk ([`Losses::inside_glue`];
///   `an_orphan_the_old_merge_dropped_can_come_out_inside_its_glue_across_a_chunk`).
/// - Limit (ii) across its own window: an orphan word the old merge
///   dropped, glued onto the word before it across the owned start of the
///   next chunk, which that chunk's recovered window holds
///   ([`Losses::orphan_across_own_window`];
///   `an_orphan_the_old_merge_dropped_can_come_out_across_a_boundary_its_window_decodes`).
/// - Limit E inside the old merge's glue: a recovery's rest inside the old
///   merge's own glue after the same word, across a gap's window's start
///   ([`Losses::rest_inside_glue`];
///   `a_recovery_that_missed_a_words_start_can_keep_its_rest_inside_the_old_merges_glue`).
/// - Limit E inside the old merge's leading glue: the same inside its glue
///   after its text-leading orphan ([`Losses::rest_inside_lead_glue`];
///   `a_recovery_that_missed_a_words_start_can_keep_its_rest_inside_the_old_merges_leading_glue`).
/// - A copy before the text-leading orphan: a gap's window's copy of a
///   piece the old merge kept, glued before the orphan a recovery begins
///   with ([`Losses::copy_before_lead`];
///   `a_copy_of_a_piece_the_old_merge_kept_can_come_out_before_its_text_leading_orphan`).
///
/// No counted limit holds a word that grew across a piece the old merge
/// kept of another word ([`RefTexts::kept_between`]): such a word moves
/// its boundary across text the old merge kept, which fails at any scale.
///
/// | What passes | Behind it | Pinned by |
/// |---|---|---|
/// | Of the old merge's word, only its pieces up to its first piece of another placed word are asked for | What the old merge glued after a word comes out as pieces of their own word, or glued as below | the CI seed |
/// | An extra piece of a placed word the old merge's text holds in a word that begins with the same placed word | The old merge's own glue, which `own` can extend by a piece the trim before it cut, and a word's twin piece | `the_boundary_check_fails_each_planted_glue_and_passes_each_limit`; cases 353 and 1556 of seed 0x3ec9e6e5fd29f060 and seed 0x642579b97f4a7c18 |
/// | An extra piece with no chunk's owned span starting or ending between it and the word's first piece | Limit E (a recovery that missed a word's start, or began at an orphan, can glue the word's rest onto the word before it) and limit (ii) (the partition keeps an orphan word the old merge dropped, glued to the word before it) | `a_recovery_that_missed_a_words_start_can_glue_its_rest_onto_the_word_before_it`, `a_recovery_that_begins_at_an_orphan_can_glue_it_onto_the_word_before_it`, and the (ii) plant of the check's own test |
/// | An extra piece of a word that follows the word's first along a chain of repeats (each repeating the word before it, or an orphan after one that does) | The fake's repeat artefact: a window that heard the first word swallows the repeats after it, and the rest after them joins the first; the old merge's text shows the same shape at about the same rate (635 in 12,500 of the gate's cases, 3,560 in 12,500 orphan cases) | case 284 of the CI seed |
/// | A word of the old merge's whose last pieces come out only as the orphan an accepted recovery begins with, glued onto another word ([`Losses::led`]) | Limit E at its exact signature: a recovery that began at an orphan glues it onto the word before it, and that orphan was the only copy of the word's end | `a_recovery_that_begins_at_an_orphan_can_glue_the_end_of_a_word_onto_the_word_before_it` (case 855 of seed 0x3ec9e6e5fd29f060) |
/// | The old merge's text-leading orphan is asked for only alone, after a word the old merge dropped, or after a copy of a word that comes out elsewhere too | `own` keeps a word from its start where the trim before it began at the orphan | `a_recovery_that_begins_at_an_orphan_can_glue_it_onto_the_word_before_it`, and plant 1 of the check's own test |
/// | A word that repeats the word before it is not asked for | The fake's repeat artefact, as in the loss check | the CI seed |
/// | Pieces compare by placed word and id, not by index | The fake's equal-piece artefact: of a word's two equal pieces it gives one, from either copy | case 1486 of seed 0x163f372fe94f82f |
fn ref_losses(case: &RefCase, run: &RefRun, slack: usize) -> Losses {
    if run.old == run.new.tokens {
        return Losses::default();
    }
    let vocab = vocab();
    let old = ref_tally(&run.old, &case.words, slack);
    let new = ref_tally(&run.new.tokens, &case.words, slack);
    let heard = ref_tally(&run.heard, &case.words, slack);
    let reached = ref_tally(&run.reached, &case.words, slack);
    let repeats = |w: usize| ref_repeats_before(&case.words, w);
    let mut out = Losses {
        doubled: old.iter().zip(&new).filter(|(m, a)| a.0 > m.0).count(),
        ..Losses::default()
    };
    for w in 0..case.words.len() {
        let repeats = repeats(w);
        if repeats && heard[w].0 == 0 {
            continue;
        }
        if old[w].0 > 0 && new[w].0 == 0 {
            out.lost.push(w);
        } else if old[w].1 & !new[w].1 != 0 {
            out.pieces.push(w);
        }
        if reached[w].0 > 0 && new[w].0 == 0 && !repeats && !ref_orphan(&case.words, &vocab, w) {
            out.unheard.push(w);
        }
    }
    let texts = RefTexts::new(case, run, &vocab, slack);
    let words = texts.words();
    for &(w, of) in &words {
        if repeats(w) || texts.held(of, Extras::Check) {
            continue;
        }
        let rest = of
            .iter()
            .rev()
            .take_while(|p| p.0.is_some_and(|q| texts.leads.contains(&q)))
            .count();
        if rest > 0 && rest < of.len() && texts.held(&of[..of.len() - rest], Extras::Check) {
            out.led += 1;
        } else if let Some(class) = Extras::COUNTED.into_iter().find(|&c| texts.held(of, c)) {
            out.count(class, false);
        } else {
            out.unplaced += usize::from(texts.held(of, Extras::Unplaced));
            out.glued.push(w);
        }
    }
    if let Some(w) = texts.lead_glued(&words, Extras::Check) {
        let held = |c: Extras| texts.lead_glued(&words, c).is_none();
        if let Some(class) = Extras::COUNTED.into_iter().find(|&c| held(c)) {
            out.count(class, true);
        } else {
            out.unplaced += usize::from(held(Extras::Unplaced));
            out.glued.push(w);
        }
    }
    out.glues = texts
        .after
        .iter()
        .filter(|word| {
            let same = |of: &Vec<RefPiece>| {
                of.iter()
                    .map(|p| texts.piece(p))
                    .eq(word.iter().map(|p| texts.piece(p)))
            };
            !texts.before.iter().any(same)
                && words
                    .iter()
                    .any(|&(_, of)| of.len() < word.len() && texts.holds(word, of, Extras::Check))
        })
        .count();
    out
}

/// The placed word that starts last before `w`.
fn ref_previous(words: &[Placed], w: usize) -> Option<usize> {
    let start = |w: usize| words[w].start(0);
    (0..words.len())
        .filter(|&o| start(o) < start(w))
        .max_by_key(|&o| start(o))
}

/// Whether placed word `w` begins with a piece that continues a word.
fn ref_orphan(words: &[Placed], vocab: &Vocab, w: usize) -> bool {
    !starts_word(vocab.piece(words[w].pieces[0]))
}

/// Whether placed word `w` repeats the word before it, or its first piece
/// that word's last.
fn ref_repeats_before(words: &[Placed], w: usize) -> bool {
    let word = &words[w];
    ref_previous(words, w).is_some_and(|o| {
        let pieces = &words[o].pieces;
        *pieces == word.pieces || pieces.last() == word.pieces.first()
    })
}

/// Whether placed word `w` follows `head` along a chain of repeats: each
/// word from `w` back to the one after `head` repeats the word before it,
/// or is an orphan after a word that does, and `w` repeats `head` if it
/// follows it.
fn ref_chain(words: &[Placed], vocab: &Vocab, w: usize, head: usize) -> bool {
    let mut at = w;
    while let Some(before) = ref_previous(words, at) {
        if !(ref_repeats_before(words, at)
            || ref_orphan(words, vocab, at) && ref_repeats_before(words, before))
        {
            return false;
        }
        if before == head {
            return at != w || ref_repeats_before(words, w);
        }
        at = before;
    }
    false
}

/// Which extra pieces [`RefTexts::holds`] allows: the boundaries' own
/// ([`ref_losses`]), or, to count a word they flag, also those of one
/// counted class, as its count defines them: [`Losses::across_gap`],
/// [`Losses::across_own_window`], [`Losses::orphan_across_own_window`],
/// [`Losses::inside_glue`], [`Losses::rest_inside_glue`] (and
/// [`Losses::rest_inside_lead_glue`]), [`Losses::copy_before_lead`] or
/// [`Losses::unplaced`].
#[derive(Clone, Copy)]
enum Extras {
    Check,
    AcrossGap,
    AcrossOwnWindow,
    OrphanAcrossOwnWindow,
    InsideGlue,
    RestInsideGlue,
    CopyBeforeLead,
    Unplaced,
}

impl Extras {
    /// The counted classes, in the order a flagged word tries them.
    const COUNTED: [Self; 6] = [
        Self::AcrossGap,
        Self::AcrossOwnWindow,
        Self::InsideGlue,
        Self::OrphanAcrossOwnWindow,
        Self::RestInsideGlue,
        Self::CopyBeforeLead,
    ];
}

/// The two texts the boundary check compares ([`ref_losses`]), as placed
/// pieces grouped into words ([`ref_joins`]).
struct RefTexts<'a> {
    case: &'a RefCase,
    vocab: &'a Vocab,
    /// The transcript's spans.
    spans: &'a [Span],
    /// The windows the transcript's words came from.
    windows: &'a [Range<usize>],
    /// The old merge's text.
    before: Vec<Vec<RefPiece>>,
    /// The transcript's text.
    after: Vec<Vec<RefPiece>>,
    /// The places the old merge kept.
    kept: HashSet<(usize, usize)>,
    /// The placed words the old merge kept a piece of.
    kept_words: HashSet<usize>,
    /// Per placed word, where the last piece of another placed word starts
    /// that the old merge's text holds in a word that begins with it.
    glue_end: HashMap<usize, usize>,
    /// Pairs of placed words `(first, other)` where the old merge's text
    /// holds a piece of `other` in a word that begins with `first`.
    glued: HashSet<(usize, usize)>,
    /// The places of the orphans accepted recoveries begin with.
    leads: HashSet<(usize, usize)>,
}

impl<'a> RefTexts<'a> {
    fn new(case: &'a RefCase, run: &'a RefRun, vocab: &'a Vocab, slack: usize) -> Self {
        let before = ref_joins(&run.old, &case.words, vocab, slack);
        let after = ref_joins(&run.new.tokens, &case.words, vocab, slack);
        let kept: HashSet<(usize, usize)> = before.iter().flatten().filter_map(|p| p.0).collect();
        let kept_words = kept.iter().map(|&(w, _)| w).collect();
        let mut glue_end = HashMap::new();
        for word in &before {
            let Some((head, _)) = word[0].0 else {
                continue;
            };
            for &(place, at) in word {
                if place.is_some_and(|(w, _)| w != head) {
                    let end = glue_end.entry(head).or_insert(at);
                    *end = at.max(*end);
                }
            }
        }
        let glued = before
            .iter()
            .filter_map(|word| Some((word[0].0?.0, word)))
            .flat_map(|(head, word)| word.iter().filter_map(move |p| Some((head, p.0?.0))))
            .collect();
        let leads = run
            .leads
            .iter()
            .filter_map(|t| ref_place(t, &case.words, slack))
            .collect();
        Self {
            case,
            vocab,
            spans: &run.new.spans,
            windows: &run.new.windows,
            before,
            after,
            kept,
            kept_words,
            glue_end,
            glued,
            leads,
        }
    }

    /// Whether the old merge's text holds a piece of another placed word
    /// strictly between the piece at `at` and `first`, the first piece of a
    /// word, outside the old merge's words that begin with the same placed
    /// word as `first`: a word that grew across it would move its boundary
    /// across text the old merge kept.
    fn kept_between(&self, at: usize, &(first, head): &RefPiece) -> bool {
        let same = |p: &RefPiece| p.0.map(|(w, _)| w) == first.map(|(w, _)| w);
        let between = |p: &RefPiece| at.min(head) < p.1 && p.1 < at.max(head);
        self.before
            .iter()
            .filter(|word| !same(&word[0]))
            .flatten()
            .any(|p| p.0.is_some() && !same(p) && between(p))
    }

    /// A piece as its placed word and id: the fake never gives one piece
    /// twice in a row, so of a word's two equal pieces either can come out.
    fn piece(&self, p: &RefPiece) -> Option<(usize, u32)> {
        p.0.map(|(w, i)| (w, self.case.words[w].pieces[i]))
    }

    /// Whether a word that begins with `first` may hold this piece beyond
    /// a word of the old merge's, under `extras`.
    fn extra(&self, &(place, at): &RefPiece, &(first, head): &RefPiece, extras: Extras) -> bool {
        let between = at.min(head)..at.max(head);
        // Whether a chunk's owned start or end that `counts` lies between
        // the two pieces.
        let crosses = |counts: &dyn Fn(usize) -> bool| {
            let cuts = |b: usize| between.start < b && b <= between.end && counts(b);
            let chunks = self.spans.iter().filter(|s| s.chunk);
            chunks.flat_map(|s| [s.owned.start, s.owned.end]).any(cuts)
        };
        let decoded = |b: usize| self.windows.iter().any(|w| w.contains(&b));
        // No decoded window wholly between.
        let clear = !self
            .windows
            .iter()
            .any(|w| between.start <= w.start && w.end <= between.end);
        // A later piece of a placed word, with no decoded window wholly
        // between.
        let later = place.is_some_and(|(_, i)| i > 0) && clear;
        // Across only owned boundaries that lie in no decoded window.
        let undecoded = || !crosses(&|b| decoded(b));
        // Across only owned boundaries a gap's span shares, each in a
        // decoded window.
        let gap_seams = || {
            let gap = |b: usize| {
                let gaps = self.spans.iter().filter(|s| !s.chunk);
                gaps.flat_map(|s| [s.owned.start, s.owned.end])
                    .any(|e| e == b)
            };
            !crosses(&|b| !(gap(b) && decoded(b)))
        };
        // A piece of a placed word the old merge kept nothing of.
        let dropped = place.is_some_and(|(w, _)| !self.kept_words.contains(&w));
        let kept_between = || self.kept_between(at, &(first, head));
        // A piece the old merge did not keep, in an accepted recovery's
        // owned span, across that span's owned start alone, which the
        // recovery's window holds.
        let own_start = || {
            let recovered = self
                .spans
                .iter()
                .zip(self.windows)
                .find(|(s, w)| s.chunk && **w != s.window && s.owned.contains(&at));
            place.is_some_and(|p| !self.kept.contains(&p))
                && recovered.is_some_and(|(s, w)| {
                    let start = s.owned.start;
                    w.contains(&start) && !crosses(&|b| b != start)
                })
        };
        let follows = |(w, _): (usize, usize), (h, _): (usize, usize)| {
            self.glued.contains(&(h, w)) || ref_chain(&self.case.words, self.vocab, w, h)
        };
        place.zip(first).is_some_and(|(p, f)| follows(p, f))
            || place.is_some() && !crosses(&|_| true)
            || match extras {
                // The copies before the text-leading orphan are no extra
                // pieces of a word: `lead_glued` allows them.
                Extras::Check | Extras::CopyBeforeLead => false,
                Extras::AcrossGap => later && undecoded() && !kept_between(),
                Extras::AcrossOwnWindow => later && own_start() && !kept_between(),
                Extras::OrphanAcrossOwnWindow => {
                    place.is_some_and(|(w, _)| ref_orphan(&self.case.words, self.vocab, w))
                        && dropped
                        && clear
                        && own_start()
                        && !kept_between()
                }
                Extras::InsideGlue => {
                    place.zip(first).is_some_and(|((w, _), (h, _))| {
                        ref_orphan(&self.case.words, self.vocab, w)
                            && !self.kept_words.contains(&w)
                            && head < at
                            && self.glue_end.get(&h).is_some_and(|&end| at < end)
                    }) && !kept_between()
                }
                Extras::RestInsideGlue => {
                    place.zip(first).is_some_and(|(p, (h, _))| {
                        p.1 > 0
                            && self.leads.contains(&p)
                            && head < at
                            && self.glue_end.get(&h).is_some_and(|&end| at < end)
                    }) && dropped
                        && gap_seams()
                        && !kept_between()
                }
                Extras::Unplaced => place.is_none(),
            }
    }

    /// Whether `word` holds `of` from its first piece on, each piece of
    /// `of` in order, every other piece of `word` an extra one under
    /// `extras`.
    fn holds(&self, word: &[RefPiece], of: &[RefPiece], extras: Extras) -> bool {
        let mut wanted = of.iter().map(|p| self.piece(p)).peekable();
        self.piece(&word[0]) == self.piece(&of[0])
            && word.iter().all(|p| {
                wanted.next_if_eq(&self.piece(p)).is_some() || self.extra(p, &word[0], extras)
            })
            && wanted.peek().is_none()
    }

    /// Whether a word of the transcript's text holds `of`.
    fn held(&self, of: &[RefPiece], extras: Extras) -> bool {
        self.after.iter().any(|word| self.holds(word, of, extras))
    }

    /// Each of the old merge's words that begins at a word start, up to its
    /// first piece of another placed word.
    fn words(&self) -> Vec<(usize, &[RefPiece])> {
        self.before
            .iter()
            .filter_map(|word| {
                let (w, i) = word[0].0?;
                if !starts_word(self.vocab.piece(self.case.words[w].pieces[i])) {
                    return None;
                }
                let own = 1 + word[1..]
                    .iter()
                    .take_while(|p| p.0.is_some_and(|(o, _)| o == w))
                    .count();
                Some((w, &word[..own]))
            })
            .collect()
    }

    /// The old merge's text-leading orphan, up to its first piece of
    /// another placed word or its first repeated place, if the transcript's
    /// text holds it nowhere alone, after pieces the old merge did not
    /// keep, or after a word whose every word of the old merge's (`words`)
    /// comes out in another word too.
    fn lead_glued(&self, words: &[(usize, &[RefPiece])], extras: Extras) -> Option<usize> {
        let lead = self.before.first()?;
        let (w, i) = lead[0].0?;
        if starts_word(self.vocab.piece(self.case.words[w].pieces[i])) {
            return None;
        }
        let mut seen = HashSet::new();
        let of = lead
            .iter()
            .take_while(|p| p.0.is_some_and(|(o, _)| o == w) && seen.insert(p.0));
        let of = &lead[..of.count()];
        let copy = |x: usize| {
            self.after[x][0].0.is_some_and(|(h, _)| {
                let mut mine = words.iter().filter(|&&(o, _)| o == h).peekable();
                mine.peek().is_some()
                    && mine.all(|&(_, of)| {
                        let elsewhere = |(y, word): (usize, &Vec<RefPiece>)| {
                            y != x && self.holds(word, of, extras)
                        };
                        self.after.iter().enumerate().any(elsewhere)
                    })
            })
        };
        let unkept = |word: &[RefPiece]| {
            word.iter()
                .all(|p| p.0.is_none_or(|q| !self.kept.contains(&q)))
        };
        // Copies of pieces of the orphan's own word in the old merge's
        // text, where an accepted recovery begins with the orphan
        // ([`Losses::copy_before_lead`]).
        let in_lead = |p: &RefPiece| {
            self.piece(p).is_some()
                && lead.iter().any(|q| self.piece(q) == self.piece(p))
                && !self.kept_between(p.1, &lead[0])
        };
        let copies = |word: &[RefPiece]| {
            matches!(extras, Extras::CopyBeforeLead)
                && self.leads.contains(&(w, i))
                && word.iter().all(in_lead)
        };
        let alone = self.after.iter().enumerate().any(|(x, word)| {
            (0..word.len()).any(|k| {
                self.holds(&word[k..], of, extras)
                    && (k == 0 || unkept(&word[..k]) || copy(x) || copies(&word[..k]))
            })
        });
        (!alone).then_some(w)
    }
}

/// What one seed's differential run counted.
#[derive(Debug, Default)]
struct Tally {
    /// Words checked.
    words: usize,
    /// Cases with an accepted recovery.
    recovered: usize,
    /// Words that came out more often than from the old merge.
    doubled: usize,
    /// Longer words the boundaries allow ([`Losses::glues`]).
    glues: usize,
    /// Words whose end comes out only as a recovery's orphan
    /// ([`Losses::led`]).
    led: usize,
    /// Words glued on across an undecoded gap ([`Losses::across_gap`]).
    across_gap: usize,
    /// Words glued on across a recovery's own window
    /// ([`Losses::across_own_window`]).
    across_own_window: usize,
    /// Orphan words glued on across a recovery's own window
    /// ([`Losses::orphan_across_own_window`]).
    orphan_across_own_window: usize,
    /// Orphan words kept inside the old merge's glue
    /// ([`Losses::inside_glue`]).
    inside_glue: usize,
    /// Recoveries' rests kept inside the old merge's glue
    /// ([`Losses::rest_inside_glue`]).
    rest_inside_glue: usize,
    /// Recoveries' rests kept inside the old merge's leading glue
    /// ([`Losses::rest_inside_lead_glue`]).
    rest_inside_lead_glue: usize,
    /// Copies before the old merge's text-leading orphan
    /// ([`Losses::copy_before_lead`]).
    copy_before_lead: usize,
    /// Flags an unplaced piece makes ([`Losses::unplaced`]).
    unplaced: usize,
}

/// `cases` random layouts from `seed`, with recovery on: the first
/// generator's words of one to three pieces, the old merge's random layout,
/// its two-sided layout around a gap with recoveries reaching into it, the
/// collision layout and the general one, on a third of its cases with orphans
/// at every seam ([`ref_seam_layout`]), under the default settings,
/// `CoreML`'s, two between and the ONNX engine's, with the extra windows off
/// and on, over the exact fake, one that moves tokens by `jitter` frames,
/// and, on half the cases, one deaf at a window's edges (`DEAF`), as a model
/// with little context there can be, so that two windows of a layout disagree
/// about a word at every kind of seam. Every word the old merge kept comes
/// out, with every piece the old merge gave it and its boundaries
/// ([`ref_losses`]): a duplicate is allowed, a loss never, and a longer word
/// only with extra pieces the boundaries allow (counted in [`Tally::glues`],
/// beside the words limit E cuts, [`Tally::led`]). And, where the fake hears
/// every frame, every word a gap's window or an accepted recovery heard in
/// its reach comes out at least once (see `ref_losses` for that check and for
/// a word that repeats the one before it).
fn differential(seed: u64, cases: usize, jitter: usize) -> Tally {
    let mut rng = Rng(seed);
    let mut tally = Tally::default();
    for case in 0..cases {
        let (kind, round) = (case % 5, case / 5);
        // The ONNX engine's settings on every collision case of an even
        // round and wherever the defaults would run without extra windows
        // (every 16th round).
        let config = if (kind == 3 && round.is_multiple_of(2)) || round.is_multiple_of(16) {
            crate::engine::pipeline_config()
        } else {
            random_config(round, (round / 4) % 4 != 0)
        };
        let layout = match kind {
            0 => {
                let (samples, words) = placed_words(&mut rng, case);
                RefCase {
                    samples,
                    words,
                    regions: None,
                }
            }
            1 => ref_random_layout(&mut rng, round),
            2 => ref_two_sided_layout(&mut rng),
            3 => ref_collision_layout(&mut rng),
            _ if round % 3 == 2 => ref_seam_layout(&mut rng, &config),
            _ => ref_general_layout(&mut rng),
        };
        let jitter = if round % 3 == 1 { jitter } else { 0 };
        let deaf = if round.is_multiple_of(2) {
            (0, 0, 1)
        } else {
            DEAF[(round / 2) % DEAF.len()]
        };
        let min_frames = match kind {
            0 if round.is_multiple_of(2) => 0,
            4 if round % 7 == 3 => 0,
            _ => 130,
        };
        let slack = 2 + jitter;
        let run = ref_run(&layout, &config, min_frames, Hearing { jitter, deaf });
        tally.words += layout.words.len();
        tally.recovered += usize::from(run.new.stats.recoveries_accepted > 0);
        let mut losses = ref_losses(&layout, &run, slack);
        tally.doubled += losses.doubled;
        tally.glues += losses.glues;
        tally.led += losses.led;
        tally.across_gap += losses.across_gap;
        tally.across_own_window += losses.across_own_window;
        tally.orphan_across_own_window += losses.orphan_across_own_window;
        tally.inside_glue += losses.inside_glue;
        tally.rest_inside_glue += losses.rest_inside_glue;
        tally.rest_inside_lead_glue += losses.rest_inside_lead_glue;
        tally.copy_before_lead += losses.copy_before_lead;
        tally.unplaced += losses.unplaced;
        // Over the deaf fake one window of a run can hear a word its
        // neighbour is deaf to, which the run's LCS drops in both merges
        // alike, so the heard check runs only where the fake hears every
        // frame; the comparison with the old merge is the gate on every
        // case.
        if deaf != (0, 0, 1) {
            losses.unheard.clear();
        }
        let show = |tokens: &[Token]| {
            let pieces = tokens.iter().map(|t| format!("{}@{}", t.id, t.sample()));
            pieces.collect::<Vec<_>>().join(" ")
        };
        let words = |list: &[usize]| {
            list.iter()
                .map(|&w| (&layout.words[w].pieces, layout.words[w].start(0)))
                .collect::<Vec<_>>()
        };
        let Losses {
            lost,
            pieces,
            glued,
            unheard,
            ..
        } = &losses;
        assert!(
            losses.passes() && unheard.is_empty(),
            "seed {seed:#x} case {case} jitter {jitter} deaf {deaf:?}: lost {:?}, pieces lost {:?}, glued {:?}, heard and lost {:?}\n spans {:?}\n windows {:?}\n old {}\n new {}\n heard {}",
            words(lost),
            words(pieces),
            words(glued),
            words(unheard),
            run.new.spans,
            run.new.windows,
            show(&run.old),
            show(&run.new.tokens),
            show(&run.heard)
        );
    }
    tally
}

/// Clause 5 of the ownership invariant over 1,500 cases of one seed;
/// `every_word_the_old_merge_keeps_comes_out_over_more_seeds` runs more.
#[test]
fn every_word_the_old_merge_keeps_comes_out() {
    let tally = differential(0x0a16_a310_c1e5_7001, 1500, 1);
    println!("{tally:?}");
    // Words checked (about 24,000 with this seed), cases with an accepted
    // recovery (about 800), and words that came out more often than from
    // the old merge (about 1 in 150); `glues` is about 25.
    // The seven counted classes, each capped at its count here.
    assert!(
        tally.words > 20_000
            && tally.recovered > 600
            && tally.doubled * 50 < tally.words
            && tally.across_gap == 0
            && tally.across_own_window == 0
            && tally.orphan_across_own_window == 0
            && tally.inside_glue == 0
            && tally.rest_inside_glue == 0
            && tally.rest_inside_lead_glue == 0
            && tally.copy_before_lead == 0,
        "{tally:?}"
    );
}

/// Eight more seeds, 2,500 cases each, with the jitter at one, two and
/// three frames: 60,000 cases, half of them over a deaf fake.
#[test]
#[ignore = "about 10 minutes in a release build; run it after a change to the partition or the merge"]
fn every_word_the_old_merge_keeps_comes_out_over_more_seeds() {
    for jitter in 1..=3 {
        for seed in 1..=8u64 {
            let tally = differential(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15), 2500, jitter);
            println!("jitter {jitter} seed {seed}: {tally:?}");
        }
    }
}

/// The transcript and the old merge's text for a hand-built case: `samples` with
/// speech in `regions`, under `config`, over a fake that decodes a window
/// under 130 frames that holds loud sound to nothing.
fn texts(
    samples: &[f32],
    regions: &[Range<usize>],
    config: &PipelineConfig,
    jitter: usize,
) -> (Transcript, String) {
    texts_over(samples, regions, config, 130, Hearing::every_frame(jitter))
}

/// `texts` over a fake that decodes a window under `min_frames` frames that
/// holds loud sound to nothing and hears as `hearing` says.
fn texts_over(
    samples: &[f32],
    regions: &[Range<usize>],
    config: &PipelineConfig,
    min_frames: usize,
    hearing: Hearing,
) -> (Transcript, String) {
    let make = || hearing.transcriber(config, min_frames, Some(regions));
    let new = make().transcribe(samples, None).unwrap();
    let mut t = make();
    let old = ref_transcribe(&mut t, samples);
    (new, t.render(&old))
}

/// Faint noise over `seconds`, loud sound in `regions`, and `words` as
/// `(piece, frame, frames)`.
fn crafted(seconds: f32, regions: &[Range<usize>], words: &[(u32, usize, usize)]) -> Vec<f32> {
    let mut samples: Vec<f32> = (0..sample_count(seconds))
        .map(|i| if i % 2 == 0 { 3e-5 } else { -3e-5 })
        .collect();
    for region in regions {
        samples[region.clone()].fill(0.05);
    }
    for &(piece, frame, frames) in words {
        let word = Placed {
            pieces: vec![piece],
            frame,
            frames,
        };
        word.put(&mut samples);
    }
    samples
}

/// `texts` for R2 and R3: speech at 1.4 to 13.7 s and 16.5 to 28.7 s
/// under the `CoreML` settings, quiet word 18 at 20.96 s, and word 20 at
/// 24.96 s, with the short pauses where the shrunk case had words when
/// `pauses` is set.
fn two_chunks_and_a_quiet_word(pauses: bool) -> (Transcript, String) {
    let regions = vec![22_400..219_200, 264_000..459_200];
    let mut samples = crafted(28.7, &regions, &[(18, 262, 5), (20, 312, 5)]);
    if pauses {
        for at in [1.9, 3.9, 5.9, 7.9, 9.9, 17.0, 19.0, 23.0] {
            let frame = sample_count(at) / FRAME_SAMPLES;
            samples[frame * FRAME_SAMPLES..(frame + 5) * FRAME_SAMPLES].fill(3e-5);
        }
    }
    texts(&samples, &regions, &random_config(1, true), 0)
}

#[test]
fn a_recovery_keeps_a_word_it_alone_heard_in_a_decoded_neighbours_share() {
    // R2: the second chunk (12.9 to 23.1 s) decodes to nothing and cannot
    // be decoded again, as every extension passes 15 s; the third chunk's
    // recovery hears word 18 inside the second chunk's owned span, and
    // keeps it, as it lies in its reach.
    let (new, old) = two_chunks_and_a_quiet_word(true);
    assert_eq!(new.stats.recoveries_accepted, 1);
    assert_ne!(new.windows[2], new.spans[2].window);
    assert!(!new.spans[2].owned.contains(&(262 * FRAME_SAMPLES)));
    assert_eq!((new.text().as_str(), old.as_str()), ("w18 w20", "w18 w20"));
}

#[test]
fn a_recovery_is_scored_on_its_chunks_window() {
    // R3: without the pauses the second chunk's recovery hears word 18
    // in the overlap with the third chunk, inside its window and past its
    // owned span. Scored on the window, as before the partition, it is
    // accepted, and keeps the word.
    let (new, old) = two_chunks_and_a_quiet_word(false);
    assert_eq!(new.stats.recoveries_accepted, 1);
    let span = &new.spans[1];
    let word = 262 * FRAME_SAMPLES;
    assert!(span.window.contains(&word) && !span.owned.contains(&word));
    assert_ne!(new.windows[1], span.window);
    assert_eq!((new.text().as_str(), old.as_str()), ("w18", "w18"));
}

#[test]
fn a_recovery_counts_a_word_a_frame_early_at_its_windows_edge() {
    // R3 over the jittering fake, 8 s chunks: the last chunk's recovery
    // hears word 12 a frame early, before its owned span; the frames of
    // its window still count it, and the recovery keeps it.
    let regions = vec![22_400..225_599, 318_400..416_000];
    let samples = crafted(26.0, &regions, &[(15, 98, 5), (12, 255, 5)]);
    let (new, old) = texts(&samples, &regions, &random_config(3, false), 1);
    assert!(new.stats.recoveries_accepted > 0);
    assert_eq!((new.text().as_str(), old.as_str()), ("w15 w12", "w15 w12"));
}

#[test]
fn a_recovered_chunk_keeps_the_rest_of_a_word_a_gaps_window_began() {
    // R4: the lead-in's window ends inside quiet word "w8s28" and hears
    // "w8"; the first chunk's recovery begins with "s28", the rest, and
    // keeps it, so the word comes out whole.
    let regions = vec![sample_count(4.0)..sample_count(11.0)];
    let mut samples = crafted(20.0, &regions, &[(12, 87, 5)]);
    let word = Placed {
        pieces: vec![8, 28],
        frame: 40,
        frames: 7,
    };
    word.put(&mut samples);
    let (new, old) = texts(&samples, &regions, &random_config(1, true), 0);
    assert_eq!(new.stats.recoveries_accepted, 1);
    assert!(!new.spans[0].chunk && new.spans[0].window.end < word.start(1));
    assert_eq!(
        (new.text().as_str(), old.as_str()),
        ("w8s28 w12", "w8s28 w12")
    );
}

#[test]
fn a_recovery_keeps_a_word_in_its_plain_neighbours_half_of_an_undecoded_gap() {
    // B1, the default settings: a 2.7 s pause no window decodes between a
    // plain chunk and one that is recovered, and quiet word 3 in the
    // plain chunk's half of it, which only the recovery hears. Forward,
    // the recovered chunk comes first; backward, second.
    let s = sample_count;
    for backward in [false, true] {
        let mut samples = vec![0.0f32; 25 * SAMPLE_RATE];
        let (regions, loud, words, quiet) = if backward {
            (
                vec![s(2.0)..s(14.0), s(17.2)..s(23.0)],
                s(16.0)..s(23.0),
                [3.0, 5.0, 7.0, 9.0, 11.0, 13.0],
                (17, 19.0, 3, 15.2),
            )
        } else {
            (
                vec![s(2.0)..s(8.0), s(11.2)..s(24.0)],
                s(2.0)..s(8.0),
                [12.0, 14.0, 16.0, 18.0, 20.0, 22.0],
                (17, 4.0, 3, 10.0),
            )
        };
        samples[loud].fill(0.05);
        for (i, at) in words.into_iter().enumerate() {
            put(&mut samples, 11 + i as u32, at, 6);
        }
        put(&mut samples, quiet.0, quiet.1, 6);
        put(&mut samples, quiet.2, quiet.3, 6);
        let (new, old) = texts(&samples, &regions, &PipelineConfig::default(), 0);
        assert_eq!(new.stats.recoveries_accepted, 1, "{backward}");
        let expected = if backward {
            "w11 w12 w13 w14 w15 w16 w3 w17"
        } else {
            "w17 w3 w11 w12 w13 w14 w15 w16"
        };
        assert_eq!(
            (new.text().as_str(), old.as_str()),
            (expected, expected),
            "{backward}"
        );
    }
}

#[test]
fn a_recovery_across_an_undecoded_pause_keeps_its_plain_neighbours_words() {
    // B1 of the capped reach, the default settings: a plain chunk dense
    // with words to its edge, a 3 s pause no window decodes, and a chunk
    // the fake decodes to nothing, which a recovery hears. A quiet word in
    // the pause repeats the id of one of the plain chunk's last (backward)
    // or first (forward) words. The recovery keeps the words of its own
    // share of the pause and no further, so the run's LCS cannot take the
    // quiet word for the plain chunk's and drop the plain words between:
    // the text is the old merge's, over the exact fake and with the
    // tokens moved one and two frames.
    let s = sample_count;
    let at = |frame: usize| frame as f32 * 0.08;
    for backward in [true, false] {
        let mut samples = vec![0.0f32; 25 * SAMPLE_RATE];
        let (regions, expected) = if backward {
            samples[s(16.0)..s(23.0)].fill(0.05);
            for (i, t) in [3.0, 5.0, 7.0, 9.0, 11.0].into_iter().enumerate() {
                put(&mut samples, 11 + i as u32, t, 6);
            }
            put(&mut samples, 4, at(167), 2);
            put(&mut samples, 16, at(170), 3);
            put(&mut samples, 4, at(179), 2);
            put(&mut samples, 17, 19.0, 6);
            (
                vec![s(2.0)..s(14.0), s(17.2)..s(23.0)],
                "w11 w12 w13 w14 w15 w4 w16 w17",
            )
        } else {
            samples[s(2.0)..s(8.0)].fill(0.05);
            put(&mut samples, 17, 4.0, 6);
            put(&mut samples, 3, at(132), 2);
            put(&mut samples, 11, at(137), 5);
            put(&mut samples, 3, at(143), 5);
            for (i, t) in [13.0, 15.0, 17.0, 19.0, 21.0].into_iter().enumerate() {
                put(&mut samples, 12 + i as u32, t, 6);
            }
            (
                vec![s(2.0)..s(8.0), s(11.2)..s(24.0)],
                "w17 w11 w3 w12 w13 w14 w15 w16",
            )
        };
        for jitter in 0..=2 {
            let (new, old) = texts(&samples, &regions, &PipelineConfig::default(), jitter);
            assert_eq!(new.stats.recoveries_accepted, 1, "{backward} {jitter}");
            assert_eq!(
                (new.text().as_str(), old.as_str()),
                (expected, expected),
                "backward {backward} jitter {jitter}"
            );
        }
    }
}

#[test]
fn a_word_across_the_seam_of_two_gap_windows_keeps_every_piece() {
    // A 30 s gap and no chunk, `CoreML`'s settings: windows of at most
    // 15 s that share 2 s. A four-piece word from five frames before the
    // seam of the first two: the earlier window ends inside it and hears three
    // pieces, the later one all four. Both keep the word, since a piece of
    // it lies in each one's reach, and the seam keeps the copy with more
    // pieces: the word comes out once and whole, as from the old merge.
    let config = random_config(1, true);
    let mut samples = crafted(30.0, &[], &[]);
    let spans = partition(&samples, &[], &config.chunker);
    assert!(
        spans.len() > 2 && spans.iter().all(|s| !s.chunk),
        "{spans:?}"
    );
    let seam = spans[1].owned.start;
    let word = Placed {
        pieces: vec![13, 34, 21, 41],
        frame: seam / FRAME_SAMPLES - 5,
        frames: 6,
    };
    assert!(word.start(3) >= spans[0].window.end && spans[0].window.end > word.start(2));
    word.put(&mut samples);
    for jitter in 0..=2 {
        let (new, old) = texts(&samples, &[], &config, jitter);
        assert_eq!(
            (new.text().as_str(), old.as_str()),
            ("w13s34s21.", "w13s34s21."),
            "{jitter}"
        );
    }
}

#[test]
fn a_word_one_window_of_a_gap_is_deaf_to_comes_out() {
    // A 30 s gap and no chunk, `CoreML`'s settings: windows of 15 s that
    // share 2 s, their owned spans meeting in the middle. Every window is
    // deaf for its first 30 frames, or its last, as a model with little
    // context at a window's edge can be. A quiet word 4 or 8 frames past
    // the seam of the first two (or before it, in the mirror) lies in the
    // later window's owned span, deaf there, and only the earlier window
    // hears it (or the other way round). A gap's windows merge as a run,
    // as before the partition, so it comes out: the old merge's text.
    let config = random_config(1, true);
    let base = crafted(30.0, &[], &[]);
    let spans = partition(&base, &[], &config.chunker);
    assert!(
        spans.len() > 2 && spans.iter().all(|s| !s.chunk),
        "{spans:?}"
    );
    let seam = spans[1].owned.start / FRAME_SAMPLES;
    for (deaf, sign) in [((30, 0, 1), 1), ((0, 30, 1), -1)] {
        for offset in [4isize, 8] {
            let mut samples = base.clone();
            for (piece, at, frames) in [(4, -60, 5), (3, offset * sign, 2), (5, 60, 5)] {
                let frame = seam.checked_add_signed(at).unwrap();
                Placed {
                    pieces: vec![piece],
                    frame,
                    frames,
                }
                .put(&mut samples);
            }
            let hearing = Hearing { jitter: 0, deaf };
            let (new, old) = texts_over(&samples, &[], &config, 130, hearing);
            assert_eq!(
                (new.text().as_str(), old.as_str()),
                ("w4 w3 w5", "w4 w3 w5"),
                "{deaf:?} {offset}"
            );
        }
    }
}

#[test]
fn a_full_stop_past_the_seam_of_two_gap_windows_comes_out_as_before() {
    // The gate's case 34 at its CI seed, shrunk: a gap and no chunk,
    // `CoreML`'s settings, windows of 15 s that share 2 s. A word of four
    // pieces, six frames each, from eight frames before the seam of the
    // first two, its full stop ten frames past it. Both windows hear it
    // whole ("w9 s29 ."); they merge as a run, so it comes out once and
    // whole, as from the old merge, over the exact fake and the jittering
    // one.
    let config = random_config(1, true);
    let mut samples = crafted(45.0, &[], &[]);
    let spans = partition(&samples, &[], &config.chunker);
    let seam = spans[1].owned.start;
    let word = Placed {
        pieces: vec![9, 29, 29, 41],
        frame: seam / FRAME_SAMPLES - 8,
        frames: 6,
    };
    assert!(word.start(0) > spans[1].window.start && word.start(3) < spans[0].window.end);
    word.put(&mut samples);
    for jitter in 0..=2 {
        let (new, old) = texts(&samples, &[], &config, jitter);
        assert_eq!(
            (new.text().as_str(), old.as_str()),
            ("w9s29.", "w9s29."),
            "{jitter}"
        );
    }
}

#[test]
fn a_recovered_word_whose_full_stop_lies_past_the_reach_comes_out_as_before() {
    // The shape of the loss the gate found at its fourth extra seed, case
    // 2019, the jitter at three frames, while a recovery's reach still
    // widened over its own share: a recovered chunk keeps a word whose
    // full stop lies past its reach, and the plain chunk after it heard
    // that word and a second one with a full stop 0.7 s later. The
    // recovery keeps the word without the full stop, where the trim before
    // the partition cut, and the text is the old merge's. Settings with 8 s
    // chunks, loud sound the fake decodes to nothing from 1 to 5.5 s with
    // word 13 in it, plain words after; the word's full stop zero to two
    // frames past the reach, over the exact fake and the jittering one.
    let config = random_config(3, false);
    let regions = vec![sample_count(1.0)..sample_count(20.0)];
    let mut base = crafted(22.0, &[sample_count(1.0)..sample_count(5.5)], &[]);
    for (piece, at) in [(13, 3.0), (14, 7.5), (16, 11.0), (17, 13.0), (19, 16.0)] {
        ref_placed(vec![piece], at, 4).put(&mut base);
    }
    let (plain, _) = texts(&base, &regions, &config, 0);
    assert_eq!(plain.stats.recoveries_accepted, 1);
    let reach = plain.spans[0].reach.end / FRAME_SAMPLES;
    for offset in 0..=2 {
        let mut samples = base.clone();
        let stop = reach + offset;
        Placed {
            pieces: vec![12, 41],
            frame: stop - 4,
            frames: 4,
        }
        .put(&mut samples);
        Placed {
            pieces: vec![18, 41],
            frame: stop + 5,
            frames: 4,
        }
        .put(&mut samples);
        for jitter in 0..=3 {
            let (new, old) = texts(&samples, &regions, &config, jitter);
            assert_eq!(new.stats.recoveries_accepted, 1, "{offset} {jitter}");
            assert_eq!(
                (new.text().as_str(), old.as_str()),
                (
                    "w13 w14 w12. w18. w16 w17 w19",
                    "w13 w14 w12. w18. w16 w17 w19"
                ),
                "offset {offset} jitter {jitter}"
            );
        }
    }
}

#[test]
fn a_recovery_that_begins_inside_a_skipped_pause_keeps_the_old_merges_text() {
    // The default settings: a plain chunk with words 11 and 12, a pause of
    // about 19 s no window decodes, and a chunk the fake decodes to
    // nothing whose recovery 6 s either side wins (windows under 200
    // frames decode to silence). That window begins inside the pause, on
    // a continuation piece, a full stop or a word start. The recovery
    // keeps its chunk ± the overlap, the range the old merge kept, so
    // nothing from the pause glues onto word 12: the old merge's text.
    let s = sample_count;
    for piece in [25, 41, 3] {
        let mut samples = vec![0.0f32; 45 * SAMPLE_RATE];
        put(&mut samples, 11, 3.0, 6);
        put(&mut samples, 12, 5.0, 6);
        samples[s(26.0)..s(33.0)].fill(0.05);
        put(&mut samples, 17, 29.0, 6);
        samples[s(19.5)..s(20.0)].fill(piece as f32 / 1000.0);
        let regions = vec![s(2.0)..s(6.0), s(26.0)..s(33.0)];
        let config = PipelineConfig::default();
        let (new, old) = texts_over(&samples, &regions, &config, 200, Hearing::every_frame(0));
        assert_eq!(new.stats.recoveries_accepted, 1, "{piece}");
        assert!(new.windows[1].start < s(20.0), "{:?}", new.windows);
        assert_eq!(
            (new.text().as_str(), old.as_str()),
            ("w11 w12 w17", "w11 w12 w17"),
            "{piece}"
        );
    }
}

#[test]
fn a_gaps_run_after_a_chunk_that_kept_nothing_drops_the_rest_of_a_word_it_begins_in() {
    // `CoreML`'s settings: a chunk with word 11, a long gap with quiet word
    // 2, a chunk the fake decodes to nothing, and the gap after it, whose
    // first window begins on the rest of a word that chunk missed (a
    // continuation piece or a full stop, at the window's first frame or
    // its second), then word 3. The first chunk kept a token before that
    // window, so the rest falls away, as from the old merge, and does not
    // glue onto word 2 across the empty chunk ("w2s25", "w2.").
    let config = random_config(1, true);
    let s = sample_count;
    let regions = vec![s(1.0)..s(4.0), s(20.0)..s(23.0)];
    let base = crafted(40.0, &regions, &[]);
    let spans = partition(&base, &regions, &config.chunker);
    let empty = &spans.iter().filter(|s| s.chunk).nth(1).unwrap().window;
    for (piece, lead) in [(25, 0), (25, 1), (41, 0)] {
        let mut samples = base.clone();
        ref_placed(vec![11], 2.0, 4).put(&mut samples);
        ref_placed(vec![2], 10.0, 4).put(&mut samples);
        Placed {
            pieces: vec![piece],
            frame: empty.end.div_ceil(FRAME_SAMPLES) + lead,
            frames: 3,
        }
        .put(&mut samples);
        ref_placed(vec![3], 30.0, 4).put(&mut samples);
        for jitter in 0..=1 {
            let hearing = Hearing::every_frame(jitter);
            let (new, old) = texts_over(&samples, &regions, &config, 0, hearing);
            assert_eq!(
                (new.text().as_str(), old.as_str()),
                ("w11 w2 w3", "w11 w2 w3"),
                "piece {piece} lead {lead} jitter {jitter}"
            );
            assert!(new.words[1].end < 11.0, "{:?}", new.words);
        }
    }
}

#[test]
fn a_recovery_that_missed_a_words_start_can_glue_its_rest_onto_the_word_before_it() {
    // A known limit, text only: `CoreML`'s settings, 20 s, loud sound the
    // fake decodes to nothing from 3 to 6.5 s with word 13 in it, which a
    // recovery hears; before it a word whose rest is a continuation, a
    // full stop or both, then word 14. The fake is deaf for a window's
    // first frames, so the gap's window and the recovery both miss that
    // word's start and hear its rest. The recovery keeps the rest, which
    // lies in its reach, and its tokens step back into the gap's window, so
    // the rest follows the gap's last word, word 14, a second time: a
    // copy glued on, never a loss. The old merge kept it once.
    let config = random_config(1, true);
    let regions = vec![sample_count(3.0)..sample_count(6.5)];
    let cases: [(&[u32], usize, &str, &str); 4] = [
        (&[25], 4, "s25 w14s25 w14 w13", "s25 w14 w13"),
        (&[41], 4, ". w14. w14 w13", ". w14 w13"),
        (&[25, 41], 4, "s25. w14s25. w14 w13", "s25. w14 w13"),
        (&[25], 6, "s25 w14s25 w14 w13", "s25 w14 w13"),
    ];
    for (rest, lead, text, before) in cases {
        let mut samples = crafted(20.0, &regions, &[]);
        Placed {
            pieces: [&[5], rest].concat(),
            frame: lead,
            frames: 6,
        }
        .put(&mut samples);
        let after = lead + 6 * (1 + rest.len()) + 4;
        Placed {
            pieces: vec![14],
            frame: after,
            frames: 5,
        }
        .put(&mut samples);
        ref_placed(vec![13], 4.5, 5).put(&mut samples);
        for jitter in 0..=1 {
            let hearing = Hearing {
                jitter,
                deaf: (lead + 8, 0, 1),
            };
            let (new, old) = texts_over(&samples, &regions, &config, 130, hearing);
            assert_eq!(new.stats.recoveries_accepted, 1, "{rest:?} {lead} {jitter}");
            assert_eq!(
                (new.text().as_str(), old.as_str()),
                (text, before),
                "{rest:?} {lead} {jitter}"
            );
        }
    }
}

/// Case 1411 of seed 0x120f372fe94f82d in the orphan layout: 11.2 s,
/// loud sound from 3.8 s to the end that the fake decodes to nothing, and
/// the recording begins with an orphan (".s40").
fn ref_case_1411() -> RefCase {
    let regions = vec![60_800..179_200];
    let words = ref_at(&[
        (&[41, 40], 21, 3),
        (&[18, 38, 38], 33, 2),
        (&[41, 25, 25, 41], 49, 6),
        (&[13], 81, 3),
        (&[18, 38, 38, 41], 113, 6),
    ]);
    ref_case(crafted(11.2, &regions, &[]), words, regions)
}

#[test]
fn a_recovery_that_begins_at_an_orphan_can_glue_it_onto_the_word_before_it() {
    // A known limit, text only, from the orphan layout (seed
    // 0x120f372fe94f82d, case 1411): settings with 15 s chunks and a
    // 3 s long pause, 11.2 s, loud sound from 3.8 s to the end that the
    // fake decodes to nothing, so the chunk's recovery from the
    // recording's start wins. The recording begins with a full stop and a
    // continuation (".s40") whose word it cuts off, then word 18 before
    // the chunk. The gap's window keeps ".s40 w18s38", the recovery
    // begins at the same orphan and keeps it, as it lies in its reach, and
    // its tokens step back into the gap's window: ".s40" follows word 18,
    // and word 18 comes out again. A copy glued on, never a loss.
    let case = ref_case_1411();
    let regions = case.regions.as_deref().unwrap();
    for jitter in 0..=1 {
        let (new, old) = texts(&case.samples, regions, &random_config(2, true), jitter);
        assert_eq!(new.stats.recoveries_accepted, 1, "{jitter}");
        assert_eq!(
            (new.text().as_str(), old.as_str()),
            (
                ".s40 w18s38.s40 w18s38.s25. w13 w18s38.",
                ".s40 w18s38.s25. w13 w18s38."
            ),
            "{jitter}"
        );
    }
}

#[test]
fn a_gaps_run_drops_the_rest_of_a_word_a_later_recovery_kept_from_its_start() {
    // Settings with 15 s chunks and a 3 s long pause: word 2 in the
    // lead-in, loud sound from 4 s that the fake decodes to nothing and
    // that cannot be decoded again (every extension passes 15 s), a word
    // whose first piece ends on that chunk's last frame, a pause of 3.2 s,
    // and a 5 s chunk with word 3 to the end. That chunk's recovery
    // keeps the word whole from its start, before the pause's window, so
    // the rest of it the window begins with falls away, as from the old
    // merge, and does not glue onto word 2 across the empty chunk
    // ("w2s25s26").
    let config = random_config(2, true);
    for (end, pause, frames, jitter) in [(13.5, 3.2, 10, 0), (13.5, 3.2, 10, 1), (13.0, 3.2, 10, 0)]
    {
        let (case, run) = ref_later_recovery_case(&config, (end, pause, frames), jitter);
        let old = quiet_transcriber(config.clone(), 130).render(&run.old);
        assert_eq!(
            (run.new.text().as_str(), old.as_str()),
            ("w2 w5s25s26 w3", "w2 w5s25s26 w3"),
            "{end} {pause} {frames} {jitter}"
        );
        let losses = ref_losses(&case, &run, 2 + jitter);
        assert!(losses.glued.is_empty(), "{losses:?}");
    }
}

/// The case of `a_gaps_run_drops_the_rest_of_a_word_a_later_recovery_kept_from_its_start`:
/// word 2 at 1.5 s, loud sound from 4 s to `end`, word [5, 25, 26] of
/// `frames` a piece whose first piece ends on that chunk's last frame, a
/// `pause`, and a 5 s chunk with word 3, run over a fake with `jitter`.
fn ref_later_recovery_case(
    config: &PipelineConfig,
    (end, pause, frames): (f32, f32, usize),
    jitter: usize,
) -> (RefCase, RefRun) {
    let s = sample_count;
    let after = end + pause;
    let regions = vec![s(4.0)..s(end), s(after)..s(after + 5.0)];
    let samples = crafted(after + 5.0, &regions, &[]);
    let spans = partition(&samples, &regions, &config.chunker);
    let empty = &spans.iter().find(|s| s.chunk).unwrap().window;
    let words = vec![
        ref_placed(vec![2], 1.5, 4),
        Placed {
            pieces: vec![5, 25, 26],
            frame: empty.end / FRAME_SAMPLES - frames,
            frames,
        },
        ref_placed(vec![3], after + 2.0, 4),
    ];
    let case = ref_case(samples, words, regions);
    let run = ref_run(&case, config, 130, Hearing::every_frame(jitter));
    (case, run)
}

/// Case 855 of seed 0x3ec9e6e5fd29f060 in the orphan layout, cut to the
/// words around the limit: 8 s chunks, 40 s without noise, speech from
/// 2.6 to 13.7 s, and loud sound the fake decodes to nothing from 19.4 to
/// 25.4 s and from 28 to 37.8 s, over a fake a frame off and deaf for a
/// window's last 30 frames.
fn ref_orphan_led_case() -> (RefCase, RefRun) {
    let regions = vec![41_600..219_200, 310_400..406_400, 448_000..604_800];
    let hearing = Hearing {
        jitter: 1,
        deaf: (0, 30, 1),
    };
    let config = random_config(3, true);
    ref_cut_case(
        &config,
        hearing,
        (640_000, regions, &[1, 2]),
        &[
            (&[16, 36, 36, 41], 108, 5),
            (&[17, 37, 41], 134, 6),
            (&[12], 175, 4),
            (&[3, 23, 23, 41], 187, 2),
            (&[10, 30], 208, 3),
            (&[10, 41], 228, 6),
            (&[41], 244, 6),
            (&[14], 296, 4),
        ],
    )
}

#[test]
fn a_recovery_that_begins_at_an_orphan_can_glue_the_end_of_a_word_onto_the_word_before_it() {
    // A known limit, text only, at limit E's exact signature
    // (`ref_orphan_led_case`). The first loud chunk's recovery keeps the
    // head of word 10 at 22.7 s and cuts it before its full stop, the
    // reach's end; the gap's window after it, deaf at its end, hears only
    // word 10 at 20.8 s again, 1.5 s before; and the next chunk's recovery
    // begins inside that full stop and keeps it, in its reach. The stop is
    // the recovery's leading orphan, and it follows the gap's last word:
    // the copy of word 10 at 20.8 s. Never a loss; the old merge dropped
    // the copy and kept "w10.".
    let (_, run) = ref_orphan_led_case();
    let old = quiet_transcriber(random_config(3, true), 130).render(&run.old);
    assert_eq!(run.new.stats.recoveries_accepted, 3);
    assert_eq!(
        (run.new.text().as_str(), old.as_str()),
        (
            "w16s36. w17s37. w12 w3s23. w10s30 w10 w10. w14",
            "w16s36. w17s37. w12 w3s23. w10s30 w10. w14"
        )
    );
}

/// A generated case cut to the words around a limit, run under `config`
/// over a fake that hears as `hearing` and decodes a window under 130
/// frames that holds loud sound to nothing: `total` samples with speech
/// in `regions`, loud sound the fake decodes to nothing in the regions
/// `loud` names, and `words` placed as [`ref_at`] reads them.
fn ref_cut_case(
    config: &PipelineConfig,
    hearing: Hearing,
    (total, regions, loud): (usize, Vec<Range<usize>>, &[usize]),
    words: &[(&[u32], usize, usize)],
) -> (RefCase, RefRun) {
    let mut samples = vec![0.0; total];
    for &r in loud {
        samples[regions[r].clone()].fill(0.05);
    }
    let case = ref_case(samples, ref_at(words), regions);
    let run = ref_run(&case, config, 130, hearing);
    (case, run)
}

/// The words of case 1929 of seed 0xa09c6d2c7ddf7445 around its limit
/// ([`ref_across_gap_case`]).
const REF_CASE_1929: &[(&[u32], usize, usize)] = &[
    (&[14, 34, 34, 41], 280, 2),
    (&[13, 33, 41], 493, 2),
    (&[36, 41], 510, 6),
    (&[13], 551, 2),
];

/// Case 1929 of seed 0xa09c6d2c7ddf7445 in the orphan layout, cut to the
/// words around the limit: settings with 25 s chunks of at most 15 s and
/// no extra windows, 51.7 s without noise, speech to 14.2 s and loud sound
/// the fake decodes to nothing from 43.5 s to the end, over a fake
/// `jitter` frames off and deaf for a window's first 30 frames.
fn ref_across_gap_case(jitter: usize) -> (RefCase, RefRun) {
    let regions = vec![0..227_200, 696_000..827_200];
    let hearing = Hearing {
        jitter,
        deaf: (30, 0, 1),
    };
    let config = random_config(385, false);
    ref_cut_case(&config, hearing, (827_200, regions, &[1]), REF_CASE_1929)
}

#[test]
fn a_recovery_that_missed_a_words_start_can_glue_its_rest_onto_a_word_across_an_undecoded_gap() {
    // A named limit, text only, counted and capped at 0 on the CI seed
    // (`ref_across_gap_case`). The middle chunk decodes word 14 at 22.4 s
    // and owns up to 33.4 s, the middle of the undecoded gap after its
    // window; the last chunk owns from there. Its window decodes to
    // nothing, and its recovery from 37.25 s wins, deaf to 39.65 s: it
    // misses the start of word 13 at 39.4 s, begins at its rest "s33.",
    // which it keeps from there as a word with a piece in its reach, and
    // the orphan "s36." after it. That rest follows word 14, the last
    // token kept before it, across the gap's middle. Never a loss; the old
    // merge dropped the rest and glued only the orphan's full stop.
    for jitter in 0..=3 {
        let (case, run) = ref_across_gap_case(jitter);
        let old = quiet_transcriber(random_config(385, false), 130).render(&run.old);
        assert_eq!(run.new.stats.recoveries_accepted, 1, "{jitter}");
        assert_eq!(
            (run.new.text().as_str(), old.as_str()),
            ("w14s34.s33.s36. w13", "w14s34.. w13"),
            "{jitter}"
        );
        let losses = ref_losses(&case, &run, 2 + jitter);
        assert!(
            losses.passes() && losses.across_gap == 1,
            "{jitter} {losses:?}"
        );
    }
}

#[test]
fn the_boundary_check_never_fails_the_old_merges_text_against_itself() {
    // Case 158 of seed 0xa09c6d2c7ddf7445 in the orphan layout, cut to
    // three words: 8 s chunks with the extra windows, 100.2 s of faint
    // noise, loud sound the fake decodes to nothing in three of its four
    // speech regions, over a fake three frames off and deaf for a window's
    // last 10 frames. The fake gives a piece of the last word more than
    // five frames from its place, so no placed word holds it and the
    // boundaries flag the old merge's own word; the two merges' tokens are
    // the same, and the sanity rule passes them. Once the texts differ
    // (a copy of the first word at the end), the flag counts as one an
    // unplaced piece makes.
    let regions = vec![
        40_000..142_400,
        576_000..672_000,
        1_208_000..1_441_600,
        1_481_600..1_603_200,
    ];
    let loud = [&regions[..2], &regions[3..]].concat();
    let words = ref_at(&[
        (&[12, 32, 32], 1005, 2),
        (&[12, 32, 41], 1018, 5),
        (&[12, 32, 32, 41], 1038, 3),
    ]);
    let case = ref_case(crafted(100.2, &loud, &[]), words, regions);
    let hearing = Hearing {
        jitter: 3,
        deaf: (0, 10, 1),
    };
    let run = ref_run(&case, &random_config(31, true), 130, hearing);
    assert_eq!(run.old, run.new.tokens);
    let unplaced = |t: &Token| ref_place(t, &case.words, 5).is_none();
    assert!(run.new.tokens.iter().any(unplaced));
    let vocab = vocab();
    let texts = RefTexts::new(&case, &run, &vocab, 5);
    let words = texts.words();
    assert!(words.iter().any(|&(_, of)| !texts.held(of, Extras::Check)));
    let losses = ref_losses(&case, &run, 5);
    assert!(losses.passes() && losses.unplaced == 0, "{losses:?}");
    let mut run = run;
    run.new.tokens.push(run.new.tokens[0]);
    let losses = ref_losses(&case, &run, 5);
    assert!(
        losses.glued.len() == 1 && losses.unplaced == 1,
        "{losses:?}"
    );
}

/// `ref_across_gap_case` with loud sound the fake decodes to nothing from
/// 27 to 29.5 s: a chunk whose decoded window hears nothing and that has
/// too little speech to recover, between the middle chunk and the last.
fn ref_across_empty_chunk_case(jitter: usize) -> (RefCase, RefRun) {
    let regions = vec![0..227_200, 432_000..472_000, 696_000..827_200];
    let hearing = Hearing {
        jitter,
        deaf: (30, 0, 1),
    };
    let config = random_config(385, false);
    ref_cut_case(&config, hearing, (827_200, regions, &[1, 2]), REF_CASE_1929)
}

#[test]
fn a_recovery_that_missed_a_words_start_can_glue_its_rest_across_a_chunk_that_heard_nothing() {
    // Text only, never a loss, and outside every counted class, so the
    // gate fails it wherever it shows (`ref_across_empty_chunk_case`). As
    // in `a_recovery_that_missed_a_words_start_can_glue_its_rest_onto_a_
    // word_across_an_undecoded_gap`, the last chunk's recovery begins at
    // the rest of word 13 and keeps it with the orphan after it, and that
    // rest follows word 14, across the undecoded audio and the whole
    // window of the chunk between, which decoded to nothing. The old
    // merge dropped the rest. Never seen in 100,000 generated cases.
    for jitter in 0..=3 {
        let (case, run) = ref_across_empty_chunk_case(jitter);
        let old = quiet_transcriber(random_config(385, false), 130).render(&run.old);
        assert_eq!(run.new.stats.recoveries_accepted, 1, "{jitter}");
        assert_eq!(
            (run.new.text().as_str(), old.as_str()),
            ("w14s34.s33.s36. w13", "w14s34.. w13"),
            "{jitter}"
        );
        let losses = ref_losses(&case, &run, 2 + jitter);
        assert!(
            losses.lost.is_empty() && losses.pieces.is_empty() && losses.glued == [0],
            "{jitter} {losses:?}"
        );
    }
}

/// Case 2406 of seed 0x6c62000000000062 in the orphan layout, cut to the
/// words around the limit: settings with 25 s chunks of at most 15 s and
/// no extra windows, 89.7 s without noise, loud sound the fake decodes to
/// nothing to 9.6 s and from 63 to 67.1 s, over a fake `jitter` frames
/// off and deaf for a window's first 30 frames.
fn ref_own_window_case(jitter: usize) -> (RefCase, RefRun) {
    let regions = vec![
        0..153_600,
        681_600..793_600,
        1_008_000..1_073_600,
        1_353_600..1_435_200,
    ];
    let hearing = Hearing {
        jitter,
        deaf: (30, 0, 1),
    };
    let config = random_config(481, false);
    ref_cut_case(
        &config,
        hearing,
        (1_435_200, regions, &[0, 2]),
        &[
            (&[20], 850, 3),
            (&[13, 33], 1006, 2),
            (&[41, 34], 1025, 3),
            (&[16, 36], 1058, 4),
        ],
    )
}

#[test]
fn a_recovery_that_missed_a_words_start_can_glue_its_rest_across_a_boundary_its_window_decodes() {
    // Limit E across its own window, text only, counted and capped at 0
    // on the CI seed (`ref_own_window_case`). The third chunk decodes word
    // 20 at 68 s and owns up to 79.05 s, the middle of the undecoded gap
    // after its window. The last chunk's window decodes to nothing, and
    // its recovery from 78.35 s wins, deaf to 80.75 s: it misses the
    // start of word 13 at 80.5 s and begins at its rest "s33", in its
    // reach. That rest follows word 20 across the third chunk's owned end,
    // which only the recovery's own window decodes, into the old merge's
    // glue of the orphan ".s34" after word 20. Never a loss; the old merge
    // dropped the rest.
    for jitter in 1..=3 {
        let (case, run) = ref_own_window_case(jitter);
        let old = quiet_transcriber(random_config(481, false), 130).render(&run.old);
        assert_eq!(run.new.stats.recoveries_accepted, 1, "{jitter}");
        assert_eq!(
            (run.new.text().as_str(), old.as_str()),
            ("w20s33.s34 w16s36", "w20.s34 w16s36"),
            "{jitter}"
        );
        let losses = ref_losses(&case, &run, 2 + jitter);
        assert!(
            losses.passes() && losses.across_gap == 0 && losses.across_own_window == 1,
            "{jitter} {losses:?}"
        );
    }
}

/// Case 1381 of seed 0xa8d2f372fe94f88e in a layout of short regions
/// split by long pauses, cut to the words around the limit: settings with
/// 25 s chunks, long pauses of 3 s and no extra windows, 65.1 s without
/// noise, loud sound the fake decodes to nothing from 15.6 to 22.3 s,
/// from 39.7 to 46.4 s and from 53.6 to 57.4 s, over a fake `jitter`
/// frames off that hears every frame.
fn ref_inside_glue_case(jitter: usize) -> (RefCase, RefRun) {
    let regions = vec![
        1_600..105_600,
        249_600..356_800,
        481_600..547_200,
        635_200..742_400,
        857_600..918_399,
    ];
    let hearing = Hearing::every_frame(jitter);
    let config = random_config(276, false);
    ref_cut_case(
        &config,
        hearing,
        (1_041_600, regions, &[1, 3, 4]),
        &[
            (&[7], 510, 6),
            (&[9, 29, 29], 601, 6),
            (&[41, 24], 628, 5),
            (&[41, 37, 37, 41], 644, 4),
            (&[12, 32, 32, 41], 708, 2),
        ],
    )
}

#[test]
fn an_orphan_the_old_merge_dropped_can_come_out_inside_its_glue_across_a_chunk() {
    // Limit (ii) inside the old merge's glue, text only, counted and
    // capped at 0 on the CI seed (`ref_inside_glue_case`), over a fake
    // that hears every frame. The fourth and fifth chunks decode to
    // nothing and both recoveries win. They keep word 9 at 48.1 s and the
    // orphan ".s24" at 50.2 s, which the old merge dropped while it glued
    // the orphan ".s37." after it onto word 9, across the fourth chunk's
    // owned end at 50 s. So the dropped orphan comes out inside that glue.
    // Never a loss.
    for jitter in 0..=3 {
        let (case, run) = ref_inside_glue_case(jitter);
        let old = quiet_transcriber(random_config(276, false), 130).render(&run.old);
        assert_eq!(run.new.stats.recoveries_accepted, 2, "{jitter}");
        assert_eq!(
            (run.new.text().as_str(), old.as_str()),
            ("w7 w9s29.s24.s37. w12s32.", "w7 w9s29.s37. w12s32."),
            "{jitter}"
        );
        let losses = ref_losses(&case, &run, 2 + jitter);
        assert!(
            losses.passes() && losses.inside_glue == 1,
            "{jitter} {losses:?}"
        );
    }
}

/// Case 742 of seed 0x6f04000000000004 in a layout of short regions split
/// by long pauses, cut to the words around the limit: settings with 25 s
/// chunks, long pauses of 3 s and no extra windows, 43.3 s without noise,
/// loud sound the fake decodes to nothing from 2.1 to 10.6 s, from 13.8
/// to 18.9 s and from 30 to 37 s, over a fake one frame off that hears
/// every frame.
fn ref_orphan_own_window_case() -> (RefCase, RefRun) {
    let regions = vec![
        33_600..169_600,
        220_799..302_400,
        377_599..423_999,
        479_999..592_000,
    ];
    ref_cut_case(
        &random_config(148, false),
        Hearing::every_frame(1),
        (692_800, regions, &[0, 1, 3]),
        &[
            (&[9, 41], 331, 2),
            (&[28, 28], 357, 2),
            (&[2, 22, 41], 466, 3),
        ],
    )
}

#[test]
fn an_orphan_the_old_merge_dropped_can_come_out_across_a_boundary_its_window_decodes() {
    // Limit (ii) across its own window, text only, counted and capped at
    // 0 on the CI seed (`ref_orphan_own_window_case`), over a fake that
    // hears every frame. The third chunk decodes word 9 at 26.5 s and owns
    // up to 28.25 s. The last chunk's window decodes to nothing, and its
    // recovery from 23.75 s wins: it keeps word 9 with its full stop, which
    // the old merge dropped, and the orphan "s28" at 28.5 s, which the old
    // merge dropped too, glued onto it across the last chunk's owned
    // start, which the recovery's own window decodes. Never a loss.
    let (case, run) = ref_orphan_own_window_case();
    let old = quiet_transcriber(random_config(148, false), 130).render(&run.old);
    assert_eq!(run.new.stats.recoveries_accepted, 1);
    assert_eq!(
        (run.new.text().as_str(), old.as_str()),
        ("w9.s28 w2s22.", "w9 w2s22.")
    );
    let losses = ref_losses(&case, &run, 3);
    assert!(
        losses.passes() && losses.across_own_window == 0 && losses.orphan_across_own_window == 1,
        "{losses:?}"
    );
}

/// Case 857 of seed 0x18ab79b97f4a7c89 in a layout of short regions split
/// by long pauses, cut to the words around the limit: settings with 8 s
/// chunks and the extra windows, 62.6 s without noise, loud sound the
/// fake decodes to nothing in all five speech regions, over a fake
/// `jitter` frames off and deaf for a window's first 45 frames.
fn ref_rest_inside_glue_case(jitter: usize) -> (RefCase, RefRun) {
    let regions = vec![
        27_200..89_600,
        254_399..321_599,
        364_800..486_400,
        678_400..718_400,
        772_800..812_800,
    ];
    let hearing = Hearing {
        jitter,
        deaf: (45, 0, 1),
    };
    ref_cut_case(
        &random_config(171, true),
        hearing,
        (1_001_600, regions, &[0, 1, 2, 3, 4]),
        &[
            (&[14, 41], 382, 3),
            (&[12, 32], 492, 4),
            (&[41, 36, 36], 504, 3),
            (&[15, 35, 41], 534, 3),
        ],
    )
}

#[test]
fn a_recovery_that_missed_a_words_start_can_keep_its_rest_inside_the_old_merges_glue() {
    // Limit E inside the old merge's glue, text only, counted and capped
    // at 0 on the CI seed (`ref_rest_inside_glue_case`). The old merge
    // glued the orphan ".s36" at 40.3 s onto word 14 at 30.56 s, across the
    // owned start of the gap at 36.98 s, whose window decodes. The last
    // chunk's recovery from 36.15 s wins, deaf to 39.75 s: it misses the
    // start of word 12 at 39.36 s and begins at its rest "s32", inside
    // that glue. The old merge dropped the rest. Never a loss.
    for jitter in 0..=3 {
        let (case, run) = ref_rest_inside_glue_case(jitter);
        let old = quiet_transcriber(random_config(171, true), 130).render(&run.old);
        assert_eq!(
            (run.new.text().as_str(), old.as_str()),
            ("w14.s32.s36 w15s35.", "w14..s36 w15s35."),
            "{jitter}"
        );
        let losses = ref_losses(&case, &run, 2 + jitter);
        assert!(
            losses.passes() && losses.rest_inside_glue == 1,
            "{jitter} {losses:?}"
        );
    }
}

/// Case 136 of seed 0xab72f372fe94f82e in a layout of short regions split
/// by long pauses, cut to the words around the limit: settings with 8 s
/// chunks and the extra windows, 63.5 s without noise, loud sound the
/// fake decodes to nothing from 51.5 to 55.4 s, over a fake `jitter`
/// frames off and deaf for a window's first 45 frames.
fn ref_rest_inside_lead_glue_case(jitter: usize) -> (RefCase, RefRun) {
    let regions = vec![
        28_800..65_600,
        251_200..339_200,
        539_200..611_200,
        824_000..886_400,
    ];
    let hearing = Hearing {
        jitter,
        deaf: (45, 0, 1),
    };
    ref_cut_case(
        &random_config(27, true),
        hearing,
        (1_016_000, regions, &[3]),
        &[
            (&[41, 41], 104, 3),
            (&[3, 23, 23], 157, 4),
            (&[41], 177, 6),
            (&[6, 26, 26, 41], 197, 3),
        ],
    )
}

#[test]
fn a_recovery_that_missed_a_words_start_can_keep_its_rest_inside_the_old_merges_leading_glue() {
    // Limit E inside the old merge's leading glue, text only, counted and
    // capped at 0 on the CI seed (`ref_rest_inside_lead_glue_case`). The
    // old merge's text begins with the orphan "." at 8.35 s, glued to the
    // orphan "." at 14.2 s. The gap's window from 9.55 s decodes; the
    // second chunk's recovery from 9.45 s wins, deaf to 13.05 s: it misses
    // the start of word 3 at 12.55 s and begins at its rest "s23", inside
    // that glue, across the gap's owned start, which the gap's window
    // decodes. The old merge dropped the rest. Never a loss.
    for jitter in 0..=3 {
        let (case, run) = ref_rest_inside_lead_glue_case(jitter);
        let old = quiet_transcriber(random_config(27, true), 130).render(&run.old);
        assert_eq!(
            (run.new.text().as_str(), old.as_str()),
            (".s23. w6s26.", ".. w6s26."),
            "{jitter}"
        );
        let losses = ref_losses(&case, &run, 2 + jitter);
        assert!(
            losses.passes() && losses.rest_inside_lead_glue == 1,
            "{jitter} {losses:?}"
        );
    }
}

/// Case 499 of seed 0xd3b79b97f4a7c19 in a layout of short regions split
/// by long pauses, cut to the words around the limit: settings with 8 s
/// chunks and the extra windows, 57.9 s without noise, loud sound the
/// fake decodes to nothing in all four speech regions, over a fake
/// `jitter` frames off and deaf for a window's first 45 frames.
fn ref_copy_before_lead_case(jitter: usize) -> (RefCase, RefRun) {
    let regions = vec![
        36_800..56_000,
        249_600..372_800,
        488_000..577_600,
        684_800..748_800,
    ];
    let hearing = Hearing {
        jitter,
        deaf: (45, 0, 1),
    };
    ref_cut_case(
        &random_config(99, true),
        hearing,
        (926_399, regions, &[0, 1, 2, 3]),
        &[
            (&[35, 35], 154, 6),
            (&[33, 33], 175, 2),
            (&[10, 30, 41], 209, 4),
        ],
    )
}

#[test]
fn a_copy_of_a_piece_the_old_merge_kept_can_come_out_before_its_text_leading_orphan() {
    // A copy before the text-leading orphan, text only, counted and
    // capped at 0 on the CI seed (`ref_copy_before_lead_case`). The second
    // chunk's recovery from 9.35 s wins, deaf to 12.95 s: it begins at
    // the orphan "s35s33" at 12.95 s, which leads the old merge's text.
    // The gap's window before it heard the same "s33" at 13.97 s, which
    // comes out first: the recovery's tokens step back over it, and the
    // seam rule cannot drop it, as it is no word start. A duplicate, never
    // a loss.
    for jitter in 0..=3 {
        let (case, run) = ref_copy_before_lead_case(jitter);
        let old = quiet_transcriber(random_config(99, true), 130).render(&run.old);
        assert_eq!(
            (run.new.text().as_str(), old.as_str()),
            ("s33s35s33 w10s30.", "s35s33 w10s30."),
            "{jitter}"
        );
        let losses = ref_losses(&case, &run, 2 + jitter);
        assert!(
            losses.passes() && losses.copy_before_lead == 1,
            "{jitter} {losses:?}"
        );
    }
}

/// The tokens of placed words' pieces, `(word, piece)`, each at its
/// piece's start.
fn ref_plant(words: &[Placed], pieces: &[(usize, usize)]) -> Vec<Token> {
    let token = |&(w, i): &(usize, usize)| Token {
        id: words[w].pieces[i],
        frame: words[w].start(i) / FRAME_SAMPLES,
        phase: 0,
        confidence: 1.0,
        duration: 0,
    };
    pieces.iter().map(token).collect()
}

/// Whether the boundary check fails `run` with the two merges' tokens
/// planted: `old` and `new` give them as placed words' pieces.
fn ref_planted_fails(
    case: &RefCase,
    run: &mut RefRun,
    old: &[(usize, usize)],
    new: &[(usize, usize)],
) -> bool {
    run.old = ref_plant(&case.words, old);
    run.new.tokens = ref_plant(&case.words, new);
    !ref_losses(case, run, 2).passes()
}

#[test]
fn the_boundary_check_fails_each_planted_glue_and_passes_each_limit() {
    // The check itself ([`ref_losses`]), on planted texts over real
    // partitions ([`ref_planted_fails`]).
    let check = ref_planted_fails;
    // The layout of `a_gaps_run_after_a_chunk_that_kept_nothing_...`: word
    // 11 in a chunk, word 2 in the gap, a chunk the fake decodes to
    // nothing, then an orphan, the rest of a word (".s25" here), and word
    // 3. The old merge drops the orphan or glues its stop onto word 2.
    let config = random_config(1, true);
    let s = sample_count;
    let regions = vec![s(1.0)..s(4.0), s(20.0)..s(23.0)];
    let samples = crafted(40.0, &regions, &[]);
    let spans = partition(&samples, &regions, &config.chunker);
    let empty = &spans.iter().filter(|s| s.chunk).nth(1).unwrap().window;
    let words = vec![
        ref_placed(vec![11], 2.0, 4),
        ref_placed(vec![2], 10.0, 4),
        Placed {
            pieces: vec![25, 41],
            frame: empty.end.div_ceil(FRAME_SAMPLES),
            frames: 3,
        },
        ref_placed(vec![3], 30.0, 4),
    ];
    let case = ref_case(samples, words, regions);
    let mut run = ref_run(&case, &config, 0, Hearing::every_frame(0));
    let main = [(0, 0), (1, 0), (3, 0)];
    // The drain that counted only the chunk run right before: the rest
    // glued onto word 2 across the empty chunk.
    assert!(check(
        &case,
        &mut run,
        &main,
        &[(0, 0), (1, 0), (2, 0), (2, 1), (3, 0)]
    ));
    // Plant 3: a dropped word's pieces across a whole chunk's owned span.
    assert!(check(
        &case,
        &mut run,
        &main,
        &[(0, 0), (2, 0), (2, 1), (1, 0), (3, 0)]
    ));
    // (ii)'s shape: the orphan the old merge dropped, kept and glued onto
    // a word with no chunk's owned span starting or ending between.
    assert!(!check(
        &case,
        &mut run,
        &main,
        &[(0, 0), (1, 0), (3, 0), (2, 0), (2, 1)]
    ));
    // The old merge's own glue (its stop after word 2), extended by the
    // piece its trim cut.
    let main = [(0, 0), (1, 0), (2, 1), (3, 0)];
    assert!(!check(
        &case,
        &mut run,
        &main,
        &[(0, 0), (1, 0), (2, 0), (2, 1), (3, 0)]
    ));

    // The drain that counted only the chunk runs before the gap: the rest
    // of word 5, which the old merge kept whole, glued onto word 2 across
    // the empty chunk; and a copy of its last piece onto word 3, across
    // only the owned start of word 3's chunk.
    let (case, mut run) = ref_later_recovery_case(&random_config(2, true), (13.5, 3.2, 10), 0);
    let main = [(0, 0), (1, 0), (1, 1), (1, 2), (2, 0)];
    let later = [(0, 0), (1, 1), (1, 2), (1, 0), (1, 1), (1, 2), (2, 0)];
    assert!(check(&case, &mut run, &main, &later));
    let copy = [(0, 0), (1, 0), (1, 1), (1, 2), (2, 0), (1, 2)];
    assert!(check(&case, &mut run, &main, &copy));

    // Case 1411: the old merge's text begins with the orphan ".s40".
    let case = ref_case_1411();
    let mut run = ref_run(&case, &random_config(2, true), 130, Hearing::every_frame(0));
    assert!(ref_losses(&case, &run, 2).glued.is_empty());
    let main = [(0, 0), (0, 1), (1, 0), (1, 1), (2, 0), (2, 1), (2, 3)];
    let main = [&main[..], &[(3, 0), (4, 0), (4, 1), (4, 3)]].concat();
    // Plant 1: the leading orphan moved onto the old merge's next word.
    let moved = [&main[2..7], &main[..2], &main[7..]].concat();
    assert!(check(&case, &mut run, &main, &moved));
    // Plant 2: word 18's kept piece dropped in every copy, or moved onto
    // word 13.
    let dropped = [&main[..3], &main[4..]].concat();
    assert!(check(&case, &mut run, &main, &dropped));
    let moved = [&main[..3], &main[4..8], &main[3..4], &main[8..]].concat();
    assert!(check(&case, &mut run, &main, &moved));

    // A full stop moved onto the next word, both in one gap's window, far
    // from any chunk: no extra piece crosses, so the word that lost it
    // fails, and it is no recovery's leading orphan.
    let regions = vec![sample_count(10.0)..sample_count(13.0)];
    let words = vec![
        ref_placed(vec![5, 41], 1.0, 4),
        ref_placed(vec![14], 2.0, 4),
    ];
    let case = ref_case(crafted(20.0, &regions, &[]), words, regions);
    let mut run = ref_run(&case, &random_config(1, true), 130, Hearing::every_frame(0));
    assert!(check(
        &case,
        &mut run,
        &[(0, 0), (0, 1), (1, 0)],
        &[(0, 0), (1, 0), (0, 1)]
    ));

    // Limit E: the layout of `a_recovery_that_missed_a_words_start_...`
    // and case 855 pass as they come out, the second at E's exact
    // signature.
    let regions = vec![sample_count(3.0)..sample_count(6.5)];
    let mut words = ref_at(&[(&[5, 25], 4, 6), (&[14], 20, 5)]);
    words.push(ref_placed(vec![13], 4.5, 5));
    let case = ref_case(crafted(20.0, &regions, &[]), words, regions);
    let hearing = Hearing {
        jitter: 0,
        deaf: (12, 0, 1),
    };
    let run = ref_run(&case, &random_config(1, true), 130, hearing);
    let losses = ref_losses(&case, &run, 2);
    assert!(losses.glued.is_empty() && losses.glues == 1, "{losses:?}");
    let (case, run) = ref_orphan_led_case();
    let losses = ref_losses(&case, &run, 3);
    assert!(losses.glued.is_empty() && losses.led == 1, "{losses:?}");
}

#[test]
fn the_boundary_check_counts_a_glue_in_a_class_only_at_its_signature() {
    // Planted glues next to the counted classes ([`Extras`]), each of
    // which no class takes, so the check fails it ([`ref_planted_fails`]).
    let check = ref_planted_fails;
    // A chunk with word 5, then a gap's window with word 10, the orphan
    // "." and word 14. The old merge glued the stop onto word 5, across
    // the chunk's owned end, where the gap's window begins.
    let regions = vec![sample_count(1.0)..sample_count(4.0)];
    let words = vec![
        ref_placed(vec![5, 25], 2.0, 4),
        ref_placed(vec![10, 30], 5.5, 4),
        ref_placed(vec![41], 7.0, 4),
        ref_placed(vec![14], 10.0, 4),
    ];
    let case = ref_case(crafted(20.0, &regions, &[]), words, regions);
    let mut run = ref_run(&case, &random_config(1, true), 130, Hearing::every_frame(0));
    let main = [(0, 0), (0, 1), (2, 0), (3, 0)];
    // A copy of word 5's last piece onto word 14, across only the chunk's
    // owned end, into the gap's window.
    let copy = [&main[..], &[(0, 1)]].concat();
    assert!(check(&case, &mut run, &main, &copy));
    // The rest of word 10, which the old merge dropped, inside its glue
    // after word 5, across the chunk's owned end that a plain gap window
    // decodes: no counted class takes it.
    let rest = [(0, 0), (0, 1), (1, 1), (2, 0), (3, 0)];
    assert!(check(&case, &mut run, &main, &rest));

    // The layout of `a_recovery_that_missed_a_words_start_...across_a_
    // chunk_that_heard_nothing`: its text is planted, and an orphan the
    // old merge dropped, glued onto word 14 across the undecoded gap.
    let (case, mut run) = ref_across_empty_chunk_case(0);
    let main = [(0, 0), (0, 1), (0, 3), (2, 1), (3, 0)];
    let across = [
        (0, 0),
        (0, 1),
        (0, 3),
        (1, 1),
        (1, 2),
        (2, 0),
        (2, 1),
        (3, 0),
    ];
    assert!(check(&case, &mut run, &main, &across));
    let (case, mut run) = ref_across_gap_case(0);
    let main = [(0, 0), (0, 1), (0, 3), (3, 0)];
    let orphan = [(0, 0), (0, 1), (0, 3), (2, 0), (2, 1), (3, 0)];
    assert!(check(&case, &mut run, &main, &orphan));
    // In the layout of the limit across its own window, an orphan the old
    // merge dropped, glued onto word 20 across the recovered chunk's owned
    // start, which its window holds: limit (ii) across its own window,
    // which counts it, and limit E there does not.
    let (mut case, mut run) = ref_own_window_case(1);
    let orphan = [(0, 0), (2, 0), (2, 1), (3, 0), (3, 1)];
    assert!(!check(&case, &mut run, &[(0, 0), (3, 0), (3, 1)], &orphan));
    let losses = ref_losses(&case, &run, 2);
    assert!(
        losses.orphan_across_own_window == 1 && losses.across_own_window == 0,
        "{losses:?}"
    );
    // And a copy of the rest of word 13, which the old merge kept whole,
    // glued onto word 20 there.
    let main = [(0, 0), (1, 0), (1, 1), (3, 0), (3, 1)];
    let copy = [(0, 0), (1, 1), (1, 0), (1, 1), (3, 0), (3, 1)];
    assert!(check(&case, &mut run, &main, &copy));
    // The limit's own text, with the windows planted so that the
    // recovery's window no longer holds its owned start, which the third
    // chunk's window holds instead.
    let main = [(0, 0), (2, 0), (2, 1), (3, 0), (3, 1)];
    let glued = [(0, 0), (1, 1), (2, 0), (2, 1), (3, 0), (3, 1)];
    let windows = run.new.windows.clone();
    run.new.windows[3].start = 1_270_000;
    run.new.windows[2].end = 1_266_000;
    assert!(check(&case, &mut run, &main, &glued));
    // And word 13's rest glued onto word 7, in the second chunk's owned
    // span but inside the third chunk's planted window, across the third
    // chunk's owned span too.
    run.new.windows = windows;
    run.new.windows[2].start = 900_000;
    case.words.push(ref_placed(vec![7], 57.12, 4));
    let main = [(4, 0), (0, 0), (3, 0), (3, 1)];
    let glued = [(4, 0), (1, 1), (0, 0), (3, 0), (3, 1)];
    assert!(check(&case, &mut run, &main, &glued));
    // The rest of a word the old merge dropped, glued across a plain
    // chunk's owned start: chunks of 8 s over one long region, which the
    // fake decodes.
    let config = random_config(3, true);
    let regions = vec![sample_count(1.0)..sample_count(19.0)];
    let samples = crafted(25.0, &regions, &[]);
    let spans = partition(&samples, &regions, &config.chunker);
    let second = spans.iter().filter(|s| s.chunk).nth(1).unwrap();
    let words = vec![
        ref_placed(vec![5, 25], 3.0, 4),
        Placed {
            pieces: vec![10, 30],
            frame: second.owned.start / FRAME_SAMPLES + 2,
            frames: 4,
        },
    ];
    let case = ref_case(samples, words, regions);
    let mut run = ref_run(&case, &config, 0, Hearing::every_frame(0));
    assert_eq!(run.new.stats.recoveries_accepted, 0);
    assert!(check(
        &case,
        &mut run,
        &[(0, 0), (0, 1)],
        &[(0, 0), (0, 1), (1, 1)]
    ));
    // In the layout of the orphan inside the old merge's glue, an orphan
    // the old merge kept after word 7, moved inside its glue after word 9.
    let (case, mut run) = ref_inside_glue_case(0);
    let after = [
        (1, 0),
        (1, 1),
        (3, 0),
        (3, 1),
        (3, 3),
        (4, 0),
        (4, 1),
        (4, 3),
    ];
    let main = [&[(0, 0), (2, 0), (2, 1)], &after[..]].concat();
    let moved = [&[(0, 0), (1, 0), (1, 1), (2, 0), (2, 1)], &after[2..]].concat();
    assert!(check(&case, &mut run, &main, &moved));
}

#[test]
fn the_boundary_check_fails_a_counted_glue_across_text_the_old_merge_kept() {
    // Each counted class planted as it comes out and then across a word
    // the old merge kept on its own between the two pieces, which fails
    // ([`RefTexts::kept_between`], [`ref_planted_fails`]).
    let check = ref_planted_fails;
    // The limit across an undecoded gap past a word 7 at 25 s, which the
    // old merge kept on its own (its tokens put after word 14's).
    let (mut case, mut run) = ref_across_gap_case(0);
    case.words.push(ref_placed(vec![7], 25.0, 4));
    let main = [(0, 0), (0, 1), (0, 3), (2, 1), (4, 0), (3, 0)];
    let over = [
        (0, 0),
        (0, 1),
        (0, 3),
        (1, 1),
        (1, 2),
        (2, 0),
        (2, 1),
        (4, 0),
        (3, 0),
    ];
    assert!(check(&case, &mut run, &main, &over));
    // Limit (ii) inside the old merge's glue past a word 8 at 49.76 s,
    // which the old merge kept on its own (its tokens put last).
    let (mut case, mut run) = ref_inside_glue_case(0);
    case.words.push(ref_placed(vec![8], 49.76, 2));
    let after = [(3, 0), (3, 1), (3, 3), (4, 0), (4, 1), (4, 3), (5, 0)];
    let main = [&[(0, 0), (1, 0), (1, 1)], &after[..]].concat();
    let over = [&[(0, 0), (1, 0), (1, 1), (2, 0), (2, 1)], &after[..]].concat();
    assert!(check(&case, &mut run, &main, &over));
    let (mut case, mut run) = ref_own_window_case(1);
    // Limit E there across text the old merge kept: a word 7 it kept at
    // 72 s, between word 20 and the rest of word 13 glued onto it. That
    // word 20 would end past word 7, the shape plant 3 met in its own way
    // at case 644 of seed 0x7a76000000000076, where a dropped word's
    // piece was glued onto the old merge's first word across five words
    // it kept.
    case.words.push(ref_placed(vec![7], 72.0, 4));
    let main = [(0, 0), (4, 0), (2, 0), (2, 1), (3, 0), (3, 1)];
    let over = [(0, 0), (1, 1), (4, 0), (2, 0), (2, 1), (3, 0), (3, 1)];
    assert!(check(&case, &mut run, &main, &over));
    // And limit (ii) there across it: the orphan glued onto word 20 past
    // word 7.
    let main = [(0, 0), (4, 0), (3, 0), (3, 1)];
    let over = [(0, 0), (2, 0), (2, 1), (4, 0), (3, 0), (3, 1)];
    assert!(check(&case, &mut run, &main, &over));

    // Limit E inside the old merge's glue, as it comes out, and across
    // text the old merge kept: a word 7 at 35.04 s that it kept on its own
    // (its tokens put after word 15), between word 14 and the rest of
    // word 12 inside its glue.
    let (mut case, mut run) = ref_rest_inside_glue_case(0);
    let after = [(2, 0), (2, 1), (3, 0), (3, 1), (3, 2)];
    let main = [&[(0, 0), (0, 1)], &after[..]].concat();
    let rest = [&[(0, 0), (0, 1), (1, 1)], &after[..]].concat();
    assert!(!check(&case, &mut run, &main, &rest));
    case.words.push(ref_placed(vec![7], 35.04, 2));
    let main = [&main[..], &[(4, 0)]].concat();
    let over = [&rest[..], &[(4, 0)]].concat();
    assert!(check(&case, &mut run, &main, &over));
    // Limit E inside the old merge's leading glue, as it comes out, and
    // across text the old merge kept: a word 7 at 10.4 s that it kept on
    // its own (its tokens put after word 6), between the leading orphan
    // and the rest of word 3 inside its glue.
    let (mut case, mut run) = ref_rest_inside_lead_glue_case(0);
    let after = [(3, 0), (3, 1), (3, 3)];
    let main = [&[(0, 0), (2, 0)], &after[..]].concat();
    let rest = [&[(0, 0), (1, 1), (2, 0)], &after[..]].concat();
    assert!(!check(&case, &mut run, &main, &rest));
    case.words.push(ref_placed(vec![7], 10.4, 2));
    let main = [&main[..], &[(4, 0)]].concat();
    let over = [&rest[..], &[(4, 0)]].concat();
    assert!(check(&case, &mut run, &main, &over));
    // A copy before the text-leading orphan, as it comes out, and across
    // text the old merge kept: a word 7 at 13.45 s that it kept on its own
    // (its tokens put after word 10), between the orphan and the copy.
    let (mut case, mut run) = ref_copy_before_lead_case(0);
    let main = [(0, 0), (1, 0), (2, 0), (2, 1), (2, 2)];
    let copy = [&[(1, 0)], &main[..]].concat();
    assert!(!check(&case, &mut run, &main, &copy));
    case.words.push(ref_placed(vec![7], 13.44, 2));
    let main = [&main[..], &[(3, 0)]].concat();
    let over = [&copy[..], &[(3, 0)]].concat();
    assert!(check(&case, &mut run, &main, &over));
}

#[test]
fn the_boundary_check_fails_a_glue_beside_each_class_found_at_scale() {
    // Planted glues next to limit (ii) across its own window, limit E
    // inside the old merge's glue and the copy before the text-leading
    // orphan, each of which no class takes ([`ref_planted_fails`]).
    let check = ref_planted_fails;
    // An orphan glued onto word 20 across the recovered chunk's owned
    // start, of a word the old merge kept a piece of elsewhere.
    let (case, mut run) = ref_own_window_case(1);
    let main = [(0, 0), (3, 0), (3, 1), (2, 1)];
    let orphan = [(0, 0), (2, 0), (3, 0), (3, 1), (2, 1)];
    assert!(check(&case, &mut run, &main, &orphan));
    // The rest of word 12 glued onto word 14 across the gap's owned start,
    // of a word the old merge kept a piece of elsewhere; and outside any
    // glue the old merge has after word 14.
    let (case, mut run) = ref_rest_inside_glue_case(0);
    let after = [(2, 0), (2, 1), (3, 0), (3, 1), (3, 2)];
    let main = [&[(0, 0), (0, 1)], &after[..], &[(1, 1)]].concat();
    let rest = [&[(0, 0), (0, 1), (1, 1)], &after[..], &[(1, 1)]].concat();
    assert!(check(&case, &mut run, &main, &rest));
    let main = [(0, 0), (0, 1), (3, 0), (3, 1), (3, 2)];
    let rest = [(0, 0), (0, 1), (1, 1), (3, 0), (3, 1), (3, 2)];
    assert!(check(&case, &mut run, &main, &rest));
    // The copy before the text-leading orphan with no recovery beginning
    // at the orphan; and an orphan the old merge kept after word 10, not
    // in the leading orphan's word, put before it.
    let (mut case, mut run) = ref_copy_before_lead_case(0);
    let main = [(0, 0), (1, 0), (2, 0), (2, 1), (2, 2)];
    let copy = [&[(1, 0)], &main[..]].concat();
    let leads = std::mem::take(&mut run.leads);
    assert!(check(&case, &mut run, &main, &copy));
    run.leads = leads;
    case.words.push(ref_placed(vec![28], 11.2, 2));
    let main = [&main[..], &[(3, 0)]].concat();
    let before = [&[(3, 0)], &main[..]].concat();
    assert!(check(&case, &mut run, &main, &before));
}

#[test]
fn the_boundary_check_counts_a_crossing_at_the_edge_of_a_window_or_a_span() {
    // A decoded window that begins at a word's first piece lies between
    // it and an extra piece, and so does a chunk's owned start an extra
    // piece lies on.
    let check = ref_planted_fails;
    // The limit across an undecoded gap as it comes out, with a decoded
    // window planted from word 14's first piece on, so that it lies wholly
    // between that piece and the rest glued on.
    let (case, mut run) = ref_across_gap_case(0);
    let main = [(0, 0), (0, 1), (0, 3), (2, 1), (3, 0)];
    let across = [
        (0, 0),
        (0, 1),
        (0, 3),
        (1, 1),
        (1, 2),
        (2, 0),
        (2, 1),
        (3, 0),
    ];
    assert!(!check(&case, &mut run, &main, &across));
    let head = case.words[0].start(0);
    run.new.windows.push(head..head + FRAME_SAMPLES);
    assert!(check(&case, &mut run, &main, &across));

    // A piece exactly on a plain chunk's owned start, glued onto a word in
    // the chunk before: the boundary lies between.
    let config = random_config(3, true);
    let regions = vec![sample_count(1.0)..sample_count(19.0)];
    let samples = crafted(25.0, &regions, &[]);
    let spans = partition(&samples, &regions, &config.chunker);
    let start = spans.iter().filter(|s| s.chunk).nth(1).unwrap().owned.start;
    let words = vec![
        ref_placed(vec![5, 25], 3.0, 4),
        Placed {
            pieces: vec![10, 30],
            frame: start / FRAME_SAMPLES - 4,
            frames: 4,
        },
    ];
    let case = ref_case(samples, words, regions);
    let mut run = ref_run(&case, &config, 0, Hearing::every_frame(0));
    assert_eq!(run.new.stats.recoveries_accepted, 0);
    run.old = ref_plant(&case.words, &[(0, 0), (0, 1)]);
    run.new.tokens = ref_plant(&case.words, &[(0, 0), (0, 1), (1, 1)]);
    run.new.tokens[2].frame = start / FRAME_SAMPLES;
    run.new.tokens[2].phase = start % FRAME_SAMPLES;
    assert!(!ref_losses(&case, &run, 2).passes());
}
