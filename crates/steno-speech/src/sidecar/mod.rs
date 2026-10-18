//! ONNX inference in a child process, the speech sidecar: [`protocol`] is the
//! framed messages over the child's stdin and stdout, [`client`] the
//! `SpeechEngine` that spawns, limits and replaces the child. Audio reaches
//! the child only over its stdin. Why speech runs in a child:
//! [`runtime`](crate::runtime).

pub mod client;
pub mod protocol;

pub use client::{
    DiarizerModels, FALLBACK_NOTICE, SIDECAR_BINARY, SidecarConfig, SidecarHealth,
    SidecarSpeechEngine, directml_switched_off,
};
