//! Steno's destinations and the meeting export: pure renderers
//! ([`rendering`]), the folder layout and slugs ([`naming`]), the Obsidian
//! vault destination ([`obsidian`]), the delivery coordinator and ledger
//! ([`runtime`]), and atomic file writes ([`fs`]). Nothing here opens a
//! network connection.

pub mod fs;
pub mod naming;
pub mod obsidian;
pub mod rendering;
pub mod runtime;

pub use obsidian::{ObsidianError, ObsidianFolderDestination};
pub use rendering::{ArtifactRenderer, LinkStyle, RenderOptions};
pub use runtime::DeliveryCoordinator;
