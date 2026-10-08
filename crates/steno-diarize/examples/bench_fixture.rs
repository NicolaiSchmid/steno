//! Times the ONNX backend on the two-voice fixture tiled to ten minutes:
//! `STENO_MODELS_DIR=... cargo run --release -p steno-diarize --example bench_fixture`,
//! the models directory whose `onnx/diarization/` holds the models.
//! A timing run, not part of the test suite.

use std::path::PathBuf;
use std::time::Instant;

use steno_core::AudioBuffer16k;
use steno_diarize::onnx::OnnxBackend;
use steno_diarize::{DiarizerConfig, Pipeline};
use steno_speech::ModelStore;

fn main() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../Tests/Fixtures/speech/two-speakers-mf.wav");
    let mut reader = hound::WavReader::open(&path).expect("fixture");
    let one: Vec<f32> = reader
        .samples::<i16>()
        .map(|s| f32::from(s.unwrap()) / 32_768.0)
        .collect();
    let repeats: usize = std::env::args()
        .nth(1)
        .and_then(|a| a.parse().ok())
        .unwrap_or(64);
    let mut samples = Vec::with_capacity(one.len() * repeats);
    for _ in 0..repeats {
        samples.extend_from_slice(&one);
    }
    let audio = AudioBuffer16k::new(samples);
    let store = ModelStore::in_models_directory(
        &ModelStore::environment_models_directory().expect("STENO_MODELS_DIR"),
    );
    let threads: usize = std::env::var("STENO_DIARIZE_THREADS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(4);
    let backend =
        OnnxBackend::from_store(&store, steno_diarize::Install::Allowed, threads).expect("backend");
    let mut pipeline = Pipeline::new(backend, DiarizerConfig::default());
    let load = std::fs::read_to_string("/proc/loadavg").unwrap_or_default();
    let started = Instant::now();
    let analysis = pipeline.analyze(&audio).expect("analysis");
    let analysed = started.elapsed().as_secs_f64();
    let mapped = pipeline.map(&analysis, DiarizerConfig::default().clustering_threshold);
    let mapped_at = started.elapsed().as_secs_f64();
    let refined = pipeline.refine(&mapped, &audio).expect("refine");
    let total = started.elapsed().as_secs_f64();
    println!(
        "audio {:.0} s, windows {}, embeddings {}, analysis {:.1} s (RTFx {:.1}), clustering+mapping {:.2} s, refinement {:.1} s, total {:.1} s (RTFx {:.1}); mapped {} clusters, refined {}; load {}",
        audio.duration(),
        analysis.activities.len(),
        analysis.embeddings.len(),
        analysed,
        audio.duration() / analysed,
        mapped_at - analysed,
        total - mapped_at,
        total,
        audio.duration() / total,
        mapped.clusters.len(),
        refined.clusters.len(),
        load.trim()
    );
}
