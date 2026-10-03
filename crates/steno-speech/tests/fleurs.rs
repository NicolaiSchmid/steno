//! Gate G1 of the speech-stack plan on the ONNX path with our own decode
//! loop: mean WER on the FLEURS German `cat/` set (ten 7-minute files, the
//! chunker in the loop) within 0.5 points of spike F's 5.3 % for the fp32
//! export through sherpa-onnx. Also the logits-split check the Rust port
//! plan lists as WP4's first test. Both need the model files and the data,
//! so they skip with a message unless `STENO_MODELS_DIR` and
//! `STENO_FLEURS_DIR` point at them (`.plans/spikes/2026-10-01-spike-fleurs-wer.md`
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
            score.wer() * 100.0,
            score.substitutions,
            score.deletions,
            score.insertions,
            transcript.chunks.len(),
            transcript.stats.recoveries_accepted,
            transcript.stats.recoveries_tried,
            audio_seconds / wall
        );
        wers.push(score.wer());
        total_words += score.reference_words;
        total_errors += score.errors;
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
