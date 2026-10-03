//! Names derived from the model: the meeting folder layout and the slugs
//! that are safe as folder and file names in a vault.

mod folder;
mod slug;

pub use folder::{MeetingFolder, Note};
pub use slug::Slug;
