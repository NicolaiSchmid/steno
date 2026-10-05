//! A child on `DirectML` whose resident set passes the memory ceiling
//! between requests switches `DirectML` off, as one during a request does:
//! what it holds then is what its last request left, on the GPU too. The
//! next call still gets no error, and its child loads on the CPU. The
//! switch-off is process-wide and nothing clears it, so this test runs in
//! a binary of its own. Windows only: the client asks for `DirectML`
//! nowhere else.

#![cfg(windows)]

mod common;

use common::{assert_works, engine_with_fault, fault_queued_soon, provider, tone};
use steno_core::SpeechEngine as _;
use steno_speech::EncoderProvider;
use steno_speech::sidecar::directml_switched_off;

#[tokio::test(flavor = "multi_thread")]
async fn a_child_over_the_ceiling_while_idle_on_directml_switches_it_off() {
    assert!(!directml_switched_off());
    let (engine, _dir) = engine_with_fault("swell", |c| c.options.directml = true);
    engine.prepare().await.unwrap();
    assert_eq!(provider(&engine).await, Some(EncoderProvider::DirectMl));
    assert_works(&engine, &tone(0.5)).await;
    assert!(
        fault_queued_soon(&engine),
        "the reader never queued the report over the ceiling"
    );
    assert_works(&engine, &tone(0.5)).await;
    assert_eq!(engine.spawns(), 2);
    assert_eq!(provider(&engine).await, Some(EncoderProvider::Cpu));
    assert!(directml_switched_off());
}
