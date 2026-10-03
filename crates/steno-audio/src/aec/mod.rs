//! Echo cancellation: the Speex canceller behind
//! [`steno_core::EchoCanceller`], a passthrough for tests and in-person
//! sessions, and the metrics the tests and `aec-bench` share.
//! Swift: `Sources/StenoAudio/AEC/`.

pub mod metrics;
pub mod passthrough;
pub mod speex;

pub use metrics::EchoMetrics;
pub use passthrough::PassthroughEchoCanceller;
pub use speex::{EchoCancellerError, SpeexEchoCanceller};
