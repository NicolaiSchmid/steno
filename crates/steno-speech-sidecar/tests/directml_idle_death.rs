//! A child on `DirectML` that dies between requests is replaced on `DirectML`.
//! A regression would switch `DirectML` off for the whole process, so this runs
//! in a binary of its own. Windows only.

#![cfg(windows)]

mod common;

use common::{assert_works, config, engine_in, kill_idle_child, provider, tone};
use steno_core::SpeechEngine as _;
use steno_speech::EncoderProvider;
use steno_speech::sidecar::directml_switched_off;

#[tokio::test(flavor = "multi_thread")]
async fn a_child_that_dies_idle_on_directml_is_replaced_on_directml() {
    assert!(!directml_switched_off());
    let dir = tempfile::tempdir().unwrap();
    let mut config = config(&[]);
    config.options.directml = true;
    let engine = engine_in(&dir, config);
    engine.prepare().await.unwrap();
    assert_eq!(provider(&engine).await, Some(EncoderProvider::DirectMl));
    let pid = kill_idle_child(&engine);
    assert_works(&engine, &tone(0.5)).await;
    assert_ne!(engine.pid(), Some(pid));
    assert_eq!(engine.spawns(), 2);
    assert_eq!(provider(&engine).await, Some(EncoderProvider::DirectMl));
    assert!(!directml_switched_off());
}
