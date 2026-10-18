//! The language the summary is written in and its English name for the
//! prompt.

use steno_core::LanguageTag;

use crate::budget::primary_subtag;

/// The language the summary is written in: the meeting's elected language,
/// English when untagged. Names come from a fixed English table so golden
/// prompts are machine-independent; an unknown tag is written as itself.
pub struct OutputLanguage;

impl OutputLanguage {
    pub const FALLBACK: &'static str = "en";

    #[must_use]
    pub fn resolve(language: Option<&LanguageTag>) -> LanguageTag {
        match language {
            Some(tag) if !tag.as_str().is_empty() => tag.clone(),
            _ => LanguageTag::from(Self::FALLBACK),
        }
    }

    /// "German" for `de` or `de-CH`; the tag itself when unknown.
    #[must_use]
    pub fn prompt_name(language: &LanguageTag) -> String {
        let code = primary_subtag(language);
        Self::NAMES
            .iter()
            .find(|(known, _)| *known == code)
            .map_or_else(
                || language.as_str().to_owned(),
                |(_, name)| (*name).to_owned(),
            )
    }

    pub const NAMES: [(&'static str, &'static str); 25] = [
        ("de", "German"),
        ("en", "English"),
        ("fr", "French"),
        ("es", "Spanish"),
        ("it", "Italian"),
        ("nl", "Dutch"),
        ("pt", "Portuguese"),
        ("pl", "Polish"),
        ("tr", "Turkish"),
        ("sv", "Swedish"),
        ("da", "Danish"),
        ("nb", "Norwegian"),
        ("no", "Norwegian"),
        ("fi", "Finnish"),
        ("cs", "Czech"),
        ("ru", "Russian"),
        ("uk", "Ukrainian"),
        ("ja", "Japanese"),
        ("zh", "Chinese"),
        ("ko", "Korean"),
        ("ar", "Arabic"),
        ("hi", "Hindi"),
        ("el", "Greek"),
        ("hu", "Hungarian"),
        ("ro", "Romanian"),
    ];
}
