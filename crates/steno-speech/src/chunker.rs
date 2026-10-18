//! The pause-aligned chunk layout (decision 1 of the speech-stack plan):
//! chunks aim at `target_seconds` and cut at the longest VAD pause inside
//! the search window either side of the target, else at the quietest
//! 100 ms frame there. A long pause that starts before the window's far
//! edge, or within `min_chunk_seconds` past it, ends the chunk early, and
//! no chunk decodes it. `overlap_seconds` of audio is shared with the next
//! chunk for the merge. No chunk exceeds `max_seconds`: for ONNX a memory
//! clamp (attention grows with the square of the window: 13 GB at 600 s,
//! spike E), not the position table's 800 s cap; for `CoreML` the models'
//! fixed 15 s window.
//!
//! A gap is a stretch of the recording no chunk covers: the lead-in before
//! the first chunk, a long pause between two chunks, the rest of a pause a
//! chunk's start moves past, and the tail after the last chunk (or the
//! whole recording, when there is no chunk). [`ChunkerConfig::decode_gaps`]
//! says which gaps of [`MIN_GAP_SECONDS`] or more are covered with extra
//! windows: none, the tail, or every one; the chunks stay where [`layout`]
//! put them. With [`ChunkerConfig::from_start`] the first chunk starts at
//! the recording's start, so the lead-in is decoded with the speech after
//! it; a lead-in longer than the first chunk is decoded only as far as that
//! chunk reaches.
//!
//! [`partition`] splits the recording once into the [`Span`]s that own it,
//! one per chunk and one per extra window, and gives each the bound its
//! decode keeps words from ([`Span::reach`]).
//!
//! The search and the cuts are ported from
//! `spikes/onnx-speech/src/chunker.rs`; the long-pause rule is not. The
//! spike laid out the tail before looking for long pauses and ignored those
//! within `min_chunk_seconds` of a chunk's start, so it decoded some of
//! them, and the spike D numbers ran that way. The rule here skips every
//! one, as the spike's doc and decision 1 say, and looks
//! `min_chunk_seconds` past the window's far edge, where the spike looked
//! up to it.
//! Swift: none; `FluidAudio`'s `ChunkProcessor` cuts at fixed 15 s strides.

use std::ops::Range;

use crate::backend::{FRAME_SAMPLES, SAMPLE_RATE, sample_count};

/// How a chunk's end was chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cut {
    /// The middle of the longest pause in the search window.
    Pause,
    /// `pad_seconds` into a pause of `long_pause_seconds` or more, or at the
    /// clamp or the end of the audio when that comes first.
    LongPause,
    /// The quietest 100 ms frame in the search window; no pause there.
    Energy,
    /// The end of speech; always the last chunk.
    Tail,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    /// Sample range into the recording.
    pub range: Range<usize>,
    pub cut: Cut,
}

impl Chunk {
    #[must_use]
    pub fn seconds(&self) -> f64 {
        self.range.len() as f64 / SAMPLE_RATE as f64
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ChunkerConfig {
    pub target_seconds: f32,
    /// Either side of the target where the cut may fall. The window never
    /// reaches past `max_seconds`: a target closer to the clamp than this
    /// moves back.
    pub search_seconds: f32,
    /// Shared with the next chunk; the plan says 1 to 2 s.
    pub overlap_seconds: f32,
    /// A pause this long ends the chunk and is not decoded, unless
    /// `decode_gaps` decodes every gap.
    pub long_pause_seconds: f32,
    /// The longest chunk or window: the memory clamp, or the models' fixed
    /// 15 s window (`CoreML`).
    pub max_seconds: f32,
    /// Silence kept around speech at a chunk's edges.
    pub pad_seconds: f32,
    /// No cut inside speech falls closer than this to the chunk's start; a
    /// long pause or the end of speech may end a chunk sooner.
    pub min_chunk_seconds: f32,
    /// Which gaps of [`MIN_GAP_SECONDS`] or more are also decoded, in extra
    /// windows ([`partition`]). None by default.
    pub decode_gaps: Gaps,
    /// The first chunk starts at the recording's start, not `pad_seconds`
    /// before the first speech, so the lead-in is decoded with the speech
    /// after it, as far as that chunk reaches. Off by default.
    pub from_start: bool,
}

/// The gaps the extra windows decode ([`ChunkerConfig::decode_gaps`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gaps {
    /// No gap: the audio the chunks leave out is not decoded.
    None,
    /// The tail after the last chunk, the audio after the last speech;
    /// without a chunk, nothing.
    Tail,
    /// Every gap: the lead-in, the long pauses, the rest of a pause a
    /// chunk's start moves past, and the tail.
    All,
}

/// The defaults are the spike D harness values
/// (`spikes/onnx-speech/src/main.rs`: target 25 s, search 4 s, long pause
/// 3 s, pad 0.25 s); `max_seconds` 60 is decision 1 (the harness ran with
/// a 190 s clamp); `overlap_seconds` 2 is `FluidAudio`'s, within decision
/// 1's 1 to 2 s, and scored better than 1.5 s on FLEURS German on both
/// backends (A2 in `.plans/2026-10-07-stable-promotion.md`);
/// `min_chunk_seconds` is new here and keeps a cut in speech from landing
/// right after a chunk's start; no gap is decoded, and the first chunk
/// starts just before the first speech.
impl Default for ChunkerConfig {
    fn default() -> Self {
        ChunkerConfig {
            target_seconds: 25.0,
            search_seconds: 4.0,
            overlap_seconds: 2.0,
            long_pause_seconds: 3.0,
            max_seconds: 60.0,
            pad_seconds: 0.25,
            min_chunk_seconds: 2.0,
            decode_gaps: Gaps::None,
            from_start: false,
        }
    }
}

/// Pauses between speech regions, including the leading and the trailing
/// silence. `speech` is sorted and non-overlapping.
#[must_use]
pub fn pauses(speech: &[Range<usize>], total: usize) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut cursor = 0;
    for region in speech {
        if region.start > cursor {
            out.push(cursor..region.start);
        }
        cursor = cursor.max(region.end);
    }
    if total > cursor {
        out.push(cursor..total);
    }
    out
}

/// The centre of the quietest 100 ms frame inside `lo..hi`, or the middle
/// when no whole frame fits.
#[must_use]
pub fn quietest_point(audio: &[f32], lo: usize, hi: usize) -> usize {
    let frame = SAMPLE_RATE / 10;
    let hi = hi.min(audio.len());
    let mut best = (f32::MAX, lo.midpoint(hi));
    let mut p = lo;
    while p + frame <= hi {
        let energy: f32 = audio[p..p + frame].iter().map(|x| x * x).sum();
        if energy < best.0 {
            best = (energy, p + frame / 2);
        }
        p += frame;
    }
    best.1
}

/// Lays `audio` out in chunks over the VAD `speech` regions.
#[must_use]
pub fn layout(audio: &[f32], speech: &[Range<usize>], config: &ChunkerConfig) -> Vec<Chunk> {
    let total = audio.len();
    let mut chunks = Vec::new();
    let Some(last_speech) = speech.last() else {
        return chunks;
    };
    let pauses = pauses(speech, total);
    let target = sample_count(config.target_seconds);
    let max = sample_count(config.max_seconds).max(1);
    let search = sample_count(config.search_seconds).min(max);
    let overlap = sample_count(config.overlap_seconds);
    let long_pause = sample_count(config.long_pause_seconds);
    let pad = sample_count(config.pad_seconds);
    let min_chunk = sample_count(config.min_chunk_seconds).clamp(1, max);
    let speech_end = last_speech.end.min(total);

    let mut start = if config.from_start {
        0
    } else {
        speech[0].start.saturating_sub(pad)
    };
    // Where the last chunk ended; every cut falls after it, so every chunk
    // adds audio even when the search window reaches back into the overlap.
    let mut previous_end = 0;
    loop {
        // A start inside a pause snaps forward to just before the next
        // speech, except the first chunk's with `from_start`.
        let snaps = !(config.from_start && chunks.is_empty());
        if snaps && let Some(pause) = pauses.iter().find(|p| p.start <= start && start < p.end) {
            if pause.end >= total {
                break;
            }
            start = start.max(pause.end.saturating_sub(pad));
        }
        if start >= speech_end {
            break;
        }
        // The search window never reaches past the clamp: a target too
        // close to it moves back.
        let want = start + target.min(max - search);
        let reach = want + search;
        // Where a cut may fall: never closer than `min_chunk` to the start,
        // and always after the previous chunk's end.
        let lo = want
            .saturating_sub(search)
            .max(start + min_chunk)
            .max(previous_end + 1);
        let hi = reach.max(lo + 1);
        // A long pause that starts before the cut window closes, or within
        // `min_chunk` past it, ends the chunk early, tail or not: a cut
        // before it would leave a sliver of a chunk up to the pause, and no
        // overlap is needed across silence.
        let far = (reach + min_chunk).max(hi).min(start + max);
        if let Some(pause) = pauses
            .iter()
            .find(|p| p.start > start && p.start < far.min(speech_end) && p.len() >= long_pause)
        {
            let end = (pause.start + pad).min(start + max).min(total);
            chunks.push(Chunk {
                range: start..end,
                cut: Cut::LongPause,
            });
            start = pause.end.saturating_sub(pad).max(end);
            previous_end = end;
            continue;
        }
        // Tail: the remaining speech fits in one chunk.
        if reach >= speech_end {
            chunks.push(Chunk {
                range: start..(speech_end + pad).min(total).min(start + max),
                cut: Cut::Tail,
            });
            break;
        }
        // The longest pause, clipped to the window, wins; the first on ties.
        let mut best: Option<Range<usize>> = None;
        for pause in &pauses {
            let clipped = pause.start.max(lo)..pause.end.min(hi);
            if clipped.end > clipped.start && best.as_ref().is_none_or(|b| clipped.len() > b.len())
            {
                best = Some(clipped);
            }
        }
        let (end, cut) = match best {
            Some(pause) => (pause.start.midpoint(pause.end), Cut::Pause),
            None => (quietest_point(audio, lo, hi), Cut::Energy),
        };
        let end = end.clamp(start + min_chunk, start + max).min(total);
        chunks.push(Chunk {
            range: start..end,
            cut,
        });
        // The next chunk starts `overlap` before this one's end and at least
        // `min_chunk` (at least one sample) after this one's start, so an
        // overlap longer than the chunk still makes linear progress.
        start = end.saturating_sub(overlap).max(start + min_chunk);
        previous_end = end;
    }
    chunks
}

/// Gaps shorter than this are not decoded: on FLEURS German `cat/` the
/// extra windows over such gaps (0.25 to 2.3 s) inserted words, eight, all
/// wrong, and recovered none, and on the calibration corpus they inserted
/// words the Swift transcripts do not have (A2 in
/// `.plans/2026-10-07-stable-promotion.md`).
pub const MIN_GAP_SECONDS: f32 = 2.5;

/// One stretch of the recording and the decode that owns it
/// ([`partition`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    /// The share of the recording this span owns. The owned ranges of a
    /// partition are sorted, disjoint and cover the recording.
    pub owned: Range<usize>,
    /// The samples decoded: a chunk of the [`layout`], or a window over a
    /// gap. It holds `owned`, except the share of an undecoded gap the span
    /// owns, and is never longer than `max_seconds`.
    pub window: Range<usize>,
    /// The samples whose words this span's decode keeps. For a gap's
    /// window, the window: it keeps its whole decode. For a chunk, the
    /// range the merge before the partition kept from an accepted
    /// recovery: the window and `overlap_seconds` either side, rounded out
    /// to whole encoder frames and clamped to the recording; a word with a
    /// piece in it is kept from its start to the punctuation after its
    /// last piece in it. A chunk's plain decode goes whole to its run's
    /// merge, and reads no reach.
    pub reach: Range<usize>,
    /// A chunk of the layout, which is decoded again with more audio around
    /// it when it looks empty; a gap's window is decoded as it is.
    pub chunk: bool,
}

/// The recording split once into the spans that own it, in order: one per
/// chunk of the [`layout`] and one per extra window over a gap that
/// [`ChunkerConfig::decode_gaps`] decodes. Two neighbouring windows share their
/// owned boundary at the middle of the audio between their edges: the
/// middle of their overlap, the edge a chunk and a gap's window share, or
/// the middle of an undecoded gap, whose halves the windows either side
/// own. The first span owns from the recording's start and the last to its
/// end; without a window there is no span.
///
/// The extra windows (see the module doc) cover each gap of at least
/// [`MIN_GAP_SECONDS`] that `decode_gaps` names and stay inside it:
/// no window reaches into a chunk.
/// A gap longer than `max_seconds` is split into windows of equal length
/// that share `overlap_seconds` with each other. A shorter gap is not
/// decoded, even where it is the uncovered edge of a longer pause. Nor is a
/// window of digital silence, where every sample is exactly zero, which
/// holds no sound to decode; on the calibration corpus two windows in three
/// were. No threshold applies: a window with any sample that is not zero
/// is decoded. The capture writes its lanes as 16-bit samples, which rounds
/// anything under half a step (about -96 dBFS) to zero, so a window of a
/// recorded lane is digital silence only when nothing in it reaches that
/// floor, and the decoder would get the same zeros without the skip.
#[must_use]
pub fn partition(audio: &[f32], speech: &[Range<usize>], config: &ChunkerConfig) -> Vec<Span> {
    spans(audio, &layout(audio, speech, config), config)
}

/// [`partition`] over the chunks of a layout.
fn spans(audio: &[f32], chunks: &[Chunk], config: &ChunkerConfig) -> Vec<Span> {
    let total = audio.len();
    let max = sample_count(config.max_seconds).max(1);
    let overlap = sample_count(config.overlap_seconds).min(max - 1);
    let shortest = sample_count(MIN_GAP_SECONDS);
    let mut windows = Vec::new();
    let mut covered = 0;
    // `None` closes the tail.
    for chunk in chunks.iter().map(Some).chain([None]) {
        let next = chunk.map_or(total, |c| c.range.start);
        let decoded = match config.decode_gaps {
            Gaps::None => false,
            Gaps::Tail => chunk.is_none() && !chunks.is_empty(),
            Gaps::All => true,
        };
        if decoded && next >= covered + shortest {
            let gap = split(covered..next, max, overlap);
            let sound = gap.filter(|window| audio[window.clone()].iter().any(|&x| x != 0.0));
            windows.extend(sound.map(|window| (window, false)));
        }
        if let Some(chunk) = chunk {
            windows.push((chunk.range.clone(), true));
            covered = covered.max(chunk.range.end);
        }
    }
    // The overlap as the merge reads it, unclamped.
    let around = sample_count(config.overlap_seconds);
    let mut start = 0;
    let mut spans = Vec::with_capacity(windows.len());
    for (i, (window, chunk)) in windows.iter().enumerate() {
        let next = windows.get(i + 1).map(|(next, _)| next.start);
        let end = next.map_or(total, |next| window.end.midpoint(next));
        let reach = if *chunk {
            let from = window.start.saturating_sub(around) / FRAME_SAMPLES * FRAME_SAMPLES;
            let to = ((window.end + around) / FRAME_SAMPLES + 1) * FRAME_SAMPLES;
            from..to.min(total)
        } else {
            window.clone()
        };
        spans.push(Span {
            owned: start..end,
            window: window.clone(),
            reach,
            chunk: *chunk,
        });
        start = end;
    }
    spans
}

/// Splits `range` into the fewest windows of at most `max` samples that
/// share `overlap` with the next, all of one length (to a sample).
fn split(range: Range<usize>, max: usize, overlap: usize) -> impl Iterator<Item = Range<usize>> {
    let stride = range.len().saturating_sub(overlap);
    let count = stride.div_ceil(max - overlap).max(1);
    (0..count).map(move |i| {
        let start = range.start + i * stride / count;
        let end = range.start + (i + 1) * stride / count + overlap;
        start..end.min(range.end)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(seconds: f32) -> usize {
        sample_count(seconds)
    }

    /// Audio that is loud inside `speech` and silent elsewhere.
    fn audio(total_seconds: f32, speech: &[Range<usize>]) -> Vec<f32> {
        let mut out = vec![0.0f32; s(total_seconds)];
        for region in speech {
            for (i, x) in out[region.clone()].iter_mut().enumerate() {
                *x = if i % 2 == 0 { 0.5 } else { -0.5 };
            }
        }
        out
    }

    /// Every chunk starts and ends after the one before it.
    fn ends_advance(chunks: &[Chunk]) -> bool {
        chunks
            .windows(2)
            .all(|w| w[1].range.start > w[0].range.start && w[1].range.end > w[0].range.end)
    }

    fn covers(chunks: &[Chunk], speech: &[Range<usize>]) -> bool {
        speech.iter().all(|region| {
            (region.start..region.end)
                .step_by(SAMPLE_RATE / 100)
                .all(|p| chunks.iter().any(|c| c.range.contains(&p)))
        })
    }

    #[test]
    fn no_speech_gives_no_chunks_and_pauses_frame_the_speech() {
        assert!(layout(&[0.0; 16_000], &[], &ChunkerConfig::default()).is_empty());
        let framed = pauses(&[s(1.0)..s(2.0), s(3.0)..s(4.0)], s(5.0));
        assert_eq!(framed, vec![0..s(1.0), s(2.0)..s(3.0), s(4.0)..s(5.0)]);
        assert_eq!(pauses(&[0..s(5.0)], s(5.0)), Vec::<Range<usize>>::new());
    }

    #[test]
    fn short_speech_is_one_padded_tail_chunk() {
        let speech = [s(1.0)..s(6.0)];
        let chunks = layout(&audio(10.0, &speech), &speech, &ChunkerConfig::default());
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].cut, Cut::Tail);
        assert_eq!(chunks[0].range, s(0.75)..s(6.25));
    }

    #[test]
    fn a_pause_near_the_target_is_the_cut_and_the_next_chunk_overlaps() {
        // Speech 0 to 24 s, pause 24 to 25 s, speech 25 to 50 s.
        let speech = [0..s(24.0), s(25.0)..s(50.0)];
        let chunks = layout(&audio(52.0, &speech), &speech, &ChunkerConfig::default());
        assert_eq!(chunks[0].cut, Cut::Pause);
        assert_eq!(chunks[0].range.end, s(24.5));
        assert_eq!(chunks[1].range.start, s(24.5) - s(2.0));
        assert_eq!(chunks.last().unwrap().cut, Cut::Tail);
        assert!(covers(&chunks, &speech));
    }

    #[test]
    fn a_long_pause_ends_the_chunk_early_without_overlap() {
        // Speech 0 to 10 s, silence 10 to 20 s, speech 20 to 30 s.
        let speech = [0..s(10.0), s(20.0)..s(30.0)];
        let chunks = layout(&audio(32.0, &speech), &speech, &ChunkerConfig::default());
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].cut, Cut::LongPause);
        assert_eq!(chunks[0].range, 0..s(10.25));
        assert_eq!(chunks[1].range.start, s(19.75));
        assert_eq!(chunks[1].cut, Cut::Tail);
    }

    #[test]
    fn continuous_speech_falls_back_to_the_quietest_frame_within_the_clamp() {
        let speech = [0..s(100.0)];
        let mut samples = audio(100.0, &speech);
        // One quiet 100 ms frame at 27 s.
        for x in &mut samples[s(27.0)..s(27.1)] {
            *x = 0.001;
        }
        let chunks = layout(&samples, &speech, &ChunkerConfig::default());
        assert_eq!(chunks[0].cut, Cut::Energy);
        assert_eq!(chunks[0].range.end, s(27.0) + SAMPLE_RATE / 20);
        assert!(chunks.iter().all(|c| c.seconds() <= 60.0));
        assert!(covers(&chunks, &speech));
        let config = ChunkerConfig {
            target_seconds: 80.0,
            search_seconds: 5.0,
            max_seconds: 30.0,
            ..ChunkerConfig::default()
        };
        let clamped = layout(&samples, &speech, &config);
        assert!(clamped.iter().all(|c| c.seconds() <= 30.0));
        assert!(covers(&clamped, &speech));
    }

    #[test]
    fn a_target_past_the_clamp_moves_the_search_window_below_it() {
        // target + search (37.7 s) reaches past max (16 s): the window moves
        // back to 1 to 16 s, so the short pause at 10 s is the cut, not the
        // longer one at 28 s beyond the clamp; the speech up to the 40 s
        // long pause is still laid out.
        let speech = [
            0..s(10.0),
            s(10.5)..s(28.0),
            s(28.6)..s(40.0),
            s(45.0)..s(60.0),
        ];
        let config = ChunkerConfig {
            target_seconds: 30.2,
            search_seconds: 7.5,
            max_seconds: 16.0,
            ..ChunkerConfig::default()
        };
        let chunks = layout(&audio(62.0, &speech), &speech, &config);
        assert_eq!(chunks[0].cut, Cut::Pause);
        assert_eq!(chunks[0].range, 0..s(10.25));
        assert!(chunks.iter().all(|c| c.seconds() <= 16.0));
        assert!(covers(&chunks, &speech), "{chunks:?}");
        // A search wider than the clamp shrinks to it: the window is 2 to
        // 16 s from the start.
        let wide = ChunkerConfig {
            search_seconds: 20.0,
            ..config
        };
        let chunks = layout(&audio(62.0, &speech), &speech, &wide);
        assert_eq!(chunks[0].range, 0..s(10.25));
        assert!(chunks.iter().all(|c| c.seconds() <= 16.0));
        assert!(covers(&chunks, &speech), "{chunks:?}");
    }

    fn cuts(chunks: &[Chunk]) -> Vec<(Range<usize>, Cut)> {
        chunks.iter().map(|c| (c.range.clone(), c.cut)).collect()
    }

    #[test]
    fn a_long_pause_right_after_the_start_still_ends_the_chunk() {
        // The 9 s pause starts 1 s in, under min_chunk.
        let speech = [0..s(1.0), s(10.0)..s(20.0)];
        let chunks = layout(&audio(21.0, &speech), &speech, &ChunkerConfig::default());
        assert_eq!(
            cuts(&chunks),
            [(0..s(1.25), Cut::LongPause), (s(9.75)..s(20.25), Cut::Tail)]
        );
    }

    #[test]
    fn a_long_pause_in_the_search_window_ends_the_chunk_at_its_start() {
        // The 4 s pause starts at 26 s, between the target and the window's
        // far edge: the chunk ends at its start, not in its middle.
        let speech = [0..s(26.0), s(30.0)..s(40.0)];
        let chunks = layout(&audio(41.0, &speech), &speech, &ChunkerConfig::default());
        assert_eq!(chunks[0].range, 0..s(26.25));
        assert_eq!(chunks[0].cut, Cut::LongPause);
    }

    #[test]
    fn a_long_pause_just_past_the_search_window_ends_the_chunk() {
        // The 120 s pause starts at 29.3 s, past the 21 to 29 s window but
        // within min_chunk of it. Cutting inside the speech before it would
        // leave a 2 s chunk between that cut and the pause.
        let speech = [0..s(29.3), s(149.3)..s(160.0)];
        let mut samples = audio(162.0, &speech);
        for x in &mut samples[s(28.9)..s(29.0)] {
            *x = 0.001;
        }
        let chunks = layout(&samples, &speech, &ChunkerConfig::default());
        assert_eq!(
            cuts(&chunks),
            [
                (0..s(29.55), Cut::LongPause),
                (s(149.05)..s(160.25), Cut::Tail)
            ]
        );
        // With min_chunk past the window's far edge, the search for a cut
        // starts beyond it, and a long pause there is still skipped.
        let speech = [0..s(5.0), s(10.0)..s(20.0)];
        let config = ChunkerConfig {
            target_seconds: 3.0,
            search_seconds: 1.0,
            min_chunk_seconds: 6.0,
            ..ChunkerConfig::default()
        };
        let chunks = layout(&audio(21.0, &speech), &speech, &config);
        assert_eq!(chunks[0].range, 0..s(5.25));
        assert_eq!(chunks[0].cut, Cut::LongPause);
    }

    #[test]
    fn a_long_pause_past_the_clamp_waits_for_the_next_chunk() {
        // The 11 s pause starts past the 10 s clamp. Ending the first chunk
        // at it would cut at the clamp and skip the speech from 10 to 11 s.
        let speech = [0..s(11.0), s(20.0)..s(25.0)];
        let config = ChunkerConfig {
            target_seconds: 8.0,
            search_seconds: 2.0,
            max_seconds: 10.0,
            ..ChunkerConfig::default()
        };
        let chunks = layout(&audio(26.0, &speech), &speech, &config);
        assert!(covers(&chunks, &speech), "{chunks:?}");
        assert_eq!(chunks[0].cut, Cut::Energy);
    }

    #[test]
    fn a_long_pause_cut_never_ends_past_the_audio() {
        // The padding after the 0.2 s pause would reach 0.2 s past the end.
        let speech = [0..s(5.0), s(5.2)..s(5.3)];
        let config = ChunkerConfig {
            long_pause_seconds: 0.1,
            pad_seconds: 0.5,
            ..ChunkerConfig::default()
        };
        let chunks = layout(&audio(5.3, &speech), &speech, &config);
        assert_eq!(cuts(&chunks), [(0..s(5.3), Cut::LongPause)]);
    }

    #[test]
    fn the_next_chunk_starts_at_least_min_chunk_after_the_last_one() {
        // The 5 s overlap reaches back past the start of the chunk that
        // cut at 3.05 s.
        let speech = [0..s(20.0)];
        let mut samples = audio(20.0, &speech);
        for x in &mut samples[s(3.0)..s(3.1)] {
            *x = 0.001;
        }
        let config = ChunkerConfig {
            target_seconds: 3.0,
            search_seconds: 0.5,
            overlap_seconds: 5.0,
            ..ChunkerConfig::default()
        };
        let chunks = layout(&samples, &speech, &config);
        assert_eq!(chunks[0].range, 0..s(3.05));
        assert_eq!(chunks[1].range.start, s(2.0));
    }

    #[test]
    fn a_tail_does_not_decode_its_own_long_pause() {
        // All the speech fits in one chunk, but the 24 s pause inside it is
        // skipped like any other long pause.
        let speech = [0..s(2.0), s(26.0)..s(27.0)];
        let chunks = layout(&audio(28.0, &speech), &speech, &ChunkerConfig::default());
        assert_eq!(
            cuts(&chunks),
            vec![
                (0..s(2.25), Cut::LongPause),
                (s(25.75)..s(27.25), Cut::Tail)
            ]
        );
    }

    #[test]
    fn the_search_window_never_reaches_behind_the_previous_chunk() {
        // target - search (4 s) is under the overlap (4.5 s), so the second
        // chunk's window opens inside the first; the quiet frame at 5.6 s
        // would otherwise end the second chunk before the first did.
        let speech = [0..s(30.0)];
        let mut samples = audio(30.0, &speech);
        for x in &mut samples[s(5.6)..s(5.7)] {
            *x = 0.001;
        }
        let config = ChunkerConfig {
            target_seconds: 6.0,
            search_seconds: 2.0,
            overlap_seconds: 4.5,
            min_chunk_seconds: 1.0,
            ..ChunkerConfig::default()
        };
        let chunks = layout(&samples, &speech, &config);
        assert_eq!(chunks[0].range.end, s(5.6) + SAMPLE_RATE / 20);
        assert_eq!(chunks[1].range.start, s(5.65) - s(4.5));
        assert!(ends_advance(&chunks), "{chunks:?}");
        assert!(covers(&chunks, &speech));
    }

    #[test]
    fn a_degenerate_configuration_still_terminates() {
        let speech = [0..s(50.0)];
        let config = ChunkerConfig {
            overlap_seconds: 10.0,
            min_chunk_seconds: 1.0,
            target_seconds: 3.0,
            search_seconds: 1.0,
            ..ChunkerConfig::default()
        };
        let chunks = layout(&audio(50.0, &speech), &speech, &config);
        assert!(!chunks.is_empty());
        assert!(ends_advance(&chunks), "{chunks:?}");
        assert_eq!(quietest_point(&[0.0; 10], 0, 10), 5);
    }

    #[test]
    fn odd_configurations_still_move_every_end_forward() {
        // Two minimal configurations from a randomised search: a search
        // window opening inside the previous chunk could end a chunk where
        // the previous one did (the midpoint of a one-sample pause); a long
        // pause shorter than twice the padding was hit repeatedly.
        let speech = [0..5_280, 21_440..516_480];
        let config = ChunkerConfig {
            target_seconds: 8.31,
            search_seconds: 15.4,
            overlap_seconds: 1.04,
            long_pause_seconds: 3.75,
            max_seconds: 66.4,
            pad_seconds: 0.95,
            min_chunk_seconds: 0.04,
            decode_gaps: Gaps::None,
            from_start: false,
        };
        let chunks = layout(&audio(33.0, &speech), &speech, &config);
        assert!(ends_advance(&chunks), "{chunks:?}");
        assert!(covers(&chunks, &speech));
        let speech = [s(14.5)..s(30.5), s(31.03)..s(40.0)];
        let config = ChunkerConfig {
            long_pause_seconds: 0.4,
            pad_seconds: 0.74,
            ..ChunkerConfig::default()
        };
        let chunks = layout(&audio(41.0, &speech), &speech, &config);
        assert_eq!(chunks.len(), 2, "{chunks:?}");
        assert!(ends_advance(&chunks), "{chunks:?}");
        assert!(covers(&chunks, &speech));
    }

    fn chunk(range: Range<usize>) -> Chunk {
        Chunk {
            range,
            cut: Cut::Pause,
        }
    }

    /// `len` samples of a faint sound, nowhere digital silence.
    fn sound(len: usize) -> Vec<f32> {
        vec![1e-6; len]
    }

    fn gaps_on(max_seconds: f32) -> ChunkerConfig {
        ChunkerConfig {
            max_seconds,
            decode_gaps: Gaps::All,
            ..ChunkerConfig::default()
        }
    }

    /// The windows of the spans over `chunks` that are not chunks.
    fn gap_windows(audio: &[f32], chunks: &[Chunk], config: &ChunkerConfig) -> Vec<Range<usize>> {
        let spans = spans(audio, chunks, config);
        let chunk_windows = spans.iter().filter(|s| s.chunk).map(|s| &s.window);
        assert!(
            chunk_windows.eq(chunks.iter().map(|c| &c.range)),
            "the chunks are decoded as laid out: {spans:?}"
        );
        spans
            .into_iter()
            .filter(|span| !span.chunk)
            .map(|span| span.window)
            .collect()
    }

    #[test]
    fn owned_shares_meet_in_the_middle_of_an_overlap_or_an_undecoded_gap() {
        // Two chunks sharing 2 s, a 1 s gap too short to decode, a 10 s
        // gap the window owns exactly, and the tail.
        let chunks = [
            chunk(s(1.0)..s(10.0)),
            chunk(s(8.0)..s(14.0)),
            chunk(s(15.0)..s(20.0)),
            chunk(s(30.0)..s(35.0)),
        ];
        let owned = |config: &ChunkerConfig| -> Vec<(Range<usize>, Range<usize>, bool)> {
            spans(&sound(s(40.0)), &chunks, config)
                .into_iter()
                .map(|span| (span.owned, span.window, span.chunk))
                .collect()
        };
        assert_eq!(
            owned(&gaps_on(15.0)),
            [
                (0..s(9.0), s(1.0)..s(10.0), true),
                (s(9.0)..s(14.5), s(8.0)..s(14.0), true),
                (s(14.5)..s(20.0), s(15.0)..s(20.0), true),
                (s(20.0)..s(30.0), s(20.0)..s(30.0), false),
                (s(30.0)..s(35.0), s(30.0)..s(35.0), true),
                (s(35.0)..s(40.0), s(35.0)..s(40.0), false),
            ]
        );
        // Without the extra windows the chunks own the gaps, half each.
        assert_eq!(
            owned(&ChunkerConfig::default()),
            [
                (0..s(9.0), s(1.0)..s(10.0), true),
                (s(9.0)..s(14.5), s(8.0)..s(14.0), true),
                (s(14.5)..s(25.0), s(15.0)..s(20.0), true),
                (s(25.0)..s(40.0), s(30.0)..s(35.0), true),
            ]
        );
        assert!(spans(&sound(s(40.0)), &[], &ChunkerConfig::default()).is_empty());
    }

    #[test]
    fn extra_windows_cover_the_stretches_the_chunks_leave_out_and_no_more() {
        let chunks = [chunk(s(3.0)..s(10.0)), chunk(s(20.0)..s(30.0))];
        assert!(gap_windows(&sound(s(40.0)), &chunks, &ChunkerConfig::default()).is_empty());
        assert_eq!(
            gap_windows(&sound(s(40.0)), &chunks, &gaps_on(15.0)),
            [0..s(3.0), s(10.0)..s(20.0), s(30.0)..s(40.0)]
        );
        // Chunks that touch or overlap leave nothing out.
        let touching = [chunk(0..s(10.0)), chunk(s(10.0)..s(20.0))];
        assert!(gap_windows(&sound(s(20.0)), &touching, &gaps_on(15.0)).is_empty());
        // Stretches under MIN_GAP_SECONDS are left out: the 2.49 s lead-in,
        // the 1 s gap and the 2 s tail; the 2.5 s gap is decoded.
        let short = [
            chunk(s(2.49)..s(6.0)),
            chunk(s(7.0)..s(9.0)),
            chunk(s(11.5)..s(14.0)),
        ];
        assert_eq!(
            gap_windows(&sound(s(16.0)), &short, &gaps_on(15.0)),
            [s(9.0)..s(11.5)]
        );
        assert!(gap_windows(&sound(s(2.0)), &[], &gaps_on(15.0)).is_empty());
        // A window of digital silence is left out; one zero sample short
        // of it is not, though its last sample is negative and far below
        // any threshold.
        let mut audio = sound(s(40.0));
        audio[s(10.0)..s(20.0)].fill(0.0);
        audio[s(30.0)..s(40.0) - 1].fill(0.0);
        audio[s(40.0) - 1] = -1e-10;
        assert_eq!(
            gap_windows(&audio, &chunks, &gaps_on(15.0)),
            [0..s(3.0), s(30.0)..s(40.0)]
        );
    }

    #[test]
    fn a_stretch_longer_than_a_window_is_split_with_the_overlap() {
        // 35 s from 5 to 40 s: three windows of 13 s sharing 2 s.
        let chunks = [chunk(0..s(5.0)), chunk(s(40.0)..s(45.0))];
        let windows = gap_windows(&sound(s(45.0)), &chunks, &gaps_on(15.0));
        assert_eq!(windows.len(), 3, "{windows:?}");
        assert_eq!(windows[0].start, s(5.0));
        assert_eq!(windows[2].end, s(40.0));
        for pair in windows.windows(2) {
            assert_eq!(pair[0].end - pair[1].start, s(2.0), "{windows:?}");
        }
        let lengths: Vec<usize> = windows.iter().map(ExactSizeIterator::len).collect();
        assert!(
            lengths.iter().all(|&n| n <= s(15.0) && n + 1 >= lengths[0]),
            "{lengths:?}"
        );
        // Without a chunk the whole recording is one stretch.
        let all = gap_windows(&sound(s(100.0)), &[], &gaps_on(15.0));
        assert_eq!(all.first().map(|w| w.start), Some(0));
        assert_eq!(all.last().map(|w| w.end), Some(s(100.0)));
        assert_eq!(all.len(), 8, "{all:?}");
    }

    #[test]
    fn the_tail_alone_is_decoded_when_only_the_tail_is_asked_for() {
        let chunks = [chunk(s(3.0)..s(10.0)), chunk(s(20.0)..s(30.0))];
        let tail = ChunkerConfig {
            decode_gaps: Gaps::Tail,
            ..gaps_on(15.0)
        };
        assert_eq!(
            gap_windows(&sound(s(40.0)), &chunks, &tail),
            [s(30.0)..s(40.0)]
        );
        // A tail under MIN_GAP_SECONDS is left out, 2.45 s too, and so is
        // a tail of digital silence; without a chunk there is no tail.
        assert!(gap_windows(&sound(s(32.0)), &chunks, &tail).is_empty());
        assert!(gap_windows(&sound(s(32.45)), &chunks, &tail).is_empty());
        let mut silent = sound(s(40.0));
        silent[s(30.0)..].fill(0.0);
        assert!(gap_windows(&silent, &chunks, &tail).is_empty());
        assert!(gap_windows(&sound(s(40.0)), &[], &tail).is_empty());
        // The undecoded gaps are owned half and half by the chunks around
        // them, the tail by its window.
        let owned: Vec<Range<usize>> = spans(&sound(s(40.0)), &chunks, &tail)
            .into_iter()
            .map(|span| span.owned)
            .collect();
        assert_eq!(owned, [0..s(15.0), s(15.0)..s(30.0), s(30.0)..s(40.0)]);
    }

    #[test]
    fn from_start_the_first_chunk_decodes_the_lead_in() {
        // Speech from 3 to 20 s and 21 to 26 s: the one chunk starts at 0
        // instead of 2.75 s.
        let speech = [s(3.0)..s(20.0), s(21.0)..s(26.0)];
        let samples = audio(28.0, &speech);
        let from_start = ChunkerConfig {
            from_start: true,
            ..ChunkerConfig::default()
        };
        assert_eq!(
            cuts(&layout(&samples, &speech, &ChunkerConfig::default())),
            vec![(s(2.75)..s(26.25), Cut::Tail)]
        );
        assert_eq!(
            cuts(&layout(&samples, &speech, &from_start)),
            vec![(0..s(26.25), Cut::Tail)]
        );
        // With no padding as well.
        let unpadded = ChunkerConfig {
            pad_seconds: 0.0,
            ..from_start
        };
        assert_eq!(
            cuts(&layout(&samples, &speech, &unpadded)),
            vec![(0..s(26.0), Cut::Tail)]
        );
        // A lead-in longer than a chunk: the first chunk decodes its first
        // 25 s or so, and the next starts again just before the speech, as
        // without the switch.
        let speech = [s(70.0)..s(75.0)];
        let chunks = layout(&audio(76.0, &speech), &speech, &from_start);
        assert_eq!(chunks.len(), 2, "{chunks:?}");
        assert_eq!(chunks[0].range.start, 0);
        assert!(chunks[0].range.end <= s(29.0), "{chunks:?}");
        assert_eq!(chunks[1].range, s(69.75)..s(75.25));
    }

    #[test]
    fn with_extra_windows_only_short_stretches_are_left_out() {
        // Random speech under the CoreML settings and others, the first
        // chunk at the recording's start or not: the chunks decode every
        // speech region, every stretch neither the chunks nor the extra
        // windows cover is under MIN_GAP_SECONDS, no window reaches into a
        // chunk or past the clamp, and the windows come in order.
        let mut seed = 0x2545_f491_4f6c_dd1d_u64;
        let mut below = |n: usize| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % n as u64) as usize
        };
        for case in 0..300 {
            let total = s(5.0) + below(s(240.0));
            let mut speech = Vec::new();
            let mut at = below(s(20.0));
            while at < total {
                let end = (at + s(0.2) + below(s(15.0))).min(total);
                speech.push(at..end);
                at = end + s(0.1) + below(s(if case % 3 == 0 { 40.0 } else { 6.0 }));
            }
            let config = ChunkerConfig {
                max_seconds: [15.0, 60.0][case % 2],
                long_pause_seconds: [f32::INFINITY, 3.0][(case / 2) % 2],
                decode_gaps: Gaps::All,
                from_start: (case / 4) % 2 == 1,
                ..ChunkerConfig::default()
            };
            // Faint sound outside the speech, so no window is digital
            // silence.
            let mut samples = vec![1e-6f32; total];
            for region in &speech {
                samples[region.clone()].fill(0.5);
            }
            let chunks = layout(&samples, &speech, &config);
            assert!(covers(&chunks, &speech), "case {case}: {chunks:?}");
            let windows = gap_windows(&samples, &chunks, &config);
            let max = sample_count(config.max_seconds);
            assert!(
                windows.iter().all(|w| w.len() <= max && w.end <= total),
                "case {case}"
            );
            assert!(
                windows.windows(2).all(|w| w[0].start < w[1].start),
                "case {case}"
            );
            assert!(
                windows.iter().all(|w| chunks
                    .iter()
                    .all(|c| w.end <= c.range.start || c.range.end <= w.start)),
                "case {case}: a window reaches into a chunk"
            );
            let mut ranges: Vec<&Range<usize>> =
                chunks.iter().map(|c| &c.range).chain(&windows).collect();
            ranges.sort_by_key(|r| r.start);
            let mut covered = 0;
            for range in ranges {
                assert!(
                    range.start.saturating_sub(covered) < sample_count(MIN_GAP_SECONDS),
                    "case {case}: {chunks:?} {windows:?}"
                );
                covered = covered.max(range.end);
            }
            assert!(
                total - covered < sample_count(MIN_GAP_SECONDS),
                "case {case}"
            );
        }
    }

    /// Checks `span`'s reach (see `a_partition_owns_every_sample_once`).
    fn check_reach(
        span: &Span,
        config: &ChunkerConfig,
        total: usize,
        context: &dyn Fn() -> String,
    ) {
        let (reach, window) = (&span.reach, &span.window);
        if !span.chunk {
            assert_eq!(reach, window, "{}", context());
            return;
        }
        // The window ± the overlap, on the frame grid, with less than a
        // frame more either side, and clamped to the recording.
        let overlap = sample_count(config.overlap_seconds);
        let (from, to) = (window.start.saturating_sub(overlap), window.end + overlap);
        assert!(reach.start.is_multiple_of(FRAME_SAMPLES), "{}", context());
        assert!(
            reach.start <= from && from < reach.start + FRAME_SAMPLES,
            "{}",
            context()
        );
        if reach.end == total {
            assert!(total <= to + FRAME_SAMPLES, "{}", context());
        } else {
            assert!(reach.end.is_multiple_of(FRAME_SAMPLES), "{}", context());
            assert!(
                to < reach.end && reach.end <= to + FRAME_SAMPLES,
                "{}",
                context()
            );
        }
    }

    #[test]
    fn a_partition_owns_every_sample_once() {
        // Clauses 1 to 3 of the ownership invariant, whatever the merge
        // does with them: the owned ranges are sorted, disjoint, never
        // empty and cover the recording; a window holds its owned range
        // except the audio no window decodes, and is never longer than the
        // clamp; the chunk spans are the layout's chunks. And the reach: a
        // gap's window's is its window; a chunk's is its window ± the
        // overlap, rounded out to the frame grid and clamped to the
        // recording. Random speech over the default settings, the engines'
        // and odd ones, with the extra windows over no gap, the tail or
        // every gap, the first chunk at the recording's start or not, and
        // some gaps of digital silence.
        let mut seed = 0x9e37_79b9_7f4a_7c15_u64;
        let mut below = |n: usize| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % n as u64) as usize
        };
        let mut undecoded_shares = 0;
        for case in 0..600 {
            let total = s(1.0) + below(s(200.0));
            let mut speech = Vec::new();
            let mut at = below(s(10.0));
            while at < total {
                let end = (at + s(0.1) + below(s(12.0))).min(total);
                speech.push(at..end);
                at = end + s(0.05) + below(s([2.0, 8.0, 40.0][case % 3]));
            }
            let config = ChunkerConfig {
                target_seconds: [25.0, 25.0, 8.0, 3.0][case % 4],
                search_seconds: [4.0, 4.0, 1.0, 0.5][case % 4],
                overlap_seconds: [2.0, 2.0, 1.5, 2.5][(case / 4) % 4],
                max_seconds: [15.0, 60.0, 15.0, 9.0][(case / 2) % 4],
                long_pause_seconds: [f32::INFINITY, 3.0][(case / 3) % 2],
                decode_gaps: [Gaps::None, Gaps::Tail, Gaps::All, Gaps::All, Gaps::All][case % 5],
                from_start: case % 6 == 1,
                ..ChunkerConfig::default()
            };
            let mut samples = vec![if case % 7 == 0 { 0.0 } else { 1e-6f32 }; total];
            for region in &speech {
                samples[region.clone()].fill(0.5);
            }
            let spans = partition(&samples, &speech, &config);
            let context = || format!("case {case}: {config:?} {spans:?}");
            let chunks = layout(&samples, &speech, &config);
            let chunk_windows = spans.iter().filter(|s| s.chunk).map(|s| &s.window);
            assert!(
                chunk_windows.eq(chunks.iter().map(|c| &c.range)),
                "{}",
                context()
            );
            let Some((first, last)) = spans.first().zip(spans.last()) else {
                assert!(chunks.is_empty(), "{}", context());
                continue;
            };
            assert_eq!(first.owned.start, 0, "{}", context());
            assert_eq!(last.owned.end, total, "{}", context());
            let max = sample_count(config.max_seconds).max(1);
            let decoded = |edge: &Range<usize>| {
                spans
                    .iter()
                    .any(|s| s.window.start < edge.end && edge.start < s.window.end)
            };
            for (i, span) in spans.iter().enumerate() {
                assert!(span.owned.start < span.owned.end, "{}", context());
                if let Some(next) = spans.get(i + 1) {
                    assert_eq!(span.owned.end, next.owned.start, "{}", context());
                    assert!(span.window.start < next.window.start, "{}", context());
                }
                assert!(
                    span.window.len() <= max && span.window.end <= total,
                    "{}",
                    context()
                );
                // Outside its window a span owns only audio no window
                // decodes: half an undecoded gap.
                for edge in [
                    span.owned.start..span.window.start.min(span.owned.end),
                    span.window.end.max(span.owned.start)..span.owned.end,
                ] {
                    if !edge.is_empty() {
                        undecoded_shares += 1;
                        assert!(!decoded(&edge), "{edge:?} {}", context());
                    }
                }
                check_reach(span, &config, total, &context);
            }
        }
        assert!(undecoded_shares > 500, "{undecoded_shares}");
    }
}
