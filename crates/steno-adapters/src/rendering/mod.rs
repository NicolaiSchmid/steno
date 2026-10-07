//! Pure renderers from the canonical model to bytes. No I/O, no clock;
//! equal inputs give equal bytes on every machine, and on the Mac
//! ([`RenderOptions::platform`]) the bytes are the Swift renderers'
//! (`Sources/StenoAdapters/Rendering`), pinned by the goldens in
//! `Tests/Fixtures/snapshots/obsidian`; a Windows or Linux call's folder
//! note differs in its info line's one word (`snapshots/platforms`).

mod artifact;
pub mod date_text;
mod folder_note;
mod frontmatter;
mod managed_block;
pub mod markdown_text;
mod names;
mod options;
mod person_page;
mod tasks;
mod timecode;
mod transcript;
mod vtt;

pub use artifact::{ArtifactRenderer, PersonPage, RenderedArtifact, RenderedArtifactKind};
pub use frontmatter::{Frontmatter, FrontmatterValue};
pub use managed_block::ManagedBlock;
pub use options::{LinkStyle, RenderOptions};
pub use timecode::Timecode;

pub(crate) use folder_note::FolderNoteRenderer;
pub(crate) use names::Names;
pub(crate) use person_page::PersonPageRenderer;
pub(crate) use tasks::TasksMarkdownRenderer;
pub(crate) use transcript::TranscriptMarkdownRenderer;
pub(crate) use vtt::WebVttRenderer;
