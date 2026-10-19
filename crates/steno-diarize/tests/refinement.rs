//! The cluster refinement tests: merge order, the 30 s rule, the 0.30 absorb
//! rule, label survival and ties.

mod common;

use common::{audio, cluster, range, sample_index};
use steno_core::{AudioBuffer16k, BoxError, Embedding, SpeakerCluster, TimeRange};
use steno_diarize::refinement::{Rules, SliceEmbedder, concatenated, refine};

/// Reads the speaker off the samples: each speaker's ranges hold one
/// constant c, and a slice embeds to the unit vector weighting axis c by its
/// sample count. Records the duration of every slice it embedded.
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

/// One voice as a main cluster and asides merges into one speaker with the
/// union's ranges, the big cluster's clip and the re-embedded vector.
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

/// Two people who each speak long do not merge; labels follow first speech.
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

/// A cluster under thirty seconds joins the substantive cluster it sounds
/// like, and one that sounds like nobody is dropped.
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

/// The closest pair merges first, not the first cluster in the input.
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

/// Without a substantive cluster the mapping's clusters come back untouched.
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

/// Fragments of one voice collapse greedily while a second voice survives;
/// no slice exceeds the cap.
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

/// The concatenation keeps time order, clips to the buffer and stops at the
/// cap.
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

/// Labels are handed out again after a merge: the union that opens the
/// recording is "Speaker 1".
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

/// A small cluster joins the substantive cluster with the highest cosine, not
/// the first one over the absorb cut; on a tie it joins the first.
#[test]
fn a_small_cluster_joins_the_closest_speaker_and_the_first_on_a_tie() {
    let anna = [range(0.0, 60.0)];
    let ben = [range(70.0, 130.0)];
    let aside = range(140.0, 150.0);
    // Where the aside switches from Anna's voice to Ben's, and who gets it.
    for (switch, joins_anna) in [(144.0, false), (145.0, true)] {
        let buffer = audio(
            &[
                (1.0, anna[0]),
                (2.0, ben[0]),
                (1.0, range(140.0, switch)),
                (2.0, range(switch, 150.0)),
            ],
            160.0,
        );
        let (refined, _) = run(
            &[
                c("Speaker 1", &anna, 1),
                c("Speaker 2", &ben, 2),
                c("Speaker 3", &[aside], 3),
            ],
            &buffer,
            &Rules::default(),
        );
        assert_eq!(labels(&refined), ["Speaker 1", "Speaker 2"]);
        let (with, without) = if joins_anna { (0, 1) } else { (1, 0) };
        assert_eq!(
            refined[with].ranges.last(),
            Some(&aside),
            "switch at {switch}"
        );
        assert_eq!(refined[without].ranges.len(), 1, "switch at {switch}");
    }
}
