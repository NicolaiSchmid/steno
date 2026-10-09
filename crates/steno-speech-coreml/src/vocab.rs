//! The vocabulary beside the CoreML models: `parakeet_vocab.json`, an
//! object from decimal ids to the 8,192 SentencePiece pieces of
//! `parakeet-tdt-0.6b-v3`, read into the shared [`Vocab`]. The file marks a
//! word start with a leading space where the ONNX export's `tokens.txt`
//! has `▁`; [`Vocab`] reads both. The blank, id 8,192, is not in the file
//! and is appended, as `tokens.txt` lists it last.
//! Swift: `FluidAudio`'s `AsrModels.loadVocabulary`.

use std::path::Path;

use steno_speech::Vocab;

use crate::SpeechError;

/// The blank (and start-of-sequence) id of the v3 joint
/// (`TdtConfig.blankId`): one past the file's last piece.
pub const BLANK_TOKEN: u32 = 8192;

/// The vocabulary in `path`.
pub fn load(path: &Path) -> Result<Vocab, SpeechError> {
    let text = std::fs::read_to_string(path).map_err(|e| SpeechError::Vocabulary {
        path: path.to_path_buf(),
        message: e.to_string(),
    })?;
    parse(&text).map_err(|message| SpeechError::Vocabulary {
        path: path.to_path_buf(),
        message,
    })
}

/// The vocabulary in the JSON `text`: every id from 0 up, once, then the
/// blank.
pub fn parse(text: &str) -> Result<Vocab, String> {
    let value: serde_json::Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let object = value.as_object().ok_or("vocabulary is not an object")?;
    let mut pieces: Vec<Option<String>> = vec![None; object.len()];
    for (key, piece) in object {
        let id: usize = key
            .parse()
            .map_err(|_| format!("id {key} is not a number"))?;
        let slot = pieces
            .get_mut(id)
            .ok_or_else(|| format!("id {id} is past the {} pieces", object.len()))?;
        *slot = Some(
            piece
                .as_str()
                .ok_or_else(|| format!("piece {key} is not a string"))?
                .to_owned(),
        );
    }
    // Two keys for one id ("1" and "01") leave another id without a piece.
    let mut pieces = pieces
        .into_iter()
        .enumerate()
        .map(|(id, piece)| piece.ok_or_else(|| format!("id {id} has no piece")))
        .collect::<Result<Vec<String>, String>>()?;
    pieces.push("<blk>".to_owned());
    Vocab::from_pieces(pieces).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_json_file_reads_in_id_order_with_the_blank_appended() {
        let vocab = parse(r#"{"1": " Hallo", "0": "<unk>", "2": "s", "3": "."}"#).unwrap();
        assert_eq!(vocab.len(), 5);
        assert_eq!(vocab.blank_id(), 4);
        assert_eq!(vocab.piece(1), " Hallo");
        assert_eq!(vocab.piece(4), "");
        assert!(vocab.is_splice_safe(1));
        assert!(!vocab.is_splice_safe(2));
        assert!(vocab.is_splice_safe(3));
    }

    #[test]
    fn gaps_duplicates_and_other_shapes_are_refused() {
        assert!(
            parse(r#"{"0": "a", "2": "b"}"#)
                .unwrap_err()
                .contains("past")
        );
        assert!(
            parse(r#"{"0": "a", "x": "b"}"#)
                .unwrap_err()
                .contains("not a number")
        );
        assert!(parse(r#"{"0": 1}"#).unwrap_err().contains("not a string"));
        assert!(parse("[]").unwrap_err().contains("not an object"));
        assert!(
            parse(r#"{"1": "a", "01": "b"}"#)
                .unwrap_err()
                .contains("id 0 has no piece")
        );
        assert!(parse(r#"{"0": "a"}"#).is_ok());
        assert!(parse("{}").is_err());
    }
}
