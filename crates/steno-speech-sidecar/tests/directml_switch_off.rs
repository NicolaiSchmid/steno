//! A child that dies during a request with `DirectML` in use switches
//! `DirectML` off for every engine for the rest of the process. The switch-off
//! is process-wide, so this runs in a binary of its own. Windows only.

#![cfg(windows)]

mod common;

use common::{assert_works, config, engine_in, engine_with_fault, provider, sidecar_error, tone};
use steno_core::SpeechEngine as _;
use steno_speech::sidecar::directml_switched_off;
use steno_speech::{EncoderProvider, SidecarError};

/// A child that aborts on `DirectML`: the next child, and every engine built
/// afterwards, is asked for the CPU.
#[tokio::test(flavor = "multi_thread")]
async fn a_child_that_dies_on_directml_leaves_every_later_engine_on_the_cpu() {
    assert!(!directml_switched_off());
    let (engine, _dir) = engine_with_fault("abort", |c| c.options.directml = true);
    engine.prepare().await.unwrap();
    assert_eq!(provider(&engine).await, Some(EncoderProvider::DirectMl));
    let error = engine.transcribe(&tone(0.5), None).await.unwrap_err();
    assert!(
        matches!(sidecar_error(error.as_ref()), SidecarError::Crashed { .. }),
        "{error}"
    );
    assert!(directml_switched_off());
    assert_works(&engine, &tone(0.5)).await;
    assert_eq!(engine.spawns(), 2);
    assert_eq!(provider(&engine).await, Some(EncoderProvider::Cpu));
    assert!(engine.config().options.directml, "the setting is untouched");

    let dir = tempfile::tempdir().unwrap();
    let mut later = config(&[]);
    later.options.directml = true;
    let later = engine_in(&dir, later);
    later.prepare().await.unwrap();
    assert_eq!(provider(&later).await, Some(EncoderProvider::Cpu));
}
