//! The `SentencePiece` vocabulary of the export: `tokens.txt` as sherpa-onnx
//! writes it (`piece id` per line, ids in order, `<blk>` last), the word
//! boundary marker and the splice-safe set the merge cuts at.
//! Swift: `FluidAudio`'s `parakeet_vocab.json` plus the word-boundary rules of
//! `Sources/StenoSpeech/Segmentation/TokenAggregator.swift`.

use std::path::Path;

use crate::error::SpeechError;

/// `SentencePiece`'s word-start marker. The `CoreML` vocabulary replaces it with
/// a leading space; both are accepted everywhere.
pub const WORD_BOUNDARY: char = '\u{2581}';

/// The pieces by id. The last id is the blank.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Vocab {
    pieces: Vec<String>,
    splice_safe: Vec<bool>,
}

impl Vocab {
    /// Reads `tokens.txt`.
    pub fn load(path: &Path) -> Result<Self, SpeechError> {
        let text = std::fs::read_to_string(path).map_err(|source| SpeechError::io(path, source))?;
        let invalid = |detail: String| SpeechError::Vocabulary {
            path: path.to_path_buf(),
            detail,
        };
        let mut pieces = Vec::new();
        for (line_number, line) in text.lines().enumerate() {
            if line.is_empty() {
                continue;
            }
            let (piece, id) = line
                .rsplit_once(' ')
                .ok_or_else(|| invalid(format!("line {} has no id: {line:?}", line_number + 1)))?;
            let id: usize = id.parse().map_err(|_| {
                invalid(format!(
                    "line {} has a non-numeric id: {line:?}",
                    line_number + 1
                ))
            })?;
            if id != pieces.len() {
                return Err(invalid(format!(
                    "line {} has id {id}, expected {}",
                    line_number + 1,
                    pieces.len()
                )));
            }
            pieces.push(piece.to_owned());
        }
        if pieces.len() < 2 {
            return Err(invalid("fewer than two pieces".to_owned()));
        }
        Ok(Self::from_pieces(pieces))
    }

    /// A vocabulary from pieces in id order, the last being the blank.
    #[must_use]
    pub fn from_pieces(pieces: Vec<String>) -> Self {
        let splice_safe = pieces
            .iter()
            .map(|p| starts_word(p) || is_punctuation_piece(p))
            .collect();
        Vocab {
            pieces,
            splice_safe,
        }
    }

    /// Pieces including the blank.
    #[must_use]
    pub fn len(&self) -> usize {
        self.pieces.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.pieces.is_empty()
    }

    /// The last id.
    #[must_use]
    pub fn blank_id(&self) -> u32 {
        u32::try_from(self.pieces.len() - 1).expect("vocabulary fits u32")
    }

    /// The piece for `id`, empty for the blank and for unknown ids.
    #[must_use]
    pub fn piece(&self, id: u32) -> &str {
        if id == self.blank_id() {
            return "";
        }
        self.pieces.get(id as usize).map_or("", String::as_str)
    }

    /// Whether a window may be cut before `id`: a word start or punctuation
    /// (`FluidAudio`'s `mergeChunks` rule).
    #[must_use]
    pub fn is_splice_safe(&self, id: u32) -> bool {
        self.splice_safe.get(id as usize).copied().unwrap_or(false)
    }
}

/// Whether the piece begins a word: it carries the marker or a leading
/// space.
#[must_use]
pub fn starts_word(piece: &str) -> bool {
    piece.starts_with(WORD_BOUNDARY) || piece.starts_with(' ')
}

/// The piece without its word-start marker or leading space.
#[must_use]
pub fn strip_boundary(piece: &str) -> &str {
    piece
        .strip_prefix(WORD_BOUNDARY)
        .or_else(|| piece.strip_prefix(' '))
        .unwrap_or(piece)
}

/// Whether the piece is punctuation only (after its boundary), so it glues
/// to the word before it and a merge may cut at it.
#[must_use]
pub fn is_punctuation_piece(piece: &str) -> bool {
    let core = strip_boundary(piece).trim();
    !core.is_empty()
        && core.chars().all(|c| {
            c.is_ascii_punctuation()
                || matches!(c, '。' | '？' | '！' | '，' | '、' | '…' | '’' | '“' | '”')
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_txt_is_read_in_id_order_with_the_blank_last() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tokens.txt");
        std::fs::write(&path, "<unk> 0\n▁ 1\n▁Hallo 2\n, 3\nwelt 4\n<blk> 5\n").unwrap();
        let vocab = Vocab::load(&path).unwrap();
        assert_eq!(vocab.len(), 6);
        assert_eq!(vocab.blank_id(), 5);
        assert_eq!(vocab.piece(2), "▁Hallo");
        assert_eq!(vocab.piece(1), "▁");
        assert_eq!(vocab.piece(5), "");
        assert_eq!(vocab.piece(99), "");
        assert!(vocab.is_splice_safe(2));
        assert!(vocab.is_splice_safe(3));
        assert!(!vocab.is_splice_safe(4));
        assert!(!vocab.is_splice_safe(5));
    }

    #[test]
    fn gaps_and_garbage_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tokens.txt");
        std::fs::write(&path, "a 0\nb 2\n").unwrap();
        assert!(matches!(
            Vocab::load(&path),
            Err(SpeechError::Vocabulary { .. })
        ));
        std::fs::write(&path, "a\n").unwrap();
        assert!(matches!(
            Vocab::load(&path),
            Err(SpeechError::Vocabulary { .. })
        ));
        std::fs::write(&path, "a 0\n").unwrap();
        assert!(matches!(
            Vocab::load(&path),
            Err(SpeechError::Vocabulary { .. })
        ));
        assert!(matches!(
            Vocab::load(&dir.path().join("missing.txt")),
            Err(SpeechError::Io { .. })
        ));
    }

    #[test]
    fn boundaries_and_punctuation_follow_both_marker_styles() {
        assert!(starts_word("▁wir"));
        assert!(starts_word(" wir"));
        assert!(!starts_word("wir"));
        assert_eq!(strip_boundary("▁wir"), "wir");
        assert_eq!(strip_boundary(" wir"), "wir");
        assert_eq!(strip_boundary("wir"), "wir");
        assert!(is_punctuation_piece("."));
        assert!(is_punctuation_piece("▁?!"));
        assert!(is_punctuation_piece("…"));
        assert!(!is_punctuation_piece("▁"));
        assert!(!is_punctuation_piece("a."));
    }
}
