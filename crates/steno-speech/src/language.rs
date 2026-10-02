//! The per-segment language tag. No engine can be forced per segment, so
//! the tag comes from the text: `whatlang` constrained to the candidates,
//! with a function-word count as the fallback for text it cannot judge.
//! Segments under `minimum_words` inherit the previous segment's language
//! (or the next tagged one at the start), because four words are not
//! enough to tell German from English reliably.
//! Swift: `Sources/StenoSpeech/Segmentation/LanguageTagger.swift`, whose
//! `NLLanguageRecognizer` is replaced by `whatlang` here (no Foundation).

use std::collections::{BTreeMap, BTreeSet};

use steno_core::{LanguageTag, RawSegment};

/// Decides the language of one piece of text among `candidates`. `hint` is
/// the language expected for the meeting; a recogniser may lean on it for
/// ambiguous text and returns `None` when it has no opinion.
pub trait LanguageRecognizer: Send + Sync {
    fn recognize(
        &self,
        text: &str,
        candidates: &BTreeSet<LanguageTag>,
        hint: Option<&LanguageTag>,
    ) -> Option<LanguageTag>;
}

/// Fills `RawSegment::language`.
pub struct LanguageTagger {
    pub candidates: BTreeSet<LanguageTag>,
    pub minimum_words: usize,
    recognizer: Box<dyn LanguageRecognizer>,
}

impl Default for LanguageTagger {
    fn default() -> Self {
        Self::new()
    }
}

impl LanguageTagger {
    /// German and English, four words, `whatlang`.
    #[must_use]
    pub fn new() -> Self {
        Self::with_recognizer(Self::default_candidates(), 4, Box::new(WhatlangRecognizer))
    }

    #[must_use]
    pub fn with_recognizer(
        candidates: BTreeSet<LanguageTag>,
        minimum_words: usize,
        recognizer: Box<dyn LanguageRecognizer>,
    ) -> Self {
        LanguageTagger {
            candidates,
            minimum_words,
            recognizer,
        }
    }

    #[must_use]
    pub fn default_candidates() -> BTreeSet<LanguageTag> {
        ["de", "en"].into_iter().map(LanguageTag::from).collect()
    }

    /// Tags every segment. `hint` is the meeting language when the pipeline
    /// knows it (the other lane's result); it breaks ties and covers
    /// segments nothing else can tag.
    #[must_use]
    pub fn tag(
        &self,
        mut segments: Vec<RawSegment>,
        hint: Option<&LanguageTag>,
    ) -> Vec<RawSegment> {
        let mut decided: Vec<Option<LanguageTag>> = segments
            .iter()
            .map(|segment| {
                (word_count(&segment.text) >= self.minimum_words)
                    .then(|| {
                        self.recognizer
                            .recognize(&segment.text, &self.candidates, hint)
                    })
                    .flatten()
            })
            .collect();
        let fallback = hint.cloned().or_else(|| {
            dominant(
                decided
                    .iter()
                    .zip(&segments)
                    .filter_map(|(language, segment)| {
                        Some((language.as_ref()?, segment.duration()))
                    }),
            )
        });
        let mut previous: Option<LanguageTag> = None;
        for index in 0..segments.len() {
            if decided[index].is_some() {
                previous.clone_from(&decided[index]);
            } else {
                decided[index] = previous
                    .clone()
                    .or_else(|| decided[index + 1..].iter().find_map(Clone::clone))
                    .or_else(|| fallback.clone());
            }
            segments[index].language.clone_from(&decided[index]);
        }
        segments
    }

    /// The language with the most seconds of tagged speech; `None` when
    /// nothing is tagged.
    #[must_use]
    pub fn dominant_language(&self, segments: &[RawSegment]) -> Option<LanguageTag> {
        dominant(
            segments
                .iter()
                .filter_map(|s| Some((s.language.as_ref()?, s.duration()))),
        )
    }

    /// Adjacent tagged segments with different languages. Untagged segments
    /// neither flip nor break a run.
    #[must_use]
    pub fn language_flips(&self, segments: &[RawSegment]) -> usize {
        let languages: Vec<&LanguageTag> = segments
            .iter()
            .filter_map(|s| s.language.as_ref())
            .collect();
        languages.windows(2).filter(|w| w[0] != w[1]).count()
    }
}

/// Most seconds wins; ties go to the alphabetically first tag.
fn dominant<'a>(tagged: impl Iterator<Item = (&'a LanguageTag, f64)>) -> Option<LanguageTag> {
    let mut seconds: BTreeMap<&LanguageTag, f64> = BTreeMap::new();
    for (language, duration) in tagged {
        *seconds.entry(language).or_default() += duration.max(0.001);
    }
    let mut best: Option<(&LanguageTag, f64)> = None;
    for (language, &total) in &seconds {
        if best.is_none_or(|(_, b)| total > b) {
            best = Some((language, total));
        }
    }
    best.map(|(language, _)| language.clone())
}

fn word_count(text: &str) -> usize {
    text.split_whitespace().count()
}

/// `whatlang` restricted to the candidates; unreliable detections and
/// candidates it does not know fall back to [`StopwordRecognizer`].
#[derive(Debug, Clone, Copy, Default)]
pub struct WhatlangRecognizer;

impl LanguageRecognizer for WhatlangRecognizer {
    fn recognize(
        &self,
        text: &str,
        candidates: &BTreeSet<LanguageTag>,
        hint: Option<&LanguageTag>,
    ) -> Option<LanguageTag> {
        let allowlist: Vec<whatlang::Lang> = candidates
            .iter()
            .filter_map(|tag| whatlang_lang(tag.as_str()))
            .collect();
        let fallback = || StopwordRecognizer.recognize(text, candidates, hint);
        if allowlist.is_empty() {
            return fallback();
        }
        let detector = whatlang::Detector::with_allowlist(allowlist);
        match detector.detect(text) {
            Some(info) if info.is_reliable() => {
                let tag = LanguageTag::from(iso_639_1(info.lang())?);
                candidates.contains(&tag).then_some(tag).or_else(fallback)
            }
            _ => fallback(),
        }
    }
}

/// ISO 639-1 codes of the languages Parakeet v3 speaks, by `whatlang`'s
/// ISO 639-3 code. Maltese is not in `whatlang`.
const CODES: [(&str, &str); 24] = [
    ("bul", "bg"),
    ("hrv", "hr"),
    ("ces", "cs"),
    ("dan", "da"),
    ("nld", "nl"),
    ("eng", "en"),
    ("est", "et"),
    ("fin", "fi"),
    ("fra", "fr"),
    ("deu", "de"),
    ("ell", "el"),
    ("hun", "hu"),
    ("ita", "it"),
    ("lav", "lv"),
    ("lit", "lt"),
    ("pol", "pl"),
    ("por", "pt"),
    ("ron", "ro"),
    ("slk", "sk"),
    ("slv", "sl"),
    ("spa", "es"),
    ("swe", "sv"),
    ("rus", "ru"),
    ("ukr", "uk"),
];

fn iso_639_1(lang: whatlang::Lang) -> Option<&'static str> {
    CODES
        .iter()
        .find(|(three, _)| *three == lang.code())
        .map(|(_, two)| *two)
}

fn whatlang_lang(tag: &str) -> Option<whatlang::Lang> {
    let primary = tag.split(['-', '_']).next()?.to_ascii_lowercase();
    CODES
        .iter()
        .find(|(_, two)| *two == primary)
        .and_then(|(three, _)| whatlang::Lang::from_code(*three))
}

/// Counts German and English function words; a tie goes to the hint. The
/// deterministic recogniser for tests and the fallback. Only knows `de`
/// and `en`.
#[derive(Debug, Clone, Copy, Default)]
pub struct StopwordRecognizer;

impl StopwordRecognizer {
    const GERMAN: [&'static str; 55] = [
        "der", "die", "das", "und", "ist", "nicht", "ich", "wir", "sie", "ein", "eine", "zu",
        "mit", "auf", "für", "von", "den", "dem", "des", "im", "ja", "nein", "auch", "noch",
        "schon", "wie", "aber", "oder", "wenn", "dann", "haben", "hat", "sind", "war", "es", "du",
        "er", "was", "dass", "bei", "nach", "über", "uns", "euch", "mal", "hier", "heute",
        "morgen", "machen", "können", "wird", "werden", "sich", "als", "zum",
    ];
    const ENGLISH: [&'static str; 44] = [
        "the", "and", "is", "not", "i", "we", "you", "a", "an", "to", "with", "on", "for", "of",
        "in", "it", "that", "this", "are", "was", "have", "has", "be", "but", "or", "if", "then",
        "they", "he", "she", "what", "at", "by", "from", "about", "can", "will", "do", "let's",
        "today", "our", "should", "would", "there",
    ];
}

impl LanguageRecognizer for StopwordRecognizer {
    fn recognize(
        &self,
        text: &str,
        candidates: &BTreeSet<LanguageTag>,
        hint: Option<&LanguageTag>,
    ) -> Option<LanguageTag> {
        let lower = text.to_lowercase();
        let words = lower
            .split(|c: char| !c.is_alphabetic() && c != '\'')
            .filter(|w| !w.is_empty());
        let (mut german_hits, mut english_hits) = (0, 0);
        for word in words {
            if Self::GERMAN.contains(&word) {
                german_hits += 1;
            }
            if Self::ENGLISH.contains(&word) {
                english_hits += 1;
            }
        }
        let german = LanguageTag::from("de");
        let english = LanguageTag::from("en");
        if german_hits > english_hits && candidates.contains(&german) {
            return Some(german);
        }
        if english_hits > german_hits && candidates.contains(&english) {
            return Some(english);
        }
        hint.filter(|h| candidates.contains(h)).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn segment(text: &str, start: f64, end: f64) -> RawSegment {
        RawSegment {
            start,
            end,
            text: text.to_owned(),
            language: None,
            word_timings: None,
        }
    }

    fn tagger() -> LanguageTagger {
        LanguageTagger::with_recognizer(
            LanguageTagger::default_candidates(),
            4,
            Box::new(StopwordRecognizer),
        )
    }

    fn languages(segments: &[RawSegment]) -> Vec<Option<&str>> {
        segments
            .iter()
            .map(|s| s.language.as_ref().map(LanguageTag::as_str))
            .collect()
    }

    #[test]
    fn tags_german_and_english_segments() {
        let tagged = tagger().tag(
            vec![
                segment("wir müssen das heute noch machen", 0.0, 2.0),
                segment("and then we can ship it today", 2.0, 4.0),
            ],
            None,
        );
        assert_eq!(languages(&tagged), [Some("de"), Some("en")]);
        assert_eq!(tagger().language_flips(&tagged), 1);
    }

    #[test]
    fn short_segments_inherit_the_previous_language_or_the_next_at_the_start() {
        let tagged = tagger().tag(
            vec![
                segment("Okay", 0.0, 0.5),
                segment("wir müssen das heute noch machen", 0.5, 3.0),
                segment("ja genau", 3.0, 3.5),
                segment("and then we can ship it today", 3.5, 6.0),
                segment("yes", 6.0, 6.2),
            ],
            None,
        );
        assert_eq!(
            languages(&tagged),
            [Some("de"), Some("de"), Some("de"), Some("en"), Some("en")]
        );
    }

    #[test]
    fn the_hint_covers_undecidable_text_and_breaks_ties() {
        let hint = LanguageTag::from("en");
        let tagged = tagger().tag(
            vec![
                segment("hm", 0.0, 1.0),
                segment("xyz qqq zzz www", 1.0, 2.0),
            ],
            Some(&hint),
        );
        assert_eq!(languages(&tagged), [Some("en"), Some("en")]);
        let untagged = tagger().tag(vec![segment("hm", 0.0, 1.0)], None);
        assert_eq!(languages(&untagged), [None]);
    }

    #[test]
    fn the_dominant_language_is_by_seconds_with_ties_to_the_first_tag() {
        let mut segments = vec![
            segment("a", 0.0, 1.0),
            segment("b", 1.0, 4.0),
            segment("c", 4.0, 5.0),
        ];
        segments[0].language = Some("en".into());
        segments[1].language = Some("de".into());
        segments[2].language = Some("en".into());
        assert_eq!(
            tagger().dominant_language(&segments).unwrap().as_str(),
            "de"
        );
        segments[1].language = None;
        assert_eq!(
            tagger().dominant_language(&segments).unwrap().as_str(),
            "en"
        );
        segments[2].language = Some("fr".into());
        // en 1 s, fr 1 s: alphabetical.
        assert_eq!(
            tagger().dominant_language(&segments).unwrap().as_str(),
            "en"
        );
        assert_eq!(tagger().dominant_language(&[segment("x", 0.0, 1.0)]), None);
        assert_eq!(tagger().language_flips(&segments), 1);
    }

    #[test]
    fn whatlang_decides_clear_text_and_the_codes_round_trip() {
        let candidates = LanguageTagger::default_candidates();
        let german = WhatlangRecognizer.recognize(
            "Die Besprechung beginnt morgen früh um neun Uhr und dauert ungefähr eine Stunde",
            &candidates,
            None,
        );
        assert_eq!(german.as_ref().map(LanguageTag::as_str), Some("de"));
        let english = WhatlangRecognizer.recognize(
            "The meeting starts tomorrow morning at nine and takes about an hour",
            &candidates,
            None,
        );
        assert_eq!(english.as_ref().map(LanguageTag::as_str), Some("en"));
        assert_eq!(whatlang_lang("de-DE"), Some(whatlang::Lang::Deu));
        assert_eq!(whatlang_lang("mt"), None);
        assert_eq!(iso_639_1(whatlang::Lang::Ukr), Some("uk"));
        // Candidates whatlang does not know fall back to the stopword count.
        let only_maltese: BTreeSet<LanguageTag> =
            ["mt"].into_iter().map(LanguageTag::from).collect();
        assert_eq!(
            WhatlangRecognizer.recognize("hello there", &only_maltese, None),
            None
        );
    }
}
