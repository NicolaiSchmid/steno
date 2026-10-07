//! What a destination tells the renderers.
//! Swift: `Sources/StenoAdapters/Rendering/RenderOptions.swift`.

use chrono_tz::Tz;
use steno_core::Platform;

/// How names are linked: `None` writes plain names (a `WebDAV` or Drive
/// destination), `Wikilink` writes `[[Name]]` for a vault.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum LinkStyle {
    #[default]
    None,
    Wikilink,
}

/// What a destination tells the renderers. Nothing here changes per meeting
/// and nothing renders the current time, so equal options and an equal
/// export always produce equal bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderOptions {
    pub link_style: LinkStyle,
    /// Whether per-person pages are rendered (the destination places them in
    /// its people folder). With `Wikilink` this also links every person's
    /// name to that page; a link to a page that does not exist is never
    /// written.
    pub person_pages: bool,
    /// Tag appended to every task line, for vaults with a Tasks global filter.
    pub task_tag: Option<String>,
    /// Time zone of dates in frontmatter, the info line and person lines.
    pub time_zone: Tz,
    /// The platform a call was recorded on, which the folder note's info
    /// line names ("Mac call", "Windows call", "Linux call"). The meeting row
    /// does not store it; the app records every call on the machine it runs
    /// on (`steno process` takes a file as recorded here), so
    /// [`ObsidianFolderDestination`](crate::ObsidianFolderDestination) passes
    /// [`Platform::CURRENT`]. Swift: none; the Swift app is the Mac.
    pub platform: Platform,
}

impl RenderOptions {
    /// Plain names, no people, no tag, UTC, the Mac, so the bytes are the
    /// same on every machine and equal the Swift renderer's. A destination
    /// sets `platform` from [`Platform::CURRENT`] itself.
    pub const PLAIN: RenderOptions = RenderOptions {
        link_style: LinkStyle::None,
        person_pages: false,
        task_tag: None,
        time_zone: Tz::UTC,
        platform: Platform::Macos,
    };

    /// A person's name becomes `[[Name]]` only when there is a page to land
    /// on and links are wikilinks.
    #[must_use]
    pub fn links_people(&self) -> bool {
        self.link_style == LinkStyle::Wikilink && self.person_pages
    }
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self::PLAIN
    }
}
