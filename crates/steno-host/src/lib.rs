//! The view models behind the three windows and the [`Host`] that answers
//! the bridge over the real [`Store`](steno_core::Store). Its own crate
//! because it needs both the core and the bridge contract.
//!
//! - [`host`]: [`Host`], [`HostConfig`], the dialogs and the lifecycle
//!   calls the shell makes between commands.
//! - [`publisher`]: the coalescing of topics; [`recording_feed`]: the
//!   `recording` topic's own thread and its 20 Hz throttle.
//! - [`services`] and `fakes`: the seams to the shell and the other
//!   crates, and a fake for each (behind the `fakes` feature).
//! - [`main_window`], [`settings`], [`onboarding`], [`speakers`]: the view
//!   models and snapshots, one module per window.
//! - [`setup`], [`labels`], [`speech`], [`summary_markdown`]: shared copy
//!   and labels, the speech engines and assets, and the detail's split of
//!   the rendered summary.
//!
//! Every call blocks until done, for as long as a native prompt is up or
//! the store waits for another process's lock. So the shell calls from
//! Tauri's blocking pool, or from its ordered lane for tray actions and
//! window requests, never on a runtime worker or the event loop's thread,
//! which it marks with [`mark_ui_thread`] so a call there fails a debug
//! build ([`assert_off_ui_thread`]). [`host`]'s `Threads` section has the
//! rest of the rules.
//!
//! The crate holds no UI framework and no I/O beyond the store: everything
//! else is a trait in [`services`], so the whole host runs without a shell
//! on a temporary database. The view models are plain state machines
//! behind the [`Host`]'s mutex; each command names the topics it changed,
//! and the [`TopicPublisher`](publisher::TopicPublisher) coalesces them
//! into one full snapshot per topic.

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
pub mod recording_feed;
pub mod services;
pub mod settings;
pub mod setup;
pub mod speakers;
pub mod speech;
pub mod summary_markdown;
pub mod tray;
mod ui_thread;

pub use host::{Host, HostConfig};
pub use services::Services;
pub use ui_thread::{assert_off_ui_thread, mark_ui_thread};
