//! The post-pass over the mapped clusters. Short turns embed far from the
//! same voice speaking at length, so each cluster with enough speech is
//! re-embedded over its own concatenated speech and close clusters merge;
//! fragments join the nearest substantive cluster or are dropped. Labels are
//! handed out again in order of first speech.

use steno_core::{AudioBuffer16k, BoxError, Embedding, SpeakerCluster, TimeRange};

use crate::first_max_by;
use crate::mapping::{display_label, merged, speech_seconds};

/// Embeds one stretch of speech as a single speaker; a trait so refinement
/// is tested without models.
pub trait SliceEmbedder {
    /// `None` when the audio holds no usable speech.
    fn embedding(&mut self, audio: &AudioBuffer16k) -> Result<Option<Embedding>, BoxError>;
}

/// The refinement constants.
#[derive(Debug, Clone, PartialEq)]
pub struct Rules {
    /// Speech a cluster needs to count as a speaker on its own.
    pub minimum_seconds: f64,
    /// How much of a cluster's speech is embedded; the first stretches win.
    pub maximum_embed_seconds: f64,
    /// Re-embedded, the same person scores 0.68 and up on the calibration corpus
    /// and different people at most 0.50.
    pub merge_threshold: f32,
    /// A fragment joins the closest substantive cluster at this cosine or above;
    /// loose on purpose, as a wrong speaker on ten seconds costs less than a
    /// phantom one.
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

    // Greedy merge, highest cosine first; each merge re-embeds the union.
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
        // The speaker keeps embedding, clip and confidence; only the ranges grow.
        let speaker = &mut substantive[index];
        speaker.ranges = union(&speaker.ranges, &fragment.ranges);
    }

    substantive.sort_by(|lhs, rhs| first_speech(lhs).total_cmp(&first_speech(rhs)));
    Ok(substantive
        .into_iter()
        .enumerate()
        .map(|(index, mut cluster)| {
            cluster.label = display_label(index);
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

/// Gives `cluster` the embedding of its own speech, concatenated up to the
/// cap. Unchanged when there is no audio or no speech is heard.
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

/// The samples under `ranges`, sorted and clipped, joined until `cap`
/// seconds are collected.
#[must_use]
pub fn concatenated(ranges: &[TimeRange], audio: &AudioBuffer16k, cap: f64) -> AudioBuffer16k {
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

/// The two clusters with the highest cosine, `first < second`; `None` when
/// fewer than two carry an embedding.
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

/// The closest cluster to `embedding` with the cosine, the first on a tie.
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

/// One cluster from two: ranges merged; clip, confidence and embedding of
/// the one with more speech. The label is provisional.
fn merge(lhs: &SpeakerCluster, rhs: &SpeakerCluster) -> SpeakerCluster {
    let (longer, shorter) = if speech_seconds(&lhs.ranges) >= speech_seconds(&rhs.ranges) {
        (lhs, rhs)
    } else {
        (rhs, lhs)
    };
    SpeakerCluster {
        label: longer.label.clone(),
        ranges: union(&lhs.ranges, &rhs.ranges),
        embedding: longer
            .embedding
            .clone()
            .or_else(|| shorter.embedding.clone()),
        cluster_confidence: longer.cluster_confidence,
        sample_clip_range: longer.sample_clip_range.or(shorter.sample_clip_range),
    }
}

fn union(lhs: &[TimeRange], rhs: &[TimeRange]) -> Vec<TimeRange> {
    merged(&[lhs, rhs].concat())
}
