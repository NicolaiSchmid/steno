//! Names derived from titles and display names that are safe as folder and
//! file names in a vault.

use unicode_normalization::UnicodeNormalization as _;
use unicode_normalization::char::is_combining_mark;

use crate::rendering::markdown_text;

/// Pure and locale-independent; only [`Slug::file_name`] depends on the
/// platform (Windows device names).
pub struct Slug;

impl Slug {
    pub const DEFAULT_MAX_LENGTH: usize = 60;

    /// `"Produktstrategie: \"90/10\" & Roadmap für Q4"` becomes
    /// `"produktstrategie-90-10-roadmap-fuer-q4"`: German umlauts
    /// transliterated, other diacritics stripped, runs outside `[a-z0-9]`
    /// to one hyphen, cut to [`Slug::DEFAULT_MAX_LENGTH`], `"meeting"` when
    /// empty.
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
    /// and controls, collapses whitespace, trims spaces and dots; `"Unnamed"`
    /// when empty. On Windows reserved device names get a `_`; elsewhere
    /// `Con` stays, so Mac pages keep their names.
    #[must_use]
    pub fn file_name(text: &str) -> String {
        Self::file_name_reserving(text, cfg!(windows))
    }

    /// [`Slug::file_name`] with the platform explicit: when
    /// `device_names_reserved`, a stem (up to the first `.`, trailing
    /// spaces dropped) that is a reserved device name gets a `_`: `Con` to
    /// `Con_`, `nul.tar` to `nul_.tar`.
    #[must_use]
    pub fn file_name_reserving(text: &str, device_names_reserved: bool) -> String {
        let name = Self::sanitized_file_name(text);
        if !device_names_reserved {
            return name;
        }
        let stem_end = name.find('.').unwrap_or(name.len());
        let stem = name[..stem_end].trim_end_matches(' ');
        if Self::is_reserved_device_name(stem) {
            format!("{stem}_{}", &name[stem.len()..])
        } else {
            name
        }
    }

    /// `CON`, `PRN`, `AUX`, `NUL`, `CONIN$`, `CONOUT$`, `COM1` to `COM9`
    /// and `LPT1` to `LPT9` (the digit also `¹ ² ³`), in any case.
    fn is_reserved_device_name(stem: &str) -> bool {
        let upper = stem.to_ascii_uppercase();
        if matches!(
            upper.as_str(),
            "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
        ) {
            return true;
        }
        let Some(digit) = upper
            .strip_prefix("COM")
            .or_else(|| upper.strip_prefix("LPT"))
        else {
            return false;
        };
        let mut digits = digit.chars();
        matches!(
            (digits.next(), digits.next()),
            (Some('1'..='9' | '\u{B9}' | '\u{B2}' | '\u{B3}'), None)
        )
    }

    fn sanitized_file_name(text: &str) -> String {
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

    /// `ä ö ü ß` to `ae oe ue ss`, after canonical composition.
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

    /// Canonical decomposition with every combining mark dropped.
    pub(crate) fn strip_diacritics(text: &str) -> String {
        text.nfd()
            .filter(|character| !is_combining_mark(*character))
            .collect()
    }

    /// Cuts at the last hyphen within `max_length` unless the cut falls on
    /// a word boundary; trims hyphens. The slug is ASCII.
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
