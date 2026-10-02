//! The SentencePiece vocabulary of `parakeet-tdt-0.6b-v3`
//! (`parakeet_v3_vocab.json`, 8,192 pieces) and the id sets FluidAudio
//! derives from it: splice-safe pieces for the seam merge (issue #683),
//! case-variant canonical ids for the overlap matcher (issue #706) and the
//! sentence-final punctuation ids (issue #905).
//!
//! The v3 file marks word starts with a leading space; the SentencePiece
//! export uses `▁` (U+2581). FluidAudio accepts both everywhere
//! (`isWordBoundary`, `stripWordBoundaryPrefix`), and so does this module.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::SpeechError;

/// The SentencePiece word-boundary marker.
pub const WORD_BOUNDARY: char = '\u{2581}';

/// The blank (and start-of-sequence) id of the v3 joint
/// (`TdtConfig.blankId`).
pub const BLANK_ID: usize = 8192;

/// A piece begins a word when it carries the marker or a leading space
/// (FluidAudio `isWordBoundary`).
#[must_use]
pub fn is_word_boundary(piece: &str) -> bool {
    piece.starts_with(WORD_BOUNDARY) || piece.starts_with(' ')
}

/// The piece without its leading marker or space
/// (FluidAudio `stripWordBoundaryPrefix`).
#[must_use]
pub fn strip_word_boundary(piece: &str) -> &str {
    piece
        .strip_prefix(WORD_BOUNDARY)
        .or_else(|| piece.strip_prefix(' '))
        .unwrap_or(piece)
}

/// Foundation's `CharacterSet.punctuationCharacters`, general categories
/// `P*`, over the characters that occur in the shipped vocabularies:
/// ASCII punctuation, the Latin-1 and General Punctuation blocks and the
/// CJK marks the Japanese vocabulary uses.
#[must_use]
pub fn is_punctuation(c: char) -> bool {
    matches!(
        c,
        '!' | '"'
            | '#'
            | '%'
            | '&'
            | '\''
            | '('
            | ')'
            | '*'
            | ','
            | '-'
            | '.'
            | '/'
            | ':'
            | ';'
            | '?'
            | '@'
            | '['
            | '\\'
            | ']'
            | '_'
            | '{'
            | '}'
            | '\u{A1}'
            | '\u{A7}'
            | '\u{AB}'
            | '\u{B6}'
            | '\u{B7}'
            | '\u{BB}'
            | '\u{BF}'
            | '\u{2010}'..='\u{2027}'
            | '\u{2030}'..='\u{205E}'
            | '\u{3001}'..='\u{3003}'
            | '\u{3008}'..='\u{3011}'
            | '\u{FF01}'
            | '\u{FF0C}'
            | '\u{FF0E}'
            | '\u{FF1A}'
            | '\u{FF1B}'
            | '\u{FF1F}'
    )
}

/// Foundation's `CharacterSet.symbols`, general categories `S*`, over the
/// same range: ASCII symbols, Latin-1 symbols and the currency block.
#[must_use]
pub fn is_symbol(c: char) -> bool {
    matches!(
        c,
        '$' | '+'
            | '<'
            | '='
            | '>'
            | '^'
            | '`'
            | '|'
            | '~'
            | '\u{A2}'..='\u{A6}'
            | '\u{A8}'
            | '\u{A9}'
            | '\u{AC}'
            | '\u{AE}'..='\u{B1}'
            | '\u{B4}'
            | '\u{B8}'
            | '\u{D7}'
            | '\u{F7}'
            | '\u{20A0}'..='\u{20C0}'
            | '\u{2100}'..='\u{214F}'
            | '\u{2190}'..='\u{21FF}'
            | '\u{2200}'..='\u{22FF}'
    )
}

/// Decoding this piece right after another word does not glue two words:
/// it starts a word or is pure punctuation and symbols
/// (`ChunkProcessor.isSpliceSafePiece`). The empty piece is neither.
#[must_use]
pub fn is_splice_safe_piece(piece: &str) -> bool {
    is_word_boundary(piece) || is_punctuation_only_piece(piece)
}

/// A non-empty piece of only punctuation and symbol scalars, boundary
/// included in the test (`ChunkProcessor.isPunctuationOnlyPiece`): `.`
/// qualifies, ` .` does not, because the space is neither.
#[must_use]
pub fn is_punctuation_only_piece(piece: &str) -> bool {
    !piece.is_empty() && piece.chars().all(|c| is_punctuation(c) || is_symbol(c))
}

/// The sentence-final marks FluidAudio resolves punctuation ids from
/// (`ASRConstants.sentenceFinalPunctuation`).
const SENTENCE_FINAL: [&str; 6] = [".", "?", "!", "。", "？", "！"];

/// The vocabulary and its derived sets.
#[derive(Debug, Clone, Default)]
pub struct Vocab {
    pieces: HashMap<usize, String>,
    splice_safe: HashSet<usize>,
    case_canonical: HashMap<usize, usize>,
    sentence_final: HashSet<usize>,
}

impl Vocab {
    /// `parakeet_v3_vocab.json` from the model directory: an object from
    /// decimal id strings to pieces.
    pub fn load(path: &Path) -> Result<Vocab, SpeechError> {
        let text = std::fs::read_to_string(path).map_err(|e| SpeechError::Vocabulary {
            path: path.to_path_buf(),
            message: e.to_string(),
        })?;
        Vocab::parse(&text).map_err(|message| SpeechError::Vocabulary {
            path: path.to_path_buf(),
            message,
        })
    }

    /// The JSON text of a vocabulary file.
    pub fn parse(text: &str) -> Result<Vocab, String> {
        let value: serde_json::Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
        let object = value.as_object().ok_or("vocabulary is not an object")?;
        let mut pieces = HashMap::with_capacity(object.len());
        for (key, piece) in object {
            let id: usize = key
                .parse()
                .map_err(|_| format!("id {key} is not a number"))?;
            let piece = piece
                .as_str()
                .ok_or_else(|| format!("piece {key} is not a string"))?;
            pieces.insert(id, piece.to_owned());
        }
        Ok(Vocab::from_pieces(pieces))
    }

    /// A vocabulary from its pieces; tests build small ones.
    #[must_use]
    pub fn from_pieces(pieces: HashMap<usize, String>) -> Vocab {
        let splice_safe = pieces
            .iter()
            .filter(|(_, piece)| is_splice_safe_piece(piece))
            .map(|(id, _)| *id)
            .collect();
        let sentence_final = pieces
            .iter()
            .filter(|(_, piece)| SENTENCE_FINAL.contains(&strip_word_boundary(piece)))
            .map(|(id, _)| *id)
            .collect();
        // `caseVariantCanonicalIds`: pieces equal under lowercasing share
        // the lower-case id (or the smallest when none is lower case).
        let mut groups: HashMap<String, Vec<usize>> = HashMap::new();
        for (id, piece) in &pieces {
            groups.entry(piece.to_lowercase()).or_default().push(*id);
        }
        let mut case_canonical = HashMap::new();
        for (folded, ids) in groups {
            if ids.len() < 2 {
                continue;
            }
            let canonical = ids
                .iter()
                .copied()
                .find(|id| pieces[id] == folded)
                .or_else(|| ids.iter().copied().min())
                .expect("a group has at least two ids");
            for id in ids {
                case_canonical.insert(id, canonical);
            }
        }
        Vocab {
            pieces,
            splice_safe,
            case_canonical,
            sentence_final,
        }
    }

    /// The piece for `id`, empty when unknown (FluidAudio's
    /// `vocabulary[id] ?? ""`).
    #[must_use]
    pub fn piece(&self, id: usize) -> &str {
        self.pieces.get(&id).map_or("", String::as_str)
    }

    /// Whether `id` has a piece at all.
    #[must_use]
    pub fn contains(&self, id: usize) -> bool {
        self.pieces.contains_key(&id)
    }

    /// Pieces in the vocabulary.
    #[must_use]
    pub fn len(&self) -> usize {
        self.pieces.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.pieces.is_empty()
    }

    /// Whether `id` may start a seam splice (`spliceSafeTokenIds`).
    #[must_use]
    pub fn is_splice_safe(&self, id: usize) -> bool {
        self.splice_safe.contains(&id)
    }

    /// Whether `id` ends a sentence (`punctuationTokenIds(in:)`).
    #[must_use]
    pub fn is_sentence_final(&self, id: usize) -> bool {
        self.sentence_final.contains(&id)
    }

    /// Whether the piece for `id` starts a word.
    #[must_use]
    pub fn starts_word(&self, id: usize) -> bool {
        is_word_boundary(self.piece(id))
    }

    /// Two ids match when equal or case-only variants of one piece
    /// (`ChunkProcessor.tokenIdsMatch`).
    #[must_use]
    pub fn ids_match(&self, left: usize, right: usize) -> bool {
        if left == right {
            return true;
        }
        match (
            self.case_canonical.get(&left),
            self.case_canonical.get(&right),
        ) {
            (Some(l), Some(r)) => l == r,
            _ => false,
        }
    }

    /// Two ids name the same piece up to case (`spliceCandidate.samePiece`).
    #[must_use]
    pub fn same_piece(&self, left: usize, right: usize) -> bool {
        if left == right {
            return true;
        }
        match (self.pieces.get(&left), self.pieces.get(&right)) {
            (Some(l), Some(r)) => l.to_lowercase() == r.to_lowercase(),
            _ => false,
        }
    }

    /// Whether the piece for `id` is punctuation and symbols only
    /// (`ChunkProcessor.isPunctuationOnlyPiece`).
    #[must_use]
    pub fn is_punctuation_only(&self, id: usize) -> bool {
        is_punctuation_only_piece(self.piece(id))
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A vocabulary with the shapes the merge rules care about.
    pub(crate) fn sample() -> Vocab {
        let pieces = [
            (1, " the"),
            (2, " The"),
            (3, "ing"),
            (4, "."),
            (5, " meeting"),
            (6, " Meeting"),
            (7, "s"),
            (8, "?"),
            (9, " ,"),
            (10, " €"),
            (11, "'"),
            (12, " hello"),
            (13, " world"),
            (14, " a"),
            (15, "b"),
            (16, "c"),
            (17, " ."),
            (18, "!"),
        ];
        Vocab::from_pieces(
            pieces
                .into_iter()
                .map(|(id, piece)| (id, piece.to_owned()))
                .collect(),
        )
    }

    #[test]
    fn boundaries_are_marker_or_space() {
        assert!(is_word_boundary(" the"));
        assert!(is_word_boundary("\u{2581}the"));
        assert!(!is_word_boundary("the"));
        assert_eq!(strip_word_boundary(" the"), "the");
        assert_eq!(strip_word_boundary("\u{2581}the"), "the");
        assert_eq!(strip_word_boundary("the"), "the");
    }

    #[test]
    fn splice_safe_is_word_start_or_pure_punctuation() {
        assert!(is_splice_safe_piece(" the"));
        assert!(is_splice_safe_piece("."));
        assert!(is_splice_safe_piece(",-"));
        assert!(is_splice_safe_piece("€"));
        assert!(is_splice_safe_piece("¿"));
        assert!(!is_splice_safe_piece("ing"));
        assert!(!is_splice_safe_piece(""));
        assert!(!is_splice_safe_piece("a."));
        assert!(is_punctuation_only_piece("."));
        assert!(!is_punctuation_only_piece(" ."));
        assert!(!is_punctuation_only_piece(""));
    }

    #[test]
    fn derived_sets_follow_fluidaudio() {
        let vocab = sample();
        assert!(vocab.is_splice_safe(1));
        assert!(vocab.is_splice_safe(4));
        assert!(!vocab.is_splice_safe(3));
        assert!(vocab.is_sentence_final(4));
        assert!(vocab.is_sentence_final(8));
        assert!(vocab.is_sentence_final(17));
        assert!(!vocab.is_sentence_final(9));
        // Case variants: the lower-case id is canonical.
        assert!(vocab.ids_match(1, 2));
        assert!(vocab.ids_match(5, 6));
        assert!(!vocab.ids_match(1, 5));
        assert!(!vocab.ids_match(3, 7));
        assert!(vocab.same_piece(1, 2));
        assert!(!vocab.same_piece(1, 3));
        assert!(vocab.is_punctuation_only(4));
        assert!(!vocab.is_punctuation_only(17));
        assert!(vocab.starts_word(1));
        assert!(!vocab.starts_word(3));
        assert_eq!(vocab.piece(999), "");
        assert_eq!(vocab.len(), 18);
    }

    #[test]
    fn parses_the_json_shape_of_the_model_file() {
        let vocab = Vocab::parse(r#"{"0": "<unk>", "1": " a", "8191": "z"}"#).unwrap();
        assert_eq!(vocab.piece(1), " a");
        assert_eq!(vocab.piece(8191), "z");
        assert!(Vocab::parse("[]").is_err());
        assert!(Vocab::parse(r#"{"x": "a"}"#).is_err());
        assert!(Vocab::parse(r#"{"1": 2}"#).is_err());
    }

    #[test]
    fn canonical_falls_back_to_the_smallest_id() {
        let vocab = Vocab::from_pieces(
            [(7, " ABC".to_owned()), (3, " Abc".to_owned())]
                .into_iter()
                .collect(),
        );
        assert!(vocab.ids_match(3, 7));
        assert_eq!(vocab.case_canonical[&7], 3);
    }
}
