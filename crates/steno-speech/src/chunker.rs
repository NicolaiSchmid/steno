//! The pause-aligned chunk layout (decision 1 of the speech-stack plan):
//! chunks aim at `target_seconds`, cut at the longest VAD pause within a
//! search window either side of the target, never inside speech when a
//! pause exists, with an energy-minimum fallback; a long pause ends a chunk
//! early and is skipped; `overlap_seconds` of audio is shared with the next
//! chunk for the merge. No chunk exceeds `max_seconds`, a memory clamp
//! (attention grows with the square of the window: 13 GB at 600 s, spike
//! E), not the position table's 800 s cap. Ported from
//! `spikes/onnx-speech/src/chunker.rs`.
//! Swift: none; `FluidAudio`'s `ChunkProcessor` cuts at fixed 15 s strides.

use std::ops::Range;

pub const SAMPLE_RATE: usize = 16_000;

/// How a chunk's end was chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cut {
    /// The middle of the longest pause in the search window.
    Pause,
    /// Just before a pause of `long_pause_seconds` or more.
    LongPause,
    /// The quietest 100 ms frame in the search window; no pause there.
    Energy,
    /// The end of speech.
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
    /// Either side of the target where the cut may fall.
    pub search_seconds: f32,
    /// Shared with the next chunk; the plan says 1 to 2 s.
    pub overlap_seconds: f32,
    /// A pause this long ends the chunk and is not decoded.
    pub long_pause_seconds: f32,
    /// The memory clamp.
    pub max_seconds: f32,
    /// Silence kept around speech at a chunk's edges.
    pub pad_seconds: f32,
    /// No chunk is shorter than this unless the speech is.
    pub min_chunk_seconds: f32,
}

impl Default for ChunkerConfig {
    fn default() -> Self {
        ChunkerConfig {
            target_seconds: 25.0,
            search_seconds: 4.0,
            overlap_seconds: 1.5,
            long_pause_seconds: 3.0,
            max_seconds: 60.0,
            pad_seconds: 0.25,
            min_chunk_seconds: 2.0,
        }
    }
}

/// Seconds to whole samples, never negative.
#[must_use]
pub fn samples(seconds: f32) -> usize {
    // Rounded down; the clamp keeps the cast in range.
    (seconds.max(0.0) * SAMPLE_RATE as f32) as usize
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
    let target = samples(config.target_seconds);
    let search = samples(config.search_seconds);
    let overlap = samples(config.overlap_seconds);
    let long_pause = samples(config.long_pause_seconds);
    let max = samples(config.max_seconds).max(1);
    let pad = samples(config.pad_seconds);
    let min_chunk = samples(config.min_chunk_seconds).clamp(1, max);
    let speech_end = last_speech.end.min(total);

    let mut start = speech[0].start.saturating_sub(pad);
    // Where the last chunk ended; no cut falls before it, so every chunk
    // adds audio even when the search window reaches back into the overlap.
    let mut previous_end = 0;
    loop {
        // A start inside a pause snaps forward to just before the next speech.
        if let Some(pause) = pauses.iter().find(|p| p.start <= start && start < p.end) {
            if pause.end >= total {
                break;
            }
            start = start.max(pause.end.saturating_sub(pad));
        }
        if start >= speech_end {
            break;
        }
        let want = start + target;
        // Tail: the remaining speech fits in one chunk.
        if want + search >= speech_end {
            let end = (speech_end + pad).min(total).min(start + max);
            chunks.push(Chunk {
                range: start..end,
                cut: Cut::Tail,
            });
            if end >= speech_end {
                break;
            }
            start = next_start(start, end, overlap, min_chunk);
            previous_end = end;
            continue;
        }
        // A long pause before the search window closes ends the chunk early;
        // no overlap is needed across silence. When the clamp cuts the chunk
        // before the pause, the speech up to the pause is still ahead and
        // the next chunk overlaps as usual.
        if let Some(pause) = pauses.iter().find(|p| {
            p.start >= start + min_chunk && p.start < want + search && p.len() >= long_pause
        }) {
            let end = (pause.start + pad).min(start + max);
            chunks.push(Chunk {
                range: start..end,
                cut: Cut::LongPause,
            });
            start = if end < pause.start + pad {
                next_start(start, end, overlap, min_chunk)
            } else {
                pause.end.saturating_sub(pad).max(start + 1)
            };
            previous_end = end;
            continue;
        }
        let lo = want
            .saturating_sub(search)
            .max(start + min_chunk)
            .max(previous_end);
        let hi = (want + search).min(start + max).max(lo + 1);
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
        start = next_start(start, end, overlap, min_chunk);
        previous_end = end;
    }
    chunks
}

/// The next chunk starts `overlap` before this one's end and at least
/// `min_chunk` (at least one sample) after this one's start, so an overlap
/// longer than the chunk still makes linear progress.
fn next_start(start: usize, end: usize, overlap: usize, min_chunk: usize) -> usize {
    end.saturating_sub(overlap).max(start + min_chunk)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(seconds: f32) -> usize {
        samples(seconds)
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
        assert_eq!(chunks[1].range.start, s(24.5) - s(1.5));
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
    fn a_long_pause_beyond_the_clamp_does_not_skip_the_speech_before_it() {
        // target + search reaches past max: the chunk is clamped at 16 s,
        // and the speech between the clamp and the 40 s pause must still be
        // laid out instead of the next chunk jumping to 45 s.
        let speech = [s(6.8)..s(40.0), s(45.0)..s(60.0)];
        let config = ChunkerConfig {
            target_seconds: 30.2,
            search_seconds: 7.5,
            max_seconds: 16.0,
            ..ChunkerConfig::default()
        };
        let chunks = layout(&audio(62.0, &speech), &speech, &config);
        assert_eq!(chunks[0].cut, Cut::LongPause);
        assert_eq!(chunks[0].range, s(6.55)..s(22.55));
        assert_eq!(chunks[1].range.start, s(22.55) - s(1.5));
        assert!(chunks.iter().all(|c| c.seconds() <= 16.0));
        assert!(covers(&chunks, &speech), "{chunks:?}");
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
}
