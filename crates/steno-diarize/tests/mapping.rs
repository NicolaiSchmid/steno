//! The cluster embedding, the sample clip picker and the mapping, plus
//! generated-input, tie and non-finite cases.

mod common;

use common::{SplitMix64, chunk, range, turn, vector};
use steno_core::{ClusterChunk, Embedding, SpeakerTurn};
use steno_diarize::mapping::{
    assigning_quality, cluster_embedding, mean_quality, merged, pick_clip, result,
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
    // Each chunk is brought to unit length first, so only durations weigh.
    assert!((embedding.0[0] / embedding.0[1] - 3.0).abs() < 1e-3);
    let loud = cluster_embedding(&[c("S1", 0.0, 2.0, 0), chunk("S1", 2.0, 4.0, 1, 0.9, 20.0)]);
    let loud = loud.unwrap();
    assert!(
        (loud.0[0] - loud.0[1]).abs() < 1e-5,
        "a high-norm chunk does not steer"
    );
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

/// The clip: the longest range, cut to ten seconds around its best chunk and
/// shifted inside the range; a range under three seconds halves the
/// confidence, and without chunks the clip is centred at confidence zero.
#[test]
fn the_clip_is_cut_from_the_longest_range_around_the_best_chunk() {
    let one = |start, end, quality| chunk("S1", start, end, 0, quality, 1.0);
    let cases: [(&[_], &[_], Option<_>, f32); 6] = [
        (
            &[range(0.0, 4.0), range(10.0, 40.0), range(50.0, 52.0)],
            // The last chunk lies outside the longest range.
            &[
                one(12.0, 14.0, 0.5),
                one(30.0, 32.0, 0.95),
                one(50.0, 52.0, 0.99),
            ],
            Some(range(26.0, 36.0)),
            (0.5 + 0.95 + 0.99) / 3.0,
        ),
        (
            &[range(0.0, 20.0)],
            &[one(0.0, 1.0, 1.0)],
            Some(range(0.0, 10.0)),
            1.0,
        ),
        (
            &[range(0.0, 20.0)],
            &[one(19.0, 20.0, 1.0)],
            Some(range(10.0, 20.0)),
            1.0,
        ),
        (
            &[range(0.0, 2.0), range(5.0, 6.5)],
            &[one(0.0, 2.0, 0.8)],
            Some(range(0.0, 2.0)),
            0.4,
        ),
        (&[range(0.0, 30.0)], &[], Some(range(10.0, 20.0)), 0.0),
        (&[], &[], None, 0.0),
    ];
    for (ranges, chunks, clip, confidence) in cases {
        let choice = pick_clip(ranges, chunks);
        assert_eq!(choice.range, clip, "{ranges:?}");
        assert!(
            (choice.cluster_confidence - confidence).abs() < 1e-5,
            "{ranges:?}: {choice:?}"
        );
    }
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
    // The confidence is the duration-weighted mean of the turns' qualities.
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

#[test]
fn without_chunks_confidence_comes_from_turns() {
    let result = result(&[turn("S1", 0.0, 6.0, 0.8), turn("S1", 6.0, 8.0, 0.4)], &[]);
    assert_eq!(result.clusters.len(), 1);
    assert_eq!(result.clusters[0].embedding, None);
    assert!((result.clusters[0].cluster_confidence - 0.7).abs() < 1e-6);
    assert_eq!(result.clusters[0].sample_clip_range, Some(range(0.0, 8.0)));
}

/// Invariants over generated diarizations: sequential labels, sorted
/// disjoint ranges, clips inside a range and within the target, unit
/// embeddings, confidence in `0...1`, and no cluster for a chunk-only label.
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

/// On a tie the earlier range and the earlier chunk win.
#[test]
fn ties_go_to_the_first_range_and_the_first_chunk() {
    let ranges = [range(0.0, 20.0), range(30.0, 50.0)];
    let chunks = [
        chunk("S1", 2.0, 4.0, 0, 0.9, 1.0),
        chunk("S1", 14.0, 16.0, 0, 0.9, 1.0),
        chunk("S1", 40.0, 42.0, 0, 0.9, 1.0),
    ];
    let clip = pick_clip(&ranges, &chunks).range.unwrap();
    assert_eq!(
        (clip.lower, clip.upper),
        (0.0, 10.0),
        "around the 2...4 chunk"
    );
    // A chunk overlapping two turns equally takes the first turn's quality.
    let turns = [t("S1", 0.0, 10.0), t("S1", 10.0, 20.0)];
    let turns = [
        SpeakerTurn {
            quality: 0.25,
            ..turns[0].clone()
        },
        SpeakerTurn {
            quality: 0.75,
            ..turns[1].clone()
        },
    ];
    let scored = assigning_quality(&[c("S1", 5.0, 15.0, 0)], &turns);
    assert!((scored[0].quality - 0.25).abs() < 1e-6);
}

/// A chunk with a non-finite vector or quality does not reach the embedding
/// or the confidence.
#[test]
fn non_finite_chunks_are_left_out() {
    let mut poisoned = c("S1", 0.0, 5.0, 0);
    poisoned.embedding[3] = f32::NAN;
    let sound = c("S1", 5.0, 10.0, 1);
    let embedding = cluster_embedding(&[poisoned.clone(), sound.clone()]).unwrap();
    assert!(embedding.0.iter().all(|value| value.is_finite()));
    assert!(embedding.0[1] > 0.99);
    assert!(cluster_embedding(&[poisoned.clone()]).is_none());
    let unscored = ClusterChunk {
        quality: f32::NAN,
        ..sound.clone()
    };
    assert!((mean_quality(&[unscored.clone(), sound]) - 0.9).abs() < 1e-6);
    assert_eq!(mean_quality(&[unscored]), 0.0);
}
