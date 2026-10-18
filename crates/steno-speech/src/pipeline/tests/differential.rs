//! The differential test: the partition's merge against a frozen copy of the
//! merge before it (as of 2fbab03dd, its gap windows reading [`Gaps::Tail`]).
//! The copy is not reused code, so the partition can change under it. Every
//! word the old merge keeps, this one keeps, with its pieces and its boundaries
//! ([`ref_losses`]).

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

/// Every token the final windows heard that their span keeps, decoded again
/// from the window the transcript records: a plain chunk's whole window (`.0`),
/// the reach of a gap's window or an accepted recovery (`.0` and `.1`), and the
/// pieces continuing a word that each accepted recovery's keep begins with
/// (`.2`).
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

/// The old merge's tests' random layout: 20 words, loud and quiet, of one or
/// two pieces, a quarter said twice, some after loud sound the fake decodes to
/// nothing.
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

/// Two chunks around a pause no window decodes: one plain and dense with words
/// to its edge, one loud sound the fake decodes to nothing with one word a
/// recovery hears. Quiet words in the pause reuse the plain chunk's ids, which
/// a recovery's LCS can take for the plain chunk's own.
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

/// One to four speech regions, plain or loud sound the fake decodes to nothing
/// with a few words, pauses of 0.3 to 46 s so some gaps take several windows, a
/// lead-in and a tail of up to 20 s; quiet words of any id in the pauses, dense
/// near the regions' edges; a quarter of the words ending in a full stop; now
/// and then a word on the recording's first or last frames.
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

/// The general layout plus quiet words of any id within 2.4 s of every owned
/// boundary and window edge of its partition under `config`, a third of them
/// orphans (a word the recording cuts off, or a lone full stop).
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

/// A token of a merge's text: its place ([`ref_place`]) and where it starts.
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

/// One differential run: the old merge's tokens, the transcript, and what its
/// windows heard ([`ref_heard`]).
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
    /// Flagged words held once a later piece of a placed word may cross chunks'
    /// owned boundaries in no decoded window, with no decoded window and no
    /// kept piece of another word between ([`RefTexts::kept_between`]): a
    /// recovery that missed a word's start gluing its rest across an undecoded
    /// gap. Counted and capped at its value on the CI seed, 0.
    across_gap: usize,
    /// Flagged words held once a piece the old merge did not keep, in an
    /// accepted recovery's owned span, may cross that span's owned start alone,
    /// which the recovery's window holds: limit E across its own window. Capped
    /// as `across_gap`.
    across_own_window: usize,
    /// As `across_own_window`, for a piece of an orphan word the old merge
    /// dropped: limit (ii) across its own window. Capped as `across_gap`.
    orphan_across_own_window: usize,
    /// Flagged words held once a piece of an orphan word the old merge dropped
    /// may lie inside the old merge's own glue after the same word: limit (ii)
    /// inside the old merge's glue. Capped as `across_gap`.
    inside_glue: usize,
    /// Flagged words held once the rest of a word the old merge kept nothing
    /// of, which an accepted recovery begins with, may lie inside the old
    /// merge's glue after the same word, across only owned boundaries a gap's
    /// span shares and a window decodes: limit E inside the old merge's glue.
    /// Capped as `across_gap`.
    rest_inside_glue: usize,
    /// `rest_inside_glue` for the old merge's text-leading orphan
    /// ([`RefTexts::lead_glued`]). Capped as `across_gap`.
    rest_inside_lead_glue: usize,
    /// The old merge's text-leading orphan, an accepted recovery's leading
    /// orphan, held once copies of pieces of its own word may come before it.
    /// Capped as `across_gap`.
    copy_before_lead: usize,
    /// Flags in `glued` an unplaced piece makes: a piece no placed word lies
    /// near, which as an extra one would hold the word. Counted.
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

/// Where `run` loses against the old merge ([`Losses`]). `unheard` also fires
/// on a word both merges drop, as a run's LCS can, so it only flags a case to
/// look at.
///
/// Identical token sequences pass, so the check never fails the old merge's
/// text against itself; a flag that a piece beyond every placed word's slack
/// makes on a differing text is counted apart ([`Losses::unplaced`]).
///
/// The boundaries: every word of the old merge's text comes out in the
/// transcript's with the same pieces at the same places, at least once, alone
/// or as the start of a longer word. A longer word passes only if each extra
/// piece is one the old merge glued after the same word, or one with no chunk's
/// owned span starting or ending between it and the word's first piece. A word
/// that grows across a chunk fails; so do two of the old merge's words joined,
/// and a full stop moved onto another word.
///
/// An orphan never counts in `unheard`. A word that repeats the word before it,
/// or its last piece, counts only where a final window heard it: the fake never
/// gives one piece twice in a row.
///
/// # The boundaries' exemptions
///
/// The table is closed: a glue shape found later becomes a named limit in
/// [`Losses`] that the gate counts and caps at its value on the CI seed. A lost
/// word or piece always fails, as does a word that grew across a piece the old
/// merge kept of another word ([`RefTexts::kept_between`]).
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

/// Whether placed word `w` follows `head` along a chain of repeats, each word
/// repeating the one before it or an orphan after one that does.
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

/// Which extra pieces [`RefTexts::holds`] allows: the boundaries' own, or also
/// those of one counted class of [`Losses`], to count a word they flag.
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
    /// strictly between the piece at `at` and the word's first piece `first`,
    /// outside its words that begin with the same placed word: growing across
    /// it would move a boundary across kept text.
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

    /// Whether `word` holds `of` in order from its first piece on, every other
    /// piece an extra one under `extras`.
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
    /// Words whose end comes out only as a recovery's orphan.
    led: usize,
    /// Words glued on across an undecoded gap ([`Losses::across_gap`]).
    across_gap: usize,
    /// Words glued on across a recovery's own window.
    across_own_window: usize,
    /// Orphan words glued on across a recovery's own window.
    orphan_across_own_window: usize,
    /// Orphan words kept inside the old merge's glue.
    inside_glue: usize,
    /// Recoveries' rests kept inside the old merge's glue.
    rest_inside_glue: usize,
    /// Recoveries' rests kept inside the old merge's leading glue.
    rest_inside_lead_glue: usize,
    /// Copies before the old merge's text-leading orphan.
    copy_before_lead: usize,
    /// Flags an unplaced piece makes ([`Losses::unplaced`]).
    unplaced: usize,
}

/// `cases` random layouts from `seed` with recovery on, under four settings and
/// the ONNX engine's, extra windows off and on, over the exact fake, one
/// `jitter` frames off and, on half the cases, one deaf at a window's edges
/// (`DEAF`). Every word the old merge kept comes out with its pieces and
/// boundaries ([`ref_losses`]); where the fake hears every frame, every word a
/// gap's window or accepted recovery heard in its reach comes out too.
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
        // Over the deaf fake a run's LCS can drop a word one window is deaf to,
        // in both merges alike, so the heard check runs only where the fake
        // hears every frame.
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

/// Every word the old merge keeps comes out, over 1,500 cases of one seed.
#[test]
fn every_word_the_old_merge_keeps_comes_out() {
    let tally = differential(0x0a16_a310_c1e5_7001, 1500, 1);
    println!("{tally:?}");
    // About 24,000 words, 800 recovered cases, 1 in 150 words doubled and 25
    // glues with this seed; each counted class is capped at its count here.
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

/// The transcript and the old merge's text for a hand-built case, over a fake
/// that decodes a loud window under 130 frames to nothing.
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

/// `texts` for the next two tests: speech at 1.4 to 13.7 s and 16.5 to 28.7 s
/// under the `CoreML` settings, quiet word 18 at 20.96 s and word 20 at 24.96
/// s, with short pauses when `pauses` is set.
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
    // The second chunk decodes to nothing and cannot be decoded again, as every
    // extension passes 15 s; the third chunk's recovery hears word 18 inside
    // the second chunk's owned span and keeps it, in its reach.
    let (new, old) = two_chunks_and_a_quiet_word(true);
    assert_eq!(new.stats.recoveries_accepted, 1);
    assert_ne!(new.windows[2], new.spans[2].window);
    assert!(!new.spans[2].owned.contains(&(262 * FRAME_SAMPLES)));
    assert_eq!((new.text().as_str(), old.as_str()), ("w18 w20", "w18 w20"));
}

#[test]
fn a_recovery_is_scored_on_its_chunks_window() {
    // Without the pauses the second chunk's recovery hears word 18 in the
    // overlap with the third chunk, inside its window and past its owned span;
    // scored on the window it is accepted and keeps the word.
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
    // Over the jittering fake with 8 s chunks the last chunk's recovery hears
    // word 12 a frame early, before its owned span; its window's frames still
    // count it.
    let regions = vec![22_400..225_599, 318_400..416_000];
    let samples = crafted(26.0, &regions, &[(15, 98, 5), (12, 255, 5)]);
    let (new, old) = texts(&samples, &regions, &random_config(3, false), 1);
    assert!(new.stats.recoveries_accepted > 0);
    assert_eq!((new.text().as_str(), old.as_str()), ("w15 w12", "w15 w12"));
}

#[test]
fn a_recovered_chunk_keeps_the_rest_of_a_word_a_gaps_window_began() {
    // The lead-in's window ends inside quiet word "w8s28" and hears "w8"; the
    // first chunk's recovery begins with "s28" and keeps it, so the word comes
    // out whole.
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
    // A 2.7 s pause no window decodes between a plain and a recovered chunk,
    // and quiet word 3 in the plain chunk's half, which only the recovery
    // hears; forward and backward.
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
    // A quiet word in a 3 s undecoded pause repeats the id of a word in the
    // dense plain chunk beside it. The recovery of the other chunk keeps only
    // its share of the pause, so the run's LCS cannot take one for the other.
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
    // A 30 s gap: of a four-piece word across the seam of two windows, the
    // earlier hears three pieces, the later four; the copy with more pieces
    // stays.
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
    // A 30 s gap, `CoreML`'s settings, every window deaf for its first (or
    // last) 30 frames. A quiet word just past the seam of the first two windows
    // is heard only by the earlier one (or the later, mirrored); the gap's
    // windows merge as a run, so it comes out: the old merge's text.
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
    // The gate's case 34 shrunk: a word of four pieces and its stop across the
    // seam of two gap windows that both hear it whole: once and whole.
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
    // A recovered chunk keeps a word whose full stop lies just past its reach,
    // and the plain chunk after it heard that word too. The recovery keeps the
    // word without the stop: the old merge's text.
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
    // A plain chunk, about 19 s of undecoded pause, and a chunk whose recovery
    // 6 s either side wins, beginning inside the pause on a continuation, a
    // full stop or a word start. The recovery keeps its chunk and the overlap,
    // so nothing from the pause glues onto word 12.
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
    // A chunk with word 11, a gap with quiet word 2, a chunk decoded to
    // nothing, and a gap whose first window begins on the rest of a word that
    // chunk missed. The first chunk kept a token before that window, so the
    // rest falls away and does not glue onto word 2 ("w2s25", "w2.").
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
    // A known limit, text only: the gap's window and a recovery both miss a
    // word's start, deaf there; the recovery's kept rest steps back into the
    // gap's window and follows word 14 a second time. A copy, never a loss.
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
    // A known limit, text only (`ref_case_1411`): the gap's window keeps ".s40
    // w18s38", and the recovery that begins at the same orphan steps back into
    // it, so ".s40" follows word 18 and word 18 comes out again. Never a loss.
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
    // A chunk that cannot be decoded again ends on a word's first piece; the
    // next chunk's recovery keeps that word whole from its start, so the rest
    // the pause's window begins with falls away and does not glue onto word 2
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

/// Case 855 of seed 0x3ec9e6e5fd29f060 in the orphan layout, cut to the words
/// around the limit: 8 s chunks, 40 s without noise, over a fake a frame off
/// and deaf for a window's last 30 frames.
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
    // A known limit at limit E's exact signature (`ref_orphan_led_case`): the
    // next chunk's recovery begins inside word 10's full stop and keeps it, so
    // the stop, its leading orphan, follows the gap's last word, a copy of word
    // 10. Never a loss; the old merge dropped the copy and kept "w10.".
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

/// A generated case cut around a limit: `total` samples, speech in `regions`,
/// loud sound in those `loud` names, `words` as [`ref_at`] reads them.
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

/// Case 1929 of seed 0xa09c6d2c7ddf7445, cut around the limit, over a fake
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
    // A named limit, text only, capped at 0 on the CI seed
    // (`ref_across_gap_case`): the last chunk's recovery misses word 13's start
    // and keeps its rest "s33.", which follows word 14 across the undecoded
    // gap. Never a loss.
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
    // Case 158 of seed 0xa09c6d2c7ddf7445: a piece more than five frames from
    // its place makes the boundaries flag the old merge's own word. Same tokens
    // pass by the sanity rule; once the texts differ the flag counts as
    // unplaced.
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
    // Text only, outside every counted class, so the gate fails it: as across
    // an undecoded gap, but across a chunk that decoded to nothing. Never seen.
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

/// Case 2406 of seed 0x6c62000000000062, cut around the limit, over a fake
/// `jitter` frames off and deaf for a window's first 30 frames.
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
    // Limit E across its own window, capped at 0 on the CI seed
    // (`ref_own_window_case`): the last chunk's recovery keeps the rest "s33"
    // of word 13, which follows word 20 across an owned end only its window
    // decodes. Never a loss.
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

/// Case 1381 of seed 0xa8d2f372fe94f88e (short regions, long pauses), cut
/// around the limit, over a fake `jitter` frames off.
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
    // Limit (ii) inside the old merge's glue, capped at 0 on the CI seed
    // (`ref_inside_glue_case`). Both recoveries keep word 9 and the orphan
    // ".s24", which the old merge dropped while it glued ".s37." onto word 9,
    // so the dropped orphan comes out inside that glue. Never a loss.
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

/// Case 742 of seed 0x6f04000000000004 (short regions, long pauses), cut around
/// the limit, over a fake one frame off.
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
    // Limit (ii) across its own window, capped at 0 on the CI seed
    // (`ref_orphan_own_window_case`): the recovery keeps word 9 and the orphan
    // "s28", both dropped by the old merge, glued across an owned start its
    // window decodes. Never a loss.
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

/// Case 857 of seed 0x18ab79b97f4a7c89 (short regions, long pauses), cut around
/// the limit, over a fake `jitter` frames off and deaf for a window's first 45 frames.
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
    // Limit E inside the old merge's glue, capped at 0 on the CI seed
    // (`ref_rest_inside_glue_case`). The old merge glued ".s36" onto word 14
    // across the gap's owned start; the last chunk's recovery misses the start
    // of word 12 and begins at its rest "s32", inside that glue. Never a loss.
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

/// Case 136 of seed 0xab72f372fe94f82e (short regions, long pauses), cut around
/// the limit, over a fake `jitter` frames off and deaf for a window's first 45 frames.
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
    // Limit E inside the old merge's leading glue, capped at 0 on the CI seed
    // (`ref_rest_inside_lead_glue_case`): the recovery misses word 3's start
    // and begins at its rest "s23", inside the glue of two leading orphans.
    // Never a loss.
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

/// Case 499 of seed 0xd3b79b97f4a7c19 (short regions, long pauses), cut around
/// the limit, over a fake `jitter` frames off and deaf for a window's first 45 frames.
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
    // A copy before the text-leading orphan, capped at 0 on the CI seed
    // (`ref_copy_before_lead_case`): the gap's window heard the recovery's
    // leading "s33" too, and its copy, no word start, comes out first. Never a
    // loss.
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

/// The tokens of placed words' pieces `(word, piece)`, each at its start.
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

/// Whether the boundary check fails `run` with the merges' tokens planted as
/// placed words' pieces.
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
    // The check itself, on planted texts over real partitions.
    let check = ref_planted_fails;
    // Word 11 in a chunk, word 2 in the gap, a chunk decoded to nothing, an
    // orphan (".s25") and word 3; the old merge drops the orphan or glues its
    // stop onto word 2.
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
    // The rest glued onto word 2 across the empty chunk.
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

    // The rest of word 5, kept whole by the old merge, glued onto word 2 across
    // the empty chunk; and a copy of its last piece onto word 3.
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

    // A full stop moved onto the next word inside one gap's window, far from
    // any chunk: the word that lost it fails.
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

    // Limit E: its two layouts pass as they come out.
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
    // Planted glues next to the counted classes, which no class takes.
    let check = ref_planted_fails;
    // A chunk with word 5, then a gap's window with word 10, the orphan "." and
    // word 14; the old merge glued the stop onto word 5.
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
    // The rest of word 10, dropped by the old merge, inside its glue after word
    // 5, across a boundary a plain gap window decodes.
    let rest = [(0, 0), (0, 1), (1, 1), (2, 0), (3, 0)];
    assert!(check(&case, &mut run, &main, &rest));

    // In the layout across a chunk that heard nothing: an orphan the old merge
    // dropped, glued onto word 14 across the undecoded gap.
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
    // An orphan the old merge dropped, glued onto word 20 across the recovered
    // chunk's owned start: limit (ii) across its own window counts it, limit E
    // does not.
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
    // The limit's own text, with windows planted so that the third chunk's
    // window, not the recovery's, holds its owned start.
    let main = [(0, 0), (2, 0), (2, 1), (3, 0), (3, 1)];
    let glued = [(0, 0), (1, 1), (2, 0), (2, 1), (3, 0), (3, 1)];
    let windows = run.new.windows.clone();
    run.new.windows[3].start = 1_270_000;
    run.new.windows[2].end = 1_266_000;
    assert!(check(&case, &mut run, &main, &glued));
    // And word 13's rest glued onto word 7, inside the third chunk's planted
    // window and across its owned span.
    run.new.windows = windows;
    run.new.windows[2].start = 900_000;
    case.words.push(ref_placed(vec![7], 57.12, 4));
    let main = [(4, 0), (0, 0), (3, 0), (3, 1)];
    let glued = [(4, 0), (1, 1), (0, 0), (3, 0), (3, 1)];
    assert!(check(&case, &mut run, &main, &glued));
    // The rest of a dropped word glued across a plain chunk's owned start, with
    // 8 s chunks over one long region.
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
    // Each counted class as it comes out, then across a word the old merge kept
    // on its own between the two pieces, which fails.
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
    // Limit E there across text the old merge kept: a word 7 it kept at 72 s,
    // between word 20 and the rest of word 13 glued onto it.
    case.words.push(ref_placed(vec![7], 72.0, 4));
    let main = [(0, 0), (4, 0), (2, 0), (2, 1), (3, 0), (3, 1)];
    let over = [(0, 0), (1, 1), (4, 0), (2, 0), (2, 1), (3, 0), (3, 1)];
    assert!(check(&case, &mut run, &main, &over));
    // And limit (ii) there: the orphan glued onto word 20 past word 7.
    let main = [(0, 0), (4, 0), (3, 0), (3, 1)];
    let over = [(0, 0), (2, 0), (2, 1), (4, 0), (3, 0), (3, 1)];
    assert!(check(&case, &mut run, &main, &over));

    // Limit E inside the old merge's glue, then across a word 7 at 35.04 s it
    // kept on its own, between word 14 and the rest of word 12.
    let (mut case, mut run) = ref_rest_inside_glue_case(0);
    let after = [(2, 0), (2, 1), (3, 0), (3, 1), (3, 2)];
    let main = [&[(0, 0), (0, 1)], &after[..]].concat();
    let rest = [&[(0, 0), (0, 1), (1, 1)], &after[..]].concat();
    assert!(!check(&case, &mut run, &main, &rest));
    case.words.push(ref_placed(vec![7], 35.04, 2));
    let main = [&main[..], &[(4, 0)]].concat();
    let over = [&rest[..], &[(4, 0)]].concat();
    assert!(check(&case, &mut run, &main, &over));
    // Limit E inside the leading glue, then across a word 7 at 10.4 s it kept
    // on its own, between the leading orphan and the rest of word 3.
    let (mut case, mut run) = ref_rest_inside_lead_glue_case(0);
    let after = [(3, 0), (3, 1), (3, 3)];
    let main = [&[(0, 0), (2, 0)], &after[..]].concat();
    let rest = [&[(0, 0), (1, 1), (2, 0)], &after[..]].concat();
    assert!(!check(&case, &mut run, &main, &rest));
    case.words.push(ref_placed(vec![7], 10.4, 2));
    let main = [&main[..], &[(4, 0)]].concat();
    let over = [&rest[..], &[(4, 0)]].concat();
    assert!(check(&case, &mut run, &main, &over));
    // A copy before the text-leading orphan, then across a word 7 at 13.45 s it
    // kept on its own, between the orphan and the copy.
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
    // Planted glues next to three more counted classes, which no class takes.
    let check = ref_planted_fails;
    // An orphan glued onto word 20 across the recovered chunk's owned
    // start, of a word the old merge kept a piece of elsewhere.
    let (case, mut run) = ref_own_window_case(1);
    let main = [(0, 0), (3, 0), (3, 1), (2, 1)];
    let orphan = [(0, 0), (2, 0), (3, 0), (3, 1), (2, 1)];
    assert!(check(&case, &mut run, &main, &orphan));
    // The rest of word 12 glued onto word 14 across the gap's owned start, of a
    // word kept elsewhere, outside the old merge's glue after word 14.
    let (case, mut run) = ref_rest_inside_glue_case(0);
    let after = [(2, 0), (2, 1), (3, 0), (3, 1), (3, 2)];
    let main = [&[(0, 0), (0, 1)], &after[..], &[(1, 1)]].concat();
    let rest = [&[(0, 0), (0, 1), (1, 1)], &after[..], &[(1, 1)]].concat();
    assert!(check(&case, &mut run, &main, &rest));
    let main = [(0, 0), (0, 1), (3, 0), (3, 1), (3, 2)];
    let rest = [(0, 0), (0, 1), (1, 1), (3, 0), (3, 1), (3, 2)];
    assert!(check(&case, &mut run, &main, &rest));
    // The copy before the leading orphan with no recovery beginning at it; and
    // an orphan kept after word 10, outside the orphan's word, put before it.
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
    // A decoded window that begins at a word's first piece lies between it and
    // an extra piece, as does a chunk's owned start an extra piece lies on.
    let check = ref_planted_fails;
    // The limit across an undecoded gap, with a decoded window planted from
    // word 14's first piece on, wholly between it and the glued rest.
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
