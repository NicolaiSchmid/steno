//! A child whose encoder lost its session on `DirectML` (a run failed
//! there and the CPU could not reopen the model) exits without answering,
//! so the parent sees a crash with `DirectML` in use and switches it off;
//! the next child loads on the CPU. The switch-off is process-wide and
//! nothing clears it, so this test runs in a binary of its own. Windows
//! only: the client asks for `DirectML` nowhere else.

#![cfg(windows)]

mod common;

use common::{assert_works, engine_with_fault, provider, sidecar_error, tone};
use steno_core::SpeechEngine as _;
use steno_speech::sidecar::directml_switched_off;
use steno_speech::{EncoderProvider, SidecarError};

#[tokio::test(flavor = "multi_thread")]
async fn a_child_that_lost_its_encoder_on_directml_switches_it_off() {
    assert!(!directml_switched_off());
    let (engine, _dir) = engine_with_fault("lose-encoder", |c| c.options.directml = true);
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
}
