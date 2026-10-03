//! Segmentation and one embedding per window and local speaker: the
//! `Analysis` every later stage works from, so a threshold sweep re-runs
//! only the clustering.

use crate::backend::{BackendError, SegmentationGeometry, TensorBackend};
use crate::error::DiarizeError;
use crate::segmentation::{self, Window, WindowActivity};
use crate::to_f64;

/// How much of a local speaker is needed before a window embeds them, as
/// `FluidAudio`'s `Embedding.community` and its `OfflineEmbeddingExtractor`
/// have it.
#[derive(Debug, Clone, PartialEq)]
pub struct ExtractionRules {
    /// Active speech a local speaker needs inside a window to be embedded.
    pub min_segment_seconds: f64,
    /// Whether frames where two speakers overlap are left out of the
    /// embedding.
    pub exclude_overlap: bool,
    /// The share of the window's frames the speaker must fill, after
    /// overlap exclusion, to be embedded at all: `minActiveRatio = 0.2`
    /// in `FluidAudio`'s
    /// `Diarizer/Offline/Extraction/OfflineEmbeddingExtractor.swift`, 118
    /// of 589 frames, about two seconds. A window that hears a speaker
    /// only in passing contributes no embedding, so an interjection does
    /// not seed a cluster of its own. `FluidAudio`'s fallback to the
    /// overlapped frames when the clean ones are under the one-second
    /// floor cannot trigger once this holds (118 frames exceed 60), so
    /// the port has none; one of the departures the crate doc lists.
    pub min_active_ratio: f64,
}

impl Default for ExtractionRules {
    fn default() -> Self {
        ExtractionRules {
            min_segment_seconds: 1.0,
            exclude_overlap: true,
            min_active_ratio: 0.2,
        }
    }
}

/// One embedding window after extraction: which window, which local
/// speaker inside it, the span of their speech and the raw vector.
#[derive(Debug, Clone, PartialEq)]
pub struct WindowEmbedding {
    pub window: usize,
    pub local_speaker: usize,
    pub start: f64,
    pub end: f64,
    pub embedding: Vec<f32>,
}

impl WindowEmbedding {
    #[must_use]
    pub fn duration(&self) -> f64 {
        (self.end - self.start).max(0.0)
    }
}

/// Everything the models say about one lane; clustering and the timeline
/// are computed from it without touching the models again.
#[derive(Debug, Clone, PartialEq)]
pub struct Analysis {
    pub geometry: SegmentationGeometry,
    pub total_samples: usize,
    pub activities: Vec<WindowActivity>,
    pub embeddings: Vec<WindowEmbedding>,
}

impl Analysis {
    /// Seconds of audio analysed.
    #[must_use]
    pub fn duration(&self) -> f64 {
        self.geometry.seconds(self.total_samples)
    }
}

/// Runs segmentation over every window and embeds every local speaker with
/// enough clean speech. `step` is in samples.
pub fn analyze(
    backend: &mut dyn TensorBackend,
    audio: &[f32],
    step: usize,
    rules: &ExtractionRules,
) -> Result<Analysis, DiarizeError> {
    let geometry = backend.geometry().clone();
    let classes = segmentation::powerset(geometry.num_speakers, 2);
    let context = Context {
        geometry: &geometry,
        rules,
        min_frames: min_frames(&geometry, rules.min_segment_seconds),
        total_samples: audio.len(),
    };
    let mut activities = Vec::new();
    let mut embeddings = Vec::new();
    for window in segmentation::windows(audio, &geometry, step) {
        let logits = backend
            .segment(&window.samples)
            .map_err(DiarizeError::backend)?;
        let expected = geometry.frames_per_window * geometry.num_classes;
        if logits.len() != expected {
            return Err(DiarizeError::Shape {
                what: "segmentation output",
                expected,
                got: logits.len(),
            });
        }
        let frames =
            segmentation::decode(&logits, &classes, geometry.num_classes, window.valid_frames);
        let activity = WindowActivity {
            window: window.index,
            offset: window.offset,
            frames,
        };
        for speaker in 0..geometry.num_speakers {
            if let Some(embedding) = embed_speaker(backend, &context, &window, &activity, speaker)
                .map_err(DiarizeError::backend)?
            {
                embeddings.push(embedding);
            }
        }
        activities.push(activity);
    }
    Ok(Analysis {
        geometry,
        total_samples: audio.len(),
        activities,
        embeddings,
    })
}

/// Frames worth `seconds` of speech, at least one.
#[must_use]
pub fn min_frames(geometry: &SegmentationGeometry, seconds: f64) -> usize {
    // Positive and small; the cast cannot truncate anything that matters.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let frames = (seconds / geometry.frame_seconds()).ceil().max(1.0) as usize;
    frames
}

/// The weights for `speaker` in one window: 1 on the frames where they
/// speak (alone, when `rules.exclude_overlap`), 0 elsewhere; `None` when
/// those frames number under `min_frames` or under
/// [`ExtractionRules::min_active_ratio`] of the window.
#[must_use]
pub fn speaker_weights(
    activity: &WindowActivity,
    speaker: usize,
    frames_per_window: usize,
    min_frames: usize,
    rules: &ExtractionRules,
) -> Option<Vec<f32>> {
    let mut weights = vec![0.0f32; frames_per_window];
    let mut count = 0usize;
    for (frame, weight) in weights.iter_mut().enumerate().take(activity.frames.len()) {
        let counted = activity.is_active(frame, speaker)
            && (!rules.exclude_overlap || activity.speaker_count(frame) == 1);
        if counted {
            *weight = 1.0;
            count += 1;
        }
    }
    if count < min_frames || to_f64(count) < rules.min_active_ratio * to_f64(frames_per_window) {
        return None;
    }
    Some(weights)
}

/// What every window's extraction shares.
struct Context<'a> {
    geometry: &'a SegmentationGeometry,
    rules: &'a ExtractionRules,
    min_frames: usize,
    total_samples: usize,
}

fn embed_speaker(
    backend: &mut dyn TensorBackend,
    context: &Context<'_>,
    window: &Window,
    activity: &WindowActivity,
    speaker: usize,
) -> Result<Option<WindowEmbedding>, BackendError> {
    let Context {
        geometry,
        rules,
        min_frames,
        total_samples,
    } = *context;
    let Some(weights) = speaker_weights(
        activity,
        speaker,
        geometry.frames_per_window,
        min_frames,
        rules,
    ) else {
        return Ok(None);
    };
    let Some(embedding) = backend.embed(&window.samples, &weights)? else {
        return Ok(None);
    };
    let marked = |weight: &f32| *weight > 0.0;
    let (Some(first), Some(last)) = (
        weights.iter().position(marked),
        weights.iter().rposition(marked),
    ) else {
        return Ok(None);
    };
    let (start, end) =
        segmentation::frame_span(geometry, window.offset, first, last, total_samples);
    Ok(Some(WindowEmbedding {
        window: window.index,
        local_speaker: speaker,
        start,
        end,
        embedding,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn activity(frames: Vec<u8>) -> WindowActivity {
        WindowActivity {
            window: 0,
            offset: 0,
            frames,
        }
    }

    #[test]
    fn a_second_of_speech_is_sixty_frames() {
        assert_eq!(min_frames(&SegmentationGeometry::PYANNOTE_3_0, 1.0), 60);
        assert_eq!(min_frames(&SegmentationGeometry::PYANNOTE_3_0, 0.0), 1);
    }

    #[test]
    fn weights_mark_the_clean_frames_or_nothing() {
        let rules = ExtractionRules::default();
        // Speaker 0 alone for 4 frames, overlapping speaker 1 for 3 frames.
        let activity = activity(vec![0b01, 0b01, 0b01, 0b01, 0b11, 0b11, 0b11, 0b10]);
        let clean = speaker_weights(&activity, 0, 10, 4, &rules).unwrap();
        assert_eq!(
            clean,
            vec![1.0, 1.0, 1.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]
        );
        // With a higher floor the clean frames are too few; the
        // overlapped ones are no fallback.
        assert!(speaker_weights(&activity, 0, 10, 5, &rules).is_none());
        // Speaker 1 is alone for one frame of ten: under the fifth.
        assert!(speaker_weights(&activity, 1, 10, 1, &rules).is_none());
        // Speaker 2 never speaks.
        assert!(speaker_weights(&activity, 2, 10, 1, &rules).is_none());
        // Overlap exclusion off: every frame where the speaker talks.
        let keep = ExtractionRules {
            exclude_overlap: false,
            ..rules.clone()
        };
        assert_eq!(
            speaker_weights(&activity, 0, 10, 4, &keep)
                .unwrap()
                .iter()
                .sum::<f32>(),
            7.0
        );
        assert_eq!(
            speaker_weights(&activity, 1, 10, 1, &keep)
                .unwrap()
                .iter()
                .sum::<f32>(),
            4.0
        );
        // The ratio alone can refuse: four clean frames of forty.
        assert!(speaker_weights(&activity, 0, 40, 1, &rules).is_none());
        let lax = ExtractionRules {
            min_active_ratio: 0.0,
            ..rules
        };
        assert!(speaker_weights(&activity, 0, 40, 1, &lax).is_some());
    }

    /// The default ratio on pyannote's geometry: 118 clean frames, about
    /// two seconds, exceed the one-second floor, so the floor never
    /// decides on its own.
    #[test]
    fn the_default_ratio_asks_for_two_seconds_of_a_ten_second_window() {
        let geometry = SegmentationGeometry::PYANNOTE_3_0;
        let rules = ExtractionRules::default();
        let floor = min_frames(&geometry, rules.min_segment_seconds);
        let frames = |alone: usize| {
            let mut frames = vec![0u8; geometry.frames_per_window];
            for frame in frames.iter_mut().take(alone) {
                *frame = 0b001;
            }
            activity(frames)
        };
        assert!(
            speaker_weights(&frames(117), 0, geometry.frames_per_window, floor, &rules).is_none()
        );
        assert!(
            speaker_weights(&frames(118), 0, geometry.frames_per_window, floor, &rules).is_some()
        );
        assert!(to_f64(floor) < rules.min_active_ratio * to_f64(geometry.frames_per_window));
    }
}
