//! Pause-aligned chunk layout (decision 1 of the cross-platform speech-stack
//! plan): chunks target `target_s`, cut at the longest VAD pause within a
//! search window either side of the target, never inside speech when a pause
//! exists, with an energy-minimum fallback; long pauses end a chunk early and
//! are skipped entirely; `overlap_s` of audio is shared with the next chunk
//! for the LCS merge. Every chunk stays under `max_s` (encoder frame limit).

pub const SR: usize = 16_000;

#[derive(Clone, Copy, Debug)]
pub struct Chunk {
    pub start: usize,
    pub end: usize,
    /// How the end was chosen: "pause", "long-pause", "energy", "tail".
    pub cut: &'static str,
}

pub struct ChunkerConfig {
    pub target_s: f32,
    pub search_s: f32,
    pub overlap_s: f32,
    pub long_pause_s: f32,
    pub max_s: f32,
    pub pad_s: f32,
}

fn s(x: f32) -> usize {
    (x * SR as f32) as usize
}

/// Energy-minimum over 100 ms frames inside [lo, hi); returns the frame centre.
pub fn quietest_point(samples: &[f32], lo: usize, hi: usize) -> usize {
    let frame = SR / 10;
    let hi = hi.min(samples.len());
    let mut best = (f32::MAX, (lo + hi) / 2);
    let mut p = lo;
    while p + frame <= hi {
        let e: f32 = samples[p..p + frame].iter().map(|x| x * x).sum();
        if e < best.0 {
            best = (e, p + frame / 2);
        }
        p += frame;
    }
    best.1
}

/// Pauses between speech regions, including the leading and trailing silence.
pub fn pauses(speech: &[(usize, usize)], total: usize) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut cursor = 0usize;
    for &(a, b) in speech {
        if a > cursor {
            out.push((cursor, a));
        }
        cursor = cursor.max(b);
    }
    if total > cursor {
        out.push((cursor, total));
    }
    out
}

pub fn layout(samples: &[f32], speech: &[(usize, usize)], cfg: &ChunkerConfig) -> Vec<Chunk> {
    let total = samples.len();
    let mut chunks = Vec::new();
    if speech.is_empty() {
        return chunks;
    }
    let pauses = pauses(speech, total);
    let target = s(cfg.target_s);
    let search = s(cfg.search_s);
    let overlap = s(cfg.overlap_s);
    let long_pause = s(cfg.long_pause_s);
    let max = s(cfg.max_s);
    let pad = s(cfg.pad_s);
    let min_chunk = s(2.0);
    let speech_end = speech.last().unwrap().1;

    let mut start = speech[0].0.saturating_sub(pad);
    loop {
        // Snap a start that lies in a pause forward to just before the next speech.
        if let Some(p) = pauses.iter().find(|p| p.0 <= start && start < p.1) {
            if p.1 >= total {
                break;
            }
            start = start.max(p.1.saturating_sub(pad));
        }
        if start >= speech_end || start >= total {
            break;
        }
        let want = start + target;
        // Tail: the remaining speech fits in one chunk.
        if want + search >= speech_end {
            let end = (speech_end + pad).min(total).min(start + max);
            chunks.push(Chunk { start, end, cut: "tail" });
            if end >= speech_end.min(total) {
                break;
            }
            start = end.saturating_sub(overlap);
            continue;
        }
        // A long pause before the search window closes ends the chunk early; no overlap needed.
        if let Some(p) = pauses
            .iter()
            .find(|p| p.0 >= start + min_chunk && p.0 < want + search && p.1 - p.0 >= long_pause)
        {
            let end = (p.0 + pad).min(start + max);
            chunks.push(Chunk { start, end, cut: "long-pause" });
            start = p.1.saturating_sub(pad);
            continue;
        }
        let lo = want.saturating_sub(search).max(start + min_chunk);
        let hi = (want + search).min(start + max);
        // Longest pause (clipped to the window) wins.
        let mut best: Option<(usize, usize)> = None;
        for &(a, b) in &pauses {
            let (ca, cb) = (a.max(lo), b.min(hi));
            if cb > ca && best.map_or(true, |(x, y)| cb - ca > y - x) {
                best = Some((ca, cb));
            }
        }
        let (end, cut) = match best {
            Some((a, b)) => ((a + b) / 2, "pause"),
            None => (quietest_point(samples, lo, hi), "energy"),
        };
        let end = end.clamp(start + min_chunk, start + max).min(total);
        chunks.push(Chunk { start, end, cut });
        start = end.saturating_sub(overlap);
    }
    chunks
}
