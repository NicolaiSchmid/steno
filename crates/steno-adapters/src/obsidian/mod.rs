//! The Obsidian vault folder destination and the managed block it owns on
//! person pages.

mod destination;

/// The block is pure text and lives with the renderers; the destination is
/// what writes it into the vault.
pub use crate::rendering::ManagedBlock;
pub use destination::{DeliveryStep, ObsidianError, ObsidianFolderDestination};
