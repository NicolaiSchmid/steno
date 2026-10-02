//! base64url without padding (RFC 4648 §5), used only inside the pairing QR
//! URL; every JSON body and header uses standard base64.
//! Swift: `Identity/Base64URL.swift`.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

#[must_use]
pub fn encode(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Accepts unpadded and padded input; rejects characters outside the
/// alphabet.
#[must_use]
pub fn decode(text: &str) -> Option<Vec<u8>> {
    if !text
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'='))
    {
        return None;
    }
    let unpadded = text.trim_end_matches('=');
    if unpadded.contains('=') {
        return None;
    }
    URL_SAFE_NO_PAD.decode(unpadded).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_rejects_foreign_characters() {
        for length in [0usize, 1, 2, 3, 31, 32, 33] {
            let data: Vec<u8> = (0..length)
                .map(|index| u8::try_from((index * 37 + 11) % 256).unwrap())
                .collect();
            let encoded = encode(&data);
            assert!(!encoded.contains('='));
            assert_eq!(decode(&encoded), Some(data));
        }
        assert_eq!(decode("AQID"), Some(vec![1, 2, 3]));
        assert_eq!(decode("AQID=="), Some(vec![1, 2, 3]));
        assert_eq!(decode("AQ+D"), None, "standard alphabet is not accepted");
        assert_eq!(decode("A"), None);
        assert!(decode("-_-_").is_some());
    }
}
