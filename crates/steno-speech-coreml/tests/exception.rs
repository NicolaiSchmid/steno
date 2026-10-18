//! An Objective-C exception from a model call, run by four workers through
//! the shared pipeline, fails the transcription as an ordinary error and the
//! process survives to run it again. No models needed.

#![cfg(target_os = "macos")]

use objc2_foundation::{NSException, NSString};
use steno_speech::{
    AdaptiveEnergyVad, DecoderState, DecoderStep, EncoderOutput, Features, JointDecision,
    LanguageTagger, ModelShape, SpeechBackend, Transcriber, VadConfig, Vocab,
};
use steno_speech_coreml::SpeechError;
use steno_speech_coreml::coreml::contained;
use steno_speech_coreml::engine::pipeline_config;

/// Every model call raises.
#[derive(Clone)]
struct Raising {
    shape: ModelShape,
}

fn raised<T>() -> Result<T, steno_speech::SpeechError> {
    Ok(contained(|| -> Result<T, SpeechError> {
        NSException::new(
            &NSString::from_str("NSGenericException"),
            Some(&NSString::from_str(
                "Failed to allocate memory IOSurface object",
            )),
            None,
        )
        .expect("an exception object")
        .raise()
    })?)
}

impl SpeechBackend for Raising {
    fn shape(&self) -> &ModelShape {
        &self.shape
    }

    fn features(&mut self, _: &[f32]) -> Result<Features, steno_speech::SpeechError> {
        raised()
    }

    fn encode(&mut self, _: &Features) -> Result<EncoderOutput, steno_speech::SpeechError> {
        raised()
    }

    fn decoder_step(
        &mut self,
        _: u32,
        _: &DecoderState,
    ) -> Result<DecoderStep, steno_speech::SpeechError> {
        raised()
    }

    fn joint_step(
        &mut self,
        _: &[f32],
        _: &[f32],
    ) -> Result<JointDecision, steno_speech::SpeechError> {
        raised()
    }
}

/// 60 s of one-second noise bursts between silences, so the layout has
/// several windows.
fn bursts() -> Vec<f32> {
    let mut state = 0x2545_f491_u32;
    (0..60 * steno_speech::SAMPLE_RATE)
        .map(|index| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            if (index / steno_speech::SAMPLE_RATE).is_multiple_of(2) {
                #[allow(clippy::cast_precision_loss)]
                let noise = (state as f32 / u32::MAX as f32) - 0.5;
                noise * 0.5
            } else {
                0.0
            }
        })
        .collect()
}

#[test]
fn an_exception_in_a_model_call_fails_the_transcription_and_the_process_lives() {
    let vocab = Vocab::from_pieces(vec!["▁a".to_owned(), "<blank>".to_owned()]).unwrap();
    let backend = Raising {
        shape: ModelShape {
            vocab_size: vocab.len(),
            blank_id: vocab.blank_id(),
            durations: vec![0, 1, 2, 3, 4],
            decoder_layers: 2,
            decoder_hidden: 4,
            encoder_hidden: 4,
        },
    };
    let mut transcriber = Transcriber::new(
        backend.clone(),
        vocab,
        Box::new(AdaptiveEnergyVad {
            config: VadConfig::default(),
        }),
        LanguageTagger::new(),
        pipeline_config(),
    )
    .with_more_workers(vec![backend; 3]);
    let audio = bursts();
    for _ in 0..2 {
        let error = SpeechError::Pipeline(transcriber.transcribe(&audio, None).unwrap_err());
        assert!(error.is_exception(), "{error:?}");
        assert_eq!(
            error.to_string(),
            "CoreML raised NSGenericException: Failed to allocate memory IOSurface object"
        );
    }
}
