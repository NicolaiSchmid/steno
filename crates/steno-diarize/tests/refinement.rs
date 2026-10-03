//! `Tests/StenoSpeechTests/ClusterRefinementTests.swift` ported: the
//! merge order, the 30 s rule, the 0.30 absorb rule, label survival.

mod common;

use common::{audio, cluster, range, sample_index};
use steno_core::{AudioBuffer16k, BoxError, Embedding, SpeakerCluster, TimeRange};
use steno_diarize::refinement::{Rules, SliceEmbedder, concatenated, refine};

/// Reads the speaker off the samples: every range of one speaker in these
/// tests is filled with one constant c, and the embedding of a slice is the
/// unit vector whose axis c carries how many samples of c it holds. Two
/// slices of the same constant have cosine 1, of different constants 0,
/// and a mixed slice sits in between with the mix as weights. Records the
/// duration of every slice it embedded.
#[derive(Default)]
struct FakeSliceEmbedder {
    embedded_durations: Vec<f64>,
}

impl SliceEmbedder for FakeSliceEmbedder {
    fn embedding(&mut self, audio: &AudioBuffer16k) -> Result<Option<Embedding>, BoxError> {
        self.embedded_durations.push(audio.duration());
        let mut values = vec![0.0f32; Embedding::DIMENSION];
        for sample in audio.samples.iter().filter(|sample| **sample > 0.0) {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let axis = *sample as usize;
            values[axis] += 1.0;
        }
        Ok(values
            .iter()
            .any(|value| *value > 0.0)
            .then(|| Embedding(values).normalized()))
    }
}

fn c(label: &str, ranges: &[TimeRange], axis: usize) -> SpeakerCluster {
    cluster(label, ranges.to_vec(), axis, 1.0)
}

fn run(
    clusters: &[SpeakerCluster],
    buffer: &AudioBuffer16k,
    rules: &Rules,
) -> (Vec<SpeakerCluster>, FakeSliceEmbedder) {
    let mut embedder = FakeSliceEmbedder::default();
    let refined = refine(clusters, buffer, rules, &mut embedder).unwrap();
    (refined, embedder)
}

fn labels(clusters: &[SpeakerCluster]) -> Vec<&str> {
    clusters.iter().map(|c| c.label.as_str()).collect()
}

/// One voice as a main cluster and a cluster of asides: re-embedded, both
/// are the same constant, cosine 1, and they merge into one speaker with
/// the union's ranges, the big cluster's clip and the re-embedded vector.
#[test]
fn short_turns_of_one_voice_merge_into_the_main_cluster() {
    let main = [range(0.0, 60.0), range(100.0, 160.0)];
    let asides = [range(70.0, 90.0), range(170.0, 185.0)];
    let layout: Vec<(f32, TimeRange)> = main.iter().chain(&asides).map(|r| (1.0, *r)).collect();
    let buffer = audio(&layout, 200.0);
    let (refined, embedder) = run(
        &[c("Speaker 1", &main, 5), c("Speaker 2", &asides, 9)],
        &buffer,
        &Rules::default(),
    );
    assert_eq!(refined.len(), 1);
    assert_eq!(refined[0].label, "Speaker 1");
    assert_eq!(
        refined[0].ranges,
        vec![
            range(0.0, 60.0),
            range(70.0, 90.0),
            range(100.0, 160.0),
            range(170.0, 185.0)
        ]
    );
    assert_eq!(refined[0].sample_clip_range, None);
    // The embedding is the re-embedded union, not the window mean.
    assert!(refined[0].embedding.as_ref().unwrap().0[1] > 0.99);
    // Two clusters embedded, then the union once.
    assert_eq!(embedder.embedded_durations.len(), 3);
}

/// Two people who each speak for long: different constants, cosine 0,
/// nothing merges, and the labels follow the order of first speech.
#[test]
fn different_voices_stay_apart_and_are_labelled_by_first_speech() {
    let anna = [range(40.0, 100.0)];
    let ben = [range(0.0, 35.0), range(110.0, 150.0)];
    let buffer = audio(&[(1.0, anna[0]), (2.0, ben[0]), (2.0, ben[1])], 160.0);
    let (refined, _) = run(
        &[c("Speaker 1", &anna, 1), c("Speaker 2", &ben, 2)],
        &buffer,
        &Rules::default(),
    );
    assert_eq!(labels(&refined), ["Speaker 1", "Speaker 2"]);
    assert_eq!(refined[0].ranges, ben.to_vec(), "Ben opens the recording");
    assert_eq!(refined[1].ranges, anna.to_vec());
}

/// A cluster under thirty seconds is nobody on its own: it joins the
/// substantive cluster it sounds like (the big cluster's embedding, clip
/// and confidence stay), and one that sounds like nobody is dropped.
#[test]
fn small_clusters_are_absorbed_or_dropped() {
    let anna = [range(0.0, 60.0)];
    let aside = [range(70.0, 80.0)];
    let noise = [range(90.0, 95.0)];
    let buffer = audio(&[(1.0, anna[0]), (1.0, aside[0]), (3.0, noise[0])], 100.0);
    let (refined, _) = run(
        &[
            cluster("Speaker 1", anna.to_vec(), 1, 0.9),
            c("Speaker 2", &aside, 7),
            c("Speaker 3", &noise, 8),
        ],
        &buffer,
        &Rules::default(),
    );
    assert_eq!(refined.len(), 1);
    assert_eq!(refined[0].ranges, vec![range(0.0, 60.0), range(70.0, 80.0)]);
    assert_eq!(refined[0].cluster_confidence, 0.9);
    assert!(
        refined[0].embedding.as_ref().unwrap().0[1] > 0.99,
        "the speaker's own embedding stays"
    );
}

/// Three clusters of one voice with different admixtures: C first in the
/// input with cosine 0.86 to A, but A and B at 0.92 are the closest pair,
/// so their union is the first one embedded (110 s), not C's with A
/// (100 s).
#[test]
fn the_closest_pair_merges_first() {
    let buffer = audio(
        &[
            (1.0, range(0.0, 25.0)),
            (3.0, range(25.0, 40.0)),
            (1.0, range(50.0, 110.0)),
            (1.0, range(120.0, 155.0)),
            (2.0, range(155.0, 170.0)),
        ],
        180.0,
    );
    let (refined, embedder) = run(
        &[
            c("Speaker 1", &[range(0.0, 40.0)], 7),
            c("Speaker 2", &[range(50.0, 110.0)], 8),
            c("Speaker 3", &[range(120.0, 170.0)], 9),
        ],
        &buffer,
        &Rules::default(),
    );
    assert_eq!(&embedder.embedded_durations[..4], [40.0, 60.0, 50.0, 110.0]);
    assert_eq!(refined.len(), 1);
    assert_eq!(
        refined[0].ranges,
        vec![range(0.0, 40.0), range(50.0, 110.0), range(120.0, 170.0)]
    );
}

/// Without a single substantive cluster (a nine-second fixture) nothing is
/// re-embedded and the mapping's clusters come back untouched.
#[test]
fn short_recordings_are_left_alone() {
    let clusters = [
        c("Speaker 1", &[range(0.0, 2.0), range(4.6, 6.6)], 1),
        c("Speaker 2", &[range(2.3, 4.3), range(7.0, 9.4)], 2),
    ];
    let (refined, embedder) = run(&clusters, &audio(&[], 10.0), &Rules::default());
    assert_eq!(refined, clusters.to_vec());
    assert_eq!(embedder.embedded_durations.len(), 0);
}

/// Merging is greedy by the highest cosine and re-embeds each union, so
/// three fragments of one voice collapse into one cluster while a second
/// voice survives; the embedder never sees more than the cap per slice.
#[test]
fn merges_greedily_and_caps_the_embedded_speech() {
    let first_voice = [
        [range(0.0, 200.0)],
        [range(210.0, 250.0)],
        [range(260.0, 300.0)],
    ];
    let second_voice = [range(310.0, 400.0)];
    let mut layout: Vec<(f32, TimeRange)> =
        first_voice.iter().flatten().map(|r| (1.0, *r)).collect();
    layout.extend(second_voice.iter().map(|r| (2.0, *r)));
    let buffer = audio(&layout, 410.0);
    let rules = Rules {
        maximum_embed_seconds: 120.0,
        ..Rules::default()
    };
    let (refined, embedder) = run(
        &[
            c("Speaker 1", &first_voice[0], 3),
            c("Speaker 2", &first_voice[1], 4),
            c("Speaker 3", &first_voice[2], 5),
            c("Speaker 4", &second_voice, 6),
        ],
        &buffer,
        &rules,
    );
    assert_eq!(labels(&refined), ["Speaker 1", "Speaker 2"]);
    assert_eq!(
        refined[0].ranges,
        vec![range(0.0, 200.0), range(210.0, 250.0), range(260.0, 300.0)]
    );
    assert_eq!(refined[1].ranges, second_voice.to_vec());
    let longest = embedder
        .embedded_durations
        .iter()
        .copied()
        .fold(0.0, f64::max);
    assert!(longest <= 120.0 + 1e-6);
}

/// The concatenation keeps time order, clips to the buffer and stops at
/// the cap.
#[test]
fn concatenation_orders_clips_and_caps() {
    let buffer = audio(&[(1.0, range(0.0, 10.0)), (2.0, range(10.0, 20.0))], 20.0);
    let joined = concatenated(&[range(15.0, 30.0), range(2.0, 4.0)], &buffer, 6.0);
    assert!((joined.duration() - 6.0).abs() < 1e-6);
    assert!(
        joined.samples[..sample_index(2.0)]
            .iter()
            .all(|s| (*s - 1.0).abs() < f32::EPSILON)
    );
    assert!(
        joined.samples[joined.len() - sample_index(4.0)..]
            .iter()
            .all(|s| (*s - 2.0).abs() < f32::EPSILON)
    );
}

/// The plan's constants: 30 s to count as a speaker, 180 s embedded at
/// most, merge at 0.60, absorb at 0.30.
#[test]
fn rules_default_to_the_plan_constants() {
    let rules = Rules::default();
    assert_eq!(rules.minimum_seconds, 30.0);
    assert_eq!(rules.maximum_embed_seconds, 180.0);
    assert_eq!(rules.merge_threshold, 0.60);
    assert_eq!(rules.absorb_threshold, 0.30);
}

/// Exactly thirty seconds of speech is a speaker; a hair under is not.
#[test]
fn thirty_seconds_of_speech_is_the_substantive_cut() {
    let anna = [range(0.0, 30.0)];
    let ben = [range(40.0, 69.9)];
    let buffer = audio(&[(1.0, anna[0]), (2.0, ben[0])], 80.0);
    let (refined, _) = run(
        &[c("Speaker 1", &anna, 1), c("Speaker 2", &ben, 2)],
        &buffer,
        &Rules::default(),
    );
    assert_eq!(refined.len(), 1);
    assert_eq!(
        refined[0].ranges,
        anna.to_vec(),
        "Ben's 29.9 s sound like nobody and are dropped"
    );
}

/// When the bigger half of a merge is the later cluster, the union keeps
/// its label and clip, so the pass must hand labels out again: the union
/// opens the recording and is "Speaker 1", the other voice moves up to
/// "Speaker 2".
#[test]
fn labels_are_reassigned_when_a_merge_keeps_the_later_label() {
    let opening = [range(0.0, 35.0)];
    let other = [range(40.0, 100.0)];
    let main = [range(110.0, 200.0)];
    let buffer = audio(&[(1.0, opening[0]), (2.0, other[0]), (1.0, main[0])], 210.0);
    let mut third = c("Speaker 3", &main, 3);
    third.sample_clip_range = Some(main[0]);
    let (refined, _) = run(
        &[
            c("Speaker 1", &opening, 1),
            c("Speaker 2", &other, 2),
            third,
        ],
        &buffer,
        &Rules::default(),
    );
    assert_eq!(labels(&refined), ["Speaker 1", "Speaker 2"]);
    assert_eq!(
        refined[0].ranges,
        vec![range(0.0, 35.0), range(110.0, 200.0)]
    );
    assert_eq!(
        refined[0].sample_clip_range,
        Some(main[0]),
        "the clip follows the bigger half"
    );
    assert_eq!(refined[1].ranges, other.to_vec());
}

/// A small cluster joins the substantive cluster with the highest cosine,
/// not the first one over the absorb cut: a ten-second aside that is
/// mostly Ben with a little Anna ends up with Ben.
#[test]
fn a_small_cluster_joins_the_closest_speaker_not_the_first_over_the_cut() {
    let anna = [range(0.0, 60.0)];
    let ben = [range(70.0, 130.0)];
    let aside = [range(140.0, 150.0)];
    let buffer = audio(
        &[
            (1.0, anna[0]),
            (2.0, ben[0]),
            (1.0, range(140.0, 144.0)),
            (2.0, range(144.0, 150.0)),
        ],
        160.0,
    );
    let (refined, _) = run(
        &[
            c("Speaker 1", &anna, 1),
            c("Speaker 2", &ben, 2),
            c("Speaker 3", &aside, 3),
        ],
        &buffer,
        &Rules::default(),
    );
    assert_eq!(labels(&refined), ["Speaker 1", "Speaker 2"]);
    assert_eq!(refined[0].ranges, anna.to_vec());
    assert_eq!(
        refined[1].ranges,
        vec![range(70.0, 130.0), range(140.0, 150.0)]
    );
}

/// The union of two merged clusters can embed to nothing (the one-speaker
/// pipeline hears no speech in it); the merged cluster then keeps the
/// embedding of the side with more speech instead of losing it.
#[test]
fn a_union_that_embeds_to_nothing_keeps_the_larger_sides_embedding() {
    let big = c("Speaker 1", &[range(0.0, 90.0)], 4);
    let mut small = c("Speaker 2", &[range(100.0, 140.0)], 4);
    small.embedding.as_mut().unwrap().0[5] = 1.0; // cosine 0.71 to `big`, over the merge cut
    let (refined, embedder) = run(&[big.clone(), small], &audio(&[], 150.0), &Rules::default());
    assert_eq!(refined.len(), 1);
    assert_eq!(refined[0].label, "Speaker 1");
    assert_eq!(
        refined[0].ranges,
        vec![range(0.0, 90.0), range(100.0, 140.0)]
    );
    assert_eq!(refined[0].embedding, big.embedding);
    // Silence yields no samples over zero, so nothing was offered.
    assert!(embedder.embedded_durations.len() <= 3);
}

/// A substantive cluster that came without an embedding and whose speech
/// the embedder hears nothing in stays a speaker of its own: the merge and
/// the absorb skip it, nothing drops it.
#[test]
fn a_substantive_cluster_without_an_embedding_survives_on_its_own() {
    let anna = [range(0.0, 60.0)];
    let silent = [range(70.0, 110.0)];
    let buffer = audio(&[(1.0, anna[0])], 120.0);
    let (refined, _) = run(
        &[
            c("Speaker 1", &anna, 1),
            SpeakerCluster {
                label: "Speaker 2".to_owned(),
                ranges: silent.to_vec(),
                embedding: None,
                cluster_confidence: 0.5,
                sample_clip_range: None,
            },
        ],
        &buffer,
        &Rules::default(),
    );
    assert_eq!(labels(&refined), ["Speaker 1", "Speaker 2"]);
    assert_eq!(refined[1].ranges, silent.to_vec());
    assert_eq!(refined[1].embedding, None);
}

/// With no substantive cluster carrying an embedding there is nothing to
/// compare a small cluster against, so it is dropped rather than attached
/// to an arbitrary speaker.
#[test]
fn small_clusters_are_dropped_when_no_speaker_has_an_embedding() {
    let silent = [range(0.0, 40.0)];
    let aside = [range(50.0, 60.0)];
    let buffer = audio(&[(2.0, aside[0])], 70.0);
    let (refined, _) = run(
        &[
            SpeakerCluster {
                label: "Speaker 1".to_owned(),
                ranges: silent.to_vec(),
                embedding: None,
                cluster_confidence: 1.0,
                sample_clip_range: None,
            },
            c("Speaker 2", &aside, 2),
        ],
        &buffer,
        &Rules::default(),
    );
    assert_eq!(refined.len(), 1);
    assert_eq!(refined[0].ranges, silent.to_vec());
    assert_eq!(refined[0].embedding, None);
}

/// Ranges past the end of the lane (a mapping a little longer than the
/// audio) yield no samples, and a slice without samples is never offered
/// to the embedder; the cluster keeps the embedding it came with.
#[test]
fn ranges_beyond_the_buffer_are_not_embedded() {
    let clusters = [c("Speaker 1", &[range(100.0, 140.0)], 1)];
    let (refined, embedder) = run(&clusters, &audio(&[], 50.0), &Rules::default());
    assert_eq!(refined, clusters.to_vec());
    assert_eq!(embedder.embedded_durations.len(), 0);
}

/// A fragment equally close to two speakers joins the first of them, as
/// Swift's `max(by:)` returns the first maximum.
#[test]
fn a_fragment_tied_between_two_speakers_joins_the_first() {
    let anna = [range(0.0, 60.0)];
    let ben = [range(70.0, 130.0)];
    let aside = [range(140.0, 150.0)];
    let buffer = audio(
        &[
            (1.0, anna[0]),
            (2.0, ben[0]),
            (1.0, range(140.0, 145.0)),
            (2.0, range(145.0, 150.0)),
        ],
        160.0,
    );
    let (refined, _) = run(
        &[
            c("Speaker 1", &anna, 1),
            c("Speaker 2", &ben, 2),
            c("Speaker 3", &aside, 3),
        ],
        &buffer,
        &Rules::default(),
    );
    assert_eq!(labels(&refined), ["Speaker 1", "Speaker 2"]);
    assert_eq!(
        refined[0].ranges,
        vec![range(0.0, 60.0), range(140.0, 150.0)]
    );
    assert_eq!(refined[1].ranges, ben.to_vec());
}
