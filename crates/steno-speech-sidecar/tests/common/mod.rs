//! What the test binaries that drive `SidecarSpeechEngine` against the
//! fake engine share. Each binary uses some of it.

#![allow(dead_code)]

use std::ffi::OsString;
use std::process::{Command, Stdio};
use std::sync::Once;
use std::time::{Duration, Instant};

use steno_core::{AudioBuffer16k, LanguageTag, SpeechEngine as _};
use steno_speech::{
    EncoderProvider, ModelStore, SidecarConfig, SidecarError, SidecarSpeechEngine, SpeechError,
};

/// Built by cargo for these tests, from this package's `[[bin]]`.
pub const BINARY: &str = env!("CARGO_BIN_EXE_steno-speech-sidecar");

/// [`BINARY`], after it has once been run to the end by hand, so the start
/// timeouts measure the child rather than a first start, which a loaded
/// runner can take seconds over (macOS checks a new executable then). A
/// first start that has not ended after two minutes fails the test rather
/// than hang it.
pub fn binary() -> &'static str {
    static WARM: Once = Once::new();
    WARM.call_once(|| {
        let mut warm = Command::new(BINARY)
            .arg("--fake-engine")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(120);
        let status = loop {
            if let Some(status) = warm.try_wait().unwrap() {
                break status;
            }
            if Instant::now() > deadline {
                let _ = warm.kill();
                let _ = warm.wait();
                panic!("the sidecar's first start had not ended after two minutes");
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        assert!(status.success(), "{status}");
    });
    BINARY
}

/// The provider the fake engine reports to an engine whose options ask for
/// `DirectML`: `DirectML` on Windows; elsewhere the client asks for the
/// CPU.
pub const ASKED_FOR_DIRECTML: EncoderProvider = if cfg!(windows) {
    EncoderProvider::DirectMl
} else {
    EncoderProvider::Cpu
};

/// A fake-engine sidecar with `args` and short limits. The start timeout
/// stays generous: only the silent child's test is about it.
pub fn config(args: &[&str]) -> SidecarConfig {
    let mut config = SidecarConfig::new(binary());
    config.args = std::iter::once("--fake-engine")
        .chain(args.iter().copied())
        .map(OsString::from)
        .collect();
    config.heartbeat = Duration::from_millis(20);
    config.transcribe_timeout_floor = Duration::from_secs(30);
    config.transcribe_timeout_ratio = 0.0;
    config.memory_ceiling_bytes = 1 << 30;
    config.startup_timeout = Duration::from_secs(60);
    config
}

/// An engine over an empty store in `dir` that installs nothing.
pub fn engine_in(dir: &tempfile::TempDir, config: SidecarConfig) -> SidecarSpeechEngine {
    SidecarSpeechEngine::with_assets(ModelStore::new(dir.path()), config, Vec::new())
}

/// An engine with `fault` committed by the first child only, so the next
/// one is healthy.
pub fn engine_with_fault(
    fault: &str,
    adjust: impl FnOnce(&mut SidecarConfig),
) -> (SidecarSpeechEngine, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("faulted");
    let mut config = config(&["--fault", fault, "--fault-once", marker.to_str().unwrap()]);
    adjust(&mut config);
    (engine_in(&dir, config), dir)
}

#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]
pub fn tone(seconds: f64) -> AudioBuffer16k {
    let n = (seconds * 16_000.0) as usize;
    AudioBuffer16k::new((0..n).map(|i| (i as f32 * 0.05).sin() * 0.5).collect())
}

/// The sidecar error inside a boundary error: as the cause of a
/// [`SpeechError`] (`prepare`, `transcribe`), or itself (`health`).
pub fn sidecar_error<'a>(
    error: &'a (dyn std::error::Error + Send + Sync + 'static),
) -> &'a SidecarError {
    if let Some(inner) = error.downcast_ref::<SidecarError>() {
        return inner;
    }
    match error.downcast_ref::<SpeechError>() {
        Some(SpeechError::Sidecar(inner)) => inner,
        _ => panic!("not a sidecar error: {error}"),
    }
}

/// Where the running child's encoder runs, by the health report.
pub async fn provider(engine: &SidecarSpeechEngine) -> Option<EncoderProvider> {
    engine.health().await.unwrap().unwrap().provider
}

/// Transcribes `audio` and checks the fake engine's answer.
pub async fn assert_works(engine: &SidecarSpeechEngine, audio: &AudioBuffer16k) {
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

/// Whether a process with `pid` exists (a zombie counts).
pub fn alive(pid: u32) -> bool {
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

/// Kills `pid`: a child the test ends, or one a failed test left running.
pub fn kill(pid: u32) -> bool {
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

/// Polls `probe` every 20 ms for up to ten seconds.
pub fn within_ten_seconds<T>(mut probe: impl FnMut() -> Option<T>) -> Option<T> {
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

/// Waits up to ten seconds for the engine's reader to queue a fault of
/// the running child ([`SidecarSpeechEngine::fault_queued`]).
pub fn fault_queued_soon(engine: &SidecarSpeechEngine) -> bool {
    within_ten_seconds(|| engine.fault_queued().then_some(())).is_some()
}

/// Kills the engine's idle child and waits until its reader has queued
/// the end of its stdout, so the next call finds the child dead rather
/// than send it a request. Not until the pid is a zombie: the main thread
/// of a child with several shows as one before the others have exited
/// and closed stdout. Returns the child's pid.
pub fn kill_idle_child(engine: &SidecarSpeechEngine) -> u32 {
    let pid = engine.pid().unwrap();
    assert!(kill(pid));
    assert!(
        fault_queued_soon(engine),
        "the reader never queued the end of the killed child's stdout"
    );
    pid
}
