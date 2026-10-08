//! Steno's destinations and the meeting export, the port of
//! `Sources/StenoAdapters`. Plan: `.plans/2026-10-02-rust-core-and-tauri-shell.md`.
//!
//! - [`rendering`]: pure renderers from [`MeetingExport`](steno_core::MeetingExport)
//!   to bytes: folder note, transcript and tasks Markdown, `WebVTT`,
//!   `meeting.json` and person pages. No I/O, no clock; equal inputs give
//!   equal bytes on every machine but for one rule: on Windows a person
//!   named `Con` gets `Con_.md` and the link `[[Con_|Con]]`
//!   ([`Slug::file_name`](naming::Slug::file_name)). On the Mac the bytes
//!   are the Swift renderers' (`Tests/Fixtures/snapshots/obsidian`); a
//!   Windows or Linux call's folder note differs in its info line's one
//!   word (`Tests/Fixtures/snapshots/platforms`).
//! - [`naming`]: the meeting folder layout and the slugs.
//! - [`obsidian`]: the Obsidian vault folder destination and the managed
//!   block it owns on person pages.
//! - [`runtime`]: the delivery coordinator (the
//!   [`DeliveryDispatcher`](steno_core::DeliveryDispatcher)) and the ledger
//!   that holds the delivery policy.
//! - [`fs`]: atomic file writes and the folder sink.
//!
//! Nothing here opens a network connection: the only destination writes to
//! a local folder. A destination that does is the one place in the product
//! besides the LLM client that may.

pub mod fs;
pub mod naming;
pub mod obsidian;
pub mod rendering;
pub mod runtime;

// The entry points a caller wires up; everything else through its module.
pub use obsidian::{ObsidianError, ObsidianFolderDestination};
pub use rendering::{ArtifactRenderer, LinkStyle, RenderOptions};
pub use runtime::DeliveryCoordinator;
