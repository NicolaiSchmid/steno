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
/// alphabet (the engine does, once the trailing `=` are gone).
#[must_use]
pub fn decode(text: &str) -> Option<Vec<u8>> {
    URL_SAFE_NO_PAD.decode(text.trim_end_matches('=')).ok()
}
