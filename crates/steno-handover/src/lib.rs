//! The phone handover server: the computer's half of the wire the iOS
//! recorder speaks. The phone finds the computer over Bonjour, pins the
//! SHA-256 of its self-signed certificate from the pairing QR code, pairs
//! once with a single-use secret, then uploads each recording in chunks over
//! TLS 1.3 with a bearer token.
//!
//! The listener serves only the LAN addresses and loopback. Audio goes no
//! further than the inbox and the [`HandoverIntake`](steno_core::HandoverIntake);
//! nothing in this crate opens an outbound connection.
//!
//! [`HandoverService`] is the entry point. The contract with the phone is
//! `mobile/modules/steno-link/src/wire.ts`, held against this crate by
//! `tests/wire_contract.rs`.

#![deny(unsafe_code)]

pub mod base64url;
pub mod configuration;
pub mod engine;
pub mod identity;
pub mod pairing;
pub mod pinning;
pub mod route;
pub mod server;
pub mod service;
pub mod upload;
pub mod wire;

pub use configuration::{Clock, HandoverConfiguration};
pub use identity::{FingerprintRecord, HandoverIdentity, IdentityError, Unavailability};
pub use pairing::{PairingPayload, PairingPayloadError};
pub use server::ServerMetrics;
pub use service::{HandoverService, ListenerState, StoreNotSynced};
