//! The four model calls of a Parakeet TDT transducer behind one trait, so
//! the decoder, the chunker and the merge are written once over any
//! backend ([`crate::onnx`] here); the integration step moves the `CoreML`
//! backend of #163 behind this trait, and the WP4 notes in the plan list
//! where the loops differ. Swift: `FluidAudio`'s `AsrModels` (Preprocessor, Encoder, Decoder,
//! `JointDecisionv3`), which `ParakeetEngine` drives through `AsrManager`.
//!
//! The `CoreML` joint returns the argmax token and the duration bin; the ONNX
//! joiner returns raw logits over the vocabulary and the duration bins in
//! one vector. [`split_logits`] is the one place that splits them.

use crate::error::SpeechError;

/// The one sample rate of the pipeline; every model here takes 16 kHz.
pub const SAMPLE_RATE: usize = 16_000;
/// Seconds per encoder frame: a 10 ms mel hop times the subsampling factor
/// of 8.
pub const FRAME_SECONDS: f64 = 0.08;
/// Samples per encoder frame.
pub const FRAME_SAMPLES: usize = SAMPLE_RATE * 8 / 100;

/// Seconds to whole samples, rounded down and never negative.
#[must_use]
pub fn sample_count(seconds: f32) -> usize {
    // The clamp keeps the cast in range.
    (seconds.max(0.0) * SAMPLE_RATE as f32) as usize
}

/// What the loop needs to know about a model before the first call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelShape {
    /// Pieces plus the blank; `tokens.txt` has this many lines.
    pub vocab_size: usize,
    /// The blank's id, `vocab_size - 1` for every `NeMo` export.
    pub blank_id: u32,
    /// The TDT duration bins in encoder frames, `[0, 1, 2, 3, 4]` for
    /// Parakeet v3; the joint's duration argmax indexes this.
    pub durations: Vec<usize>,
    /// Prediction network layers and hidden size (`pred_rnn_layers`,
    /// `pred_hidden`).
    pub decoder_layers: usize,
    pub decoder_hidden: usize,
    /// The encoder's output width per frame (`d_model`).
    pub encoder_hidden: usize,
}

/// Mel features for one window, mel-major (`data[mel * frames + frame]`),
/// the layout the encoder's `[1, mels, frames]` input takes.
#[derive(Debug, Clone, PartialEq)]
pub struct Features {
    pub mels: usize,
    pub frames: usize,
    pub data: Vec<f32>,
}

/// Encoder frames for one window, frame-major (`data[frame * hidden ..]`),
/// so [`EncoderOutput::frame`] is a slice.
#[derive(Debug, Clone, PartialEq)]
pub struct EncoderOutput {
    pub hidden: usize,
    /// Valid frames; the model may have padded beyond them.
    pub len: usize,
    pub data: Vec<f32>,
}

impl EncoderOutput {
    /// The encoder vector of frame `t`.
    #[must_use]
    pub fn frame(&self, t: usize) -> &[f32] {
        &self.data[t * self.hidden..(t + 1) * self.hidden]
    }
}

/// The prediction network's LSTM state, `[layers, 1, hidden]` flattened.
#[derive(Debug, Clone, PartialEq)]
pub struct DecoderState {
    pub h: Vec<f32>,
    pub c: Vec<f32>,
}

impl DecoderState {
    /// The state before the first token.
    #[must_use]
    pub fn zeros(layers: usize, hidden: usize) -> Self {
        DecoderState {
            h: vec![0.0; layers * hidden],
            c: vec![0.0; layers * hidden],
        }
    }
}

/// One prediction-network step: the projection the joint consumes and the
/// state after the fed token.
#[derive(Debug, Clone, PartialEq)]
pub struct DecoderStep {
    pub projection: Vec<f32>,
    pub state: DecoderState,
}

/// What the joint decides for one encoder frame and one projection.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct JointDecision {
    pub token: u32,
    /// The softmax probability of `token` over the vocabulary, in `0..=1`.
    pub probability: f32,
    /// Index into [`ModelShape::durations`].
    pub duration_bin: usize,
}

/// The model calls. Implementations hold their sessions and buffers and are
/// `Send`, so a transcriber can move to a worker thread or, later, behind
/// the sidecar boundary of speech-stack decision 5; nothing is shared
/// through globals.
pub trait SpeechBackend: Send {
    fn shape(&self) -> &ModelShape;

    /// The preprocessor: 16 kHz mono samples to mel features.
    fn features(&mut self, samples: &[f32]) -> Result<Features, SpeechError>;

    /// The encoder over one window of features.
    fn encode(&mut self, features: &Features) -> Result<EncoderOutput, SpeechError>;

    /// One prediction-network step: feed `token` (the blank primes the
    /// network) with `state`, get the projection and the next state.
    fn decoder_step(
        &mut self,
        token: u32,
        state: &DecoderState,
    ) -> Result<DecoderStep, SpeechError>;

    /// The joint over one encoder frame and one projection.
    fn joint_step(
        &mut self,
        encoder_frame: &[f32],
        projection: &[f32],
    ) -> Result<JointDecision, SpeechError>;
}

/// Splits the ONNX joiner's logits (`vocab_size` vocabulary logits followed
/// by one logit per duration bin) into a [`JointDecision`]: argmax over the
/// vocabulary with its softmax probability, argmax over the duration bins.
/// The `CoreML` joint does this inside the model.
pub fn split_logits(logits: &[f32], vocab_size: usize) -> Result<JointDecision, SpeechError> {
    if vocab_size == 0 {
        return Err(SpeechError::Shape(
            "a joint without a vocabulary has no token to pick".into(),
        ));
    }
    if logits.len() <= vocab_size {
        return Err(SpeechError::Shape(format!(
            "joint returned {} logits for a vocabulary of {vocab_size}; no duration bins",
            logits.len()
        )));
    }
    let (vocabulary, durations) = logits.split_at(vocab_size);
    let token = argmax(vocabulary);
    let duration_bin = argmax(durations);
    // Softmax of the argmax: exp(max - max) over the sum of exp(x - max).
    let max = vocabulary[token];
    let sum: f32 = vocabulary.iter().map(|x| (x - max).exp()).sum();
    let probability = if sum.is_finite() && sum > 0.0 {
        (1.0 / sum).clamp(0.0, 1.0)
    } else {
        0.0
    };
    Ok(JointDecision {
        token: u32::try_from(token).map_err(|_| SpeechError::Shape("token id over u32".into()))?,
        probability,
        duration_bin,
    })
}

/// The index of the largest value; the first on ties, 0 for an empty slice.
fn argmax(values: &[f32]) -> usize {
    let mut best = 0;
    for (i, v) in values.iter().enumerate() {
        if *v > values[best] {
            best = i;
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_split_takes_the_vocabulary_first_and_the_durations_after() {
        // Vocabulary of 4 (blank is 3), 5 duration bins.
        let logits = [0.0, 2.0, 1.0, -1.0, 0.1, 0.2, 3.0, 0.0, 0.0];
        let decision = split_logits(&logits, 4).unwrap();
        assert_eq!(decision.token, 1);
        assert_eq!(decision.duration_bin, 2);
        let expected = 1.0 / (1.0 + (-2.0f32).exp() + (-1.0f32).exp() + (-3.0f32).exp());
        assert!((decision.probability - expected).abs() < 1e-6);
    }

    #[test]
    fn the_blank_can_win_and_a_vector_without_durations_or_vocabulary_is_refused() {
        let logits = [0.0, 0.0, 0.0, 5.0, 1.0];
        let decision = split_logits(&logits, 4).unwrap();
        assert_eq!(decision.token, 3);
        assert_eq!(decision.duration_bin, 0);
        assert!(matches!(
            split_logits(&logits[..4], 4),
            Err(SpeechError::Shape(_))
        ));
        assert!(matches!(
            split_logits(&logits, 0),
            Err(SpeechError::Shape(_))
        ));
    }

    #[test]
    fn encoder_frames_are_slices() {
        let output = EncoderOutput {
            hidden: 2,
            len: 2,
            data: vec![1.0, 2.0, 3.0, 4.0],
        };
        assert_eq!(output.frame(1), &[3.0, 4.0]);
        assert_eq!(DecoderState::zeros(2, 3).h.len(), 6);
    }
}
