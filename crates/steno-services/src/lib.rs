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
//! | [`app`] | [`AppOptions`], [`build`], [`App`] with `host()`, `launch()`, `launch_finished()` and `shutdown()`, [`ExitGate`](app::ExitGate), [`SHUTDOWN_PATIENCE`](app::SHUTDOWN_PATIENCE), [`BuildError`], [`open_store`], [`lock_database`] with [`LOCK_PATIENCE`](app::LOCK_PATIENCE) |
//! | [`pipeline`] | [`CurrentPipeline`](pipeline::CurrentPipeline), the swappable [`ProcessingPipeline`](steno_pipeline::ProcessingPipeline) with the [`BuiltEngine`](pipeline::BuiltEngine) it was built with, and [`HostPipeline`](pipeline::HostPipeline), the host's `Pipeline` over it and the retention sweep |
//! | [`recorder`] | The host's `Recorder` over the capture session and the Mac intake |
//! | [`recovery`] | Recovery of an interrupted recording from its master on disk, at launch and after a failed stop |
//! | [`speech`] | The models directory, the speech settings, the speech engine per platform (the speech sidecar off the Mac), the ONNX diarizer, the host's `SpeechModels`, and [`SpeechEngines`](speech::SpeechEngines), the engines and the diarizer the pipelines share across reloads |
//! | [`llm`] | The LLM passes from the settings and the host's `LlmService` |
//! | [`logs`] | The shell's and the CLI's log output, which never waits for stderr: [`log_to_stderr`], [`LOG_FILTER`], [`flush_logs`] |
//! | [`handover`] | The identity in the secret store and the host's `Handover` over the listener |
//! | [`secrets`] | The platform keyring and the 0600 secrets file behind `SecretStore` |
//! | [`export`] | The host's `ExportValidator` over the Obsidian destination |
//! | [`files`] | Durable writes, from `steno-pipeline`: the secrets file, `preferences.json`, the CLI's `meeting.json` |
//! | [`platform`] | The clock, the folder usage walk, the input device list, the first-launch flags |
//!
//! What stays a fake here is named in [`build`]'s doc: the platform
//! services the shell does not supply yet (permissions, updater, clip
//! player, QR encoder; the login item when the shell passes none), each
//! with its reason and owner in the plan's "Pipeline and services (WP6b)"
//! list.
//!
//! Off the Mac, and on the Mac when the speech settings choose it or the
//! stored engine id has no Rust engine, Parakeet runs in the speech
//! sidecar ([`speech::SpeechSetup::runtime`]); the diarizer's ONNX models
//! run in this process.
//!
//! Secrets live in the platform keyring on macOS (the Keychain) and on
//! Windows (the credential store). On Linux they live in the 0600
//! `secrets.json` under the support directory, the store the CLI uses
//! everywhere: the `keyring` crate's `linux-native` store is the kernel
//! keyring, which does not survive a reboot (the handover identity and the
//! LLM API key would vanish), and its Secret Service store needs D-Bus and
//! a running secret service, which headless machines and the CI runners
//! do not have. The Secret Service has no work package yet.
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
//! // `build` blocks on the runtime for the secret store, so it runs inside it.
//! let app = runtime.block_on(async { build(options) })?;
//! let host = Arc::new(app.host()?);
//! let _guard = runtime.enter();
//! app.launch(&host);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

pub mod app;
pub mod export;
pub mod handover;
#[cfg(test)]
mod kill_tests;
pub mod llm;
pub mod logs;
pub mod pipeline;
pub mod platform;
pub mod recorder;
pub mod recovery;
pub mod secrets;
pub mod speech;
#[cfg(test)]
mod testing;

pub use app::{App, AppOptions, BuildError, build, lock_database, open_store};
pub use logs::{LOG_FILTER, flush_logs, log_to_stderr};
pub use secrets::{FileSecretStore, KeyringSecretStore, secret_store};
/// The durable writes live with the pipeline, whose phone intake needs
/// them; the secrets file and the CLI's `meeting.json` use them from here.
pub use steno_pipeline::files;

/// Runs `future` to completion on `runtime` from a synchronous host
/// service (the host's traits are synchronous, the clients are async).
pub(crate) fn block_on<T>(
    runtime: &tokio::runtime::Handle,
    future: impl std::future::Future<Output = T>,
) -> T {
    tokio::task::block_in_place(|| runtime.block_on(future))
}
