//! Steno's destinations and the meeting export, the port of
//! `Sources/StenoAdapters`. Plan: `.plans/2026-10-02-rust-core-and-tauri-shell.md`,
//! WP7.
//!
//! - [`rendering`]: pure renderers from [`MeetingExport`](steno_core::MeetingExport)
//!   to bytes: folder note, transcript and tasks Markdown, `WebVTT`,
//!   `meeting.json` and person pages. No I/O, no clock; equal inputs give
//!   equal bytes on every machine, and the bytes equal the Swift renderers'
//!   (`Tests/Fixtures/snapshots/obsidian`).
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

pub use obsidian::{ObsidianError, ObsidianFolderDestination};
pub use rendering::{
    ArtifactRenderer, Frontmatter, FrontmatterValue, LinkStyle, PersonPage, RenderOptions,
    RenderedArtifact, RenderedArtifactKind, Timecode,
};
pub use runtime::DeliveryCoordinator;

/// SHA-256 of `data`, the digest delivery receipts carry. Swift:
/// `ContentHash.sha256`.
#[must_use]
pub fn sha256(data: &[u8]) -> Vec<u8> {
    use sha2::Digest as _;
    sha2::Sha256::digest(data).to_vec()
}
