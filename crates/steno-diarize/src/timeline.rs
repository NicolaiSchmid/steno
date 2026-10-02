//! From window votes to exclusive speaker turns, the reconstruction step
//! of the pyannote pipeline as sherpa-onnx and `FluidAudio` do it: every
//! window votes per frame for the clusters of its local speakers, the
//! frame's speaker count is the rounded mean count over the windows that
//! cover it, the top clusters by votes are active, runs become segments,
//! short gaps close, short segments go, and overlaps are resolved in
//! favour of the earlier speaker so no two speakers share a moment, as
//! the transcript lanes require.

use steno_core::SpeakerTurn;

use crate::backend::to_f64;
use crate::extraction::Analysis;
use crate::segmentation::frame_span;

/// `FluidAudio`'s `PostProcessing.community` and `Embedding.community`:
/// gaps up to 0.1 s between turns of one speaker close; turns under one
/// second are dropped.
#[derive(Debug, Clone, PartialEq)]
pub struct TimelineRules {
    pub min_duration_on: f64,
    pub min_duration_off: f64,
}

impl Default for TimelineRules {
    fn default() -> Self {
        TimelineRules {
            min_duration_on: 1.0,
            min_duration_off: 0.1,
        }
    }
}

/// `assignments[i]` is the cluster of `analysis.embeddings[i]`, or `None`
/// when it was left out of clustering. Turns carry the labels `S1`, `S2`
/// ... by cluster index and a quality in `0...1`: the share of the windows
/// covering the turn's frames that voted for its cluster.
#[must_use]
pub fn turns(
    analysis: &Analysis,
    assignments: &[Option<usize>],
    rules: &TimelineRules,
) -> Vec<SpeakerTurn> {
    let geometry = &analysis.geometry;
    let cluster_count = assignments.iter().flatten().max().map_or(0, |max| max + 1);
    let total_frames = analysis.total_samples / geometry.receptive_field_shift;
    if cluster_count == 0 || total_frames == 0 {
        return Vec::new();
    }
    // Per global frame: votes per cluster, how many windows cover it, and
    // the summed local speaker counts.
    let mut votes = vec![0u32; total_frames * cluster_count];
    let mut coverage = vec![0u32; total_frames];
    let mut counts = vec![0u32; total_frames];
    for activity in &analysis.activities {
        let base = geometry.global_frame(activity.offset);
        for (local, _) in activity.frames.iter().enumerate() {
            let frame = base + local;
            if frame >= total_frames {
                break;
            }
            coverage[frame] += 1;
            counts[frame] += activity.speaker_count(local);
        }
    }
    for (embedding, cluster) in analysis.embeddings.iter().zip(assignments) {
        let Some(cluster) = cluster else { continue };
        let activity = &analysis.activities[embedding.window];
        let base = geometry.global_frame(activity.offset);
        for local in 0..activity.frames.len() {
            if activity.is_active(local, embedding.local_speaker) {
                let frame = base + local;
                if frame < total_frames {
                    votes[frame * cluster_count + cluster] += 1;
                }
            }
        }
    }
    // Active clusters per frame: the top `k` by votes, `k` the rounded mean
    // speaker count, only clusters somebody voted for.
    let mut active: Vec<Vec<usize>> = Vec::with_capacity(total_frames);
    for frame in 0..total_frames {
        if coverage[frame] == 0 {
            active.push(Vec::new());
            continue;
        }
        let expected = (counts[frame] + coverage[frame] / 2) / coverage[frame];
        let k = (expected as usize).min(cluster_count.min(geometry.num_speakers));
        let row = &votes[frame * cluster_count..(frame + 1) * cluster_count];
        let mut ranked: Vec<usize> = (0..cluster_count).filter(|c| row[*c] > 0).collect();
        ranked.sort_by(|lhs, rhs| row[*rhs].cmp(&row[*lhs]).then(lhs.cmp(rhs)));
        ranked.truncate(k);
        active.push(ranked);
    }
    // Runs per cluster become segments with their mean vote share.
    let mut segments: Vec<Segment> = Vec::new();
    for cluster in 0..cluster_count {
        let mut run: Option<(usize, f64, usize)> = None;
        for frame in 0..=total_frames {
            let is_active = frame < total_frames && active[frame].contains(&cluster);
            if is_active {
                let share =
                    f64::from(votes[frame * cluster_count + cluster]) / f64::from(coverage[frame]);
                run = Some(match run {
                    Some((start, sum, length)) => (start, sum + share, length + 1),
                    None => (frame, share, 1),
                });
            } else if let Some((start, sum, length)) = run.take() {
                let (time_start, time_end) =
                    frame_span(geometry, 0, start, frame - 1, analysis.total_samples);
                segments.push(Segment {
                    cluster,
                    start: time_start,
                    end: time_end,
                    quality: (sum / to_f64(length)).clamp(0.0, 1.0),
                });
            }
        }
    }
    segments.sort_by(|lhs, rhs| {
        lhs.start
            .total_cmp(&rhs.start)
            .then(lhs.cluster.cmp(&rhs.cluster))
    });
    let segments = close_gaps(segments, rules.min_duration_off);
    let segments = exclusive(segments, rules.min_duration_on);
    segments
        .into_iter()
        .map(|segment| SpeakerTurn {
            speaker_label: format!("S{}", segment.cluster + 1),
            start: segment.start,
            end: segment.end,
            // Qualities lie in 0...1; the narrowing is intended.
            #[allow(clippy::cast_possible_truncation)]
            quality: segment.quality as f32,
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq)]
struct Segment {
    cluster: usize,
    start: f64,
    end: f64,
    quality: f64,
}

/// Joins consecutive segments of one cluster whose gap is at most
/// `max_gap`, quality weighted by duration. `segments` sorted by start.
fn close_gaps(segments: Vec<Segment>, max_gap: f64) -> Vec<Segment> {
    let mut result: Vec<Segment> = Vec::with_capacity(segments.len());
    for segment in segments {
        if let Some(last) = result
            .iter_mut()
            .rev()
            .find(|last| last.cluster == segment.cluster)
            .filter(|last| segment.start - last.end <= max_gap)
        {
            let lhs = (last.end - last.start).max(0.0);
            let rhs = (segment.end - segment.start).max(0.0);
            let total = lhs + rhs;
            last.quality = if total > 0.0 {
                (last.quality * lhs + segment.quality * rhs) / total
            } else {
                f64::midpoint(last.quality, segment.quality)
            };
            last.end = last.end.max(segment.end);
        } else {
            result.push(segment);
        }
    }
    result.sort_by(|lhs, rhs| {
        lhs.start
            .total_cmp(&rhs.start)
            .then(lhs.cluster.cmp(&rhs.cluster))
    });
    result
}

/// Pushes each segment's start to the previous segment's end, drops what
/// vanishes or falls under `min_duration`, and scales the quality by what
/// survived, as `FluidAudio`'s `excludeOverlaps` does.
fn exclusive(segments: Vec<Segment>, min_duration: f64) -> Vec<Segment> {
    let mut result: Vec<Segment> = Vec::with_capacity(segments.len());
    let mut previous_end = f64::NEG_INFINITY;
    for mut segment in segments {
        let original = (segment.end - segment.start).max(0.0);
        segment.start = segment.start.max(previous_end);
        let duration = segment.end - segment.start;
        if duration <= 0.0 || duration < min_duration {
            continue;
        }
        if original > 0.0 {
            segment.quality = (segment.quality * duration / original).clamp(0.0, 1.0);
        }
        previous_end = segment.end;
        result.push(segment);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::SegmentationGeometry;
    use crate::extraction::WindowEmbedding;
    use crate::segmentation::WindowActivity;

    const GEOMETRY: SegmentationGeometry = SegmentationGeometry::PYANNOTE_3_0;

    fn segment(cluster: usize, start: f64, end: f64, quality: f64) -> Segment {
        Segment {
            cluster,
            start,
            end,
            quality,
        }
    }

    #[test]
    fn gaps_close_only_within_a_cluster_and_under_the_limit() {
        let joined = close_gaps(
            vec![
                segment(0, 0.0, 1.0, 1.0),
                segment(1, 1.0, 2.0, 0.5),
                segment(0, 1.05, 3.0, 0.5),
                segment(0, 4.0, 5.0, 1.0),
            ],
            0.1,
        );
        assert_eq!(joined.len(), 3);
        assert_eq!((joined[0].start, joined[0].end), (0.0, 3.0));
        assert!((joined[0].quality - (1.0 + 0.5 * 1.95) / 2.95).abs() < 1e-9);
        assert_eq!(joined[2].start, 4.0);
    }

    #[test]
    fn overlaps_resolve_towards_the_earlier_speaker_and_short_turns_go() {
        let turns = exclusive(
            vec![
                segment(0, 0.0, 5.0, 1.0),
                segment(1, 4.0, 8.0, 1.0),
                segment(2, 7.5, 8.2, 1.0),
                segment(0, 8.0, 8.5, 1.0),
            ],
            1.0,
        );
        assert_eq!(turns.len(), 2);
        assert_eq!((turns[1].start, turns[1].end), (5.0, 8.0));
        assert!(
            (turns[1].quality - 0.75).abs() < 1e-9,
            "three of four seconds survived"
        );
    }

    /// Two windows over 12 s of one speaker, embedded in both, agree: one
    /// turn, quality 1, spanning the speech.
    #[test]
    fn agreeing_windows_yield_one_turn_with_full_quality() {
        let frames_a: Vec<u8> = (0..589).map(|f| u8::from((59..530).contains(&f))).collect();
        let frames_b: Vec<u8> = (0..GEOMETRY.valid_frames(192_000 - 32_000))
            .map(|f| u8::from(f < 411))
            .collect();
        let analysis = Analysis {
            geometry: GEOMETRY.clone(),
            total_samples: 192_000,
            activities: vec![
                WindowActivity {
                    window: 0,
                    offset: 0,
                    frames: frames_a,
                },
                WindowActivity {
                    window: 1,
                    offset: 32_000,
                    frames: frames_b,
                },
            ],
            embeddings: vec![
                WindowEmbedding {
                    window: 0,
                    local_speaker: 0,
                    start: 1.0,
                    end: 9.0,
                    embedding: vec![1.0],
                },
                WindowEmbedding {
                    window: 1,
                    local_speaker: 0,
                    start: 2.0,
                    end: 9.0,
                    embedding: vec![1.0],
                },
            ],
        };
        let turns = turns(&analysis, &[Some(0), Some(0)], &TimelineRules::default());
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].speaker_label, "S1");
        assert!((turns[0].start - 1.0).abs() < 0.05, "{}", turns[0].start);
        assert!((turns[0].end - 9.0).abs() < 0.05, "{}", turns[0].end);
        assert!((turns[0].quality - 1.0).abs() < 1e-6);
        assert_eq!(
            super::turns(&analysis, &[None, None], &TimelineRules::default()).len(),
            0
        );
    }
}
