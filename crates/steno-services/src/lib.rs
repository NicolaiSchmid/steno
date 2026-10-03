//! The composition root of the Rust app: one function, [`build`], turns
//! the stored settings into the real object graph and the
//! [`steno_host::Services`] the host runs on. The Tauri shell and the
//! `steno` CLI both call it; nothing here contains logic the other would
//! not also need. Swift: `apps/macos/Steno/AppEnvironment.swift` and
//! `Sources/steno/Wiring.swift`.
//! Plan: `.plans/2026-10-02-rust-core-and-tauri-shell.md` (`WP6b`).
//!
//! | Module | What it holds |
//! |--------|---------------|
//! | [`app`] | [`AppOptions`], [`build`], [`App`] with `host()` and `launch()`, [`BuildError`] |
//! | [`pipeline`] | [`CurrentPipeline`](pipeline::CurrentPipeline), the swappable [`ProcessingPipeline`](steno_pipeline::ProcessingPipeline), and [`HostPipeline`](pipeline::HostPipeline), the host's `Pipeline` over it and the retention sweep |
//! | [`recorder`] | The host's `Recorder` over the capture session and the Mac intake |
//! | [`speech`] | The models directory, the speech engine per platform, the ONNX diarizer, the host's `SpeechModels` |
//! | [`llm`] | The LLM passes from the settings and the host's `LlmService` |
//! | [`handover`] | The identity in the secret store and the host's `Handover` over the listener |
//! | [`secrets`] | The platform keyring and the 0600 secrets file behind `SecretStore` |
//! | [`export`] | The host's `ExportValidator` over the Obsidian destination |
//! | [`platform`] | The clock, the folder usage walk, the input device list, the first-launch flags |
//!
//! What stays a fake here is named in [`build`]'s doc: the shell's
//! platform services (permissions, login item, updater, clip player, QR
//! encoder) wait for the plan's `WP8`; the speech sidecar process is `WP4c`'s,
//! so ONNX inference runs in this process until it lands.
//!
//! Secrets live in the platform keyring on macOS (the Keychain) and on
//! Windows (the credential store). On Linux they live in the 0600
//! `secrets.json` under the support directory, the store the CLI uses
//! everywhere: the `keyring` crate's `linux-native` store is the kernel
//! keyring, which does not survive a reboot (the handover identity and the
//! LLM API key would vanish), and its Secret Service store needs D-Bus and
//! a running secret service, which headless machines and the CI runners
//! do not have. The Secret Service is `WP8`'s Linux release item.
//!
//! The shell's launch, in one piece:
//!
//! ```no_run
//! use std::sync::Arc;
//!
//! use steno_host::services::Opener;
//! use steno_services::{AppOptions, build};
//!
//! struct NoOpener;
//!
//! impl Opener for NoOpener {
//!     fn reveal(&self, _path: &std::path::Path) {}
//!     fn open_url(&self, _url: &str) {}
//!     fn open_window(&self, _window: steno_bridge::BridgeWindow) {}
//!     fn close_window(&self, _window: steno_bridge::BridgeWindow) {}
//! }
//!
//! let runtime = tokio::runtime::Runtime::new()?;
//! let options = AppOptions::product(runtime.handle().clone(), Arc::new(NoOpener), "0.1.0")?;
//! let app = runtime.block_on(async { build(options) })?;
//! let host = Arc::new(app.host()?);
//! let _guard = runtime.enter();
//! app.launch(&host);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

pub mod app;
pub mod export;
pub mod handover;
pub mod llm;
pub mod pipeline;
pub mod platform;
pub mod recorder;
pub mod secrets;
pub mod speech;
#[cfg(test)]
mod testing;

pub use app::{App, AppOptions, BuildError, build, open_store};
pub use secrets::{FileSecretStore, KeyringSecretStore, secret_store};

/// Runs `future` to completion on `runtime` from a synchronous host
/// service (the host's traits are synchronous, the clients are async).
pub(crate) fn block_on<T>(
    runtime: &tokio::runtime::Handle,
    future: impl std::future::Future<Output = T>,
) -> T {
    tokio::task::block_in_place(|| runtime.block_on(future))
}
