//! Transcribes 16 kHz mono PCM-16 WAV files with the ONNX backend and
//! prints the text, the segments and the decode statistics; with
//! `--range START-END` (seconds) it decodes that one window without the
//! chunker, which is how the zero-token window of spike D is probed.
//! `STENO_MODELS_DIR` names the store root, the directory with
//! `parakeet-tdt-0.6b-v3-fp32/` and `silero-vad/` in it; for the app's
//! copies that is `<models directory>/onnx`.
//!
//! ```sh
//! STENO_MODELS_DIR=/path/to/models/onnx cargo run --release -p steno-speech --example transcribe -- file.wav
//! cargo run --release -p steno-speech --example transcribe -- --range 25-48 file.wav
//! ```

#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::float_cmp,
    clippy::assert_is_empty
)]

// The WAV reader is the integration tests'; the scorer beside it is unused here.
#[path = "../tests/common/mod.rs"]
mod common;

use std::path::PathBuf;
use std::time::Instant;

use common::read_wav;
use steno_speech::{
    DecodeStats, ModelStore, OnnxOptions, OnnxSpeechEngine, PipelineConfig, SAMPLE_RATE, VadConfig,
    sample_count,
};

fn main() {
    let mut range: Option<(f32, f32)> = None;
    let mut files: Vec<PathBuf> = Vec::new();
    let mut args = std::env::args().skip(1);
    let usage = "usage: transcribe [--range START-END] <wav>...";
    while let Some(arg) = args.next() {
        if arg == "--help" || arg == "-h" {
            println!("{usage}");
            return;
        }
        if arg == "--range" {
            let value = args.next().expect("--range START-END");
            let (start, end) = value.split_once('-').expect("--range START-END");
            range = Some((start.parse().unwrap(), end.parse().unwrap()));
        } else {
            files.push(PathBuf::from(arg));
        }
    }
    assert!(!files.is_empty(), "{usage}");

    let started = Instant::now();
    let mut transcriber = OnnxSpeechEngine::open_transcriber(
        &ModelStore::from_environment(),
        &OnnxOptions::default(),
        PipelineConfig::default(),
        VadConfig::default(),
    )
    .unwrap_or_else(|e| panic!("{e}"));
    eprintln!("models loaded in {:.1} s", started.elapsed().as_secs_f64());

    for file in &files {
        let samples = read_wav(file);
        let seconds = samples.len() as f64 / SAMPLE_RATE as f64;
        let started = Instant::now();
        if let Some((start, end)) = range {
            let mut stats = DecodeStats::default();
            let tokens = transcriber
                .decode_range(&samples, sample_count(start)..sample_count(end), &mut stats)
                .unwrap_or_else(|e| panic!("{e}"));
            let text = transcriber.render(&tokens);
            println!(
                "{} {start:.1}-{end:.1}s: {} tokens, {} words, {} joint calls, {:.1} s\n{text}",
                file.display(),
                tokens.len(),
                text.split_whitespace().count(),
                stats.joint_calls,
                started.elapsed().as_secs_f64()
            );
            continue;
        }
        let transcript = transcriber
            .transcribe(&samples, None)
            .unwrap_or_else(|e| panic!("{e}"));
        let wall = started.elapsed().as_secs_f64();
        println!(
            "# {} ({seconds:.1} s audio, {wall:.1} s wall, RTFx {:.1})",
            file.display(),
            seconds / wall
        );
        println!(
            "# {} speech regions, {} chunks, {} tokens, {} words, {:?}",
            transcript.speech.len(),
            transcript.chunks.len(),
            transcript.tokens.len(),
            transcript.words.len(),
            transcript.stats
        );
        for segment in &transcript.segments {
            println!(
                "[{:>7.2} - {:>7.2}] {} {}",
                segment.start,
                segment.end,
                segment.language.as_ref().map_or("--", |l| l.as_str()),
                segment.text
            );
        }
        println!();
    }
}
