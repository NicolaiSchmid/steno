//! The differential test: the partition's merge against a frozen copy of
//! the merge before it (`chunker::gap_windows`, `splice_gaps`, `Seam::fit`,
//! `keep_chunk_and_overlap` and the recovery that scored on the chunk's
//! frames, as of 2fbab03dd). The copy is not reused code, so the partition
//! can change under it; it gives the same tokens (id and sample) as that
//! commit on the jitter-free cases. It checks clause 5 of the ownership
//! invariant (A16a in `.plans/2026-10-07-stable-promotion.md`): every word
//! the old merge keeps, this one keeps.

use super::*;

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
    if !config.decode_gaps {
        return Vec::new();
    }
    let total = audio.len();
    let max = sample_count(config.max_seconds).max(1);
    let overlap = sample_count(config.overlap_seconds).min(max - 1);
    let shortest = sample_count(crate::chunker::MIN_GAP_SECONDS);
    let mut windows = Vec::new();
    let mut covered = 0;
    for next in chunks
        .iter()
        .map(|c| c.range.clone())
        .chain(std::iter::once(total..total))
    {
        if next.start >= covered + shortest {
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

/// Every token the final windows heard that the span keeps by its
/// stated bound, taken from the window the transcript records
/// ([`Transcript::windows`]) and decoded again: a plain chunk's whole
/// window, an accepted recovery's reach, a gap window's owned span and
/// [`SEAM_SAMPLES`] either side.
fn ref_heard(
    t: &mut Transcriber<FrameTokenBackend>,
    samples: &[f32],
    tr: &Transcript,
) -> Vec<Token> {
    let mut stats = DecodeStats::default();
    let mut heard = Vec::new();
    for (span, window) in tr.spans.iter().zip(&tr.windows) {
        let bound = if !span.chunk {
            span.owned.start.saturating_sub(SEAM_SAMPLES)..span.owned.end + SEAM_SAMPLES
        } else if *window != span.window {
            span.reach.clone()
        } else {
            span.window.clone()
        };
        let tokens = t.decode_range(samples, window.clone(), &mut stats).unwrap();
        heard.extend(tokens.into_iter().filter(|t| bound.contains(&t.sample())));
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

/// A differential case: the background (without the words), the
/// words, and fixed speech regions or the detector's.
struct RefCase {
    base: Vec<f32>,
    words: Vec<Placed>,
    regions: Option<Vec<Range<usize>>>,
}

impl RefCase {
    fn samples(&self, keep: &[bool]) -> Vec<f32> {
        let mut samples = self.base.clone();
        for (word, &k) in self.words.iter().zip(keep) {
            if k {
                word.put(&mut samples);
            }
        }
        samples
    }

    /// Takes the words out of finished samples, back to the background.
    fn from_samples(
        mut samples: Vec<f32>,
        words: Vec<Placed>,
        regions: Option<Vec<Range<usize>>>,
        noisy: bool,
    ) -> Self {
        for word in &words {
            for i in word.span() {
                samples[i] = if !noisy {
                    0.0
                } else if i % 2 == 0 {
                    3e-5
                } else {
                    -3e-5
                };
            }
        }
        RefCase {
            base: samples,
            words,
            regions,
        }
    }
}

/// The old merge's tests' random layout: the 20 words, loud and quiet, one or two
/// pieces, a quarter said twice in a row, some after loud sound the
/// fake decodes to nothing.
fn ref_random_layout(rng: &mut Rng, case: usize) -> RefCase {
    let total = (20 + rng.below(50)) * SAMPLE_RATE;
    let noisy = case.is_multiple_of(2);
    let mut samples = vec![0.0f32; total];
    if noisy {
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
    RefCase::from_samples(samples, words, None, noisy)
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
    let noisy = rng.below(2) == 0;
    if noisy {
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
    RefCase::from_samples(samples, words, Some(regions), noisy)
}

/// Per word, how often its first piece came out and which pieces did;
/// each token goes to the nearest piece of that id within `slack`
/// frames.
fn ref_tally(tokens: &[Token], words: &[Placed], keep: &[bool], slack: usize) -> Vec<(usize, u8)> {
    let mut per = vec![(0, 0u8); words.len()];
    for token in tokens {
        let near = words
            .iter()
            .enumerate()
            .filter(|&(w, _)| keep[w])
            .flat_map(|(w, word)| (0..word.pieces.len()).map(move |i| (w, i, word)))
            .filter(|&(_, i, word)| word.piece_of(token, slack) == Some(i))
            .min_by_key(|&(_, i, word)| word.start(i).abs_diff(token.sample()));
        if let Some((w, i, _)) = near {
            per[w].0 += usize::from(i == 0);
            per[w].1 |= 1 << i;
        }
    }
    per
}

/// One differential run: the old merge's tokens, the transcript and what
/// its final windows heard.
struct RefRun {
    old: Vec<Token>,
    new: Transcript,
    heard: Vec<Token>,
}

fn ref_run(
    case: &RefCase,
    keep: &[bool],
    config: &PipelineConfig,
    min_frames: usize,
    jitter: bool,
) -> RefRun {
    let samples = case.samples(keep);
    let make = || {
        let mut t = quiet_transcriber(config.clone(), min_frames);
        t.workers[0].jitter = jitter;
        if let Some(regions) = &case.regions {
            t.vad = Box::new(FixedVad(regions.clone()));
        }
        t
    };
    let old = ref_transcribe(&mut make(), &samples);
    let mut t = make();
    let new = t.transcribe(&samples, None).unwrap();
    let heard = ref_heard(&mut t, &samples, &new);
    RefRun { old, new, heard }
}

/// Words the old merge keeps and the transcript loses (`.0`), pieces
/// likewise (`.1`), and words its final windows heard that it loses (`.2`).
fn ref_losses(
    case: &RefCase,
    keep: &[bool],
    run: &RefRun,
    slack: usize,
) -> (Vec<usize>, Vec<usize>, Vec<usize>) {
    let old = ref_tally(&run.old, &case.words, keep, slack);
    let new = ref_tally(&run.new.tokens, &case.words, keep, slack);
    let heard = ref_tally(&run.heard, &case.words, keep, slack);
    let mut out = (Vec::new(), Vec::new(), Vec::new());
    for w in (0..case.words.len()).filter(|&w| keep[w]) {
        if old[w].0 > 0 && new[w].0 == 0 {
            out.0.push(w);
        } else if old[w].1 & !new[w].1 != 0 {
            out.1.push(w);
        }
        if heard[w].0 > 0 && new[w].0 == 0 {
            out.2.push(w);
        }
    }
    out
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
}

/// `cases` random layouts from `seed`, with recovery on: the first
/// generator's words of one to three pieces, the old merge's random
/// layout, and its two-sided layout around a gap with recoveries reaching
/// into it, under the default settings, `CoreML`'s and two between, with
/// the extra windows off and on, over the exact fake and a jittering one.
/// Every word the old merge kept comes out, with every piece the old merge
/// gave it: a duplicate is allowed, a loss never. And every word a final
/// window heard inside the bound its span keeps comes out at least once,
/// except the second of a word said twice in a row, which a run's LCS may
/// take for one.
fn differential(seed: u64, cases: usize) -> Tally {
    let mut rng = Rng(seed);
    let mut tally = Tally::default();
    for case in 0..cases {
        let layout = match case % 3 {
            0 => {
                let (samples, words) = placed_words(&mut rng, case);
                RefCase::from_samples(samples, words, None, case.is_multiple_of(2))
            }
            1 => ref_random_layout(&mut rng, case),
            _ => ref_two_sided_layout(&mut rng),
        };
        let jitter = (case / 3) % 3 == 1;
        let config = random_config(case / 3, (case / 12) % 4 != 0);
        let min_frames = if case % 3 == 0 && (case / 3) % 2 == 0 {
            0
        } else {
            130
        };
        let slack = 2 + usize::from(jitter);
        let all = vec![true; layout.words.len()];
        let run = ref_run(&layout, &all, &config, min_frames, jitter);
        tally.words += all.len();
        tally.recovered += usize::from(run.new.stats.recoveries_accepted > 0);
        let old = ref_tally(&run.old, &layout.words, &all, slack);
        let new = ref_tally(&run.new.tokens, &layout.words, &all, slack);
        tally.doubled += old.iter().zip(&new).filter(|(m, a)| a.0 > m.0).count();
        let (lost, pieces, unheard) = ref_losses(&layout, &all, &run, slack);
        let repeated = |w: usize| w > 0 && layout.words[w - 1].pieces == layout.words[w].pieces;
        let unheard: Vec<usize> = unheard.into_iter().filter(|&w| !repeated(w)).collect();
        let show = |tokens: &[Token]| {
            let pieces = tokens.iter().map(|t| format!("{}@{}", t.id, t.sample()));
            pieces.collect::<Vec<_>>().join(" ")
        };
        let words = |list: &[usize]| {
            list.iter()
                .map(|&w| (&layout.words[w].pieces, layout.words[w].start(0)))
                .collect::<Vec<_>>()
        };
        assert!(
            lost.is_empty() && pieces.is_empty() && unheard.is_empty(),
            "seed {seed:#x} case {case}: lost {:?}, pieces lost {:?}, heard and lost {:?}\n spans {:?}\n windows {:?}\n old {}\n new {}\n heard {}",
            words(&lost),
            words(&pieces),
            words(&unheard),
            run.new.spans,
            run.new.windows,
            show(&run.old),
            show(&run.new.tokens),
            show(&run.heard)
        );
    }
    tally
}

/// Clause 5 of the ownership invariant over 900 cases of one seed;
/// `every_word_the_old_merge_keeps_comes_out_over_more_seeds` runs eight
/// more.
#[test]
fn every_word_the_old_merge_keeps_comes_out() {
    let tally = differential(0x0a16_a310_c1e5_7001, 900);
    println!("{tally:?}");
    // Words checked (about 10,000 with this seed), cases with an accepted
    // recovery (about 300), and words that came out more often than from
    // the old merge (about 1 in 100).
    assert!(
        tally.words > 9_000 && tally.recovered > 250 && tally.doubled * 50 < tally.words,
        "{tally:?}"
    );
}

#[test]
#[ignore = "about 3 minutes in a debug build; run it after a change to the partition or the merge"]
fn every_word_the_old_merge_keeps_comes_out_over_more_seeds() {
    for seed in 1..=8u64 {
        let tally = differential(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15), 900);
        println!("seed {seed}: {tally:?}");
    }
}

/// The transcript and the old merge's text for a hand-built case: `samples` with
/// speech in `regions`, under `config`, over a fake that decodes a window
/// under 130 frames that holds loud sound to nothing.
fn texts(
    samples: &[f32],
    regions: &[Range<usize>],
    config: &PipelineConfig,
    jitter: bool,
) -> (Transcript, String) {
    let make = || {
        let mut t = quiet_transcriber(config.clone(), 130);
        t.workers[0].jitter = jitter;
        t.vad = Box::new(FixedVad(regions.to_vec()));
        t
    };
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
    texts(&samples, &regions, &random_config(1, true), false)
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
    let (new, old) = texts(&samples, &regions, &random_config(3, false), true);
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
    let (new, old) = texts(&samples, &regions, &random_config(1, true), false);
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
        let (new, old) = texts(&samples, &regions, &PipelineConfig::default(), false);
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
