//! `SidecarSpeechEngine` against the real `steno-speech-sidecar` binary:
//! the protocol round trip, health and graceful shutdown, and every way
//! the child can fail (killed mid-request, aborting the way an uncaught
//! C++ exception does, panicking, panicking after a flood of stderr,
//! exiting, hanging past the deadline,
//! allocating past the memory ceiling, writing garbage, reporting an
//! error, staying silent at start, speaking another protocol version).
//! Each failure must come back as an error from the engine, never take the
//! test process down, and leave an engine that works on the next call.
//! Dropping the engine stops its child, inside a runtime or not. Driven by
//! hand, without the client, a child must exit when its parent's pipes
//! close, idle or busy.
//!
//! The fake engine needs no models; the last test, ignored by default,
//! runs the real one when `STENO_MODELS_DIR` holds them
//! (`cargo test -p steno-speech-sidecar --release -- --ignored`).

#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::assert_is_empty
)]

use std::ffi::OsString;
use std::io::BufReader;
use std::process::{Child, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use steno_core::{AudioBuffer16k, LanguageTag, SpeechEngine};
use steno_speech::sidecar::protocol::{self, PROTOCOL_VERSION, Reply, Request};
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
    config.memory_ceiling_bytes = 1 << 30;
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

/// Whether a process with `pid` exists (a zombie counts).
fn alive(pid: u32) -> bool {
    if cfg!(windows) {
        let out = Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).contains(&format!("\"{pid}\""))
    } else {
        Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success()
    }
}

/// Polls `probe` every 20 ms for up to ten seconds.
fn within_ten_seconds<T>(mut probe: impl FnMut() -> Option<T>) -> Option<T> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(value) = probe() {
            return Some(value);
        }
        if Instant::now() > deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Waits up to ten seconds for `pid` to be gone.
fn gone_soon(pid: u32) -> bool {
    within_ten_seconds(|| (!alive(pid)).then_some(())).is_some()
}

/// A child with `args`, started by hand without the client.
fn spawn_by_hand(args: &[&str]) -> Child {
    Command::new(BINARY)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap()
}

/// Reads past the heartbeats to the child's ready message: its protocol
/// and pid. A child that never sends one fails the test, not hangs it.
fn ready(stdout: &mut BufReader<ChildStdout>) -> (u32, u32) {
    for _ in 0..100 {
        match protocol::read_header::<_, Reply>(stdout).unwrap().unwrap() {
            Reply::Ready { protocol, pid } => return (protocol, pid),
            Reply::Memory { .. } => {}
            other => panic!("{other:?}"),
        }
    }
    panic!("no ready message");
}

/// Waits up to ten seconds for `child` to exit; kills it and panics with
/// `outlived` if it does not.
fn exit_status(child: &mut Child, outlived: &str) -> ExitStatus {
    within_ten_seconds(|| child.try_wait().unwrap()).unwrap_or_else(|| {
        let _ = child.kill();
        let _ = child.wait();
        panic!("{outlived}")
    })
}

/// Kills `pid`, for a test that failed with a child left running.
fn kill(pid: u32) -> bool {
    let status = if cfg!(windows) {
        Command::new("taskkill")
            .args(["/F", "/PID", &pid.to_string()])
            .stdout(Stdio::null())
            .status()
    } else {
        Command::new("kill")
            .args(["-KILL", &pid.to_string()])
            .status()
    };
    status.unwrap().success()
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
    assert!(kill(pid));
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
        assert!(stderr.contains("native noise"), "{stderr}");
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
async fn a_flood_of_stderr_leaves_a_crash_report_of_bounded_size() {
    // 1 MiB without a newline, then the panic: the report keeps the panic
    // and at most 20 pieces of 4 KiB.
    let (engine, _dir) = engine_with_fault("flood", |_| {});
    assert_recovers(&engine, |error| {
        let SidecarError::Crashed { stderr, .. } = error else {
            panic!("{error}");
        };
        assert!(stderr.contains("after a flood of stderr"), "{stderr}");
        assert!(stderr.len() <= 20 * 4097, "{} bytes", stderr.len());
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_child_that_hangs_is_killed_at_the_deadline() {
    // The floor also bounds the recovery call, a fresh child's first
    // transcription, so it leaves a busy runner room.
    let (engine, _dir) = engine_with_fault("hang", |c| {
        c.transcribe_timeout_floor = Duration::from_secs(2);
    });
    let started = Instant::now();
    assert_recovers(&engine, |error| {
        assert!(
            matches!(error, SidecarError::Timeout { after } if *after == Duration::from_secs(2)),
            "{error}"
        );
    })
    .await;
    assert!(started.elapsed() < Duration::from_secs(20));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_child_over_the_memory_ceiling_is_killed() {
    const CEILING: u64 = 256 << 20;
    let (engine, _dir) = engine_with_fault("allocate", |c| c.memory_ceiling_bytes = CEILING);
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
async fn a_child_of_another_protocol_version_is_refused() {
    let (engine, dir) = engine_with_fault("wrong-protocol", |_| {});
    let error = engine.prepare().await.unwrap_err();
    assert!(
        matches!(sidecar_error(error.as_ref()), SidecarError::Protocol(detail)
            if detail.contains("speaks protocol 0")),
        "{error}"
    );
    assert_eq!(engine.pid(), None);
    let refused: u32 = std::fs::read_to_string(dir.path().join("faulted"))
        .unwrap()
        .parse()
        .unwrap();
    assert!(gone_soon(refused), "the refused child {refused} still runs");
    // The next child speaks the protocol.
    assert_works(&engine, &tone(0.5)).await;
    assert_eq!(engine.spawns(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_child_that_never_greets_is_killed_when_its_start_times_out() {
    // It reads nothing, so a closed stdin does not end it: only the kill
    // when the client drops the failed start does. The timeout leaves a
    // slow first start (a busy macOS runner) time to write its marker.
    let (engine, dir) = engine_with_fault("silent", |c| {
        c.startup_timeout = Duration::from_secs(5);
    });
    let error = engine.prepare().await.unwrap_err();
    assert!(
        matches!(sidecar_error(error.as_ref()), SidecarError::Timeout { .. }),
        "{error}"
    );
    let silent: u32 = std::fs::read_to_string(dir.path().join("faulted"))
        .expect("the silent child started within the timeout")
        .parse()
        .unwrap();
    if !gone_soon(silent) {
        kill(silent);
        panic!("the silent child {silent} outlived its failed start");
    }
    assert_works(&engine, &tone(0.5)).await;
}

#[test]
fn dropping_the_engine_stops_its_child_inside_a_runtime_or_not() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let started = |engine: &SidecarSpeechEngine| {
        runtime.block_on(engine.prepare()).unwrap();
        engine.pid().unwrap()
    };
    // Inside a runtime the stop runs on a blocking thread, after the drop.
    let engine = engine_in(&dir, config(&[]));
    let pid = started(&engine);
    runtime.block_on(async move { drop(engine) });
    assert!(gone_soon(pid), "the child {pid} outlived its engine");
    // Outside one the drop stops and reaps the child before it returns.
    let engine = engine_in(&dir, config(&[]));
    let pid = started(&engine);
    drop(engine);
    assert!(!alive(pid), "the child {pid} outlived its engine");
}

#[test]
fn a_busy_child_exits_when_its_parent_goes_away() {
    // Driven by hand: the child hangs inside a transcription, so it never
    // reads the closed stdin; its next heartbeat finds stdout gone.
    let mut child = spawn_by_hand(&["--fake-engine", "--fault", "hang", "--heartbeat-ms", "20"]);
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    ready(&mut stdout);
    let samples = [0.5f32; 160];
    protocol::write_frame(
        &mut stdin,
        &Request::Transcribe {
            id: 1,
            sample_count: samples.len() as u64,
            hint: None,
        },
        &protocol::encode_samples(&samples),
    )
    .unwrap();
    std::thread::sleep(Duration::from_millis(200));
    assert!(child.try_wait().unwrap().is_none(), "the child is busy");
    drop(stdin);
    drop(stdout);
    let status = exit_status(&mut child, "the busy child outlived its parent's pipes");
    assert!(status.success(), "{status}");
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
    let mut child = spawn_by_hand(&["--fake-engine", "--heartbeat-ms", "1000"]);
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    assert_eq!(ready(&mut stdout), (PROTOCOL_VERSION, child.id()));
    drop(child.stdin.take());
    let status = exit_status(&mut child, "the child outlived its stdin");
    assert!(status.success(), "{status}");
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the models: STENO_MODELS_DIR, and STENO_FLEURS_DIR for the parity clip; run with -- --ignored"]
async fn the_real_models_load_and_transcribe_in_the_sidecar_when_installed() {
    let Some(models_directory) = ModelStore::environment_models_directory() else {
        eprintln!("skipped: set STENO_MODELS_DIR to run the real sidecar");
        return;
    };
    let store = ModelStore::in_models_directory(&models_directory);
    if ![ModelAsset::silero_vad(), ModelAsset::parakeet_v3_fp32()]
        .iter()
        .all(|asset| store.is_installed(asset))
    {
        eprintln!("skipped: the models are not installed under STENO_MODELS_DIR");
        return;
    }
    let mut config = SidecarConfig::new(BINARY);
    config.memory_ceiling_bytes = 8 << 30;
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
