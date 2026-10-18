//! The Obsidian vault folder destination and the managed block it owns on
//! person pages.

mod destination;

pub use crate::rendering::ManagedBlock;
#[cfg(feature = "testing")]
pub use destination::DeliveryStep;
pub use destination::{ObsidianError, ObsidianFolderDestination};
