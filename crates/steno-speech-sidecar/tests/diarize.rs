//! The diarizer in the sidecar: [`SidecarSpeechEngine::diarize`] and
//! `steno_diarize::SidecarDiarizer` against the real binary. The fake
//! engine's tests need no models: the clusters cross bit for bit, a child
//! started only to diarize is stopped after the call, one that holds
//! speech's models is kept, a child that dies, aborts, hangs or overruns
//! the memory ceiling while it diarizes fails that call, leaves `DirectML`
//! alone and is replaced by the next, one that failed while idle is
//! replaced before the diarization, and a refused load keeps a child that
//! holds speech. A child that aborts, hangs or overruns the ceiling in the
//! diarizer's load is killed and fails the call as a failed load, which
//! `SidecarDiarizer` checks against the manifest: files of the right size
//! that fail their checksum are deleted and the call is not installed,
//! while an abort in the diarization itself stays the sidecar's crash. The
//! real engine refuses model files it cannot load without dying.
//!
//! The ignored tests run the real models: set `STENO_MODELS_DIR` to a
//! models directory holding `onnx/diarization/`. They compare the sidecar's
//! result with the in-process pipeline's on the repository's two-voice
//! fixture (exactly: the same code on the same ops), kill a child
//! mid-diarization of ten minutes of it, and fail loads over a copy of the
//! models: intact files are kept, and only one that fails its checksum is
//! deleted.

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use common::{
    ASKED_FOR_DIRECTML, alive, assert_works, binary, config, engine_in, engine_with_fault,
    fault_queued_soon, kill, kill_idle_child, provider, sidecar_error, tone, within_ten_seconds,
};
use steno_core::{AudioBuffer16k, Diarizer as _, SpeechEngine as _};
use steno_diarize::onnx::OnnxBackend;
use steno_diarize::{DiarizeError, DiarizerConfig, Install, Pipeline, SidecarDiarizer};
use steno_speech::sidecar::{DiarizerModels, directml_switched_off};
use steno_speech::{ModelStore, SidecarConfig, SidecarError, SidecarSpeechEngine, SpeechError};

/// The pid the `--fault-once` marker in `dir` holds, once the faulting
/// child has written it.
fn faulted(dir: &tempfile::TempDir) -> Option<u32> {
    std::fs::read_to_string(dir.path().join("faulted"))
        .ok()?
        .parse()
        .ok()
}

/// Model files the fake engine never opens.
fn fake_models() -> DiarizerModels {
    DiarizerModels {
        segmentation: PathBuf::from("/nowhere/segmentation.onnx"),
        embedding: PathBuf::from("/nowhere/embedding.onnx"),
        intra_threads: 4,
    }
}

/// Whether `error` is a diarizer load the child refused, reporting it
/// itself, rather than one it died or hung in.
fn refused_load(error: &SidecarError) -> bool {
    match error {
        SidecarError::DiarizerLoad(load) => matches!(**load, SidecarError::Remote(_)),
        _ => false,
    }
}

/// Diarizes `audio` and checks the fake diarizer's answer bit for bit.
async fn assert_diarizes(engine: &SidecarSpeechEngine, audio: &AudioBuffer16k) {
    let clusters = engine.diarize(fake_models(), audio).await.unwrap();
    let peak = audio.samples.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    assert_eq!(clusters.len(), 1, "{clusters:?}");
    let cluster = &clusters[0];
    assert_eq!(cluster.label, "Speaker 1");
    assert_eq!(cluster.ranges.len(), 1);
    assert_eq!(cluster.ranges[0].upper, audio.duration());
    assert_eq!(cluster.cluster_confidence.to_bits(), peak.to_bits());
    let embedding = cluster.embedding.as_ref().unwrap();
    let expected: Vec<u32> = (1..=256u16)
        .map(|i| (peak * f32::from(i) / 256.0).to_bits())
        .collect();
    let got: Vec<u32> = embedding.0.iter().map(|value| value.to_bits()).collect();
    assert_eq!(got, expected);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_child_started_to_diarize_is_stopped_after_the_call_and_one_with_speech_is_kept() {
    let dir = tempfile::tempdir().unwrap();
    let engine = engine_in(&dir, config(&[]));
    // Empty audio needs no child.
    let none = engine
        .diarize(fake_models(), &AudioBuffer16k::default())
        .await
        .unwrap();
    assert_eq!(none, []);
    assert_eq!(engine.spawns(), 0);

    // An odd sample proves the audio crossed as `f32` bits.
    let mut odd = tone(2.0);
    odd.samples[7] = 0.123_456_79;
    assert_diarizes(&engine, &odd).await;
    assert_eq!(engine.spawns(), 1);
    assert_eq!(engine.pid(), None, "the diarizer's own child is gone");
    assert_diarizes(&engine, &tone(1.0)).await;
    assert_eq!(engine.spawns(), 2);
    assert_eq!(engine.pid(), None);

    // With speech's models loaded, the diarizer uses that child and
    // leaves it for speech, which still answers in it.
    engine.prepare().await.unwrap();
    let pid = engine.pid().unwrap();
    assert_diarizes(&engine, &tone(1.5)).await;
    assert_diarizes(&engine, &tone(0.5)).await;
    assert_eq!(engine.pid(), Some(pid));
    assert!(engine.health().await.unwrap().unwrap().loaded);
    assert_works(&engine, &tone(0.5)).await;
    assert_eq!(engine.spawns(), 3);
    // Speech's release ends it, diarizer and all.
    engine.release().await.unwrap();
    assert_eq!(engine.pid(), None);
    assert!(within_ten_seconds(|| (!alive(pid)).then_some(())).is_some());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_child_killed_mid_diarization_fails_the_call_and_the_next_call_recovers() {
    let (engine, dir) = engine_with_fault("hang", |_| {});
    let engine = Arc::new(engine);
    let running = {
        let engine = Arc::clone(&engine);
        tokio::spawn(async move {
            engine
                .diarize(fake_models(), &tone(2.0))
                .await
                .map_err(|e| e.to_string())
        })
    };
    // The diarization is in flight once the child has read it and written
    // its marker (a blocking wait: the runtime has other workers).
    let pid = within_ten_seconds(|| faulted(&dir)).expect("the child faulted");
    assert_eq!(engine.pid(), Some(pid));
    assert!(!running.is_finished());
    assert!(kill(pid));
    let error = running.await.unwrap().unwrap_err();
    assert!(error.contains("died before answering"), "{error}");
    assert_eq!(engine.pid(), None);
    assert_diarizes(&engine, &tone(1.0)).await;
    assert_eq!(engine.spawns(), 2);
}

/// An abort inside a diarization, with speech loaded on `DirectML` where
/// that is asked for: the call fails as a crash, the child is gone, and
/// `DirectML` stays on, as the diarizer runs on the CPU; the next calls
/// load speech on it again and diarize.
#[tokio::test(flavor = "multi_thread")]
async fn an_abort_while_diarizing_is_a_crash_that_leaves_directml_on() {
    let (engine, _dir) = engine_with_fault("abort-diarizing", |config| {
        config.options.directml = true;
    });
    engine.prepare().await.unwrap();
    assert_eq!(provider(&engine).await, Some(ASKED_FOR_DIRECTML));
    let error = engine.diarize(fake_models(), &tone(1.0)).await.unwrap_err();
    let SidecarError::Crashed { status, .. } = sidecar_error(&error) else {
        panic!("{error}");
    };
    if cfg!(unix) {
        assert!(
            status.contains("SIGABRT") || status.contains("signal: 6"),
            "{status}"
        );
    }
    assert_eq!(engine.pid(), None);
    assert!(!directml_switched_off());
    assert_works(&engine, &tone(0.5)).await;
    assert_eq!(provider(&engine).await, Some(ASKED_FOR_DIRECTML));
    assert_diarizes(&engine, &tone(1.0)).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_diarization_the_child_fails_keeps_a_child_that_holds_speech() {
    let (engine, _dir) = engine_with_fault("error", |_| {});
    engine.prepare().await.unwrap();
    let pid = engine.pid().unwrap();
    let error = engine
        .diarize(fake_models(), &tone(1.0))
        .await
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("simulated failure in the diarizer"),
        "{error}"
    );
    assert_eq!(engine.pid(), Some(pid));
    assert_diarizes(&engine, &tone(1.0)).await;
    assert_eq!(engine.spawns(), 1);
}

/// A diarization that never answers is killed at its deadline (the floor,
/// for a second of audio); the next call diarizes in a new child.
#[tokio::test(flavor = "multi_thread")]
async fn a_hung_diarization_is_killed_at_the_deadline() {
    let (engine, _dir) = engine_with_fault("hang", |c| {
        c.transcribe_timeout_floor = Duration::from_secs(2);
    });
    let error = engine.diarize(fake_models(), &tone(1.0)).await.unwrap_err();
    assert!(
        matches!(sidecar_error(&error), SidecarError::Timeout { after } if *after == Duration::from_secs(2)),
        "{error}"
    );
    assert_eq!(engine.pid(), None);
    assert_diarizes(&engine, &tone(1.0)).await;
    assert_eq!(engine.spawns(), 2);
    assert_eq!(engine.pid(), None);
}

/// A hang in a child that holds speech ends that child too; speech and the
/// diarizer both work again in a new one.
#[tokio::test(flavor = "multi_thread")]
async fn a_hung_diarization_in_a_child_with_speech_ends_it_and_both_recover() {
    let (engine, _dir) = engine_with_fault("hang", |c| {
        c.transcribe_timeout_floor = Duration::from_secs(2);
    });
    // The hang is the first transcription's or diarization's, so speech
    // is only loaded here.
    engine.prepare().await.unwrap();
    let pid = engine.pid().unwrap();
    let error = engine.diarize(fake_models(), &tone(1.0)).await.unwrap_err();
    assert!(
        matches!(sidecar_error(&error), SidecarError::Timeout { .. }),
        "{error}"
    );
    assert_eq!(engine.pid(), None);
    assert!(within_ten_seconds(|| (!alive(pid)).then_some(())).is_some());
    assert_works(&engine, &tone(0.5)).await;
    assert_diarizes(&engine, &tone(1.0)).await;
    assert_eq!(engine.spawns(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_diarization_over_the_memory_ceiling_is_killed() {
    const CEILING: u64 = 256 << 20;
    let (engine, _dir) = engine_with_fault("allocate", |c| c.memory_ceiling_bytes = CEILING);
    let error = engine.diarize(fake_models(), &tone(1.0)).await.unwrap_err();
    let SidecarError::MemoryCeiling {
        rss_bytes,
        ceiling_bytes,
    } = sidecar_error(&error)
    else {
        panic!("{error}");
    };
    assert_eq!(*ceiling_bytes, CEILING);
    assert!(*rss_bytes > CEILING);
    // Well under what the child would have taken (4 GiB) unchecked.
    assert!(*rss_bytes < 2 << 30, "{rss_bytes}");
    assert_eq!(engine.pid(), None);
    assert_diarizes(&engine, &tone(1.0)).await;
    assert_eq!(engine.spawns(), 2);
}

/// A child that died between requests is replaced before the diarization,
/// without an error; the new one holds no speech, so it stops after.
#[tokio::test(flavor = "multi_thread")]
async fn a_child_that_died_idle_is_replaced_before_a_diarization() {
    let dir = tempfile::tempdir().unwrap();
    let engine = engine_in(&dir, config(&[]));
    engine.prepare().await.unwrap();
    kill_idle_child(&engine);
    assert_diarizes(&engine, &tone(1.0)).await;
    assert_eq!(engine.pid(), None);
    assert_eq!(engine.spawns(), 2);
}

/// A child over the ceiling while idle, after speech asked for `DirectML`
/// where that is asked for, is replaced before the diarization, without an
/// error; found by a diarization, the overrun leaves `DirectML` on.
#[tokio::test(flavor = "multi_thread")]
async fn a_child_over_the_ceiling_while_idle_is_replaced_before_a_diarization() {
    let (engine, _dir) = engine_with_fault("swell", |config| {
        config.options.directml = true;
    });
    engine.prepare().await.unwrap();
    assert_works(&engine, &tone(0.5)).await;
    assert!(
        fault_queued_soon(&engine),
        "the reader never queued the report over the ceiling"
    );
    assert_diarizes(&engine, &tone(1.0)).await;
    assert_eq!(engine.pid(), None);
    assert_eq!(engine.spawns(), 2);
    assert!(!directml_switched_off());
}

/// A load the child refuses, in a child that holds speech for another
/// job: the call fails as a refused load and the child stays, speech and
/// all.
#[tokio::test(flavor = "multi_thread")]
async fn a_refused_load_keeps_a_child_that_holds_speech() {
    let (engine, _dir) = engine_with_fault("refuse-diarizer", |_| {});
    engine.prepare().await.unwrap();
    let pid = engine.pid().unwrap();
    for _ in 0..2 {
        let error = engine.diarize(fake_models(), &tone(1.0)).await.unwrap_err();
        assert!(refused_load(sidecar_error(&error)), "{error}");
        assert_eq!(engine.pid(), Some(pid));
    }
    assert_works(&engine, &tone(0.5)).await;
    assert_eq!(engine.spawns(), 1);
}

/// An abort inside the diarizer's load, in a child that holds speech on
/// `DirectML` where that is asked for: the call fails as a failed load
/// over the crash, so the caller checks the files; the child is killed,
/// `DirectML` stays on, and the next calls load speech and the diarizer
/// in a new child. The `DirectML` assertion has power only on Windows,
/// the one place the fake engine's speech runs on `DirectML`.
#[tokio::test(flavor = "multi_thread")]
async fn an_abort_in_the_diarizer_load_is_a_failed_load_that_ends_the_child() {
    let (engine, _dir) = engine_with_fault("abort-on-diarizer-load", |config| {
        config.options.directml = true;
    });
    engine.prepare().await.unwrap();
    // The fault waits for the diarizer's load.
    assert_works(&engine, &tone(0.5)).await;
    let pid = engine.pid().unwrap();
    let error = engine.diarize(fake_models(), &tone(1.0)).await.unwrap_err();
    let SidecarError::DiarizerLoad(load) = sidecar_error(&error) else {
        panic!("{error}");
    };
    assert!(matches!(**load, SidecarError::Crashed { .. }), "{error}");
    assert_eq!(engine.pid(), None);
    assert!(within_ten_seconds(|| (!alive(pid)).then_some(())).is_some());
    assert!(!directml_switched_off());
    assert_works(&engine, &tone(0.5)).await;
    assert_eq!(provider(&engine).await, Some(ASKED_FOR_DIRECTML));
    assert_diarizes(&engine, &tone(1.0)).await;
    assert_eq!(engine.spawns(), 2);
}

/// A diarizer load that never answers is killed at the load timeout and
/// fails the call as a failed load over the timeout; the next call
/// diarizes in a new child.
#[tokio::test(flavor = "multi_thread")]
async fn a_hung_diarizer_load_is_killed_at_the_load_timeout() {
    let (engine, _dir) = engine_with_fault("hang-on-diarizer-load", |config| {
        config.load_timeout = Duration::from_secs(2);
    });
    let error = engine.diarize(fake_models(), &tone(1.0)).await.unwrap_err();
    let SidecarError::DiarizerLoad(load) = sidecar_error(&error) else {
        panic!("{error}");
    };
    assert!(
        matches!(**load, SidecarError::Timeout { after } if after == Duration::from_secs(2)),
        "{error}"
    );
    assert_eq!(engine.pid(), None);
    assert_diarizes(&engine, &tone(1.0)).await;
    assert_eq!(engine.spawns(), 2);
}

/// A child over the memory ceiling while it loads the diarizer's models
/// (a ceiling under its idle resident set, and a load that never answers,
/// so the first heartbeat ends it there): killed, and the call fails as a
/// failed load over the overrun, so the caller checks the files.
#[tokio::test(flavor = "multi_thread")]
async fn a_diarizer_load_over_the_memory_ceiling_is_a_failed_load() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = config(&["--fault", "hang-on-diarizer-load"]);
    config.memory_ceiling_bytes = 1 << 20;
    let engine = engine_in(&dir, config);
    let error = engine.diarize(fake_models(), &tone(1.0)).await.unwrap_err();
    let SidecarError::DiarizerLoad(load) = sidecar_error(&error) else {
        panic!("{error}");
    };
    assert!(
        matches!(**load, SidecarError::MemoryCeiling { ceiling_bytes, .. } if ceiling_bytes == 1 << 20),
        "{error}"
    );
    assert_eq!(engine.pid(), None);
    assert_eq!(engine.spawns(), 1);
}

/// The diarizer the app runs over an engine with `fault` (in its first
/// child only, see `engine_with_fault`) and model files of the manifest's
/// sizes in its store, all zeros, which the fake never opens and which
/// fail their checksums; the models' folder.
fn diarizer_over_junk(
    fault: &str,
    adjust: impl FnOnce(&mut SidecarConfig),
) -> (SidecarDiarizer, PathBuf, tempfile::TempDir) {
    let (engine, dir) = engine_with_fault(fault, adjust);
    let folder = engine.store().directory(&steno_diarize::models::asset());
    std::fs::create_dir_all(&folder).unwrap();
    for file in &steno_diarize::models::asset().files {
        std::fs::File::create(folder.join(&file.name))
            .unwrap()
            .set_len(file.size)
            .unwrap();
    }
    let diarizer = SidecarDiarizer::new(Arc::new(engine), Install::Never, 4);
    (diarizer, folder, dir)
}

/// A child that aborts or hangs while it loads files of the right size
/// that are not the models: the files fail their checksum and are
/// deleted, and the call is not installed, so the app's gate parks the
/// meeting until a download replaces them, rather than giving it the
/// fallback's speakers.
#[tokio::test(flavor = "multi_thread")]
async fn a_load_that_aborts_or_hangs_on_corrupt_files_deletes_them_and_is_not_installed() {
    for fault in ["abort-on-diarizer-load", "hang-on-diarizer-load"] {
        let (diarizer, folder, _dir) = diarizer_over_junk(fault, |config| {
            config.load_timeout = Duration::from_secs(2);
        });
        let error = diarizer.diarize(&tone(2.0)).await.unwrap_err();
        let Some(DiarizeError::NotInstalled { missing, .. }) = error.downcast_ref::<DiarizeError>()
        else {
            panic!("{fault}: {error}");
        };
        let names: Vec<String> = steno_diarize::models::asset()
            .files
            .iter()
            .map(|file| file.name.clone())
            .collect();
        assert_eq!(missing, &names, "{fault}");
        for name in &names {
            assert!(!folder.join(name).exists(), "{fault}: {name}");
        }
    }
}

/// An abort in the diarization itself, over the same files: the sidecar's
/// crash, with no checksum check, and the files stay.
#[tokio::test(flavor = "multi_thread")]
async fn an_abort_while_diarizing_corrupt_files_is_the_crash_and_keeps_them() {
    let (diarizer, folder, _dir) = diarizer_over_junk("abort-diarizing", |_| {});
    let error = diarizer.diarize(&tone(2.0)).await.unwrap_err();
    assert!(
        matches!(
            error.downcast_ref::<DiarizeError>(),
            Some(DiarizeError::Sidecar(SpeechError::Sidecar(
                SidecarError::Crashed { .. }
            )))
        ),
        "{error}"
    );
    for file in &steno_diarize::models::asset().files {
        assert!(folder.join(&file.name).exists(), "{}", file.name);
    }
}

/// The real engine handed files it cannot load: the call fails as a
/// refused load, reported by a child that is then stopped, not killed.
#[tokio::test(flavor = "multi_thread")]
async fn the_real_engine_refuses_model_files_it_cannot_load_and_keeps_running() {
    let dir = tempfile::tempdir().unwrap();
    let junk = DiarizerModels {
        segmentation: dir.path().join("segmentation.onnx"),
        embedding: dir.path().join("embedding.onnx"),
        intra_threads: 1,
    };
    std::fs::write(&junk.segmentation, b"not a model").unwrap();
    let mut config = SidecarConfig::new(binary());
    config.heartbeat = Duration::from_millis(20);
    let engine = SidecarSpeechEngine::with_assets(ModelStore::new(dir.path()), config, Vec::new());
    let error = engine.diarize(junk, &tone(1.0)).await.unwrap_err();
    assert!(refused_load(sidecar_error(&error)), "{error}");
    assert_eq!(engine.pid(), None);
    assert_eq!(engine.spawns(), 1);
}

/// The diarizer the app runs, over the fake engine and model files of the
/// manifest's sizes, which the fake never opens: a lane under a second
/// needs no child, and `prepare` starts none.
#[tokio::test(flavor = "multi_thread")]
async fn the_sidecar_diarizer_starts_a_child_only_to_diarize() {
    let dir = tempfile::tempdir().unwrap();
    let store = ModelStore::new(dir.path());
    let asset = steno_diarize::models::asset();
    let folder = store.directory(&asset);
    std::fs::create_dir_all(&folder).unwrap();
    for file in &asset.files {
        std::fs::File::create(folder.join(&file.name))
            .unwrap()
            .set_len(file.size)
            .unwrap();
    }
    let engine = Arc::new(SidecarSpeechEngine::with_assets(
        store,
        config(&[]),
        Vec::new(),
    ));
    let diarizer = SidecarDiarizer::new(Arc::clone(&engine), Install::Never, 4);
    diarizer.prepare().await.unwrap();
    let short = diarizer.diarize(&tone(0.9)).await.unwrap();
    assert_eq!(short.clusters, []);
    assert_eq!(engine.spawns(), 0);
    let result = diarizer.diarize(&tone(2.0)).await.unwrap();
    assert_eq!(result.clusters.len(), 1);
    assert_eq!(engine.spawns(), 1);
    assert_eq!(engine.pid(), None);
}

/// The repository's two-voice `say` fixture, tiled `times` over.
fn two_voices(times: usize) -> AudioBuffer16k {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../Tests/Fixtures/speech/two-speakers-mf.wav");
    let one = steno_speech::wav::read_pcm16(&path).unwrap();
    AudioBuffer16k::new(one.repeat(times))
}

/// The store `STENO_MODELS_DIR` names, the diarizer's models verified.
fn real_store() -> ModelStore {
    let store = ModelStore::from_environment();
    store
        .verify(&steno_diarize::models::asset())
        .expect("STENO_MODELS_DIR holds onnx/diarization/");
    store
}

/// The real child over `store`, the diarizer the app builds over it.
fn real_diarizer(store: ModelStore) -> (Arc<SidecarSpeechEngine>, SidecarDiarizer) {
    let engine = Arc::new(SidecarSpeechEngine::with_assets(
        store,
        SidecarConfig::new(binary()),
        Vec::new(),
    ));
    let diarizer = SidecarDiarizer::new(Arc::clone(&engine), Install::Never, 4);
    (engine, diarizer)
}

/// The fixture once (under thirty seconds a voice, so no refinement) and
/// eight times over (75 s, so each voice is refined): the child's clusters
/// are the in-process pipeline's, every bit of every float included.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the ONNX diarizer models (STENO_MODELS_DIR)"]
async fn the_sidecar_diarizes_exactly_as_the_pipeline_in_this_process() {
    let store = real_store();
    let backend = OnnxBackend::from_store(&store, Install::Never, 4).unwrap();
    let mut pipeline = Pipeline::new(backend, DiarizerConfig::default());
    let (engine, diarizer) = real_diarizer(store);
    for times in [1, 8] {
        let audio = two_voices(times);
        let here = pipeline.diarize(&audio).unwrap();
        let there = diarizer.diarize(&audio).await.unwrap();
        eprintln!(
            "{times}x: {} clusters, ranges {:?}",
            there.clusters.len(),
            there
                .clusters
                .iter()
                .map(|cluster| cluster.ranges.len())
                .collect::<Vec<_>>()
        );
        assert_eq!(here.clusters.len(), 2, "{here:?}");
        // Tiled, a voice passes the refinement's thirty seconds.
        let longest = here
            .clusters
            .iter()
            .map(|cluster| {
                cluster
                    .ranges
                    .iter()
                    .map(|range| range.upper - range.lower)
                    .sum::<f64>()
            })
            .fold(0.0, f64::max);
        assert_eq!(longest >= 30.0, times > 1, "{times}x: {longest:.1} s");
        assert_eq!(there, here);
        for (there, here) in there.clusters.iter().zip(&here.clusters) {
            let bits = |cluster: &steno_core::SpeakerCluster| -> Vec<u32> {
                cluster
                    .embedding
                    .as_ref()
                    .unwrap()
                    .0
                    .iter()
                    .map(|value| value.to_bits())
                    .collect()
            };
            assert_eq!(bits(there), bits(here));
        }
        assert_eq!(engine.pid(), None, "the diarizer's child is gone");
    }
}

/// Ten minutes of the fixture: the child is killed while it diarizes, the
/// call fails with the crash, and the next call diarizes in a new child,
/// as in this process.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the ONNX diarizer models (STENO_MODELS_DIR)"]
async fn a_child_killed_mid_diarization_of_real_audio_fails_the_call_and_the_next_succeeds() {
    let store = real_store();
    let (engine, diarizer) = real_diarizer(store.clone());
    let diarizer = Arc::new(diarizer);
    let running = {
        let diarizer = Arc::clone(&diarizer);
        tokio::spawn(async move {
            diarizer
                .diarize(&two_voices(64))
                .await
                .map_err(|e| e.to_string())
        })
    };
    let pid = within_ten_seconds(|| engine.pid()).expect("a child started");
    // Past the load of two small models, well inside the diarization.
    // (A blocking wait: the runtime has other workers.)
    std::thread::sleep(Duration::from_secs(3));
    assert!(!running.is_finished(), "ten minutes take longer");
    assert!(kill(pid));
    let error = running.await.unwrap().unwrap_err();
    assert!(error.contains("died before answering"), "{error}");
    assert_eq!(engine.pid(), None);

    let audio = two_voices(1);
    let backend = OnnxBackend::from_store(&store, Install::Never, 4).unwrap();
    let here = Pipeline::new(backend, DiarizerConfig::default())
        .diarize(&audio)
        .unwrap();
    assert_eq!(diarizer.diarize(&audio).await.unwrap(), here);
    assert_eq!(engine.spawns(), 2);
}

/// A writable store in `dir` holding a copy of the models in
/// `STENO_MODELS_DIR`, verified; the copy's folder. The tests below delete
/// files, so they never run over the originals.
fn copy_of_real_models(dir: &Path) -> PathBuf {
    let asset = steno_diarize::models::asset();
    let from = real_store().directory(&asset);
    let store = ModelStore::new(dir);
    let to = store.directory(&asset);
    std::fs::create_dir_all(&to).unwrap();
    for file in &asset.files {
        std::fs::copy(from.join(&file.name), to.join(&file.name)).unwrap();
    }
    store.verify(&asset).unwrap();
    to
}

/// The inner error of a failed diarizer load, as `SidecarDiarizer`
/// returns it, if `error` is one.
fn failed_load<'a>(
    error: &'a (dyn std::error::Error + Send + Sync + 'static),
) -> Option<&'a SidecarError> {
    match error.downcast_ref::<DiarizeError>() {
        Some(DiarizeError::Sidecar(SpeechError::Sidecar(SidecarError::DiarizerLoad(load)))) => {
            Some(load)
        }
        _ => None,
    }
}

/// The fake engine aborting or hanging in its load over a copy of the
/// real, intact models: the files are hashed and kept byte for byte, the
/// call keeps the load's error (so the job falls back), and the next call
/// diarizes over the same files.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the ONNX diarizer models (STENO_MODELS_DIR)"]
async fn a_crash_or_hang_in_the_load_keeps_the_real_models() {
    for fault in ["abort-on-diarizer-load", "hang-on-diarizer-load"] {
        let (engine, dir) = engine_with_fault(fault, |config| {
            config.load_timeout = Duration::from_secs(2);
        });
        copy_of_real_models(dir.path());
        let engine = Arc::new(engine);
        let diarizer = SidecarDiarizer::new(Arc::clone(&engine), Install::Never, 4);
        let error = diarizer.diarize(&tone(2.0)).await.unwrap_err();
        let load = failed_load(error.as_ref()).unwrap_or_else(|| panic!("{fault}: {error}"));
        if fault.starts_with("abort") {
            assert!(matches!(load, SidecarError::Crashed { .. }), "{load}");
        } else {
            assert!(matches!(load, SidecarError::Timeout { .. }), "{load}");
        }
        assert_eq!(engine.pid(), None);
        engine
            .store()
            .verify(&steno_diarize::models::asset())
            .unwrap();
        let result = diarizer.diarize(&tone(2.0)).await.unwrap();
        assert_eq!(result.clusters.len(), 1);
    }
}

/// One byte flipped in the copied embedding model, and a load that
/// aborts: only that file is deleted and named, and the segmentation model
/// keeps its checksum.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the ONNX diarizer models (STENO_MODELS_DIR)"]
async fn only_the_real_model_that_fails_its_checksum_is_deleted() {
    let (engine, dir) = engine_with_fault("abort-on-diarizer-load", |_| {});
    let folder = copy_of_real_models(dir.path());
    let embedding = folder.join(steno_diarize::models::EMBEDDING_FILE);
    let mut bytes = std::fs::read(&embedding).unwrap();
    bytes[1_000_000] ^= 0x01;
    std::fs::write(&embedding, bytes).unwrap();
    let diarizer = SidecarDiarizer::new(Arc::new(engine), Install::Never, 4);
    let error = diarizer.diarize(&tone(2.0)).await.unwrap_err();
    let Some(DiarizeError::NotInstalled { missing, .. }) = error.downcast_ref::<DiarizeError>()
    else {
        panic!("{error}");
    };
    assert_eq!(missing, &[steno_diarize::models::EMBEDDING_FILE.to_owned()]);
    assert!(!embedding.exists());
    let segmentation = &steno_diarize::models::asset().files[0];
    assert_eq!(
        steno_speech::model_store::sha256_of(&folder.join(&segmentation.name)).unwrap(),
        segmentation.sha256
    );
}

/// The real engine over a copy of the real models, failing its load the
/// way a starved machine does (a load timeout of 1 ms) and over memory
/// ceilings its load overruns: the child is gone and the files are kept
/// every time, and at least one ceiling ends the load itself.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the ONNX diarizer models (STENO_MODELS_DIR)"]
async fn the_real_engine_failing_its_load_keeps_the_real_models() {
    let real_engine = |dir: &Path, adjust: &dyn Fn(&mut SidecarConfig)| {
        let mut config = SidecarConfig::new(binary());
        config.heartbeat = Duration::from_millis(5);
        adjust(&mut config);
        Arc::new(SidecarSpeechEngine::with_assets(
            ModelStore::new(dir),
            config,
            Vec::new(),
        ))
    };
    let asset = steno_diarize::models::asset();

    let dir = tempfile::tempdir().unwrap();
    copy_of_real_models(dir.path());
    let engine = real_engine(dir.path(), &|config| {
        config.load_timeout = Duration::from_millis(1);
    });
    let diarizer = SidecarDiarizer::new(Arc::clone(&engine), Install::Never, 4);
    let error = diarizer.diarize(&tone(2.0)).await.unwrap_err();
    let load = failed_load(error.as_ref()).unwrap_or_else(|| panic!("{error}"));
    assert!(matches!(load, SidecarError::Timeout { .. }), "{load}");
    assert_eq!(engine.pid(), None);
    engine.store().verify(&asset).unwrap();

    let mut ended_in_the_load = 0;
    for mib in [16u64, 32, 64] {
        let dir = tempfile::tempdir().unwrap();
        copy_of_real_models(dir.path());
        let engine = real_engine(dir.path(), &|config| {
            config.memory_ceiling_bytes = mib << 20;
        });
        let diarizer = SidecarDiarizer::new(Arc::clone(&engine), Install::Never, 4);
        match diarizer.diarize(&tone(2.0)).await {
            Ok(result) => eprintln!("{mib} MiB: {} clusters", result.clusters.len()),
            Err(error) => {
                eprintln!("{mib} MiB: {error}");
                if matches!(
                    failed_load(error.as_ref()),
                    Some(SidecarError::MemoryCeiling { .. })
                ) {
                    ended_in_the_load += 1;
                }
            }
        }
        assert_eq!(engine.pid(), None, "{mib} MiB");
        engine.store().verify(&asset).unwrap();
    }
    assert!(ended_in_the_load > 0, "no ceiling ended the load");
}
