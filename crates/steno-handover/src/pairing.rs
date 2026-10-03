//! The pairing QR payload, the open pairing window and the bearer tokens.
//! Swift: `Pairing/PairingPayload.swift`, `Pairing/PairingSession.swift`,
//! `Pairing/DeviceTokens.swift`.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use chrono::{DateTime, TimeZone as _, Utc};
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use rand::RngCore as _;
use sha2::{Digest as _, Sha256};
use thiserror::Error;
use uuid::Uuid;

use crate::base64url;
use crate::configuration::Clock;
use crate::pinning::constant_time_equals;

/// What the QR code carries, and the deep link it doubles as:
///
/// ```text
/// steno://pair/v1?mac=<uuid>&name=<pct>&fp=<base64url>&secret=<base64url>&exp=<unix>
/// ```
///
/// `fp` is the SHA-256 of the computer's leaf certificate DER, `secret` the
/// single-use pairing secret, both 32 bytes in base64url without padding
/// (the one place the wire uses base64url; JSON and headers use standard
/// base64). The phone's `pairing-payload.ts` parses exactly this.
#[derive(Clone, PartialEq, Eq)]
pub struct PairingPayload {
    pub mac_id: Uuid,
    pub mac_name: String,
    pub fingerprint: Vec<u8>,
    pub secret: Vec<u8>,
    /// Whole seconds; the phone refuses a payload past this on its own
    /// clock.
    pub expires_at: DateTime<Utc>,
}

/// The secret pairs a phone; it stays out of every log line.
impl std::fmt::Debug for PairingPayload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PairingPayload")
            .field("mac_id", &self.mac_id)
            .field("mac_name", &self.mac_name)
            .field("fingerprint", &base64url::encode(&self.fingerprint))
            .field("secret", &"[redacted]")
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

impl PairingPayload {
    pub const SCHEME: &'static str = "steno";
    pub const VERSION: &'static str = "v1";

    /// `expires_at` is rounded down to the second, as the URL carries it.
    #[must_use]
    pub fn new(
        mac_id: Uuid,
        mac_name: impl Into<String>,
        fingerprint: Vec<u8>,
        secret: Vec<u8>,
        expires_at: DateTime<Utc>,
    ) -> Self {
        PairingPayload {
            mac_id,
            mac_name: mac_name.into(),
            fingerprint,
            secret,
            expires_at: Utc
                .timestamp_opt(expires_at.timestamp(), 0)
                .single()
                .unwrap_or(expires_at),
        }
    }

    /// The QR code's text.
    #[must_use]
    pub fn url_string(&self) -> String {
        let name = utf8_percent_encode(&self.mac_name, PERCENT_ENCODED).to_string();
        format!(
            "{}://pair/{}?mac={}&name={name}&fp={}&secret={}&exp={}",
            Self::SCHEME,
            Self::VERSION,
            self.mac_id.hyphenated(),
            base64url::encode(&self.fingerprint),
            base64url::encode(&self.secret),
            self.expires_at.timestamp()
        )
    }

    /// Parses what [`PairingPayload::url_string`] wrote, with the phone
    /// parser's failures.
    pub fn parse(url: &str) -> Result<Self, PairingPayloadError> {
        let prefix = format!("{}://pair/", Self::SCHEME);
        let rest = url
            .get(..prefix.len())
            .filter(|head| head.eq_ignore_ascii_case(&prefix))
            .map(|_| &url[prefix.len()..])
            .ok_or(PairingPayloadError::NotSteno)?;
        let (version, query) = rest.split_once('?').unwrap_or((rest, ""));
        if version != Self::VERSION {
            return Err(PairingPayloadError::Version);
        }
        let mut fields: Vec<(String, String)> = Vec::new();
        for pair in query.split('&').filter(|pair| !pair.is_empty()) {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            let key = percent_decode(key)?;
            if fields.iter().any(|(name, _)| *name == key) {
                return Err(PairingPayloadError::BadEncoding(format!("duplicate {key}")));
            }
            fields.push((key, percent_decode(value)?));
        }
        let field = |name: &str| {
            fields
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.as_str())
        };
        let (Some(mac), Some(name), Some(fp), Some(secret), Some(exp)) = (
            field("mac"),
            field("name"),
            field("fp"),
            field("secret"),
            field("exp"),
        ) else {
            return Err(PairingPayloadError::MissingField);
        };
        let mac_id =
            parse_uuid(mac).ok_or_else(|| PairingPayloadError::BadEncoding("mac".into()))?;
        if name.is_empty() {
            return Err(PairingPayloadError::BadEncoding("name".into()));
        }
        let fingerprint = base64url::decode(fp)
            .filter(|bytes| bytes.len() == 32)
            .ok_or_else(|| PairingPayloadError::BadEncoding("fp".into()))?;
        let secret_bytes = base64url::decode(secret)
            .filter(|bytes| bytes.len() == 32)
            .ok_or_else(|| PairingPayloadError::BadEncoding("secret".into()))?;
        let seconds = Some(exp)
            .filter(|exp| {
                !exp.is_empty() && exp.len() <= 12 && exp.bytes().all(|b| b.is_ascii_digit())
            })
            .and_then(|exp| exp.parse::<i64>().ok())
            .ok_or_else(|| PairingPayloadError::BadEncoding("exp".into()))?;
        let expires_at = Utc
            .timestamp_opt(seconds, 0)
            .single()
            .ok_or_else(|| PairingPayloadError::BadEncoding("exp".into()))?;
        Ok(PairingPayload::new(
            mac_id,
            name,
            fingerprint,
            secret_bytes,
            expires_at,
        ))
    }
}

/// A hyphenated UUID (8-4-4-4-12, either case), the one form Swift's
/// `UUID(uuidString:)` accepts.
pub(crate) fn parse_uuid(text: &str) -> Option<Uuid> {
    if text.len() != 36 {
        return None;
    }
    Uuid::try_parse(text).ok()
}

/// Everything but RFC 3986's unreserved characters is percent-encoded,
/// `+` included, so the phone's `decodeURIComponent` reads it back.
const PERCENT_ENCODED: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

fn percent_decode(text: &str) -> Result<String, PairingPayloadError> {
    percent_encoding::percent_decode_str(text)
        .decode_utf8()
        .map(std::borrow::Cow::into_owned)
        .map_err(|_| PairingPayloadError::BadEncoding("percent encoding".into()))
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PairingPayloadError {
    #[error("not a steno://pair URL")]
    NotSteno,
    #[error("unsupported pairing payload version")]
    Version,
    #[error("a pairing field is missing")]
    MissingField,
    #[error("pairing field {0} is malformed")]
    BadEncoding(String),
}

/// One open pairing window: the secret in the QR code, expiring on the
/// injected wall clock. Single use because the engine drops the session
/// before it saves the paired device; `begin_pairing` replaces any open
/// session.
pub(crate) struct PairingSession {
    pub payload: PairingPayload,
    now: Clock,
}

impl PairingSession {
    pub fn open(
        mac_id: Uuid,
        mac_name: &str,
        fingerprint: &[u8],
        window: std::time::Duration,
        now: Clock,
    ) -> Self {
        let expires_at =
            now() + chrono::Duration::from_std(window).unwrap_or(chrono::Duration::MAX);
        PairingSession {
            payload: PairingPayload::new(
                mac_id,
                mac_name,
                fingerprint.to_vec(),
                DeviceTokens::random_bytes().to_vec(),
                expires_at,
            ),
            now,
        }
    }

    pub fn is_open(&self) -> bool {
        (self.now)() < self.payload.expires_at
    }

    /// Checks a presented credential without leaking timing: both sides are
    /// hashed and the digests compared in constant time. The phone sends the
    /// secret as standard base64 (`wire.ts` converts the QR's base64url);
    /// both encodings are accepted.
    pub fn matches(&self, presented: &str) -> bool {
        if !self.is_open() {
            return false;
        }
        let Some(bytes) = STANDARD
            .decode(presented)
            .ok()
            .or_else(|| base64url::decode(presented))
        else {
            return false;
        };
        constant_time_equals(
            &Sha256::digest(&bytes),
            &Sha256::digest(&self.payload.secret),
        )
    }
}

/// Bearer tokens: 32 random bytes as standard base64. The store keeps only
/// `hash(token)`, so a copy of the database pairs no phone.
pub struct DeviceTokens;

impl DeviceTokens {
    pub const BYTE_COUNT: usize = 32;

    #[must_use]
    pub fn mint() -> String {
        STANDARD.encode(Self::random_bytes())
    }

    #[must_use]
    pub fn hash(token: &str) -> Vec<u8> {
        Sha256::digest(token.as_bytes()).to_vec()
    }

    /// 32 bytes from the system generator, for tokens and pairing secrets.
    #[must_use]
    pub fn random_bytes() -> [u8; Self::BYTE_COUNT] {
        let mut bytes = [0u8; Self::BYTE_COUNT];
        rand::rng().fill_bytes(&mut bytes);
        bytes
    }
}
