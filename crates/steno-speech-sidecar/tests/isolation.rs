//! `SidecarSpeechEngine` against the real `steno-speech-sidecar` binary:
//! the protocol round trip, health and graceful shutdown, and every way
//! the child can fail (killed mid-request, aborting as a C++ exception
//! does, panicking, exiting, hanging past the deadline, allocating past
//! the memory ceiling, writing garbage, reporting an error). Each failure
//! must come back as an error from the engine, never take the test
//! process down, and leave an engine that works on the next call.
//!
//! The fake engine needs no models; the last test runs the real one when
//! `STENO_MODELS_DIR` holds them.

#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::assert_is_empty
)]

use std::ffi::OsString;
use std::io::BufReader;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use steno_core::{AudioBuffer16k, LanguageTag, SpeechEngine};
use steno_speech::sidecar::protocol::{self, PROTOCOL_VERSION, Reply};
use steno_speech::{
    ModelAsset, ModelStore, OnnxOptions, OnnxSpeechEngine, SidecarConfig, SidecarError,
    SidecarSpeechEngine, SpeechError,
};

const BINARY: &str = env!("CARGO_BIN_EXE_steno-speech-sidecar");

/// A fake-engine sidecar with `args` and short limits.
fn config(args: &[&str]) -> SidecarConfig {
    let mut config = SidecarConfig::new(BINARY);
    config.args = std::iter::once("--fake-engine")
        .chain(args.iter().copied())
        .map(OsString::from)
        .collect();
    config.heartbeat = Duration::from_millis(20);
    config.transcribe_timeout_floor = Duration::from_secs(30);
    config.transcribe_timeout_ratio = 0.0;
    config.memory_ceiling = 1 << 30;
    config
}

/// An engine over an empty store in `dir` that installs nothing.
fn engine_in(dir: &tempfile::TempDir, config: SidecarConfig) -> SidecarSpeechEngine {
    SidecarSpeechEngine::with_assets(ModelStore::new(dir.path()), config, Vec::new())
}

/// An engine with `fault` committed by the first child only, so the next
/// one is healthy.
fn engine_with_fault(
    fault: &str,
    adjust: impl FnOnce(&mut SidecarConfig),
) -> (SidecarSpeechEngine, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("faulted");
    let mut config = config(&["--fault", fault, "--fault-once", marker.to_str().unwrap()]);
    adjust(&mut config);
    (engine_in(&dir, config), dir)
}

fn tone(seconds: f64) -> AudioBuffer16k {
    let n = (seconds * 16_000.0) as usize;
    AudioBuffer16k::new((0..n).map(|i| (i as f32 * 0.05).sin() * 0.5).collect())
}

/// The sidecar error inside a boundary error.
fn sidecar_error<'a>(
    error: &'a (dyn std::error::Error + Send + Sync + 'static),
) -> &'a SidecarError {
    match error.downcast_ref::<SpeechError>() {
        Some(SpeechError::Sidecar(inner)) => inner,
        _ => panic!("not a sidecar error: {error}"),
    }
}

/// Transcribes `audio` and checks the fake engine's answer.
async fn assert_works(engine: &SidecarSpeechEngine, audio: &AudioBuffer16k) {
    let segments = engine
        .transcribe(audio, Some(&LanguageTag::from("de")))
        .await
        .unwrap();
    assert_eq!(segments.len(), 1);
    let peak = audio.samples.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    assert_eq!(
        segments[0].text,
        format!("{} samples, peak {peak}", audio.samples.len())
    );
    assert_eq!(segments[0].language, Some(LanguageTag::from("de")));
    assert_eq!(segments[0].end, audio.duration());
}

/// The fault fires on the first transcription; it must fail with an error
/// `check` accepts, and the next call must work in a new child.
async fn assert_recovers(engine: &SidecarSpeechEngine, check: impl FnOnce(&SidecarError)) {
    engine.prepare().await.unwrap();
    let first = engine.pid().unwrap();
    let error = engine.transcribe(&tone(0.5), None).await.unwrap_err();
    check(sidecar_error(error.as_ref()));
    assert_eq!(engine.pid(), None, "the failed child is gone");
    assert_works(engine, &tone(0.5)).await;
    assert_ne!(engine.pid(), Some(first));
    assert_eq!(engine.spawns(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn requests_round_trip_the_audio_bit_for_bit_in_one_child() {
    let dir = tempfile::tempdir().unwrap();
    let engine = engine_in(&dir, config(&[]));
    assert_eq!(engine.health().await.unwrap(), None);
    engine.prepare().await.unwrap();
    engine.prepare().await.unwrap();
    let pid = engine.pid().unwrap();
    let health = engine.health().await.unwrap().unwrap();
    assert_eq!(health.pid, pid);
    assert!(health.loaded);
    assert!(
        health.rss_bytes > 0 || cfg!(not(any(target_os = "linux", target_os = "macos", windows)))
    );
    // An exact peak proves the samples crossed as `f32` bits.
    let mut odd = tone(2.0);
    odd.samples[7] = 0.123_456_79;
    assert_works(&engine, &odd).await;
    assert_works(&engine, &tone(0.01)).await;
    let segments = engine.transcribe(&tone(1.0), None).await.unwrap();
    assert_eq!(segments[0].language, None);
    assert_eq!(engine.pid(), Some(pid));
    assert_eq!(engine.spawns(), 1);
    let status = engine.shut_down().await.unwrap().unwrap();
    assert!(status.success(), "{status}");
    assert_eq!(engine.pid(), None);
    // A released engine starts again on demand.
    assert_works(&engine, &tone(0.2)).await;
    assert_eq!(engine.spawns(), 2);
    // A pipeline holding the trait object frees the child the same way.
    let pipeline: &dyn SpeechEngine = &engine;
    pipeline.release().await.unwrap();
    assert_eq!(engine.pid(), None);
    assert_eq!(engine.health().await.unwrap(), None);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_child_killed_mid_request_is_an_error_and_the_next_call_recovers() {
    let (engine, _dir) = engine_with_fault("hang", |_| {});
    let engine = Arc::new(engine);
    engine.prepare().await.unwrap();
    let pid = engine.pid().unwrap();
    let running = {
        let engine = Arc::clone(&engine);
        tokio::spawn(async move {
            engine
                .transcribe(&tone(1.0), None)
                .await
                .map_err(|e| e.to_string())
        })
    };
    // The request is in flight once the child has read it; give it a
    // moment (a blocking sleep: the runtime has other workers).
    std::thread::sleep(Duration::from_millis(300));
    assert!(!running.is_finished());
    let killed = if cfg!(windows) {
        Command::new("taskkill")
            .args(["/F", "/PID", &pid.to_string()])
            .stdout(Stdio::null())
            .status()
    } else {
        Command::new("kill")
            .args(["-KILL", &pid.to_string()])
            .status()
    };
    assert!(killed.unwrap().success());
    let error = running.await.unwrap().unwrap_err();
    assert!(error.contains("died mid-request"), "{error}");
    assert_eq!(engine.pid(), None);
    assert_works(&engine, &tone(0.5)).await;
    assert_ne!(engine.pid(), Some(pid));
}

#[tokio::test(flavor = "multi_thread")]
async fn an_abort_as_a_cpp_exception_ends_it_takes_only_the_child_down() {
    let (engine, _dir) = engine_with_fault("abort", |_| {});
    assert_recovers(&engine, |error| {
        let SidecarError::Crashed { status, .. } = error else {
            panic!("{error}");
        };
        if cfg!(unix) {
            assert!(
                status.contains("SIGABRT") || status.contains("signal: 6"),
                "{status}"
            );
        }
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_panic_or_an_exit_is_a_crash_with_the_child_s_last_words() {
    let (engine, _dir) = engine_with_fault("panic", |_| {});
    assert_recovers(&engine, |error| {
        let SidecarError::Crashed { stderr, .. } = error else {
            panic!("{error}");
        };
        assert!(stderr.contains("simulated panic"), "{stderr}");
    })
    .await;
    let (engine, _dir) = engine_with_fault("exit", |_| {});
    assert_recovers(&engine, |error| {
        let SidecarError::Crashed { status, .. } = error else {
            panic!("{error}");
        };
        assert!(status.contains('3'), "{status}");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_child_that_hangs_is_killed_at_the_deadline() {
    let (engine, _dir) = engine_with_fault("hang", |c| {
        c.transcribe_timeout_floor = Duration::from_millis(800);
    });
    let started = Instant::now();
    assert_recovers(&engine, |error| {
        assert!(
            matches!(error, SidecarError::Timeout { after } if *after == Duration::from_millis(800)),
            "{error}"
        );
    })
    .await;
    assert!(started.elapsed() < Duration::from_secs(20));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_child_over_the_memory_ceiling_is_killed() {
    const CEILING: u64 = 256 << 20;
    let (engine, _dir) = engine_with_fault("allocate", |c| c.memory_ceiling = CEILING);
    assert_recovers(&engine, |error| {
        let SidecarError::MemoryCeiling {
            rss_bytes,
            ceiling_bytes,
        } = error
        else {
            panic!("{error}");
        };
        assert_eq!(*ceiling_bytes, CEILING);
        assert!(*rss_bytes > CEILING);
        // Well under what the child would have taken (4 GiB) unchecked.
        assert!(*rss_bytes < 2 << 30, "{rss_bytes}");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn garbage_on_stdout_is_a_protocol_violation() {
    // The garbage claims a 16 MiB header; the parent refuses it at the
    // first byte instead of waiting out the 30 s deadline for the rest.
    let (engine, _dir) = engine_with_fault("garbage", |_| {});
    let started = Instant::now();
    assert_recovers(&engine, |error| {
        assert!(
            matches!(error, SidecarError::Protocol(detail) if detail.contains("not a protocol message")),
            "{error}"
        );
    })
    .await;
    assert!(started.elapsed() < Duration::from_secs(20));
}

#[tokio::test(flavor = "multi_thread")]
async fn an_error_the_child_reports_keeps_the_child() {
    let dir = tempfile::tempdir().unwrap();
    let engine = engine_in(&dir, config(&["--fault", "error"]));
    engine.prepare().await.unwrap();
    let pid = engine.pid();
    for _ in 0..2 {
        let error = engine.transcribe(&tone(0.1), None).await.unwrap_err();
        assert!(
            matches!(sidecar_error(error.as_ref()), SidecarError::Remote(detail) if detail.contains("simulated failure")),
            "{error}"
        );
        assert_eq!(engine.pid(), pid);
    }
    assert_eq!(engine.spawns(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_real_engine_reports_missing_models_and_keeps_running() {
    // No `--fake-engine`: the child tries the store and says what is
    // missing; it downloads nothing (the client installs nothing either,
    // with no assets).
    let dir = tempfile::tempdir().unwrap();
    let mut config = SidecarConfig::new(BINARY);
    config.heartbeat = Duration::from_millis(20);
    let engine = engine_in(&dir, config);
    let error = engine.prepare().await.unwrap_err();
    let detail = error.to_string();
    assert!(
        matches!(sidecar_error(error.as_ref()), SidecarError::Remote(_)),
        "{detail}"
    );
    assert!(
        detail.contains("silero-vad") && detail.contains("not installed"),
        "{detail}"
    );
    let health = engine.health().await.unwrap().unwrap();
    assert!(!health.loaded);
    assert!(engine.shut_down().await.unwrap().unwrap().success());
}

#[test]
fn the_child_greets_and_exits_when_its_parent_goes_away() {
    // Driven by hand, without the client: a closed stdin is a dead parent.
    let mut child = Command::new(BINARY)
        .args(["--fake-engine", "--heartbeat-ms", "1000"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut greeted = false;
    for _ in 0..3 {
        match protocol::read_header::<_, Reply>(&mut stdout)
            .unwrap()
            .unwrap()
        {
            Reply::Ready { protocol, pid } => {
                assert_eq!(protocol, PROTOCOL_VERSION);
                assert_eq!(pid, child.id());
                greeted = true;
                break;
            }
            Reply::Memory { .. } => {}
            other => panic!("{other:?}"),
        }
    }
    assert!(greeted);
    drop(child.stdin.take());
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        assert!(Instant::now() < deadline, "the child outlived its stdin");
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(status.success(), "{status}");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_real_models_load_and_transcribe_in_the_sidecar_when_installed() {
    let Some(root) = ModelStore::environment_root() else {
        eprintln!("skipped: set STENO_MODELS_DIR to run the real sidecar");
        return;
    };
    let store = ModelStore::new(root);
    if ![ModelAsset::silero_vad(), ModelAsset::parakeet_v3_fp32()]
        .iter()
        .all(|asset| store.is_installed(asset))
    {
        eprintln!("skipped: the models are not installed under STENO_MODELS_DIR");
        return;
    }
    let mut config = SidecarConfig::new(BINARY);
    config.memory_ceiling = 8 << 30;
    let engine = SidecarSpeechEngine::new(store.clone(), config);
    engine.prepare().await.unwrap();
    let health = engine.health().await.unwrap().unwrap();
    assert!(health.loaded);
    eprintln!(
        "sidecar with models loaded: {} MB resident",
        health.rss_bytes >> 20
    );
    assert!(
        engine
            .transcribe(&AudioBuffer16k::silence(2.0), None)
            .await
            .unwrap()
            .is_empty()
    );
    // With FLEURS at hand: the sidecar's transcript of a clip is the
    // in-process engine's, segment for segment.
    let clip = std::env::var_os("STENO_FLEURS_DIR")
        .map(|dir| std::path::PathBuf::from(dir).join("cat/fleurs-de-cat-01.wav"))
        .filter(|path| path.is_file());
    if let Some(clip) = clip {
        let audio = AudioBuffer16k::new(steno_speech::wav::read_pcm16(&clip).unwrap());
        let started = Instant::now();
        let sidecar = engine.transcribe(&audio, None).await.unwrap();
        let elapsed = started.elapsed();
        let in_process = OnnxSpeechEngine::new(store, OnnxOptions::default());
        let expected = in_process.transcribe(&audio, None).await.unwrap();
        assert!(!sidecar.is_empty());
        assert_eq!(sidecar, expected);
        eprintln!(
            "{:.0} s of FLEURS in {:.1} s through the sidecar, {} segments identical to in-process",
            audio.duration(),
            elapsed.as_secs_f64(),
            sidecar.len()
        );
    }
    assert!(engine.shut_down().await.unwrap().unwrap().success());
}
