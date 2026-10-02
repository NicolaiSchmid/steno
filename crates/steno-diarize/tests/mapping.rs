//! `Tests/StenoSpeechTests/FluidDiarizerMappingTests.swift` ported:
//! the cluster embedding, the sample clip picker and the mapping.

mod common;

use common::{SplitMix64, chunk, range, turn, vector};
use steno_core::{ClusterChunk, Embedding, SpeakerTurn};
use steno_diarize::mapping::{
    CLIP_MINIMUM_SECONDS, CLIP_TARGET_SECONDS, ClipChoice, assigning_quality, cluster_embedding,
    merged, pick_clip, result,
};

fn c(label: &str, start: f64, end: f64, axis: usize) -> ClusterChunk {
    chunk(label, start, end, axis, 0.9, 1.0)
}

fn t(label: &str, start: f64, end: f64) -> SpeakerTurn {
    turn(label, start, end, 0.9)
}

#[test]
fn mean_is_duration_weighted_and_unit_length() {
    let chunks = [chunk("S1", 0.0, 3.0, 0, 0.9, 5.0), c("S1", 3.0, 4.0, 1)];
    let embedding = cluster_embedding(&chunks).unwrap();
    assert_eq!(embedding.0.len(), Embedding::DIMENSION);
    assert!((embedding.magnitude() - 1.0).abs() < 1e-5);
    assert!(embedding.0[0] > embedding.0[1], "three seconds beat one");
    // Each chunk is brought to unit length first, so only the durations
    // weigh: axis 0 gets 3, axis 1 gets 1. The norm of 5 plays no part.
    assert!((embedding.0[0] / embedding.0[1] - 3.0).abs() < 1e-3);
}

#[test]
fn a_high_norm_chunk_does_not_steer_the_mean() {
    let chunks = [c("S1", 0.0, 2.0, 0), chunk("S1", 2.0, 4.0, 1, 0.9, 20.0)];
    let embedding = cluster_embedding(&chunks).unwrap();
    assert!((embedding.0[0] - embedding.0[1]).abs() < 1e-5);
    assert!((embedding.0[0] - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-3);
}

#[test]
fn chunks_with_the_wrong_dimension_are_ignored() {
    let odd = ClusterChunk {
        speaker_label: "S1".to_owned(),
        start: 0.0,
        end: 1.0,
        embedding: vec![1.0, 0.0],
        quality: 1.0,
    };
    assert_eq!(cluster_embedding(&[odd]), None);
    assert_eq!(cluster_embedding(&[]), None);
    assert_eq!(
        cluster_embedding(&[c("S1", 1.0, 1.0, 0)]),
        None,
        "zero duration"
    );
}

#[test]
fn picks_the_longest_range_capped_to_ten_seconds_around_the_best_chunk() {
    let ranges = [range(0.0, 4.0), range(10.0, 40.0), range(50.0, 52.0)];
    let chunks = [
        chunk("S1", 12.0, 14.0, 0, 0.5, 1.0),
        chunk("S1", 30.0, 32.0, 0, 0.95, 1.0),
        chunk("S1", 50.0, 52.0, 0, 0.99, 1.0), // outside the longest range
    ];
    let choice = pick_clip(&ranges, &chunks);
    let clip = choice.range.unwrap();
    assert_eq!(clip.upper - clip.lower, 10.0);
    assert_eq!(
        (clip.lower, clip.upper),
        (26.0, 36.0),
        "centred on the 30...32 chunk"
    );
    assert!((choice.cluster_confidence - (0.5 * 2.0 + 0.95 * 2.0 + 0.99 * 2.0) / 6.0).abs() < 1e-5);
}

#[test]
fn clip_is_shifted_inside_the_range_near_its_edges() {
    let choice = pick_clip(&[range(0.0, 20.0)], &[chunk("S1", 0.0, 1.0, 0, 1.0, 1.0)]);
    assert_eq!(choice.range, Some(range(0.0, 10.0)));
    let late = pick_clip(&[range(0.0, 20.0)], &[chunk("S1", 19.0, 20.0, 0, 1.0, 1.0)]);
    assert_eq!(late.range, Some(range(10.0, 20.0)));
}

#[test]
fn short_longest_range_halves_confidence_and_shortens_the_clip() {
    let choice = pick_clip(
        &[range(0.0, 2.0), range(5.0, 6.5)],
        &[chunk("S1", 0.0, 2.0, 0, 0.8, 1.0)],
    );
    assert_eq!(choice.range, Some(range(0.0, 2.0)));
    assert!((choice.cluster_confidence - 0.4).abs() < 1e-6);
}

#[test]
fn without_chunks_the_clip_is_centred_and_confidence_zero() {
    let choice = pick_clip(&[range(0.0, 30.0)], &[]);
    assert_eq!(choice.range, Some(range(10.0, 20.0)));
    assert_eq!(choice.cluster_confidence, 0.0);
    assert_eq!(
        pick_clip(&[], &[]),
        ClipChoice {
            range: None,
            cluster_confidence: 0.0
        }
    );
}

#[test]
fn clusters_are_labelled_in_order_of_first_speech_with_merged_ranges() {
    let turns = [
        t("S7", 5.0, 8.0),
        t("S2", 0.0, 2.0),
        t("S2", 2.0, 4.5),
        t("S7", 8.0, 9.0),
        t("S2", 12.0, 13.0),
    ];
    let chunks = [
        c("S2", 0.0, 4.0, 0),
        c("S7", 5.0, 9.0, 1),
        c("S2", 12.0, 13.0, 0),
    ];
    let result = result(&turns, &chunks);
    let labels: Vec<&str> = result.clusters.iter().map(|c| c.label.as_str()).collect();
    assert_eq!(labels, ["Speaker 1", "Speaker 2"]);
    let first = &result.clusters[0];
    assert_eq!(
        first.ranges,
        vec![range(0.0, 4.5), range(12.0, 13.0)],
        "touching turns merge, the gap stays"
    );
    assert_eq!(result.clusters[1].ranges, vec![range(5.0, 9.0)]);
    let embedding = first.embedding.as_ref().unwrap();
    assert!((embedding.magnitude() - 1.0).abs() < 1e-5);
    assert!(embedding.0[0] > 0.99, "axis 0 only");
    assert!(result.clusters[1].embedding.as_ref().unwrap().0[1] > 0.99);
    assert_eq!(first.sample_clip_range, Some(range(0.0, 4.5)));
    assert!((first.cluster_confidence - 0.9).abs() < 1e-6);
}

#[test]
fn chunk_quality_comes_from_the_overlapping_turn() {
    let turns = [turn("S1", 0.0, 5.0, 0.3), turn("S1", 5.0, 10.0, 0.7)];
    let chunks = assigning_quality(
        &[
            c("S1", 0.0, 4.0, 0),
            c("S1", 4.0, 9.0, 0),
            c("S1", 20.0, 21.0, 0),
        ],
        &turns,
    );
    let qualities: Vec<f32> = chunks.iter().map(|c| c.quality).collect();
    assert_eq!(qualities, [0.3, 0.7, 1.0]);
    // `result` assigns first: the confidence is the duration-weighted mean of
    // the turns' qualities, whatever the chunks arrived with.
    let result = result(
        &turns,
        &[chunk("S1", 0.0, 4.0, 0, 0.0, 1.0), c("S1", 4.0, 9.0, 0)],
    );
    let expected: f32 = 4.7 / 9.0;
    assert!((result.clusters[0].cluster_confidence - expected).abs() < 1e-5);
}

#[test]
fn short_speakers_are_penalised_and_turnless_chunk_labels_are_not_speakers() {
    let turns = [turn("S1", 0.0, 2.0, 1.0), turn("S1", 4.0, 5.0, 1.0)];
    let chunks = [
        chunk("S1", 0.0, 2.0, 0, 1.0, 1.0),
        chunk("S9", 30.0, 31.0, 1, 0.5, 1.0),
    ];
    let result = result(&turns, &chunks);
    assert_eq!(result.clusters.len(), 1, "S9 lost every frame vote");
    assert_eq!(result.clusters[0].label, "Speaker 1");
    assert!(
        (result.clusters[0].cluster_confidence - 0.5).abs() < 1e-6,
        "under three seconds: halved"
    );
}

/// The shape the real model produced on an Anna + Daniel `say` fixture:
/// two speakers alternate in the turns, and a third cluster label exists
/// only in the chunks with a window-sized span over both. It must not
/// become a speaker, and no two speakers' ranges may overlap.
#[test]
fn a_chunk_only_label_spanning_the_window_does_not_overlap_the_real_speakers() {
    let turns = [
        t("S1", 0.0, 1.94),
        t("S2", 2.28, 4.35),
        t("S1", 4.67, 6.64),
        t("S2", 6.98, 9.37),
    ];
    let chunks = [
        c("S1", 0.0, 6.64, 0),
        c("S2", 2.28, 9.37, 1),
        c("S3", 4.00, 9.35, 2),
    ];
    let result = result(&turns, &chunks);
    let labels: Vec<&str> = result.clusters.iter().map(|c| c.label.as_str()).collect();
    assert_eq!(labels, ["Speaker 1", "Speaker 2"]);
    assert_eq!(
        result.clusters[0].ranges,
        vec![range(0.0, 1.94), range(4.67, 6.64)]
    );
    assert_eq!(
        result.clusters[1].ranges,
        vec![range(2.28, 4.35), range(6.98, 9.37)]
    );
    let all: Vec<(&str, _)> = result
        .clusters
        .iter()
        .flat_map(|cluster| {
            cluster
                .ranges
                .iter()
                .map(move |r| (cluster.label.as_str(), *r))
        })
        .collect();
    for lhs in &all {
        for rhs in all.iter().filter(|rhs| rhs.0 != lhs.0) {
            assert!(
                !steno_diarize::mapping::overlaps(&lhs.1, &rhs.1),
                "{lhs:?} overlaps {rhs:?}"
            );
        }
    }
}

#[test]
fn without_chunks_confidence_comes_from_turns() {
    let result = result(&[turn("S1", 0.0, 6.0, 0.8), turn("S1", 6.0, 8.0, 0.4)], &[]);
    assert_eq!(result.clusters.len(), 1);
    assert_eq!(result.clusters[0].embedding, None);
    assert!((result.clusters[0].cluster_confidence - 0.7).abs() < 1e-6);
    assert_eq!(result.clusters[0].sample_clip_range, Some(range(0.0, 8.0)));
}

#[test]
fn one_speaker_gives_one_cluster_with_the_clip_inside_the_range() {
    let turns = [turn("S1", 0.5, 6.0, 0.8), turn("S1", 6.0, 14.0, 0.9)];
    let chunks = [
        chunk("S1", 0.5, 7.0, 3, 0.8, 1.0),
        chunk("S1", 7.0, 14.0, 3, 0.9, 1.0),
    ];
    let result = result(&turns, &chunks);
    assert_eq!(result.clusters.len(), 1);
    let only = &result.clusters[0];
    assert_eq!(only.label, "Speaker 1");
    assert_eq!(only.ranges, vec![range(0.5, 14.0)]);
    let clip = only.sample_clip_range.unwrap();
    assert_eq!(clip.upper - clip.lower, 10.0);
    assert!(clip.lower >= 0.5 && clip.upper <= 14.0);
    assert!(
        clip.lower <= 10.5 && 10.5 <= clip.upper,
        "centred on the better second chunk"
    );
    #[allow(clippy::cast_possible_truncation)]
    let expected = ((0.8 * 6.5 + 0.9 * 7.0) / 13.5) as f32;
    assert!((only.cluster_confidence - expected).abs() < 1e-5);
    assert!(only.embedding.as_ref().unwrap().0[3] > 0.99);
}

/// A two-second recording: everything is under the three-second floor, so
/// the clip is the whole range and the confidence is halved, and no clip
/// ever reaches past the audio.
#[test]
fn short_audio_keeps_clips_inside_the_audio_and_penalises_everyone() {
    let turns = [turn("S1", 0.0, 1.2, 1.0), turn("S2", 1.2, 2.0, 1.0)];
    let chunks = [
        chunk("S1", 0.0, 1.2, 0, 1.0, 1.0),
        chunk("S2", 1.2, 2.0, 1, 1.0, 1.0),
    ];
    let result = result(&turns, &chunks);
    assert_eq!(result.clusters.len(), 2);
    assert_eq!(result.clusters[0].sample_clip_range, Some(range(0.0, 1.2)));
    assert_eq!(result.clusters[1].sample_clip_range, Some(range(1.2, 2.0)));
    assert!(
        result
            .clusters
            .iter()
            .all(|c| (c.cluster_confidence - 0.5).abs() < 1e-6)
    );
    assert!(
        result
            .clusters
            .iter()
            .all(|c| c.sample_clip_range.unwrap().upper <= 2.0)
    );
}

/// Core documents `sample_clip_range` as at most ten seconds; the picker's
/// constants are the one place that number lives and no config can raise it.
#[test]
fn the_clip_never_exceeds_cores_ten_second_cap() {
    assert_eq!(CLIP_TARGET_SECONDS, 10.0);
    assert_eq!(CLIP_MINIMUM_SECONDS, 3.0);
    let result = result(&[turn("S1", 0.0, 300.0, 1.0)], &[c("S1", 100.0, 102.0, 0)]);
    let clip = result.clusters[0].sample_clip_range.unwrap();
    assert_eq!(clip.upper - clip.lower, 10.0);
    assert_eq!(clip, range(96.0, 106.0), "centred on the one chunk");
}

/// Invariants over generated diarizations, whatever the shape: labels are
/// sequential in order of first speech, ranges are sorted and disjoint,
/// every clip lies inside one of its cluster's ranges and within the
/// target, embeddings are unit vectors, confidence is in `0...1`. Half the
/// recordings also carry a chunk whose label has no turn (`S0`, spanning
/// the window over whoever spoke), the phantom the real model produced; it
/// must not become a cluster.
#[test]
fn invariants_hold_over_generated_inputs() {
    let mut rng = SplitMix64(2026);
    for _ in 0..40 {
        let speakers = rng.int(1, 4);
        let mut turns: Vec<SpeakerTurn> = Vec::new();
        let mut chunks: Vec<ClusterChunk> = Vec::new();
        let mut cursor = 0.0;
        for _ in 0..rng.int(1, 12) {
            let label = format!("S{}", rng.int(1, speakers));
            let length = rng.float(0.2, 12.0);
            #[allow(clippy::cast_possible_truncation)]
            let quality = rng.unit() as f32;
            turns.push(turn(&label, cursor, cursor + length, quality));
            if rng.bool() {
                chunks.push(ClusterChunk {
                    speaker_label: label.clone(),
                    start: cursor,
                    end: cursor + length.min(5.0),
                    embedding: vector(usize::try_from(rng.below(8)).unwrap(), 3.0),
                    quality: 1.0,
                });
            }
            cursor += length + rng.float(0.0, 1.0);
        }
        if rng.bool() {
            chunks.push(ClusterChunk {
                speaker_label: "S0".to_owned(),
                start: (cursor - 10.0).max(0.0),
                end: cursor,
                embedding: vector(8, 3.0),
                quality: 1.0,
            });
        }
        let result = result(&turns, &chunks);
        let mut sorted = turns.clone();
        sorted.sort_by(|lhs, rhs| lhs.start.total_cmp(&rhs.start));
        let mut order: Vec<&str> = Vec::new();
        for turn in &sorted {
            if !order.contains(&turn.speaker_label.as_str()) {
                order.push(turn.speaker_label.as_str());
            }
        }
        assert_eq!(result.clusters.len(), order.len());
        let labels: Vec<String> = result.clusters.iter().map(|c| c.label.clone()).collect();
        let expected: Vec<String> = (1..=order.len()).map(|i| format!("Speaker {i}")).collect();
        assert_eq!(labels, expected);
        let labelled: Vec<(&str, _)> = result
            .clusters
            .iter()
            .flat_map(|cluster| {
                cluster
                    .ranges
                    .iter()
                    .map(move |r| (cluster.label.as_str(), *r))
            })
            .collect();
        for lhs in &labelled {
            for rhs in labelled
                .iter()
                .filter(|rhs| rhs.0 != lhs.0 && lhs.1.lower < rhs.1.lower)
            {
                assert!(
                    lhs.1.upper <= rhs.1.lower,
                    "speakers overlap: {lhs:?} {rhs:?}"
                );
            }
        }
        for cluster in &result.clusters {
            for pair in cluster.ranges.windows(2) {
                assert!(pair[0].upper < pair[1].lower, "ranges sorted and disjoint");
            }
            let clip = cluster.sample_clip_range.expect("every cluster has a clip");
            assert!(clip.upper - clip.lower <= 10.0 + 1e-9);
            assert!(
                cluster
                    .ranges
                    .iter()
                    .any(|r| r.lower <= clip.lower + 1e-9 && clip.upper <= r.upper + 1e-9),
                "{clip:?} outside {:?}",
                cluster.ranges
            );
            assert!((0.0..=1.0).contains(&cluster.cluster_confidence));
            if let Some(embedding) = &cluster.embedding {
                assert!((embedding.magnitude() - 1.0).abs() < 1e-4);
            }
        }
    }
}

#[test]
fn empty_input_gives_no_clusters() {
    assert_eq!(result(&[], &[]).clusters.len(), 0);
    assert_eq!(merged(&[]), vec![]);
    assert_eq!(
        merged(&[range(3.0, 4.0), range(0.0, 1.0), range(1.0, 2.0)]),
        vec![range(0.0, 2.0), range(3.0, 4.0)]
    );
}
