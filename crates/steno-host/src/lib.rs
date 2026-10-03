//! The host module of the Rust port: the view models behind the three
//! windows and the [`Host`] that answers the bridge over the real
//! [`Store`](steno_core::Store). Ported from the Swift app's view models
//! (`apps/macos/Steno/{Main,Settings,Onboarding,Speakers}`) and its window
//! bridges (`apps/macos/Steno/Web/*Bridge.swift`, `*Snapshots.swift`,
//! `TopicPublisher.swift`). Plan: `.plans/2026-10-02-rust-core-and-tauri-shell.md`,
//! WP6. The plan placed this module in `steno-core`; the bridge contract
//! depends on the core, so the host, which needs both, sits above them in
//! its own crate instead.
//!
//! - [`host`]: [`Host`], [`HostConfig`], the dialog closures and the
//!   lifecycle calls the shell makes between commands.
//! - [`publisher`]: the coalescing of topics and the 20 Hz throttle.
//! - [`services`] and `fakes`: the seams to the shell, the pipeline and
//!   the other crates, and a fake for each (behind the `fakes` feature).
//! - [`main_window`], [`settings`], [`onboarding`], [`speakers`]: the view
//!   models and snapshots, one module per window.
//! - [`setup`], [`labels`], [`speech`], [`summary_markdown`]: the copy and
//!   the labels the view models share, and the two tables (the speech
//!   engines and assets, the summary Markdown) that move to WP4 and WP6b.
//!
//! The crate holds no UI framework and no I/O of its own beyond the store:
//! everything the Swift app reached through a system framework (TCC,
//! `ServiceManagement`, Sparkle, Core Audio, `EventKit`, the capture session,
//! the pipeline, the model store, the LLM probe, the handover listener, the
//! Finder) is a trait in [`services`], with a fake for each in `fakes`,
//! so the whole host runs without a shell on a temporary database: that is how the
//! tests work and how the CLI can drive it. The core's own boundaries
//! (`steno_core::protocols`) are consumed where one exists
//! ([`SecretStore`](steno_core::SecretStore) for the API key, with the
//! core's `testing` fake behind it in `fakes`); the rest wait for the
//! crates that implement them (WP4, WP5, WP7) and are shaped so the switch
//! is a `use` line.
//!
//! The view models are plain state machines behind the [`Host`]'s mutex,
//! which is what the Swift `@MainActor` amounted to. Where Swift followed
//! the store through GRDB observation, the Rust host reloads on
//! [`Host::store_changed`], which the pipeline and the shell call after a
//! write; where Swift republished a topic through observation tracking,
//! each command here names the topics it changed and the
//! [`TopicPublisher`](publisher::TopicPublisher) coalesces them into one
//! full snapshot per topic, `recording` at most at 20 Hz, as
//! `TopicPublisher.swift` did.

// Bridge snapshots and recorder status values carry many flags each; the
// shape is the contract's, not a design choice to lint.
#![allow(clippy::struct_excessive_bools)]

#[cfg(any(test, feature = "fakes"))]
pub mod fakes;
pub mod host;
pub mod labels;
pub mod main_window;
pub mod onboarding;
pub mod publisher;
pub mod services;
pub mod settings;
pub mod setup;
pub mod speakers;
pub mod speech;
pub mod summary_markdown;

pub use host::{Host, HostConfig};
pub use services::Services;
