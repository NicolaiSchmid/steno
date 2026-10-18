//! Turns and chunks into `DiarizationResult`: one `SpeakerCluster` per label
//! that has a turn, "Speaker n" in order of first speech, with merged ranges,
//! the cluster embedding and the sample clip. Turns decide who spoke when; a
//! label that appears only in chunks gets no speaker.

use std::collections::HashMap;

use steno_core::{
    ClusterChunk, DiarizationResult, Embedding, SpeakerCluster, SpeakerTurn, TimeRange,
};

use crate::first_max_by;

/// The user-facing label of the cluster at `index`: `Speaker 1`, `Speaker 2`, ...
#[must_use]
pub fn display_label(index: usize) -> String {
    format!("Speaker {}", index + 1)
}

/// Chunks take the quality of the turn they overlap most.
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
            let embedding = cluster_embedding(&own_chunks);
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
                own_chunks
            };
            let choice = pick_clip(&ranges, &scored);
            SpeakerCluster {
                label: display_label(index),
                ranges,
                embedding,
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

/// Each chunk takes the quality of its speaker's turn it overlaps most, 1
/// when none overlaps.
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

/// The cluster embedding: each chunk vector at unit length, summed with its
/// duration as weight, then L2-normalised, so a few high-norm windows cannot
/// steer the mean. `None` without a usable chunk; bad chunks are skipped.
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
        #[allow(clippy::cast_possible_truncation)]
        let weight = chunk.duration() as f32;
        for (target, value) in sum.iter_mut().zip(&unit.0) {
            *target += value * weight;
        }
        usable = true;
    }
    usable.then(|| Embedding(sum).normalized())
}

/// Core documents `sample_clip_range` as at most ten seconds.
pub const CLIP_TARGET_SECONDS: f64 = 10.0;
/// Below this the longest range is too short to name a voice from.
pub const CLIP_MINIMUM_SECONDS: f64 = 3.0;

/// The clip a user hears when naming a speaker, and its confidence.
#[derive(Debug, Clone, PartialEq)]
pub struct ClipChoice {
    pub range: Option<TimeRange>,
    pub cluster_confidence: f32,
}

/// The longest range of the cluster, capped to [`CLIP_TARGET_SECONDS`] and
/// centred on its highest-quality chunk. Confidence is the duration-weighted
/// mean chunk quality, halved under [`CLIP_MINIMUM_SECONDS`].
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

/// Duration-weighted mean chunk quality in `0...1`, ignoring non-finite ones.
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
    #[allow(clippy::cast_possible_truncation)]
    let quality = (weighted / total).clamp(0.0, 1.0) as f32;
    quality
}

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
