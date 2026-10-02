//! Sliding windows over the lane and the segmentation model's powerset
//! output as per-frame speaker activity.

use crate::backend::SegmentationGeometry;

/// One window handed to the segmentation and embedding models.
#[derive(Debug, Clone, PartialEq)]
pub struct Window {
    /// Position in the window sequence.
    pub index: usize,
    /// First sample of the window in the recording.
    pub offset: usize,
    /// Exactly `window_samples` samples, zero-padded past the recording.
    pub samples: Vec<f32>,
    /// Frames whose centre lies inside the recording.
    pub valid_frames: usize,
}

/// Windows every `step` samples, as sherpa-onnx lays them out: audio up to
/// one window long is one zero-padded window; otherwise every window that
/// fits, then one padded window for the tail when samples remain. A step
/// of zero reads as one window.
#[must_use]
pub fn windows(audio: &[f32], geometry: &SegmentationGeometry, step: usize) -> Vec<Window> {
    let length = geometry.window_samples;
    let step = step.max(1);
    let total = audio.len();
    let mut offsets = Vec::new();
    if total <= length {
        offsets.push(0);
    } else {
        let full = (total - length) / step + 1;
        offsets.extend((0..full).map(|index| index * step));
        if !(total - length).is_multiple_of(step) {
            offsets.push(full * step);
        }
    }
    offsets
        .into_iter()
        .enumerate()
        .map(|(index, offset)| {
            let available = total.saturating_sub(offset).min(length);
            let mut samples = vec![0.0f32; length];
            samples[..available].copy_from_slice(&audio[offset..offset + available]);
            Window {
                index,
                offset,
                samples,
                valid_frames: geometry.valid_frames(available),
            }
        })
        .collect()
}

/// The powerset classes of a segmentation model as speaker bit masks:
/// nobody, each speaker alone, then every pair in order, as pyannote
/// enumerates them (`[∅, {0}, {1}, {2}, {0,1}, {0,2}, {1,2}]` for three
/// speakers and at most two at once).
#[must_use]
pub fn powerset(num_speakers: usize, max_simultaneous: usize) -> Vec<u8> {
    let mut classes = vec![0u8];
    if max_simultaneous >= 1 {
        classes.extend((0..num_speakers).map(|speaker| 1u8 << speaker));
    }
    if max_simultaneous >= 2 {
        for first in 0..num_speakers {
            for second in first + 1..num_speakers {
                classes.push((1u8 << first) | (1u8 << second));
            }
        }
    }
    if max_simultaneous >= 3 {
        for first in 0..num_speakers {
            for second in first + 1..num_speakers {
                for third in second + 1..num_speakers {
                    classes.push((1u8 << first) | (1u8 << second) | (1u8 << third));
                }
            }
        }
    }
    classes
}

/// Which local speakers are active in each frame of one window.
#[derive(Debug, Clone, PartialEq)]
pub struct WindowActivity {
    pub window: usize,
    pub offset: usize,
    /// One bit mask per valid frame; bit `s` set when local speaker `s`
    /// speaks.
    pub frames: Vec<u8>,
}

impl WindowActivity {
    /// Active speakers in `frame`.
    #[must_use]
    pub fn speaker_count(&self, frame: usize) -> u32 {
        self.frames.get(frame).map_or(0, |mask| mask.count_ones())
    }

    /// Whether local speaker `speaker` is active in `frame`.
    #[must_use]
    pub fn is_active(&self, frame: usize, speaker: usize) -> bool {
        self.frames
            .get(frame)
            .is_some_and(|mask| mask & (1u8 << speaker) != 0)
    }

    /// Frames in which `speaker` is active.
    #[must_use]
    pub fn active_frames(&self, speaker: usize) -> usize {
        self.frames
            .iter()
            .filter(|mask| *mask & (1u8 << speaker) != 0)
            .count()
    }
}

/// Hard argmax per frame over the powerset classes, then the class's
/// speaker mask, as pyannote and sherpa-onnx decode it. `logits` holds
/// `num_classes` values per frame; frames past `valid_frames` are dropped.
#[must_use]
pub fn decode(logits: &[f32], classes: &[u8], num_classes: usize, valid_frames: usize) -> Vec<u8> {
    logits
        .chunks_exact(num_classes)
        .take(valid_frames)
        .map(|row| {
            let best = row
                .iter()
                .enumerate()
                .max_by(|lhs, rhs| lhs.1.total_cmp(rhs.1))
                .map_or(0, |(index, _)| index);
            classes.get(best).copied().unwrap_or(0)
        })
        .collect()
}

/// The time a run of frames `first..=last` in a window at `offset`
/// covers: half a frame either side of the frame centres, clamped to the
/// recording.
#[must_use]
pub fn frame_span(
    geometry: &SegmentationGeometry,
    offset: usize,
    first: usize,
    last: usize,
    total_samples: usize,
) -> (f64, f64) {
    let base = geometry.seconds(offset);
    let half = geometry.frame_seconds() / 2.0;
    let duration = geometry.seconds(total_samples);
    let start = (base + geometry.frame_centre_seconds(first) - half).clamp(0.0, duration);
    let end = (base + geometry.frame_centre_seconds(last) + half).clamp(start, duration);
    (start, end)
}

#[cfg(test)]
mod tests {
    use super::*;

    const GEOMETRY: SegmentationGeometry = SegmentationGeometry::PYANNOTE_3_0;

    #[test]
    fn short_audio_is_one_padded_window() {
        let audio = vec![0.5f32; 16_000];
        let windows = windows(&audio, &GEOMETRY, 32_000);
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].offset, 0);
        assert_eq!(windows[0].samples.len(), 160_000);
        assert!(
            windows[0].samples[..16_000]
                .iter()
                .all(|s| (*s - 0.5).abs() < f32::EPSILON)
        );
        assert!(
            windows[0].samples[16_000..]
                .iter()
                .all(|s| s.abs() < f32::EPSILON)
        );
        assert_eq!(windows[0].valid_frames, GEOMETRY.valid_frames(16_000));
        assert_eq!(super::windows(&[], &GEOMETRY, 32_000).len(), 1);
    }

    #[test]
    fn long_audio_gets_every_full_window_and_a_padded_tail() {
        // 25 s: full windows at 0, 2, ..., 14 s (eight), then the tail at 16 s.
        let audio = vec![0.1f32; 400_000];
        let windows = windows(&audio, &GEOMETRY, 32_000);
        let offsets: Vec<usize> = windows.iter().map(|w| w.offset).collect();
        assert_eq!(offsets, (0..9).map(|i| i * 32_000).collect::<Vec<_>>());
        assert_eq!(
            windows[8].valid_frames,
            GEOMETRY.valid_frames(400_000 - 256_000)
        );
        assert!(windows.iter().all(|w| w.valid_frames <= 589));
        // Exactly one window's worth plus one step: two windows, no tail.
        let exact = super::windows(&vec![0.0f32; 192_000], &GEOMETRY, 32_000);
        assert_eq!(exact.len(), 2);
        assert_eq!(exact[1].valid_frames, 589);
    }

    #[test]
    fn the_powerset_enumerates_pyannotes_seven_classes() {
        assert_eq!(
            powerset(3, 2),
            vec![0b000, 0b001, 0b010, 0b100, 0b011, 0b101, 0b110]
        );
        assert_eq!(powerset(2, 1), vec![0, 1, 2]);
        assert_eq!(powerset(3, 3).len(), 8);
    }

    #[test]
    fn decoding_takes_the_argmax_class_per_frame() {
        let classes = powerset(3, 2);
        // Frame 0: silence; frame 1: speaker 1; frame 2: speakers 0 and 2.
        let logits = [
            [5.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 4.0, 0.0, 0.0, 0.0, 0.0],
            [0.0, 0.0, 0.0, 0.0, 0.0, 3.0, 1.0],
            [9.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
        ]
        .concat();
        let frames = decode(&logits, &classes, 7, 3);
        assert_eq!(frames, vec![0b000, 0b010, 0b101]);
        let activity = WindowActivity {
            window: 0,
            offset: 0,
            frames,
        };
        assert_eq!(activity.speaker_count(2), 2);
        assert!(activity.is_active(2, 0) && activity.is_active(2, 2) && !activity.is_active(2, 1));
        assert_eq!(activity.active_frames(0), 1);
        assert_eq!(activity.speaker_count(7), 0);
    }

    #[test]
    fn frame_spans_sit_half_a_frame_either_side_of_the_centres() {
        let (start, end) = frame_span(&GEOMETRY, 0, 0, 0, 160_000);
        let centre = 495.0 / 16_000.0;
        assert!((start - (centre - 0.016_875 / 2.0)).abs() < 1e-9);
        assert!((end - (centre + 0.016_875 / 2.0)).abs() < 1e-9);
        // A run reaching past the recording is clamped to it.
        let (_, clamped) = frame_span(&GEOMETRY, 0, 0, 588, 16_000);
        assert_eq!(clamped, 1.0);
        let (base, _) = frame_span(&GEOMETRY, 32_000, 0, 0, 160_000);
        assert!(base > 2.0);
    }
}
