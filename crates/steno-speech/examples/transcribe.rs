//! Transcribes 16 kHz mono PCM-16 WAV files with the ONNX backend and
//! prints the text, the segments and the decode statistics; with
//! `--range START-END` (seconds) it decodes that one window without the
//! chunker, which is how the zero-token window of spike D is probed.
//!
//! ```sh
//! STENO_MODELS_DIR=/path/to/models cargo run --release -p steno-speech --example transcribe -- file.wav
//! cargo run --release -p steno-speech --example transcribe -- --range 25-48 file.wav
//! ```

#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::float_cmp,
    clippy::assert_is_empty
)]

use std::path::{Path, PathBuf};
use std::time::Instant;

use steno_speech::{
    DecodeStats, LanguageTagger, ModelAsset, ModelStore, OnnxBackend, OnnxOptions, PipelineConfig,
    SileroVad, Transcriber, VadConfig,
};

fn read_wav(path: &Path) -> Vec<f32> {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    assert_eq!(&bytes[0..4], b"RIFF", "{}: not a WAV", path.display());
    let mut offset = 12;
    let mut channels = 1usize;
    while offset + 8 <= bytes.len() {
        let id = &bytes[offset..offset + 4];
        let size = u32::from_le_bytes(bytes[offset + 4..offset + 8].try_into().unwrap()) as usize;
        let body = &bytes[offset + 8..(offset + 8 + size).min(bytes.len())];
        if id == b"fmt " {
            channels = usize::from(u16::from_le_bytes(body[2..4].try_into().unwrap()));
            let rate = u32::from_le_bytes(body[4..8].try_into().unwrap());
            assert_eq!(rate, 16_000, "{}: {rate} Hz, need 16 kHz", path.display());
        } else if id == b"data" {
            return body
                .chunks_exact(2 * channels)
                .map(|frame| f32::from(i16::from_le_bytes([frame[0], frame[1]])) / 32_768.0)
                .collect();
        }
        offset += 8 + size + (size & 1);
    }
    panic!("{}: no data chunk", path.display());
}

fn main() {
    let mut range: Option<(f32, f32)> = None;
    let mut files: Vec<PathBuf> = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--range" {
            let value = args.next().expect("--range START-END");
            let (start, end) = value.split_once('-').expect("--range START-END");
            range = Some((start.parse().unwrap(), end.parse().unwrap()));
        } else {
            files.push(PathBuf::from(arg));
        }
    }
    assert!(
        !files.is_empty(),
        "usage: transcribe [--range START-END] <wav>..."
    );

    let store = ModelStore::from_environment();
    let options = OnnxOptions::default();
    let started = Instant::now();
    let model_dir = store
        .ensure(&ModelAsset::parakeet_v3_fp32(), &mut |_| {})
        .unwrap_or_else(|e| panic!("{e}"));
    let vad_dir = store
        .ensure(&ModelAsset::silero_vad(), &mut |_| {})
        .unwrap_or_else(|e| panic!("{e}"));
    let (backend, vocab) =
        OnnxBackend::load(&model_dir, &options).unwrap_or_else(|e| panic!("{e}"));
    let vad = SileroVad::load(
        &vad_dir.join("silero_vad.onnx"),
        &options,
        VadConfig::default(),
    )
    .unwrap_or_else(|e| panic!("{e}"));
    let mut transcriber = Transcriber::new(
        backend,
        vocab,
        Box::new(vad),
        LanguageTagger::new(),
        PipelineConfig::default(),
    );
    eprintln!("models loaded in {:.1} s", started.elapsed().as_secs_f64());

    for file in &files {
        let samples = read_wav(file);
        let seconds = samples.len() as f64 / 16_000.0;
        let started = Instant::now();
        if let Some((start, end)) = range {
            let range = ((start * 16_000.0) as usize).min(samples.len())
                ..((end * 16_000.0) as usize).min(samples.len());
            let mut stats = DecodeStats::default();
            let tokens = transcriber
                .decode_range(&samples, range.clone(), &mut stats)
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
