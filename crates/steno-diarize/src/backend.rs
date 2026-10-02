//! The two tensor calls the pipeline needs and what their frames mean.

/// Any error a backend raises; printed for the user.
pub type BackendError = Box<dyn std::error::Error + Send + Sync + 'static>;

/// What the segmentation model's output frames mean in samples. The
/// numbers are pyannote segmentation 3.0's: a ten-second window yields 589
/// frames, each the model's receptive field of 991 samples advanced by 270
/// samples (16.875 ms). The ONNX backend reads them from the model's
/// metadata; the `CoreML` backend uses the constants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentationGeometry {
    pub sample_rate: usize,
    /// Samples per window.
    pub window_samples: usize,
    /// Output frames per window.
    pub frames_per_window: usize,
    /// Samples one frame looks at.
    pub receptive_field_size: usize,
    /// Samples between two frames.
    pub receptive_field_shift: usize,
    /// Local speakers the model tells apart inside one window.
    pub num_speakers: usize,
    /// Powerset classes the model emits per frame.
    pub num_classes: usize,
}

impl SegmentationGeometry {
    /// pyannote segmentation 3.0 as the sherpa-onnx export describes it.
    pub const PYANNOTE_3_0: SegmentationGeometry = SegmentationGeometry {
        sample_rate: 16_000,
        window_samples: 160_000,
        frames_per_window: 589,
        receptive_field_size: 991,
        receptive_field_shift: 270,
        num_speakers: 3,
        num_classes: 7,
    };

    /// `samples` samples as seconds at this geometry's rate.
    #[must_use]
    pub fn seconds(&self, samples: usize) -> f64 {
        to_f64(samples) / to_f64(self.sample_rate)
    }

    /// Seconds one frame advances.
    #[must_use]
    pub fn frame_seconds(&self) -> f64 {
        self.seconds(self.receptive_field_shift)
    }

    /// Seconds the window lasts.
    #[must_use]
    pub fn window_seconds(&self) -> f64 {
        self.seconds(self.window_samples)
    }

    /// The time at the centre of `frame` counted from the start of the
    /// window (or recording) it belongs to.
    #[must_use]
    pub fn frame_centre_seconds(&self, frame: usize) -> f64 {
        self.seconds(frame * self.receptive_field_shift + self.receptive_field_size / 2)
    }

    /// The first global frame of a window at `offset` samples: the window's
    /// frame 0 lands on the global grid rounded to the nearest frame.
    #[must_use]
    pub fn global_frame(&self, offset_samples: usize) -> usize {
        (offset_samples + self.receptive_field_shift / 2) / self.receptive_field_shift
    }

    /// Frames whose centre lies inside `samples` samples of audio starting
    /// at the window's start; the rest of a padded window is silence the
    /// model never heard.
    #[must_use]
    pub fn valid_frames(&self, samples: usize) -> usize {
        if samples <= self.receptive_field_size / 2 {
            return 0;
        }
        let covered =
            (samples - self.receptive_field_size / 2).div_ceil(self.receptive_field_shift);
        covered.min(self.frames_per_window)
    }
}

/// The models behind the pipeline. One instance serves one pipeline; calls
/// arrive one after another.
pub trait TensorBackend: Send {
    fn geometry(&self) -> &SegmentationGeometry;

    /// Segmentation logits for one window of exactly
    /// [`SegmentationGeometry::window_samples`] samples:
    /// `frames_per_window × num_classes` values, row-major, one row per
    /// frame.
    fn segment(&mut self, window: &[f32]) -> Result<Vec<f32>, BackendError>;

    /// The embedding of the speech `weights` marks in `window`: one weight
    /// per segmentation frame, 1 where the speaker is active and 0
    /// elsewhere. `None` when too little speech is marked for the model to
    /// say anything.
    fn embed(&mut self, window: &[f32], weights: &[f32]) -> Result<Option<Vec<f32>>, BackendError>;
}

impl<B: TensorBackend + ?Sized> TensorBackend for Box<B> {
    fn geometry(&self) -> &SegmentationGeometry {
        (**self).geometry()
    }

    fn segment(&mut self, window: &[f32]) -> Result<Vec<f32>, BackendError> {
        (**self).segment(window)
    }

    fn embed(&mut self, window: &[f32], weights: &[f32]) -> Result<Option<Vec<f32>>, BackendError> {
        (**self).embed(window, weights)
    }
}

/// Exact for every count below 2^53, far beyond any sample count.
#[allow(clippy::cast_precision_loss)]
pub(crate) fn to_f64(count: usize) -> f64 {
    count as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pyannote_geometry_derives_its_frame_grid() {
        let geometry = SegmentationGeometry::PYANNOTE_3_0;
        assert!((geometry.frame_seconds() - 0.016_875).abs() < 1e-9);
        assert_eq!(geometry.window_seconds(), 10.0);
        // Frame 0 is centred on the receptive field's half: 495 samples.
        assert!((geometry.frame_centre_seconds(0) - 495.0 / 16_000.0).abs() < 1e-9);
        // A window at 32 000 samples starts at frame 119 (118.5 rounded up).
        assert_eq!(geometry.global_frame(32_000), 119);
        assert_eq!(geometry.global_frame(0), 0);
    }

    #[test]
    fn valid_frames_cover_the_audio_and_cap_at_the_window() {
        let geometry = SegmentationGeometry::PYANNOTE_3_0;
        assert_eq!(geometry.valid_frames(160_000), 589);
        assert_eq!(geometry.valid_frames(400_000), 589);
        assert_eq!(geometry.valid_frames(0), 0);
        assert_eq!(geometry.valid_frames(400), 0);
        // One second of audio: frames centred under 16 000 samples.
        assert_eq!(geometry.valid_frames(16_000), 58);
    }
}
