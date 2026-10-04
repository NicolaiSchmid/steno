//! A child that aborts inside a load that asks for `DirectML`, where the
//! probe runs, costs no job: no audio was sent yet, so the same call loads
//! again in a new child on the CPU. That switches `DirectML` off for the
//! rest of the process's run, so this test runs in a binary of its own.
//! Windows only: the client asks for `DirectML` nowhere else.

#![cfg(windows)]

mod common;

use common::{assert_works, config, engine_in, provider, tone};
use steno_speech::EncoderProvider;
use steno_speech::sidecar::directml_switched_off;

#[tokio::test(flavor = "multi_thread")]
async fn a_crash_in_the_probe_loads_again_on_the_cpu_within_the_same_call() {
    assert!(!directml_switched_off());
    let dir = tempfile::tempdir().unwrap();
    let mut config = config(&["--fault", "abort-on-directml-load"]);
    config.options.directml = true;
    let engine = engine_in(&dir, config);
    // The first transcription loads lazily, as the pipeline's does.
    assert_works(&engine, &tone(0.5)).await;
    assert_eq!(engine.spawns(), 2);
    assert_eq!(provider(&engine).await, Some(EncoderProvider::Cpu));
    assert!(directml_switched_off());
}
