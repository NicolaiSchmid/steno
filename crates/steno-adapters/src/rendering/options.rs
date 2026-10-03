//! What a destination tells the renderers.
//! Swift: `Sources/StenoAdapters/Rendering/RenderOptions.swift`.

use chrono_tz::Tz;

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
}

impl RenderOptions {
    /// Plain names, no people, no tag, UTC.
    pub const PLAIN: RenderOptions = RenderOptions {
        link_style: LinkStyle::None,
        person_pages: false,
        task_tag: None,
        time_zone: Tz::UTC,
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
