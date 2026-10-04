//! `SidecarSpeechEngine` against the real `steno-speech-sidecar` binary:
//! the protocol round trip, health and graceful shutdown, installing the
//! models, and every way the child can fail. During a request (killed,
//! aborting the way an uncaught C++ exception does, panicking, panicking
//! after a flood of stderr, exiting, failing to send its reply, hanging
//! past the deadline, allocating past the memory ceiling, writing garbage,
//! reporting an error) or at start (staying silent, speaking another
//! protocol version), each failure must come back as an error from the
//! engine. Between requests (killed, over the ceiling), the next call
//! replaces the child without an error. None may take the test process
//! down, and the engine must work on the next call. Dropping the engine
//! stops its child, inside a runtime or not. Driven by hand, without the
//! client, a child must greet first, exit when its parent's pipes close,
//! idle or busy, with status 2 on a request it cannot read, and on unix
//! answer a request that SIGINT, SIGTERM and SIGHUP reach mid-request.
//!
//! Nothing here ends a child with `DirectML` in use. During a load or a
//! request that switches `DirectML` off for the rest of the process's run,
//! so those tests run in binaries of their own (`directml_switch_off.rs`,
//! `directml_probe_crash.rs`, `directml_lost_encoder.rs`); so do the tests
//! of a child on `DirectML` that ends between requests
//! (`directml_idle_death.rs`, `directml_idle_overrun.rs`), so that a
//! regression there cannot switch it off here.
//!
//! The fake engine needs no models; the last test, ignored by default,
//! runs the real one and fails unless `STENO_MODELS_DIR` holds them
//! (`cargo test -p steno-speech-sidecar --release -- --ignored`).

#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::assert_is_empty
)]

mod common;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::process::{Child, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use common::{
    ASKED_FOR_DIRECTML, alive, assert_works, binary, config, engine_in, engine_with_fault, kill,
    kill_idle_child, provider, sidecar_error, tone, within_ten_seconds,
};
use steno_core::{AudioBuffer16k, SpeechEngine};
use steno_speech::sidecar::protocol::{self, PROTOCOL_VERSION, Reply, Request};
use steno_speech::sidecar::{FALLBACK_NOTICE, directml_switched_off};
use steno_speech::{
    EncoderProvider, ModelAsset, ModelFile, ModelSource, ModelStore, OnnxOptions, OnnxSpeechEngine,
    SidecarConfig, SidecarError, SidecarSpeechEngine,
};

/// Waits up to ten seconds for `pid` to be gone.
fn gone_soon(pid: u32) -> bool {
    within_ten_seconds(|| (!alive(pid)).then_some(())).is_some()
}

/// The pid the `--fault-once` marker in `dir` holds, once the faulting
/// child has written it.
fn faulted(dir: &tempfile::TempDir) -> Option<u32> {
    std::fs::read_to_string(dir.path().join("faulted"))
        .ok()?
        .parse()
        .ok()
}

/// [`faulted`] for a child that must have written its marker by now; the
/// test fails saying which child never did.
fn faulted_child(dir: &tempfile::TempDir, which: &str) -> u32 {
    faulted(dir).unwrap_or_else(|| {
        panic!(
            "the {which} child never wrote its marker {}: it did not get as far as its fault",
            dir.path().join("faulted").display()
        )
    })
}

/// A child with `args`, started by hand without the client.
fn spawn_by_hand(args: &[&str]) -> Child {
    Command::new(binary())
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}

/// Reads past the heartbeats to the child's next other message. A child
/// that sends none fails the test, not hangs it.
fn next_reply(stdout: &mut BufReader<ChildStdout>) -> Reply {
    for _ in 0..100 {
        match protocol::read_header::<_, Reply>(stdout).unwrap().unwrap() {
            Reply::Memory { .. } => {}
            reply => return reply,
        }
    }
    panic!("nothing but heartbeats");
}

/// Reads past the heartbeats to the child's ready message.
fn ready(stdout: &mut BufReader<ChildStdout>) {
    let reply = next_reply(stdout);
    assert!(matches!(reply, Reply::Ready { .. }), "{reply:?}");
}

/// All the child's stderr, once it has exited.
fn stderr_of(child: &mut Child) -> String {
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    stderr
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

/// The `DirectML` request crosses to the child with the load on Windows,
/// and the provider it answers with comes back in the health report; the
/// fake engine answers `DirectML` whenever it is asked, as if the probe
/// passed. Elsewhere the client asks for the CPU whatever the options say.
#[tokio::test(flavor = "multi_thread")]
async fn the_directml_request_reaches_the_child_on_windows_and_its_provider_comes_back() {
    for (directml, expected) in [(false, EncoderProvider::Cpu), (true, ASKED_FOR_DIRECTML)] {
        let dir = tempfile::tempdir().unwrap();
        let mut config = config(&[]);
        config.options.directml = directml;
        let engine = engine_in(&dir, config);
        engine.prepare().await.unwrap();
        assert_eq!(provider(&engine).await, Some(expected));
        engine.shut_down().await.unwrap();
    }
}

/// A child whose encoder leaves `DirectML` after it loaded says so in its
/// next answers, and the health report follows.
#[tokio::test(flavor = "multi_thread")]
async fn the_provider_follows_a_fallback_after_the_load() {
    let (engine, _dir) = engine_with_fault("fallback", |c| c.options.directml = true);
    engine.prepare().await.unwrap();
    assert_eq!(provider(&engine).await, Some(ASKED_FOR_DIRECTML));
    assert_works(&engine, &tone(0.5)).await;
    assert_eq!(provider(&engine).await, Some(EncoderProvider::Cpu));
    assert_eq!(engine.spawns(), 1);
}

/// The transcript carries the provider too: a child that fell back to
/// the CPU mid-job and then dies, before any health request, ended on the
/// CPU, so the next child is asked for `DirectML` again. The death is
/// found by a health request, which counts it as one during a request
/// (`prepare` and `transcribe` would replace the child on `DirectML`
/// whatever its provider). The crash report holds the child's line about
/// the fallback, written once though two answers followed it.
#[tokio::test(flavor = "multi_thread")]
async fn a_child_that_dies_after_falling_back_to_the_cpu_leaves_directml_on() {
    let (engine, _dir) = engine_with_fault("fallback", |c| c.options.directml = true);
    assert_works(&engine, &tone(0.5)).await;
    assert_works(&engine, &tone(0.5)).await;
    kill_idle_child(&engine);
    let error = engine.health().await.unwrap_err();
    match sidecar_error(error.as_ref()) {
        SidecarError::Crashed { stderr, .. } => {
            assert_eq!(stderr.matches(FALLBACK_NOTICE).count(), 1, "{stderr}");
        }
        other => panic!("{other}"),
    }
    engine.prepare().await.unwrap();
    assert_eq!(engine.spawns(), 2);
    assert_eq!(provider(&engine).await, Some(ASKED_FOR_DIRECTML));
    assert!(!directml_switched_off());
}

/// Driven by hand: the line about the fallback goes to stderr once,
/// though every answer after it reports the CPU.
#[test]
fn a_child_says_once_that_its_encoder_fell_back_to_the_cpu() {
    let mut child = spawn_by_hand(&["--fake-engine", "--fault", "fallback"]);
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    ready(&mut stdout);
    let samples = [0.5f32; 160];
    for id in 1..=2 {
        let request = Request::Transcribe {
            id,
            sample_count: samples.len() as u64,
            hint: None,
        };
        protocol::write_frame(&mut stdin, &request, &protocol::encode_samples(&samples)).unwrap();
        match next_reply(&mut stdout) {
            Reply::Transcript { provider, .. } => {
                assert_eq!(provider, Some(EncoderProvider::Cpu));
            }
            other => panic!("{other:?}"),
        }
    }
    drop(stdin);
    let status = exit_status(&mut child, "the child outlived its stdin");
    assert!(status.success(), "{status}");
    let stderr = stderr_of(&mut child);
    assert_eq!(stderr.matches(FALLBACK_NOTICE).count(), 1, "{stderr}");
}

/// An error the child reports and a release are no reason to give up on
/// `DirectML`: the next child is asked for it again. (A child on the CPU
/// that crashes does not count either; the client's unit tests show it.)
#[tokio::test(flavor = "multi_thread")]
async fn an_error_or_a_release_leaves_directml_on() {
    let (engine, _dir) = engine_with_fault("error", |c| c.options.directml = true);
    engine.prepare().await.unwrap();
    let error = engine.transcribe(&tone(0.1), None).await.unwrap_err();
    assert!(
        matches!(sidecar_error(error.as_ref()), SidecarError::Remote(_)),
        "{error}"
    );
    engine.release().await.unwrap();
    engine.prepare().await.unwrap();
    assert_eq!(engine.spawns(), 2);
    assert_eq!(provider(&engine).await, Some(ASKED_FOR_DIRECTML));
    assert!(!directml_switched_off());
}

/// A child that dies inside a load on the CPU is a crash, with no second
/// child within the call: only a load that asked for `DirectML` is the
/// probe's to retry. The next call starts a child that loads.
#[tokio::test(flavor = "multi_thread")]
async fn a_crash_inside_a_load_on_the_cpu_is_not_retried_within_the_call() {
    let (engine, _dir) = engine_with_fault("abort-on-load", |_| {});
    let error = engine.prepare().await.unwrap_err();
    assert!(
        matches!(sidecar_error(error.as_ref()), SidecarError::Crashed { .. }),
        "{error}"
    );
    assert_eq!(engine.spawns(), 1);
    assert_eq!(engine.pid(), None);
    assert_works(&engine, &tone(0.5)).await;
    assert_eq!(engine.spawns(), 2);
    assert!(!directml_switched_off());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_child_killed_mid_request_is_an_error_and_the_next_call_recovers() {
    let (engine, dir) = engine_with_fault("hang", |_| {});
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
    // The request is in flight once the child has read it and written its
    // marker (a blocking wait: the runtime has other workers).
    assert_eq!(within_ten_seconds(|| faulted(&dir)), Some(pid));
    assert!(!running.is_finished());
    assert!(kill(pid));
    let error = running.await.unwrap().unwrap_err();
    assert!(error.contains("died before answering"), "{error}");
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
    assert_recovers(&engine, |error| {
        assert!(
            matches!(error, SidecarError::Timeout { after } if *after == Duration::from_secs(2)),
            "{error}"
        );
    })
    .await;
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
    // first byte, a protocol violation, instead of waiting out the 30 s
    // deadline for the rest, which would be a timeout.
    let (engine, _dir) = engine_with_fault("garbage", |_| {});
    assert_recovers(&engine, |error| {
        assert!(
            matches!(error, SidecarError::Protocol(detail) if detail.contains("not a protocol message")),
            "{error}"
        );
    })
    .await;
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
    let refused = faulted_child(&dir, "refused");
    assert!(gone_soon(refused), "the refused child {refused} still runs");
    // The next child speaks the protocol.
    assert_works(&engine, &tone(0.5)).await;
    assert_eq!(engine.spawns(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_child_that_never_greets_is_killed_when_its_start_times_out() {
    // It reads nothing, so a closed stdin does not end it: only the kill
    // when the client drops the failed start does. The timeout leaves a
    // slow start on a busy runner time to write its marker.
    let (engine, dir) = engine_with_fault("silent", |c| {
        c.startup_timeout = Duration::from_secs(20);
    });
    let error = engine.prepare().await.unwrap_err();
    assert!(
        matches!(sidecar_error(error.as_ref()), SidecarError::Timeout { .. }),
        "{error}"
    );
    // A child that has not started within the 20 s start timeout is
    // killed before it writes its marker: then this says so.
    let silent = faulted_child(&dir, "silent");
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
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("faulted");
    let mut child = spawn_by_hand(&[
        "--fake-engine",
        "--fault",
        "hang",
        "--fault-once",
        marker.to_str().unwrap(),
        "--heartbeat-ms",
        "20",
    ]);
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
    // Busy once it has written its marker, inside the transcription.
    assert_eq!(within_ten_seconds(|| faulted(&dir)), Some(child.id()));
    assert!(child.try_wait().unwrap().is_none(), "the child is busy");
    drop(stdin);
    drop(stdout);
    let status = exit_status(&mut child, "the busy child outlived its parent's pipes");
    assert!(status.success(), "{status}");
}

#[cfg(unix)]
#[test]
fn the_signals_that_end_the_app_leave_a_request_in_the_child_answered() {
    use std::io::Write as _;

    // Driven by hand: the child has half of a request's audio, so it is
    // inside the request when the signals arrive, as Ctrl-C, a closed
    // terminal or systemd deliver them to the app's child too.
    let mut child = spawn_by_hand(&["--fake-engine", "--heartbeat-ms", "20"]);
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    ready(&mut stdout);
    let samples = [0.5f32; 160];
    let mut frame = Vec::new();
    protocol::write_frame(
        &mut frame,
        &Request::Transcribe {
            id: 1,
            sample_count: samples.len() as u64,
            hint: None,
        },
        &protocol::encode_samples(&samples),
    )
    .unwrap();
    let (first, rest) = frame.split_at(frame.len() - 100);
    stdin.write_all(first).unwrap();
    let pid = libc::pid_t::try_from(child.id()).unwrap();
    for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
        // SAFETY: `kill` only sends `signal` to the child's pid.
        let sent = unsafe { libc::kill(pid, signal) };
        assert_eq!(sent, 0, "signal {signal}");
    }
    std::thread::sleep(Duration::from_millis(200));
    assert!(
        child.try_wait().unwrap().is_none(),
        "a signal ended the child"
    );
    stdin.write_all(rest).unwrap();
    let segments = loop {
        match protocol::read_header::<_, Reply>(&mut stdout)
            .unwrap()
            .unwrap()
        {
            Reply::Transcript {
                id: 1, segments, ..
            } => break segments,
            Reply::Memory { .. } => {}
            other => panic!("{other:?}"),
        }
    };
    assert_eq!(segments[0].text, "160 samples, peak 0.5");
    drop(stdin);
    drop(stdout);
    let status = exit_status(&mut child, "the child outlived its stdin");
    assert!(status.success(), "{status}");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_real_engine_reports_missing_models_and_keeps_running() {
    // No `--fake-engine`: the child tries the store and says what is
    // missing; it downloads nothing (the client installs nothing either,
    // with no assets).
    let dir = tempfile::tempdir().unwrap();
    let mut config = SidecarConfig::new(binary());
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
    assert_eq!(health.provider, None);
    assert!(engine.shut_down().await.unwrap().unwrap().success());
}

#[test]
fn the_child_greets_and_exits_when_its_parent_goes_away() {
    // Driven by hand, without the client: a closed stdin is a dead parent.
    // The ready message comes first, even before a heartbeat 1 ms apart.
    let mut child = spawn_by_hand(&["--fake-engine", "--heartbeat-ms", "1"]);
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let first = protocol::read_header::<_, Reply>(&mut stdout).unwrap();
    assert_eq!(
        first,
        Some(Reply::Ready {
            protocol: PROTOCOL_VERSION,
            pid: child.id()
        })
    );
    drop(child.stdin.take());
    let status = exit_status(&mut child, "the child outlived its stdin");
    assert!(status.success(), "{status}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_child_that_died_while_idle_is_replaced_without_an_error() {
    // Killed between jobs (by the system for memory, say): the next call
    // starts a new child instead of failing on the dead one.
    let dir = tempfile::tempdir().unwrap();
    let engine = engine_in(&dir, config(&[]));
    engine.prepare().await.unwrap();
    let pid = kill_idle_child(&engine);
    assert_works(&engine, &tone(0.5)).await;
    assert_ne!(engine.pid(), Some(pid));
    assert_eq!(engine.spawns(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_child_over_the_ceiling_while_idle_is_replaced_without_an_error() {
    // It answers, then reports a resident set over the ceiling. The next
    // call starts a new child instead of handing the request to it.
    let (engine, _dir) = engine_with_fault("swell", |_| {});
    engine.prepare().await.unwrap();
    let pid = engine.pid().unwrap();
    assert_works(&engine, &tone(0.5)).await;
    // The report is queued while no request runs, so the next call finds
    // it before it sends one.
    assert!(
        within_ten_seconds(|| engine.fault_queued().then_some(())).is_some(),
        "the reader never queued the report over the ceiling"
    );
    assert_works(&engine, &tone(0.5)).await;
    assert_ne!(engine.pid(), Some(pid));
    assert_eq!(engine.spawns(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_reply_the_child_cannot_send_is_a_crash_that_says_why() {
    // A transcript over the frame limit: the child says so on stderr and
    // exits with status 2 rather than leave the parent waiting.
    let (engine, _dir) = engine_with_fault("oversize", |_| {});
    assert_recovers(&engine, |error| {
        let SidecarError::Crashed { status, stderr } = error else {
            panic!("{error}");
        };
        assert!(status.contains('2'), "{status}");
        assert!(stderr.contains("could not send a reply"), "{stderr}");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_slow_heartbeat_does_not_hold_back_the_answers() {
    // Every frame is flushed as it is written: with a heartbeat an hour
    // apart, nothing else would push the ready message and the replies
    // out of the child's stdout buffer before the deadlines (60 s to
    // start, 30 s for the health request).
    let dir = tempfile::tempdir().unwrap();
    let mut config = config(&[]);
    config.heartbeat = Duration::from_secs(3600);
    config.control_timeout = Duration::from_secs(30);
    let engine = engine_in(&dir, config);
    engine.prepare().await.unwrap();
    assert!(engine.health().await.unwrap().unwrap().loaded);
    assert_works(&engine, &tone(0.5)).await;
}

#[test]
fn a_child_that_cannot_read_a_request_says_so_and_exits_with_status_2() {
    // A header that is no frame, then audio cut short by a closed stdin.
    // stdout stays open, so a heartbeat cannot end the child first.
    type Write = fn(&mut std::process::ChildStdin);
    let cases: [(&str, Write); 2] = [
        ("unreadable request", |stdin| {
            stdin.write_all(&3u32.to_le_bytes()).unwrap();
            stdin.write_all(b"abc").unwrap();
        }),
        ("unreadable audio", |stdin| {
            let request = Request::Transcribe {
                id: 1,
                sample_count: 1000,
                hint: None,
            };
            let payload = protocol::encode_samples(&[0.25; 100]);
            protocol::write_frame(stdin, &request, &payload).unwrap();
        }),
    ];
    for (why, write) in cases {
        let mut child = spawn_by_hand(&["--fake-engine", "--heartbeat-ms", "1000"]);
        let mut stdout = BufReader::new(child.stdout.take().unwrap());
        ready(&mut stdout);
        let mut stdin = child.stdin.take().unwrap();
        write(&mut stdin);
        drop(stdin);
        let status = exit_status(&mut child, "the child outlived a request it could not read");
        let stderr = stderr_of(&mut child);
        assert_eq!(status.code(), Some(2), "{why}: {status}");
        assert!(stderr.contains(why), "{why}: {stderr}");
    }
    // With its stderr gone too: still status 2, not a panic's 101.
    let mut child = spawn_by_hand(&["--fake-engine", "--heartbeat-ms", "1000"]);
    drop(child.stderr.take());
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    ready(&mut stdout);
    let mut stdin = child.stdin.take().unwrap();
    cases[0].1(&mut stdin);
    drop(stdin);
    let status = exit_status(&mut child, "the child outlived a request it could not read");
    assert_eq!(status.code(), Some(2), "without stderr: {status}");
}

/// Serves `body` to every connection; the one numbered `slow` (from 0)
/// gets its headers at once, says so on the returned receiver, and gets
/// the body once the returned sender fires.
fn serve_model(body: Vec<u8>, slow: usize) -> (String, mpsc::Receiver<()>, mpsc::Sender<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/model.onnx", listener.local_addr().unwrap());
    let (release, gate) = mpsc::channel::<()>();
    let (asked, held) = mpsc::channel::<()>();
    std::thread::spawn(move || {
        let mut gate = Some(gate);
        for (k, stream) in listener.incoming().enumerate() {
            let Ok(stream) = stream else { continue };
            let body = body.clone();
            let wait = if k == slow {
                gate.take().map(|gate| (gate, asked.clone()))
            } else {
                None
            };
            std::thread::spawn(move || {
                let mut reader = BufReader::new(&stream);
                let mut line = String::new();
                while reader.read_line(&mut line).unwrap_or(0) > 0 && line != "\r\n" {
                    line.clear();
                }
                let mut out = &stream;
                let status = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = out.write_all(status.as_bytes());
                let _ = out.flush();
                if let Some((gate, asked)) = wait {
                    let _ = asked.send(());
                    let _ = gate.recv();
                }
                let _ = out.write_all(&body);
            });
        }
    });
    (url, held, release)
}

#[tokio::test(flavor = "multi_thread")]
async fn the_engine_installs_its_models_on_first_use_and_outside_its_lock() {
    let body = b"a model the fake engine never reads".to_vec();
    let dir = tempfile::tempdir().unwrap();
    let digest_of = dir.path().join("digest");
    std::fs::write(&digest_of, &body).unwrap();
    let (url, asked, release) = serve_model(body.clone(), 1);
    let asset = ModelAsset {
        id: "test-asset".to_owned(),
        display_name: "Test".to_owned(),
        licence: "MIT".to_owned(),
        attribution: String::new(),
        files: vec![ModelFile {
            name: "model.onnx".to_owned(),
            source: Some(ModelSource::Url(url)),
            sha256: steno_speech::model_store::sha256_of(&digest_of).unwrap(),
            size: body.len() as u64,
        }],
    };
    let store = ModelStore::new(dir.path().join("models"));
    let engine = Arc::new(SidecarSpeechEngine::with_assets(
        store.clone(),
        config(&[]),
        vec![asset.clone()],
    ));
    // A transcription without `prepare` installs them too.
    assert_works(&engine, &tone(0.2)).await;
    assert!(store.is_installed(&asset));
    // Gone while the child runs: `prepare` fetches the file again, and
    // the running child answers while that download is held.
    std::fs::remove_file(store.directory(&asset).join("model.onnx")).unwrap();
    let preparing = {
        let engine = Arc::clone(&engine);
        tokio::spawn(async move { engine.prepare().await.map_err(|e| e.to_string()) })
    };
    asked
        .recv_timeout(Duration::from_secs(30))
        .expect("prepare did not download");
    // The download cannot end before the release below, so a health
    // answer now did not wait for it.
    let health = engine.health().await.unwrap().unwrap();
    assert!(!preparing.is_finished());
    assert!(health.loaded);
    release.send(()).unwrap();
    preparing.await.unwrap().unwrap();
    assert!(store.is_installed(&asset));
    assert_eq!(engine.spawns(), 1);
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the models: STENO_MODELS_DIR, and STENO_FLEURS_DIR for the parity clip; run with -- --ignored"]
async fn the_real_models_load_and_transcribe_in_the_sidecar_when_installed() {
    // Run on purpose, so missing models fail it rather than pass it.
    let models_directory = ModelStore::environment_models_directory()
        .expect("set STENO_MODELS_DIR to the models directory to run the real sidecar");
    let store = ModelStore::in_models_directory(&models_directory);
    for asset in [ModelAsset::silero_vad(), ModelAsset::parakeet_v3_fp32()] {
        assert!(
            store.is_installed(&asset),
            "{} is not installed in {}: missing {:?}",
            asset.id,
            store.root().display(),
            store.missing_files(&asset)
        );
    }
    let mut config = SidecarConfig::new(binary());
    config.memory_ceiling_bytes = 8 << 30;
    let engine = SidecarSpeechEngine::new(store.clone(), config);
    engine.prepare().await.unwrap();
    let health = engine.health().await.unwrap().unwrap();
    assert!(health.loaded);
    assert_eq!(health.provider, Some(EncoderProvider::Cpu));
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
        .map(|dir| std::path::PathBuf::from(dir).join("cat/fleurs-de-cat-01.wav"));
    if let Some(clip) = clip {
        assert!(clip.is_file(), "STENO_FLEURS_DIR has no {}", clip.display());
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
