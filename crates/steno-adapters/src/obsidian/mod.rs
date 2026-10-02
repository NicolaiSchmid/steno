//! The Obsidian vault folder destination and the managed block it owns on
//! person pages.

mod destination;
mod managed_block;

pub use destination::{ObsidianError, ObsidianFolderDestination};
pub use managed_block::ManagedBlock;
