//! Turns and chunks into `DiarizationResult`: one `SpeakerCluster` per
//! label that has a turn, labelled "Speaker n" in order of first speech,
//! with merged ranges, the normalised cluster embedding and the sample
//! clip. Swift: `Sources/StenoSpeech/Diarization/DiarizationMapping.swift`,
//! `ClusterEmbedding.swift` and `SampleClipPicker.swift`, one for one.
//!
//! The turns are the final word on who spoke when (frame voting over the
//! whole recording); the chunks are the per-window embeddings that fed
//! clustering, and a chunk's span is a local speaker's extent inside one
//! window, not a turn. A label that appears only in chunks lost every
//! frame vote and gets no speaker.

use std::collections::HashMap;

use steno_core::{
    ClusterChunk, DiarizationResult, Embedding, SpeakerCluster, SpeakerTurn, TimeRange,
};

use crate::first_max_by;

/// Chunks arrive without a quality; each takes the quality of the turn it
/// overlaps most first.
#[must_use]
pub fn result(turns: &[SpeakerTurn], raw: &[ClusterChunk]) -> DiarizationResult {
    let chunks = assigning_quality(raw, turns);
    let mut sorted: Vec<&SpeakerTurn> = turns.iter().filter(|turn| turn.duration() > 0.0).collect();
    sorted.sort_by(|lhs, rhs| lhs.start.total_cmp(&rhs.start));
    let mut order: Vec<&str> = Vec::new();
    let mut by_label: HashMap<&str, Vec<&SpeakerTurn>> = HashMap::new();
    for turn in sorted {
        let entry = by_label.entry(turn.speaker_label.as_str()).or_default();
        if entry.is_empty() {
            order.push(turn.speaker_label.as_str());
        }
        entry.push(turn);
    }
    let clusters = order
        .iter()
        .enumerate()
        .map(|(index, label)| {
            let own_turns = by_label.get(label).map(Vec::as_slice).unwrap_or_default();
            let own_chunks: Vec<ClusterChunk> = chunks
                .iter()
                .filter(|chunk| chunk.speaker_label == *label)
                .cloned()
                .collect();
            let ranges = merged(
                &own_turns
                    .iter()
                    .map(|turn| TimeRange {
                        lower: turn.start,
                        upper: turn.end.max(turn.start),
                    })
                    .collect::<Vec<_>>(),
            );
            // Without chunks the turns carry the quality (and no embedding).
            let scored: Vec<ClusterChunk> = if own_chunks.is_empty() {
                own_turns
                    .iter()
                    .map(|turn| ClusterChunk {
                        speaker_label: (*label).to_owned(),
                        start: turn.start,
                        end: turn.end,
                        embedding: Vec::new(),
                        quality: turn.quality,
                    })
                    .collect()
            } else {
                own_chunks.clone()
            };
            let choice = pick_clip(&ranges, &scored);
            SpeakerCluster {
                label: format!("Speaker {}", index + 1),
                ranges,
                embedding: cluster_embedding(&own_chunks),
                cluster_confidence: choice.cluster_confidence,
                sample_clip_range: choice.range,
            }
        })
        .collect();
    DiarizationResult { clusters }
}

/// Sorted ranges with touching or overlapping ones joined.
#[must_use]
pub fn merged(ranges: &[TimeRange]) -> Vec<TimeRange> {
    let mut sorted = ranges.to_vec();
    sorted.sort_by(|lhs, rhs| lhs.lower.total_cmp(&rhs.lower));
    let mut result: Vec<TimeRange> = Vec::with_capacity(sorted.len());
    for range in sorted {
        match result.last_mut() {
            Some(last) if range.lower <= last.upper => {
                last.upper = last.upper.max(range.upper);
            }
            _ => result.push(range),
        }
    }
    result
}

/// Each chunk takes the quality of the turn of its speaker it overlaps
/// most, 1 when none overlaps.
#[must_use]
pub fn assigning_quality(chunks: &[ClusterChunk], turns: &[SpeakerTurn]) -> Vec<ClusterChunk> {
    chunks
        .iter()
        .map(|chunk| {
            let best = first_max_by(
                turns
                    .iter()
                    .filter(|turn| turn.speaker_label == chunk.speaker_label),
                |lhs, rhs| overlap(lhs, chunk).total_cmp(&overlap(rhs, chunk)),
            );
            let quality = best.map_or(1.0, |turn| {
                if overlap(turn, chunk) > 0.0 {
                    turn.quality
                } else {
                    1.0
                }
            });
            ClusterChunk {
                quality,
                ..chunk.clone()
            }
        })
        .collect()
}

fn overlap(turn: &SpeakerTurn, chunk: &ClusterChunk) -> f64 {
    (turn.end.min(chunk.end) - turn.start.max(chunk.start)).max(0.0)
}

/// The embedding the mapping gives a cluster: each chunk vector brought to
/// unit length, summed with its duration as weight, then L2-normalised.
/// Normalising first means a few high-norm windows (crosstalk, music,
/// clipping) cannot steer the mean. `None` without a usable chunk; chunks
/// of another dimension, of zero duration or with a non-finite value
/// (the backends reject those, so a defence) are skipped.
#[must_use]
pub fn cluster_embedding(chunks: &[ClusterChunk]) -> Option<Embedding> {
    let mut sum = vec![0.0f32; Embedding::DIMENSION];
    let mut usable = false;
    for chunk in chunks.iter().filter(|chunk| {
        chunk.embedding.len() == Embedding::DIMENSION
            && chunk.duration() > 0.0
            && chunk.embedding.iter().all(|value| value.is_finite())
    }) {
        let unit = Embedding(chunk.embedding.clone()).normalized();
        // Durations are seconds; f32 holds them to the microsecond.
        #[allow(clippy::cast_possible_truncation)]
        let weight = chunk.duration() as f32;
        for (target, value) in sum.iter_mut().zip(&unit.0) {
            *target += value * weight;
        }
        usable = true;
    }
    usable.then(|| Embedding(sum).normalized())
}

/// Core documents `sample_clip_range` as "at most ten seconds"; this is
/// the one place that number lives and nothing can raise it.
pub const CLIP_TARGET_SECONDS: f64 = 10.0;
/// Below this the longest range is too short to name a voice from.
pub const CLIP_MINIMUM_SECONDS: f64 = 3.0;

/// The clip a user hears when naming a speaker and the confidence stored
/// beside it.
#[derive(Debug, Clone, PartialEq)]
pub struct ClipChoice {
    pub range: Option<TimeRange>,
    pub cluster_confidence: f32,
}

/// The longest contiguous range of the cluster, capped to
/// [`CLIP_TARGET_SECONDS`] and centred on the highest-quality chunk inside
/// it; on a tie the earlier range and the earlier chunk, as Swift's
/// `max(by:)` picks them. Confidence is the duration-weighted mean chunk
/// quality, halved when the longest range is under
/// [`CLIP_MINIMUM_SECONDS`].
#[must_use]
pub fn pick_clip(ranges: &[TimeRange], chunks: &[ClusterChunk]) -> ClipChoice {
    let Some(longest) = first_max_by(ranges, |lhs, rhs| length(lhs).total_cmp(&length(rhs))) else {
        return ClipChoice {
            range: None,
            cluster_confidence: 0.0,
        };
    };
    let mut confidence = mean_quality(chunks);
    if length(longest) < CLIP_MINIMUM_SECONDS {
        confidence /= 2.0;
    }
    let clip_length = CLIP_TARGET_SECONDS.min(length(longest));
    let best = first_max_by(
        chunks
            .iter()
            .filter(|chunk| overlaps(&chunk.range(), longest)),
        |lhs, rhs| {
            lhs.quality
                .total_cmp(&rhs.quality)
                .then(lhs.duration().total_cmp(&rhs.duration()))
        },
    );
    let centre = match best {
        Some(chunk) => f64::midpoint(chunk.start.max(longest.lower), chunk.end.min(longest.upper)),
        None => f64::midpoint(longest.lower, longest.upper),
    };
    let mut lower = centre - clip_length / 2.0;
    lower = longest.lower.max(lower.min(longest.upper - clip_length));
    let upper = longest.upper.min(lower + clip_length);
    ClipChoice {
        range: Some(TimeRange {
            lower,
            upper: upper.max(lower),
        }),
        cluster_confidence: confidence,
    }
}

/// Duration-weighted mean chunk quality in `0...1`; zero without
/// duration. A chunk with a non-finite quality does not count.
#[must_use]
pub fn mean_quality(chunks: &[ClusterChunk]) -> f32 {
    let scored = || chunks.iter().filter(|chunk| chunk.quality.is_finite());
    let total: f64 = scored().map(ClusterChunk::duration).sum();
    if total <= 0.0 {
        return 0.0;
    }
    let weighted: f64 = scored()
        .map(|chunk| f64::from(chunk.quality) * chunk.duration())
        .sum();
    // Clamped to 0...1 before narrowing.
    #[allow(clippy::cast_possible_truncation)]
    let quality = (weighted / total).clamp(0.0, 1.0) as f32;
    quality
}

/// `upper - lower`.
#[must_use]
pub fn length(range: &TimeRange) -> f64 {
    range.upper - range.lower
}

/// Whether two ranges share more than a point.
#[must_use]
pub fn overlaps(lhs: &TimeRange, rhs: &TimeRange) -> bool {
    lhs.lower < rhs.upper && rhs.lower < lhs.upper
}

/// Seconds of speech in `ranges`.
#[must_use]
pub fn speech_seconds(ranges: &[TimeRange]) -> f64 {
    ranges.iter().map(length).sum()
}
