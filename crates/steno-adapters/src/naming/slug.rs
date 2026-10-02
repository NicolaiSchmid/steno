//! Names derived from titles and display names that are safe as folder and
//! file names in a vault.
//! Swift: `Sources/StenoAdapters/Naming/Slug.swift`.

use unicode_normalization::UnicodeNormalization as _;
use unicode_normalization::char::is_combining_mark;

use crate::rendering::markdown_text;

/// Both functions are pure and machine-independent: no locale enters, so
/// `de_DE` and `en_US` processes produce the same bytes.
pub struct Slug;

impl Slug {
    pub const DEFAULT_MAX_LENGTH: usize = 60;

    /// `"Produktstrategie: \"90/10\" & Roadmap für Q4"` becomes
    /// `"produktstrategie-90-10-roadmap-fuer-q4"`: lowercased, `ä ö ü ß`
    /// transliterated to `ae oe ue ss`, other diacritics stripped, every run
    /// outside `[a-z0-9]` collapsed to one hyphen, hyphens trimmed, cut to
    /// [`Slug::DEFAULT_MAX_LENGTH`] at the last hyphen, `"meeting"` when
    /// nothing is left.
    #[must_use]
    pub fn title(text: &str) -> String {
        Self::title_with_max_length(text, Self::DEFAULT_MAX_LENGTH)
    }

    /// [`Slug::title`] cut to `max_length`.
    #[must_use]
    pub fn title_with_max_length(text: &str, max_length: usize) -> String {
        let folded = Self::strip_diacritics(&Self::transliterate_german(&text.to_lowercase()));
        let slug = folded
            .split(|character: char| !Self::is_slug_character(character))
            .filter(|piece| !piece.is_empty())
            .collect::<Vec<_>>()
            .join("-");
        let cut = Self::truncate(&slug, max_length);
        if cut.is_empty() {
            "meeting".to_owned()
        } else {
            cut
        }
    }

    /// A display name as a note file name: strips `/ \ : * ? " < > | # ^ [ ]`
    /// and control characters, collapses whitespace, trims spaces and dots.
    /// `"Unnamed"` when nothing is left.
    #[must_use]
    pub fn file_name(text: &str) -> String {
        let kept: String = text
            .chars()
            .filter(|character| {
                !Self::FORBIDDEN_IN_FILE_NAME.contains(*character)
                    && *character >= ' '
                    && *character != '\u{7F}'
            })
            .collect();
        let name = markdown_text::single_line(&kept)
            .trim_matches([' ', '.'])
            .to_owned();
        if name.is_empty() {
            "Unnamed".to_owned()
        } else {
            name
        }
    }

    const FORBIDDEN_IN_FILE_NAME: &'static str = "/\\:*?\"<>|#^[]";

    fn is_slug_character(character: char) -> bool {
        character.is_ascii_lowercase() || character.is_ascii_digit()
    }

    /// `ä ö ü ß` to `ae oe ue ss`, matched after canonical composition so
    /// decomposed forms (`a` + U+0308) fold the same way as precomposed ones.
    pub(crate) fn transliterate_german(text: &str) -> String {
        text.nfc()
            .map(|character| {
                match character {
                    'ä' => "ae",
                    'ö' => "oe",
                    'ü' => "ue",
                    'ß' => "ss",
                    other => return other.to_string(),
                }
                .to_owned()
            })
            .collect()
    }

    /// Canonical decomposition, then every combining mark dropped: `é` to
    /// `e`, `ñ` to `n`. Unicode data only, no locale.
    pub(crate) fn strip_diacritics(text: &str) -> String {
        text.nfd()
            .filter(|character| !is_combining_mark(*character))
            .collect()
    }

    /// Cuts at the last hyphen inside the first `max_length` characters
    /// unless the cut already falls on a word boundary; trims hyphens either
    /// way. The slug is ASCII, so characters are bytes.
    pub(crate) fn truncate(slug: &str, max_length: usize) -> String {
        if slug.len() <= max_length {
            return slug.to_owned();
        }
        let mut head = &slug[..max_length];
        if slug.as_bytes()[max_length] != b'-'
            && let Some(last_hyphen) = head.rfind('-')
            && last_hyphen != 0
        {
            head = &head[..last_hyphen];
        }
        head.trim_end_matches('-').to_owned()
    }
}
