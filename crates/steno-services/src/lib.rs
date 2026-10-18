//! The composition root of the Rust app: [`build`] turns the stored
//! settings into the real object graph and the [`steno_host::Services`]
//! the host runs on. The `steno` CLI calls [`build`]; the Tauri shell calls
//! [`build_with_import`], which adds the Mac's import of the Swift app's
//! preferences, API key and handover identity.
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

#![deny(unsafe_code)]

#[cfg(test)]
mod admission_copy_tests;
#[cfg(test)]
mod admission_kill_tests;
pub mod app;
pub mod audio_folders;
pub mod auto_stop;
pub mod calendar;
pub mod clip_player;
pub mod detection;
pub mod export;
pub mod handover;
#[cfg(test)]
mod keychain_reset_tests;
#[cfg(test)]
mod kill_tests;
pub mod llm;
#[cfg(test)]
mod locked_store_tests;
pub mod logs;
pub mod model_gate;
pub mod permissions;
pub mod pipeline;
pub mod platform;
pub mod qr;
pub mod recorder;
pub mod recovery;
pub mod secrets;
pub mod speech;
pub mod swift_import;
#[cfg(test)]
mod testing;
pub mod updates;

pub use app::{App, AppOptions, BuildError, build, build_with_import, lock_database, open_store};
pub use logs::{LOG_FILTER, flush_logs, log_to_stderr};
pub use secrets::{
    FileSecretStore, KeyringSecretStore, KeyringUnavailable, secret_store, secret_store_with_unlock,
};
/// The durable writes, which live with the pipeline's phone intake.
pub use steno_pipeline::files;

/// Runs `future` to completion on `runtime` from a synchronous host service.
pub(crate) fn block_on<T>(
    runtime: &tokio::runtime::Handle,
    future: impl std::future::Future<Output = T>,
) -> T {
    tokio::task::block_in_place(|| runtime.block_on(future))
}
