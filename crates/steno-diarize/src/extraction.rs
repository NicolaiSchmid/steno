//! Segmentation and one embedding per window and local speaker: the
//! `Analysis` every later stage works from, so a threshold sweep re-runs
//! only the clustering.

use crate::backend::{BackendError, SegmentationGeometry, TensorBackend};
use crate::error::DiarizeError;
use crate::segmentation::{self, Window, WindowActivity};

/// How much of a local speaker is needed before a window embeds them, as
/// `FluidAudio`'s `Embedding.community` has it.
#[derive(Debug, Clone, PartialEq)]
pub struct ExtractionRules {
    /// Active speech a local speaker needs inside a window to be embedded.
    pub min_segment_seconds: f64,
    /// Whether frames where two speakers overlap are left out of the
    /// embedding, as long as enough clean frames remain.
    pub exclude_overlap: bool,
}

impl Default for ExtractionRules {
    fn default() -> Self {
        ExtractionRules {
            min_segment_seconds: 1.0,
            exclude_overlap: true,
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
        crate::backend::to_f64(self.total_samples)
            / crate::backend::to_f64(self.geometry.sample_rate)
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

/// The weights for `speaker` in one window: the frames where they speak
/// alone when at least `min_frames` of those exist, otherwise every frame
/// where they speak; `None` when they have under `min_frames` at all.
#[must_use]
pub fn speaker_weights(
    activity: &WindowActivity,
    speaker: usize,
    frames_per_window: usize,
    min_frames: usize,
    rules: &ExtractionRules,
) -> Option<Vec<f32>> {
    let mut base = vec![0.0f32; frames_per_window];
    let mut clean = vec![0.0f32; frames_per_window];
    let mut base_count = 0usize;
    let mut clean_count = 0usize;
    for (frame, weight) in base.iter_mut().enumerate().take(activity.frames.len()) {
        if activity.is_active(frame, speaker) {
            *weight = 1.0;
            base_count += 1;
            if activity.speaker_count(frame) == 1 {
                clean[frame] = 1.0;
                clean_count += 1;
            }
        }
    }
    if base_count < min_frames {
        return None;
    }
    if rules.exclude_overlap && clean_count >= min_frames {
        Some(clean)
    } else {
        Some(base)
    }
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
    let active: Vec<usize> = weights
        .iter()
        .enumerate()
        .filter(|(_, weight)| **weight > 0.0)
        .map(|(frame, _)| frame)
        .collect();
    let (Some(first), Some(last)) = (active.first(), active.last()) else {
        return Ok(None);
    };
    let (start, end) =
        segmentation::frame_span(geometry, window.offset, *first, *last, total_samples);
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
    fn weights_prefer_clean_frames_and_fall_back_to_all_of_them() {
        let rules = ExtractionRules::default();
        // Speaker 0 alone for 4 frames, overlapping speaker 1 for 3 frames.
        let activity = activity(vec![0b01, 0b01, 0b01, 0b01, 0b11, 0b11, 0b11, 0b10]);
        let clean = speaker_weights(&activity, 0, 10, 4, &rules).unwrap();
        assert_eq!(
            clean,
            vec![1.0, 1.0, 1.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]
        );
        // With a higher floor the clean frames are too few: all seven count.
        let all = speaker_weights(&activity, 0, 10, 5, &rules).unwrap();
        assert_eq!(all.iter().sum::<f32>(), 7.0);
        // Under the floor altogether: nothing.
        assert!(speaker_weights(&activity, 1, 10, 5, &rules).is_none());
        // Speaker 2 never speaks.
        assert!(speaker_weights(&activity, 2, 10, 1, &rules).is_none());
        // Overlap exclusion off: the base mask even when clean frames suffice.
        let keep = ExtractionRules {
            exclude_overlap: false,
            ..rules
        };
        assert_eq!(
            speaker_weights(&activity, 0, 10, 4, &keep)
                .unwrap()
                .iter()
                .sum::<f32>(),
            7.0
        );
    }
}
