//! The post-pass over the mapped clusters decided in
//! `.plans/2026-09-29-speaker-calibration.md`. Swift:
//! `Sources/StenoSpeech/Diarization/ClusterRefinement.swift`, one for
//! one. A speaker's short turns embed far from the
//! same voice speaking at length, because each ten-second window holds
//! little of them, so a 1:1 call comes out as a main cluster and one or
//! two clusters of interjections. Re-embedding each cluster over its own
//! concatenated speech gives vectors that compare the way the long
//! clusters do.
//!
//! The pass, in order: clusters with at least [`Rules::minimum_seconds`] of
//! speech are re-embedded and merged greedily, highest cosine first, while
//! a pair reaches [`Rules::merge_threshold`]; each merged cluster is
//! re-embedded over the union. Fragments under the minimum join the
//! substantive cluster they are closest to when that cosine reaches
//! [`Rules::absorb_threshold`], otherwise they are dropped (their segments
//! stay speaker-less). Labels are handed out again as "Speaker n" in order
//! of first speech. A result without a substantive cluster is returned
//! unchanged, so a short recording keeps its speakers.

use steno_core::{AudioBuffer16k, BoxError, Embedding, SpeakerCluster, TimeRange};

use crate::first_max_by;
use crate::mapping::{merged, speaker_label, speech_seconds};

/// Embeds one stretch of speech as a single speaker. The pipeline fulfils
/// it with its own models; pure, so the refinement is tested without them.
pub trait SliceEmbedder {
    /// `None` when the audio holds no usable speech.
    fn embedding(&mut self, audio: &AudioBuffer16k) -> Result<Option<Embedding>, BoxError>;
}

/// The plan's constants.
#[derive(Debug, Clone, PartialEq)]
pub struct Rules {
    /// Speech a cluster needs to count as a speaker on its own.
    pub minimum_seconds: f64,
    /// How much of a cluster's speech is embedded; the first stretches win.
    pub maximum_embed_seconds: f64,
    /// Re-embedded, the same person scores 0.68 and up on the calibration
    /// corpus and different people at most 0.50; 0.60 sits between.
    pub merge_threshold: f32,
    /// A fragment joins the substantive cluster it is closest to at this
    /// cosine or above; below it, its segments stay speaker-less. Loose on
    /// purpose: a fragment carries little evidence, and a wrong speaker on
    /// ten seconds costs less than a phantom speaker.
    pub absorb_threshold: f32,
}

impl Default for Rules {
    fn default() -> Self {
        Rules {
            minimum_seconds: 30.0,
            maximum_embed_seconds: 180.0,
            merge_threshold: 0.60,
            absorb_threshold: 0.30,
        }
    }
}

/// The pass over `clusters` in `audio`.
pub fn refine(
    clusters: &[SpeakerCluster],
    audio: &AudioBuffer16k,
    rules: &Rules,
    embedder: &mut dyn SliceEmbedder,
) -> Result<Vec<SpeakerCluster>, BoxError> {
    let mut substantive: Vec<SpeakerCluster> = clusters
        .iter()
        .filter(|cluster| speech_seconds(&cluster.ranges) >= rules.minimum_seconds)
        .cloned()
        .collect();
    if substantive.is_empty() {
        return Ok(clusters.to_vec());
    }
    let fragments: Vec<&SpeakerCluster> = clusters
        .iter()
        .filter(|cluster| speech_seconds(&cluster.ranges) < rules.minimum_seconds)
        .collect();

    for cluster in &mut substantive {
        reembed(cluster, audio, rules, embedder)?;
    }

    // Greedy merge, highest cosine first. Each merge re-embeds the union,
    // so the loop runs at most `substantive.len() - 1` more embeddings.
    while substantive.len() > 1 {
        let Some((first, second, cosine)) = closest_pair(&substantive) else {
            break;
        };
        if cosine < rules.merge_threshold {
            break;
        }
        let mut union = merge(&substantive[first], &substantive[second]);
        reembed(&mut union, audio, rules, embedder)?;
        substantive[first] = union;
        substantive.remove(second);
    }

    for fragment in fragments {
        let mut probe = fragment.clone();
        reembed(&mut probe, audio, rules, embedder)?;
        let Some(embedding) = probe.embedding.as_ref() else {
            continue;
        };
        let Some((index, cosine)) = closest(embedding, &substantive) else {
            continue;
        };
        if cosine < rules.absorb_threshold {
            continue;
        }
        // The speaker keeps their embedding, clip and confidence; only the
        // ranges grow.
        let mut ranges = substantive[index].ranges.clone();
        ranges.extend(fragment.ranges.iter().copied());
        substantive[index].ranges = merged(&ranges);
    }

    substantive.sort_by(|lhs, rhs| first_speech(lhs).total_cmp(&first_speech(rhs)));
    Ok(substantive
        .into_iter()
        .enumerate()
        .map(|(index, mut cluster)| {
            cluster.label = speaker_label(index);
            cluster
        })
        .collect())
}

fn first_speech(cluster: &SpeakerCluster) -> f64 {
    cluster
        .ranges
        .iter()
        .map(|range| range.lower)
        .fold(f64::INFINITY, f64::min)
}

/// Gives `cluster` the embedding of its own speech: ranges in time order,
/// concatenated up to the cap, embedded as one voice. Unchanged when there
/// is no audio or the embedder hears no speech.
fn reembed(
    cluster: &mut SpeakerCluster,
    audio: &AudioBuffer16k,
    rules: &Rules,
    embedder: &mut dyn SliceEmbedder,
) -> Result<(), BoxError> {
    let speech = concatenated(&cluster.ranges, audio, rules.maximum_embed_seconds);
    if speech.is_empty() {
        return Ok(());
    }
    if let Some(embedding) = embedder.embedding(&speech)? {
        cluster.embedding = Some(embedding);
    }
    Ok(())
}

/// The samples under `ranges`, sorted and clipped to the buffer, joined
/// until `cap` seconds are collected.
#[must_use]
pub fn concatenated(ranges: &[TimeRange], audio: &AudioBuffer16k, cap: f64) -> AudioBuffer16k {
    // Positive seconds times the rate; far below any truncation.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let limit = (cap.max(0.0) * AudioBuffer16k::SAMPLE_RATE) as usize;
    let mut samples: Vec<f32> = Vec::with_capacity(limit.min(audio.len()));
    for range in merged(ranges) {
        let remaining = limit.saturating_sub(samples.len());
        if remaining == 0 {
            break;
        }
        let slice = audio.slice(range);
        samples.extend(slice.samples.iter().take(remaining));
    }
    AudioBuffer16k::new(samples)
}

/// The two clusters whose embeddings have the highest cosine, as indices
/// with `first < second`; `None` when fewer than two carry an embedding.
fn closest_pair(clusters: &[SpeakerCluster]) -> Option<(usize, usize, f32)> {
    let mut best: Option<(usize, usize, f32)> = None;
    for (first, lhs) in clusters.iter().enumerate() {
        let Some(left) = lhs.embedding.as_ref() else {
            continue;
        };
        for (second, rhs) in clusters.iter().enumerate().skip(first + 1) {
            let Some(right) = rhs.embedding.as_ref() else {
                continue;
            };
            let cosine = left.cosine_similarity(right);
            if best.is_none_or(|(_, _, known)| cosine > known) {
                best = Some((first, second, cosine));
            }
        }
    }
    best
}

/// The cluster whose embedding is closest to `embedding`, with the cosine,
/// the first on a tie; `None` when none carries an embedding.
fn closest(embedding: &Embedding, clusters: &[SpeakerCluster]) -> Option<(usize, f32)> {
    first_max_by(
        clusters.iter().enumerate().filter_map(|(index, cluster)| {
            cluster
                .embedding
                .as_ref()
                .map(|candidate| (index, candidate.cosine_similarity(embedding)))
        }),
        |lhs, rhs| lhs.1.total_cmp(&rhs.1),
    )
}

/// One cluster from two: ranges merged; clip, confidence and, until the
/// caller re-embeds the union, embedding of the one with more speech. The
/// label is provisional: `refine` relabels every survivor by first speech.
fn merge(lhs: &SpeakerCluster, rhs: &SpeakerCluster) -> SpeakerCluster {
    let (longer, shorter) = if speech_seconds(&lhs.ranges) >= speech_seconds(&rhs.ranges) {
        (lhs, rhs)
    } else {
        (rhs, lhs)
    };
    let mut ranges = lhs.ranges.clone();
    ranges.extend(rhs.ranges.iter().copied());
    SpeakerCluster {
        label: longer.label.clone(),
        ranges: merged(&ranges),
        embedding: longer
            .embedding
            .clone()
            .or_else(|| shorter.embedding.clone()),
        cluster_confidence: longer.cluster_confidence,
        sample_clip_range: longer.sample_clip_range.or(shorter.sample_clip_range),
    }
}
