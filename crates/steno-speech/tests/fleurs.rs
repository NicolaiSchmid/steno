//! Gate G1 of the speech-stack plan on the ONNX path with our own decode
//! loop: mean WER on the FLEURS German `cat/` set (ten 7-minute files, the
//! chunker in the loop) within 0.5 points of spike F's 5.3 % for the fp32
//! export through sherpa-onnx. Also the logits-split check the Rust port
//! plan lists as WP4's first test, and the stable plan's A9 comparison:
//! the same speech at 44.1 kHz through the decoder's resamplers gives a
//! WER at most a tenth of a point over the same speech at 48 kHz. They
//! need the model files and the data, so they skip with a message unless
//! `STENO_MODELS_DIR` and `STENO_FLEURS_DIR` point at them (`.plans/spikes/2026-10-01-spike-fleurs-wer.md`
//! has the recipe); CI never has them.

#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::float_cmp,
    clippy::assert_is_empty
)]

mod common;

use std::time::Instant;

use steno_audio::SymphoniaAudioCodec;
use steno_audio::realtime::RateConverter;
use steno_speech::{
    ModelAsset, OnnxBackend, OnnxOptions, OnnxSpeechEngine, PipelineConfig, SAMPLE_RATE, VadConfig,
};

const SPIKE_F_MEAN_WER: f64 = 0.053;
const MODELS: &str = "the fp32 export and Silero VAD";
const TOLERANCE: f64 = 0.005;

#[test]
fn the_export_splits_into_8193_pieces_and_five_duration_bins() {
    let Some(models) = common::installed_store() else {
        return common::skip("STENO_MODELS_DIR", MODELS);
    };
    let (backend, vocab) = OnnxBackend::load(
        &models.directory(&ModelAsset::parakeet_v3_fp32()),
        &OnnxOptions::default(),
    )
    .unwrap();
    let shape = steno_speech::SpeechBackend::shape(&backend);
    assert_eq!(vocab.len(), 8_193);
    assert_eq!(shape.blank_id, 8_192);
    assert_eq!(shape.vocab_size, 8_193);
    assert_eq!(shape.durations, vec![0, 1, 2, 3, 4]);
    assert_eq!(
        (
            shape.decoder_layers,
            shape.decoder_hidden,
            shape.encoder_hidden
        ),
        (2, 640, 1_024)
    );
    assert_eq!(vocab.piece(8_192), "");
}

#[test]
fn fleurs_cat_mean_wer_is_within_half_a_point_of_spike_f() {
    let Some(models) = common::installed_store() else {
        return common::skip("STENO_MODELS_DIR", MODELS);
    };
    let Some(fleurs) = common::fleurs_dir() else {
        return common::skip(
            "STENO_FLEURS_DIR",
            "the FLEURS German cat/ and utt/ directories",
        );
    };
    let mut transcriber = OnnxSpeechEngine::open_transcriber(
        &models,
        &OnnxOptions::default(),
        PipelineConfig::default(),
        VadConfig::default(),
    )
    .expect("open the export and Silero");
    let mut files: Vec<_> = std::fs::read_dir(fleurs.join("cat"))
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "wav"))
        .collect();
    files.sort();
    assert!(!files.is_empty());
    let mut wers = Vec::new();
    let (mut total_words, mut total_errors, mut subs, mut dels, mut ins) = (0, 0, 0, 0, 0);
    println!("\n| File | Ref words | Hyp words | WER | S/D/I | Chunks | Recoveries | RTFx |");
    println!("|---|---:|---:|---:|---|---:|---:|---:|");
    for path in &files {
        let samples = common::read_wav(path);
        let reference = std::fs::read_to_string(path.with_extension("ref.txt")).unwrap();
        let started = Instant::now();
        let transcript = transcriber.transcribe(&samples, None).unwrap();
        let wall = started.elapsed().as_secs_f64();
        let hypothesis = transcript.text();
        let score = common::score(&reference, &hypothesis);
        let audio_seconds = samples.len() as f64 / SAMPLE_RATE as f64;
        println!(
            "| {} | {} | {} | {:.1}% | {}/{}/{} | {} | {}/{} | {:.1} |",
            path.file_stem().unwrap().to_string_lossy(),
            score.reference_words,
            common::normalise(&hypothesis).len(),
            score.rate() * 100.0,
            score.substitutions,
            score.deletions,
            score.insertions,
            transcript.chunks.len(),
            transcript.stats.recoveries_accepted,
            transcript.stats.recoveries_tried,
            audio_seconds / wall
        );
        wers.push(score.rate());
        total_words += score.reference_words;
        total_errors += score.edits();
        subs += score.substitutions;
        dels += score.deletions;
        ins += score.insertions;
    }
    let mean = wers.iter().sum::<f64>() / wers.len() as f64;
    let pooled = total_errors as f64 / total_words as f64;
    println!(
        "| mean | {total_words} | | {:.1}% | {subs}/{dels}/{ins} | | | pooled {:.1}% |\n",
        mean * 100.0,
        pooled * 100.0
    );
    // One-sided on purpose: a better mean than spike F's is not a failure.
    assert!(
        mean <= SPIKE_F_MEAN_WER + TOLERANCE,
        "mean WER {:.2}% is more than {:.1} points over spike F's {:.1}%",
        mean * 100.0,
        TOLERANCE * 100.0,
        SPIKE_F_MEAN_WER * 100.0
    );
}

/// The A9 bound: a tenth of a point of WER.
const RATE_TOLERANCE: f64 = 0.001;

/// `samples` at 16 kHz to `rate` through the capture's converter: its
/// last half window flushed with zeros, then cut to the exact length.
fn upsample(samples: &[f32], rate: f64) -> Vec<f32> {
    const BLOCK: usize = 1_600;
    let length = (samples.len() as f64 * rate / 16_000.0).round() as usize;
    let mut converter = RateConverter::new(16_000.0, rate, BLOCK);
    let mut output = Vec::with_capacity(length + BLOCK * 3);
    let mut block = vec![0.0f32; converter.max_output()];
    let tail = [0.0f32; 64];
    for chunk in samples.chunks(BLOCK).chain([&tail[..]]) {
        let written = converter.process(chunk, &mut block);
        output.extend_from_slice(&block[..written]);
    }
    output.resize(length, 0.0);
    output
}

/// Stable plan A9: speech recorded at 48 kHz and the same speech at
/// 44.1 kHz (the phone's rate) give the same transcript quality. Each
/// FLEURS file is brought up to both rates by the capture's converter and
/// back to 16 kHz by the decoder's path for that rate (the exact 3:1 FIR
/// from 48 kHz, the windowed sinc from 44.1 kHz), then transcribed; the
/// 44.1 kHz mean is at most a tenth of a point over the 48 kHz one.
/// One-sided, as the G1 gate above is: the two lanes differ only at the
/// resamplers' passband edges, yet that flips single words either way,
/// so a file's WER moves by up to two points between them and the means
/// by half a point (measured on atlas: 5.51 % from 48 kHz, 5.02 % from
/// 44.1 kHz). FLEURS is 16 kHz speech, so this measures the two
/// passbands and phases on real speech; the aliasing of content above
/// 8 kHz is `steno-audio`'s `tests/resampler_sweep.rs`.
#[test]
fn fleurs_wer_from_44k1_is_at_most_a_tenth_of_a_point_over_48k() {
    let Some(models) = common::installed_store() else {
        return common::skip("STENO_MODELS_DIR", MODELS);
    };
    let Some(fleurs) = common::fleurs_dir() else {
        return common::skip("STENO_FLEURS_DIR", "the FLEURS German cat/ directory");
    };
    let mut transcriber = OnnxSpeechEngine::open_transcriber(
        &models,
        &OnnxOptions::default(),
        PipelineConfig::default(),
        VadConfig::default(),
    )
    .expect("open the export and Silero");
    let mut files: Vec<_> = std::fs::read_dir(fleurs.join("cat"))
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "wav"))
        .collect();
    files.sort();
    assert!(!files.is_empty());
    let mut wers: [Vec<f64>; 2] = [Vec::new(), Vec::new()];
    let mut errors = [0usize; 2];
    let mut words = 0;
    println!("\n| File | Ref words | WER from 48 kHz | WER from 44.1 kHz |");
    println!("|---|---:|---:|---:|");
    for path in &files {
        let samples = common::read_wav(path);
        let reference = std::fs::read_to_string(path.with_extension("ref.txt")).unwrap();
        let lanes = [
            SymphoniaAudioCodec::to_16k(&upsample(&samples, 48_000.0), 48_000),
            SymphoniaAudioCodec::to_16k(&upsample(&samples, 44_100.0), 44_100),
        ];
        let mut row = [0.0; 2];
        let mut file_words = 0;
        for (index, lane) in lanes.iter().enumerate() {
            assert!(
                lane.len().abs_diff(samples.len()) <= 1,
                "{}",
                path.display()
            );
            let transcript = transcriber.transcribe(lane, None).unwrap();
            let score = common::score(&reference, &transcript.text());
            wers[index].push(score.rate());
            errors[index] += score.edits();
            row[index] = score.rate();
            file_words = score.reference_words;
        }
        words += file_words;
        println!(
            "| {} | {file_words} | {:.2}% | {:.2}% |",
            path.file_stem().unwrap().to_string_lossy(),
            row[0] * 100.0,
            row[1] * 100.0
        );
    }
    let mean = |wers: &[f64]| wers.iter().sum::<f64>() / wers.len() as f64;
    let (at_48k, at_44k1) = (mean(&wers[0]), mean(&wers[1]));
    println!(
        "| mean | {words} | {:.2}% (pooled {:.2}%) | {:.2}% (pooled {:.2}%) |\n",
        at_48k * 100.0,
        errors[0] as f64 / words as f64 * 100.0,
        at_44k1 * 100.0,
        errors[1] as f64 / words as f64 * 100.0
    );
    assert!(
        at_44k1 <= at_48k + RATE_TOLERANCE,
        "mean WER {:.2}% from 44.1 kHz against {:.2}% from 48 kHz",
        at_44k1 * 100.0,
        at_48k * 100.0
    );
}
