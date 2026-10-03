//! From window votes to exclusive speaker turns, the reconstruction step
//! of the pyannote pipeline as sherpa-onnx and `FluidAudio` do it: every
//! window votes per frame once for each cluster its active local speakers
//! belong to, the
//! frame's speaker count is the rounded mean count over the windows that
//! cover it, the top clusters by votes are active, runs become segments,
//! short gaps close, short segments go, and overlaps are resolved in
//! favour of the earlier speaker so no two speakers share a moment, as
//! the transcript lanes require.
//!
//! Two deliberate departures from `FluidAudio`'s `OfflineReconstruction`,
//! listed with the others in the crate doc: the speaker count rounds half up (`(sum + n / 2) / n`) where Swift
//! rounds half to even, which differs only where an even number of
//! windows cover a frame and split evenly, the first and last eight
//! seconds of a lane, and there one window hearing a voice means a
//! speaker rather than nobody; and where at least one cluster has a vote,
//! only voted clusters are active, where Swift ranks the whole row and
//! fills the count with clusters nobody voted for. Where no cluster has a
//! vote on a frame the windows count as speech, the first `k` clusters are
//! active as Swift's ranking of an all-zero row has it: a one-to-two-second
//! utterance is alone in no window for the two seconds extraction asks,
//! so it is embedded nowhere and casts no vote, and without that fallback
//! it would be a hole no transcript segment could be attributed to.

use steno_core::SpeakerTurn;

use crate::extraction::Analysis;
use crate::segmentation::frame_span;
use crate::to_f64;

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

/// The turn and chunk label of cluster `index`: `S1`, `S2`, ... The
/// mapping joins turns and chunks by this string, so it is spelled here
/// and nowhere else.
#[must_use]
pub fn cluster_label(index: usize) -> String {
    format!("S{}", index + 1)
}

/// `assignments[i]` is the cluster of `analysis.embeddings[i]`, or `None`
/// when it was left out of clustering. Turns carry [`cluster_label`] of
/// their cluster and a quality in `0...1`: the share of the windows
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
    // the summed local speaker counts. A window casts one vote for a
    // cluster on a frame however many of its local speakers the cluster
    // holds, as `FluidAudio` takes the maximum activation per window and
    // cluster (`OfflineReconstruction.swift`), so a voice the model split
    // in two inside one window does not outvote the other windows. A vote
    // cell holds at most the windows covering the frame: five at the
    // default two-second step, 65 535 at a step of 0.15 ms, which no
    // configuration reaches; a finer step saturates rather than wrapping,
    // which can only flatten the ranking. Two bytes, not four, because the
    // matrix is the largest thing here (frames times clusters, 59 frames a
    // second, dozens of clusters before refinement on a group call).
    let mut votes = vec![0u16; total_frames * cluster_count];
    let mut coverage = vec![0u32; total_frames];
    let mut counts = vec![0u32; total_frames];
    // Each window's embedded local speakers with their clusters.
    let mut members: Vec<Vec<(usize, usize)>> = vec![Vec::new(); analysis.activities.len()];
    for (embedding, cluster) in analysis.embeddings.iter().zip(assignments) {
        if let Some(cluster) = cluster {
            members[embedding.window].push((embedding.local_speaker, *cluster));
        }
    }
    for (activity, members) in analysis.activities.iter().zip(&members) {
        let base = geometry.global_frame(activity.offset);
        for (local, mask) in activity.frames.iter().enumerate() {
            let frame = base + local;
            if frame >= total_frames {
                break;
            }
            coverage[frame] += 1;
            counts[frame] += mask.count_ones();
            for (index, &(speaker, cluster)) in members.iter().enumerate() {
                let counted = members[..index].iter().any(|&(other, earlier)| {
                    earlier == cluster && activity.is_active(local, other)
                });
                if activity.is_active(local, speaker) && !counted {
                    let cell = &mut votes[frame * cluster_count + cluster];
                    *cell = cell.saturating_add(1);
                }
            }
        }
    }
    // Active clusters per frame, laid out like `votes`: the top `k` by
    // votes, `k` the rounded mean speaker count, only clusters somebody
    // voted for; the first `k` when nobody did (see the module doc).
    let mut active = vec![false; total_frames * cluster_count];
    let mut ranked: Vec<usize> = Vec::with_capacity(cluster_count);
    for frame in 0..total_frames {
        if coverage[frame] == 0 {
            continue;
        }
        let expected = (counts[frame] + coverage[frame] / 2) / coverage[frame];
        let k = (expected as usize).min(cluster_count.min(geometry.num_speakers));
        let row = &votes[frame * cluster_count..(frame + 1) * cluster_count];
        ranked.clear();
        ranked.extend((0..cluster_count).filter(|c| row[*c] > 0));
        ranked.sort_by(|lhs, rhs| row[*rhs].cmp(&row[*lhs]).then(lhs.cmp(rhs)));
        ranked.truncate(k);
        if ranked.is_empty() {
            ranked.extend(0..k);
        }
        for cluster in &ranked {
            active[frame * cluster_count + cluster] = true;
        }
    }
    // Runs per cluster become segments with their mean vote share.
    let mut runs: Vec<Run> = Vec::new();
    for cluster in 0..cluster_count {
        let mut run: Option<(usize, f64, usize)> = None;
        for frame in 0..=total_frames {
            let is_active = frame < total_frames && active[frame * cluster_count + cluster];
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
                runs.push(Run {
                    cluster,
                    start: time_start,
                    end: time_end,
                    quality: (sum / to_f64(length)).clamp(0.0, 1.0),
                });
            }
        }
    }
    runs.sort_by(|lhs, rhs| {
        lhs.start
            .total_cmp(&rhs.start)
            .then(lhs.cluster.cmp(&rhs.cluster))
    });
    let runs = close_gaps(runs, rules.min_duration_off);
    let runs = exclusive(runs, rules.min_duration_on);
    runs.into_iter()
        .map(|run| SpeakerTurn {
            speaker_label: cluster_label(run.cluster),
            start: run.start,
            end: run.end,
            // Qualities lie in 0...1; the narrowing is intended.
            #[allow(clippy::cast_possible_truncation)]
            quality: run.quality as f32,
        })
        .collect()
}

/// A stretch of frames one cluster is active in, before the gaps close
/// and the overlaps resolve; a turn in the making. Not a transcript
/// segment.
#[derive(Debug, Clone, PartialEq)]
struct Run {
    cluster: usize,
    start: f64,
    end: f64,
    quality: f64,
}

/// Joins a run into the run immediately before it in start order when
/// both belong to one cluster and the gap is at most `max_gap`, quality
/// weighted by duration; `FluidAudio`'s `mergeSegments`. Only the
/// immediately preceding run counts: `A B A` with a short second gap
/// stays three runs, so `B` is not swallowed by the join of the two
/// `A`s. `runs` sorted by start.
fn close_gaps(runs: Vec<Run>, max_gap: f64) -> Vec<Run> {
    let mut result: Vec<Run> = Vec::with_capacity(runs.len());
    for run in runs {
        if let Some(last) = result
            .last_mut()
            .filter(|last| last.cluster == run.cluster)
            .filter(|last| run.start - last.end <= max_gap)
        {
            let lhs = (last.end - last.start).max(0.0);
            let rhs = (run.end - run.start).max(0.0);
            let total = lhs + rhs;
            last.quality = if total > 0.0 {
                (last.quality * lhs + run.quality * rhs) / total
            } else {
                f64::midpoint(last.quality, run.quality)
            };
            last.end = last.end.max(run.end);
        } else {
            result.push(run);
        }
    }
    result
}

/// Pushes each run's start to the previous run's end, drops what
/// vanishes or falls under `min_duration`, and scales the quality by what
/// survived, as `FluidAudio`'s `excludeOverlaps` does.
fn exclusive(runs: Vec<Run>, min_duration: f64) -> Vec<Run> {
    let mut result: Vec<Run> = Vec::with_capacity(runs.len());
    let mut previous_end = f64::NEG_INFINITY;
    for mut run in runs {
        let original = (run.end - run.start).max(0.0);
        run.start = run.start.max(previous_end);
        let duration = run.end - run.start;
        if duration <= 0.0 || duration < min_duration {
            continue;
        }
        if original > 0.0 {
            run.quality = (run.quality * duration / original).clamp(0.0, 1.0);
        }
        previous_end = run.end;
        result.push(run);
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

    fn run(cluster: usize, start: f64, end: f64, quality: f64) -> Run {
        Run {
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
                run(0, 0.0, 1.0, 1.0),
                run(0, 1.05, 3.0, 0.5),
                run(1, 3.0, 4.0, 0.5),
                run(0, 4.05, 5.0, 1.0),
                run(0, 6.0, 7.0, 1.0),
            ],
            0.1,
        );
        assert_eq!(joined.len(), 4);
        assert_eq!((joined[0].start, joined[0].end), (0.0, 3.0));
        assert!((joined[0].quality - (1.0 + 0.5 * 1.95) / 2.95).abs() < 1e-9);
        // Another cluster in between, then a gap over the limit: no joins.
        assert_eq!((joined[1].cluster, joined[2].start), (1, 4.05));
        assert_eq!(joined[3].start, 6.0);
    }

    /// `A B A` with the second `A` starting 0.05 s after the first ends:
    /// `B` sits between them in start order, so the two `A`s stay apart
    /// and `B` survives, as `FluidAudio`'s `mergeSegments` has it.
    #[test]
    fn a_run_joins_only_the_one_immediately_before_it() {
        let joined = close_gaps(
            vec![
                run(0, 0.0, 5.0, 1.0),
                run(1, 4.0, 7.0, 1.0),
                run(0, 5.05, 10.0, 1.0),
            ],
            0.1,
        );
        assert_eq!(joined.len(), 3);
        assert_eq!((joined[0].cluster, joined[0].end), (0, 5.0));
        assert_eq!((joined[1].cluster, joined[1].start), (1, 4.0));
        assert_eq!((joined[2].cluster, joined[2].start), (0, 5.05));
        let turns = exclusive(joined, 1.0);
        assert_eq!(turns.len(), 3, "{turns:?}");
        assert_eq!((turns[1].start, turns[1].end), (5.0, 7.0));
        assert_eq!((turns[2].start, turns[2].end), (7.0, 10.0));
    }

    #[test]
    fn overlaps_resolve_towards_the_earlier_speaker_and_short_turns_go() {
        let turns = exclusive(
            vec![
                run(0, 0.0, 5.0, 1.0),
                run(1, 4.0, 8.0, 1.0),
                run(2, 7.5, 8.2, 1.0),
                run(0, 8.0, 8.5, 1.0),
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

    /// One speaker for eight seconds, a second and a half of speech from
    /// nine to ten and a half that no window embeds (it is alone in none
    /// for two seconds), then the speaker again from twelve. The windows
    /// count one speaker over the short utterance, nobody voted for it,
    /// and it still becomes a turn of the first cluster, as `FluidAudio`
    /// has it, rather than a hole; its quality is zero because no vote
    /// backs it.
    #[test]
    fn speech_nobody_voted_for_goes_to_the_first_cluster() {
        // Window 0 (0 s to 10 s): speaker 0 to 8 s, the utterance from 9 s.
        let frames_a: Vec<u8> = (0..589)
            .map(|f| match f {
                0..=472 => 0b01,
                532..=588 => 0b10,
                _ => 0,
            })
            .collect();
        // Window 1 (10 s to 20 s): the utterance to 10.5 s, speaker 0
        // from 12 s.
        let frames_b: Vec<u8> = (0..589)
            .map(|f| match f {
                0..=28 => 0b10,
                118..=588 => 0b01,
                _ => 0,
            })
            .collect();
        let analysis = Analysis {
            geometry: GEOMETRY.clone(),
            total_samples: 320_000,
            activities: vec![
                WindowActivity {
                    window: 0,
                    offset: 0,
                    frames: frames_a,
                },
                WindowActivity {
                    window: 1,
                    offset: 160_000,
                    frames: frames_b,
                },
            ],
            embeddings: vec![
                WindowEmbedding {
                    window: 0,
                    local_speaker: 0,
                    start: 0.0,
                    end: 8.0,
                    embedding: vec![1.0],
                },
                WindowEmbedding {
                    window: 1,
                    local_speaker: 0,
                    start: 12.0,
                    end: 20.0,
                    embedding: vec![1.0],
                },
            ],
        };
        let turns = turns(&analysis, &[Some(0), Some(0)], &TimelineRules::default());
        assert_eq!(turns.len(), 3, "{turns:?}");
        assert!(turns.iter().all(|turn| turn.speaker_label == "S1"));
        let short = &turns[1];
        assert!((short.start - 9.0).abs() < 0.05, "{short:?}");
        assert!((short.end - 10.5).abs() < 0.05, "{short:?}");
        assert_eq!(short.quality, 0.0, "no vote backs it");
        assert!(turns[0].quality > 0.99 && turns[2].quality > 0.99);
    }

    /// Two local speakers of one window that clustered together vote once
    /// per frame, as `FluidAudio` takes the maximum per window and
    /// cluster. Window 0 hears local speakers 0 and 1 throughout, both in
    /// cluster 0; window 1, from two seconds, hears cluster 1. Where both
    /// windows cover, cluster 0 has one vote of two windows, not two, so
    /// its turn's quality is the mean of 1 over the first 119 frames and
    /// one half over the 470 shared ones.
    #[test]
    fn a_window_votes_once_for_a_cluster_whatever_its_local_speakers() {
        let analysis = Analysis {
            geometry: GEOMETRY.clone(),
            total_samples: 192_000,
            activities: vec![
                WindowActivity {
                    window: 0,
                    offset: 0,
                    frames: vec![0b11u8; 589],
                },
                WindowActivity {
                    window: 1,
                    offset: 32_000,
                    frames: vec![0b01u8; 589],
                },
            ],
            embeddings: [(0, 0), (0, 1), (1, 0)]
                .into_iter()
                .map(|(window, local_speaker)| WindowEmbedding {
                    window,
                    local_speaker,
                    start: 0.0,
                    end: 10.0,
                    embedding: vec![1.0],
                })
                .collect(),
        };
        let turns = turns(
            &analysis,
            &[Some(0), Some(0), Some(1)],
            &TimelineRules::default(),
        );
        assert_eq!(turns.len(), 2, "{turns:?}");
        let first = &turns[0];
        assert_eq!(first.speaker_label, "S1");
        let expected = (119.0 + 470.0 * 0.5) / 589.0;
        assert!(
            (f64::from(first.quality) - expected).abs() < 1e-4,
            "{first:?}, expected {expected}"
        );
    }

    /// Two windows disagree on how many speak: the first hears two, the
    /// second one. Where both cover, the mean count is 1.5, which rounds
    /// to 2, so the second speaker keeps the floor until the first window
    /// ends; a truncating mean would hand those eight seconds to the
    /// first speaker.
    #[test]
    fn the_speaker_count_is_the_rounded_mean_over_the_covering_windows() {
        // Window 0: local speaker 1 alone for 119 frames, then both.
        let frames_a: Vec<u8> = (0..589)
            .map(|f| if f < 119 { 0b10 } else { 0b11 })
            .collect();
        // Window 1 (from 2 s): local speaker 0 alone throughout.
        let frames_b = vec![0b01u8; 589];
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
                    start: 2.0,
                    end: 10.0,
                    embedding: vec![1.0],
                },
                WindowEmbedding {
                    window: 0,
                    local_speaker: 1,
                    start: 0.0,
                    end: 10.0,
                    embedding: vec![1.0],
                },
                WindowEmbedding {
                    window: 1,
                    local_speaker: 0,
                    start: 2.0,
                    end: 12.0,
                    embedding: vec![1.0],
                },
            ],
        };
        let turns = turns(
            &analysis,
            &[Some(0), Some(1), Some(0)],
            &TimelineRules::default(),
        );
        let second = turns.iter().find(|t| t.speaker_label == "S2").unwrap();
        let first = turns.iter().find(|t| t.speaker_label == "S1").unwrap();
        assert!(second.start < 0.05, "{second:?}");
        assert!((second.end - 9.95).abs() < 0.05, "{second:?}");
        assert!((first.start - second.end).abs() < 1e-9, "{first:?}");
        assert!((first.end - 12.0).abs() < 0.05, "{first:?}");
    }
}
