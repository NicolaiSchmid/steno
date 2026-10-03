//! The digest delivery receipts and handover manifests carry.
//! Swift: `Sources/StenoCore/Support/ContentHash.swift`.

use sha2::Digest as _;

/// SHA-256 of `data` as the 32 raw bytes a `DeliveredFile` or a handover
/// manifest stores. One definition, so the adapters, the handover intake
/// and the fakes agree on what a receipt's hash is.
#[must_use]
pub fn sha256(data: &[u8]) -> Vec<u8> {
    sha2::Sha256::digest(data).to_vec()
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;

    use super::sha256;

    #[test]
    fn sha256_is_the_reference_digest() {
        let digest = sha256(b"abc");
        assert_eq!(digest.len(), 32);
        let mut hex = String::new();
        for byte in &digest {
            let _ = write!(hex, "{byte:02x}");
        }
        assert_eq!(
            hex,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256(b"").len(),
            32,
            "the empty input hashes like any other"
        );
    }
}
