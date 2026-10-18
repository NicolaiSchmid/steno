//! What a destination tells the renderers.

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

/// What a destination tells the renderers. Nothing here changes per
/// meeting, so equal options and export give equal bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderOptions {
    pub link_style: LinkStyle,
    /// Whether per-person pages are rendered; with `Wikilink` names then
    /// link to them.
    pub person_pages: bool,
    /// Tag appended to every task line, for vaults with a Tasks global filter.
    pub task_tag: Option<String>,
    /// Time zone of dates in frontmatter, the info line and person lines.
    pub time_zone: Tz,
    /// The platform the folder note's info line names a call by. The row
    /// does not store it; the destination passes [`Platform::CURRENT`].
    pub platform: Platform,
}

impl RenderOptions {
    /// Plain names, no people, no tag, UTC, the Mac: the same bytes on every
    /// machine.
    pub const PLAIN: RenderOptions = RenderOptions {
        link_style: LinkStyle::None,
        person_pages: false,
        task_tag: None,
        time_zone: Tz::UTC,
        platform: Platform::Macos,
    };

    /// `[[Name]]` only when there is a page and links are wikilinks.
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
