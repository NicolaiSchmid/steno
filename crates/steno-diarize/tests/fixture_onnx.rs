//! The ONNX backend on the repository's two-voice `say` fixture: two
//! speakers, as the Swift model test expects. Ignored because it needs the
//! model files: set `STENO_MODELS_DIR` to a models directory holding
//! `onnx/diarization/`, or allow the download (about 32 MB) into it or,
//! without the variable, into the default models directory
//! (`steno_speech::ModelStore::from_environment`).

#![cfg(feature = "onnx")]

use std::path::PathBuf;

use steno_core::AudioBuffer16k;
use steno_diarize::onnx::OnnxBackend;
use steno_diarize::{DiarizerConfig, Pipeline};
use steno_speech::ModelStore;

fn fixture(name: &str) -> AudioBuffer16k {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../Tests/Fixtures/speech")
        .join(name);
    let mut reader = hound::WavReader::open(&path).expect("fixture opens");
    assert_eq!(reader.spec().sample_rate, 16_000);
    assert_eq!(reader.spec().channels, 1);
    let samples = reader
        .samples::<i16>()
        .map(|sample| f32::from(sample.unwrap()) / 32_768.0)
        .collect();
    AudioBuffer16k::new(samples)
}

fn store() -> ModelStore {
    ModelStore::from_environment()
}

#[test]
#[ignore = "needs the ONNX model files (STENO_MODELS_DIR or a download)"]
fn two_voices_fixture_gives_two_speakers() {
    let backend = OnnxBackend::from_store(&store(), 2).expect("backend loads");
    let mut pipeline = Pipeline::new(backend, DiarizerConfig::default());
    let audio = fixture("two-speakers-mf.wav");
    let result = pipeline.diarize(&audio).expect("diarizes");
    eprintln!("{result:#?}");
    // The fixture is under thirty seconds, so the refinement leaves the
    // mapping alone; the mapping itself must find two voices.
    assert_eq!(result.clusters.len(), 2, "{result:?}");
    for cluster in &result.clusters {
        let embedding = cluster.embedding.as_ref().expect("embedding");
        assert_eq!(embedding.0.len(), 256);
        assert!((embedding.magnitude() - 1.0).abs() < 1e-4);
    }
    let cosine = result.clusters[0]
        .embedding
        .as_ref()
        .unwrap()
        .cosine_similarity(result.clusters[1].embedding.as_ref().unwrap());
    eprintln!("cosine between the two voices: {cosine:.3}");
    assert!(cosine < 0.6, "two different voices: {cosine}");
}

#[test]
#[ignore = "needs the ONNX model files (STENO_MODELS_DIR or a download)"]
fn the_segmentation_model_reports_pyannotes_geometry() {
    let backend = OnnxBackend::from_store(&store(), 2).expect("backend loads");
    let geometry = steno_diarize::DiarizationBackend::geometry(&backend);
    assert_eq!(geometry, &steno_diarize::SegmentationGeometry::PYANNOTE_3_0);
    assert_eq!(backend.embedding_dimension(), 256);
    eprintln!("{backend:?}");
}
