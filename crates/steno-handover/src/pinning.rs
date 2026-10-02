//! The one place that decides whether a TLS server is the paired computer:
//! the SHA-256 of the leaf certificate's DER bytes, compared in constant
//! time against the fingerprint learned from the pairing QR code. No system
//! trust evaluation is consulted: the certificate is self-signed and
//! rotation is a non-goal.
//!
//! The phone runs `mobile/modules/steno-link/ios/PinnedTrustEvaluator.swift`
//! (symlinked into the Swift tests); this is the same rule as a rustls
//! [`ServerCertVerifier`], for every Rust client of the listener: the tests,
//! and a future `steno` CLI probe.

use std::sync::Arc;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, verify_tls12_signature, verify_tls13_signature};
use rustls::{ClientConfig, DigitallySignedStruct, Error, SignatureScheme};
use rustls_pki_types::{CertificateDer, ServerName, UnixTime};

use crate::identity::HandoverIdentity;

/// Trusts exactly one leaf: the one whose DER hashes to the pinned
/// fingerprint. Intermediates, the server name and the clock play no part.
#[derive(Debug)]
pub struct PinnedVerifier {
    fingerprint: Vec<u8>,
    provider: Arc<CryptoProvider>,
}

impl PinnedVerifier {
    /// `fingerprint` is the 32-byte SHA-256 from the pairing; any other
    /// length trusts nothing, as on the phone.
    #[must_use]
    pub fn new(fingerprint: &[u8]) -> Self {
        PinnedVerifier {
            fingerprint: fingerprint.to_vec(),
            provider: Arc::new(rustls::crypto::ring::default_provider()),
        }
    }

    /// True iff `leaf_der` hashes to the pinned fingerprint.
    #[must_use]
    pub fn evaluate(&self, leaf_der: &[u8]) -> bool {
        self.fingerprint.len() == 32
            && constant_time_equals(
                &HandoverIdentity::fingerprint_of_der(leaf_der),
                &self.fingerprint,
            )
    }
}

impl ServerCertVerifier for PinnedVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, Error> {
        if self.evaluate(end_entity.as_ref()) {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(Error::General(
                "the server's certificate is not the paired computer's".to_owned(),
            ))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// A rustls client configuration that speaks TLS 1.3 to the listener and
/// trusts only the pinned leaf.
pub fn pinned_client_config(fingerprint: &[u8]) -> Result<Arc<ClientConfig>, Error> {
    let verifier = PinnedVerifier::new(fingerprint);
    let config = ClientConfig::builder_with_provider(verifier.provider.clone())
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_no_client_auth();
    Ok(Arc::new(config))
}

/// Equal length and equal bytes, without an early exit on the first
/// difference.
#[must_use]
pub fn constant_time_equals(lhs: &[u8], rhs: &[u8]) -> bool {
    if lhs.len() != rhs.len() {
        return false;
    }
    let difference = lhs
        .iter()
        .zip(rhs)
        .fold(0u8, |acc, (left, right)| acc | (left ^ right));
    difference == 0
}
