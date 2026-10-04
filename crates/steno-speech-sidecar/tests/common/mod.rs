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

/// The sidecar error inside a boundary error.
pub fn sidecar_error<'a>(
    error: &'a (dyn std::error::Error + Send + Sync + 'static),
) -> &'a SidecarError {
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
