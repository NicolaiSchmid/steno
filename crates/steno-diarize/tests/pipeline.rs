//! The whole chain over a fake backend that reads the speaker off the
//! samples: analysis, clustering, timeline, mapping and refinement without
//! a model. Audio holds a constant `c` wherever speaker `c` talks and zero
//! for silence; the fake segments by that constant and embeds a window as
//! the unit vector of the marked speaker's constant, so same speaker means
//! cosine 1 and different speakers cosine 0.

mod common;

use common::{audio, range};
use steno_core::{AudioBuffer16k, Embedding, TimeRange};
use steno_diarize::backend::{BackendError, SegmentationGeometry, TensorBackend};
use steno_diarize::{DiarizerConfig, ModelDiarizer, Pipeline};

struct FakeBackend {
    geometry: SegmentationGeometry,
    segment_calls: usize,
    embed_calls: usize,
}

impl FakeBackend {
    fn new() -> Self {
        FakeBackend {
            geometry: SegmentationGeometry::PYANNOTE_3_0,
            segment_calls: 0,
            embed_calls: 0,
        }
    }

    /// The speaker constants heard in frame `frame` of `window`, up to
    /// three, in order of appearance inside the window.
    fn speakers_in_window(window: &[f32]) -> Vec<u32> {
        let mut speakers: Vec<u32> = Vec::new();
        for sample in window.iter().filter(|s| **s > 0.0) {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let speaker = *sample as u32;
            if !speakers.contains(&speaker) && speakers.len() < 3 {
                speakers.push(speaker);
            }
        }
        speakers
    }
}

impl TensorBackend for FakeBackend {
    fn geometry(&self) -> &SegmentationGeometry {
        &self.geometry
    }

    fn segment(&mut self, window: &[f32]) -> Result<Vec<f32>, BackendError> {
        self.segment_calls += 1;
        let g = &self.geometry;
        let speakers = Self::speakers_in_window(window);
        let mut logits = vec![0.0f32; g.frames_per_window * g.num_classes];
        for frame in 0..g.frames_per_window {
            let centre = frame * g.receptive_field_shift + g.receptive_field_size / 2;
            let sample = window.get(centre).copied().unwrap_or(0.0);
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let speaker = sample as u32;
            let class = if sample > 0.0 {
                speakers
                    .iter()
                    .position(|s| *s == speaker)
                    .map_or(0, |local| local + 1)
            } else {
                0
            };
            logits[frame * g.num_classes + class] = 10.0;
        }
        Ok(logits)
    }

    fn embed(&mut self, window: &[f32], weights: &[f32]) -> Result<Option<Vec<f32>>, BackendError> {
        self.embed_calls += 1;
        let g = &self.geometry;
        let mut values = vec![0.0f32; Embedding::DIMENSION];
        for (frame, _) in weights.iter().enumerate().filter(|(_, w)| **w > 0.0) {
            let centre = frame * g.receptive_field_shift + g.receptive_field_size / 2;
            let sample = window.get(centre).copied().unwrap_or(0.0);
            if sample > 0.0 {
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let axis = sample as usize;
                values[axis] += 1.0;
            }
        }
        Ok(values.iter().any(|v| *v > 0.0).then_some(values))
    }
}

fn speech(cluster: &steno_core::SpeakerCluster) -> f64 {
    cluster.ranges.iter().map(|r| r.upper - r.lower).sum()
}

/// One remote speaker who talks at length and interjects briefly between
/// long silences: with refinement off the interjections may be their own
/// cluster or not, with it on they are the one speaker.
#[test]
fn one_voice_with_asides_is_one_speaker_after_refinement() {
    let layout: Vec<(f32, TimeRange)> = vec![
        (1.0, range(0.0, 50.0)),
        (1.0, range(60.0, 63.0)),
        (1.0, range(70.0, 120.0)),
        (1.0, range(130.0, 132.0)),
    ];
    let buffer = audio(&layout, 140.0);
    let mut pipeline = Pipeline::new(FakeBackend::new(), DiarizerConfig::default());
    let result = pipeline.diarize(&buffer).unwrap();
    assert_eq!(result.clusters.len(), 1, "{result:?}");
    let only = &result.clusters[0];
    assert_eq!(only.label, "Speaker 1");
    assert!((speech(only) - 105.0).abs() < 1.0, "{:?}", only.ranges);
    assert!(only.embedding.as_ref().unwrap().0[1] > 0.99);
    let clip = only.sample_clip_range.unwrap();
    assert!(clip.upper - clip.lower <= 10.0 + 1e-9);
    assert!(
        pipeline.backend().segment_calls > 60,
        "every two seconds, plus the re-embedding"
    );
}

/// Two voices taking turns: two speakers in order of first speech, with
/// their speech kept apart and the embeddings orthogonal.
#[test]
fn two_voices_become_two_speakers_in_order_of_first_speech() {
    let layout: Vec<(f32, TimeRange)> = vec![
        (2.0, range(0.0, 40.0)),
        (1.0, range(45.0, 90.0)),
        (2.0, range(95.0, 130.0)),
        (1.0, range(135.0, 170.0)),
    ];
    let buffer = audio(&layout, 175.0);
    let mut pipeline = Pipeline::new(FakeBackend::new(), DiarizerConfig::default());
    let result = pipeline.diarize(&buffer).unwrap();
    assert_eq!(result.clusters.len(), 2, "{result:?}");
    let first = &result.clusters[0];
    let second = &result.clusters[1];
    assert_eq!(
        (first.label.as_str(), second.label.as_str()),
        ("Speaker 1", "Speaker 2")
    );
    assert!(
        first.ranges[0].lower < 1.0,
        "the opening voice is Speaker 1"
    );
    assert!((speech(first) - 75.0).abs() < 1.5, "{:?}", first.ranges);
    assert!((speech(second) - 80.0).abs() < 1.5, "{:?}", second.ranges);
    let cosine = first
        .embedding
        .as_ref()
        .unwrap()
        .cosine_similarity(second.embedding.as_ref().unwrap());
    assert!(cosine.abs() < 1e-6);
    // No two speakers overlap.
    for lhs in &first.ranges {
        for rhs in &second.ranges {
            assert!(
                lhs.upper <= rhs.lower || rhs.upper <= lhs.lower,
                "{lhs:?} overlaps {rhs:?}"
            );
        }
    }
}

/// The threshold sweep the harness runs: one analysis, many cuts. A
/// negative cut splits every window into its own two-second speaker; none
/// of those reaches thirty seconds, so the refinement pass leaves such a
/// result alone, as the Swift rules say.
#[test]
fn the_analysis_is_reused_across_thresholds() {
    let buffer = audio(&[(1.0, range(0.0, 60.0)), (3.0, range(65.0, 125.0))], 130.0);
    let mut pipeline = Pipeline::new(FakeBackend::new(), DiarizerConfig::default());
    let analysis = pipeline.analyze(&buffer).unwrap();
    assert!(analysis.embeddings.len() > 40);
    let before = pipeline.backend().segment_calls;
    let tight = pipeline.map(&analysis, -0.1);
    let loose = pipeline.map(&analysis, 0.5);
    assert_eq!(
        pipeline.backend().segment_calls,
        before,
        "mapping touches no model"
    );
    assert!(
        tight.clusters.len() > 2,
        "every window its own speaker: {}",
        tight.clusters.len()
    );
    assert_eq!(loose.clusters.len(), 2);
    let untouched = pipeline.refine(&tight, &buffer).unwrap();
    assert_eq!(
        untouched.clusters.len(),
        tight.clusters.len(),
        "no substantive cluster, nothing to refine"
    );
    let refined = pipeline.refine(&loose, &buffer).unwrap();
    assert_eq!(refined.clusters.len(), 2, "{refined:?}");
}

#[test]
fn silence_and_short_audio_yield_no_speakers() {
    let mut pipeline = Pipeline::new(FakeBackend::new(), DiarizerConfig::default());
    let silence = pipeline.diarize(&AudioBuffer16k::silence(30.0)).unwrap();
    assert_eq!(silence.clusters.len(), 0, "{silence:?}");
    let blip = pipeline.diarize(&AudioBuffer16k::silence(0.5)).unwrap();
    assert_eq!(blip.clusters.len(), 0);
    assert_eq!(
        pipeline.backend().segment_calls,
        11,
        "thirty seconds: eleven windows; the half second none"
    );
}

#[tokio::test]
async fn the_diarizer_loads_its_backend_once_and_serialises_calls() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use steno_core::Diarizer;

    let loads = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&loads);
    let diarizer = ModelDiarizer::new(
        DiarizerConfig::default(),
        Box::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(FakeBackend::new()) as Box<dyn TensorBackend>)
        }),
    );
    diarizer.prepare().await.unwrap();
    diarizer.prepare().await.unwrap();
    let buffer = audio(&[(2.0, range(0.0, 45.0))], 50.0);
    let result = diarizer.diarize(&buffer).await.unwrap();
    assert_eq!(result.clusters.len(), 1);
    assert_eq!(loads.load(Ordering::SeqCst), 1);
    let shared: Arc<dyn Diarizer> = Arc::new(diarizer);
    let quiet = shared.diarize(&AudioBuffer16k::silence(2.0)).await.unwrap();
    assert_eq!(quiet.clusters.len(), 0);
}
