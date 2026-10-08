//! The phone handover server: the computer's half of the wire the iOS
//! recorder speaks. Swift: `Sources/StenoHandover`. Plan:
//! `.plans/2026-10-02-rust-core-and-tauri-shell.md` (WP7c).
//!
//! The phone finds the computer over Bonjour (`_steno._tcp`), pins the
//! SHA-256 of its self-signed leaf certificate from the pairing QR code,
//! pairs once with a single-use secret, and then uploads each recording in
//! chunks over TLS 1.3 with a bearer token. Audio arrives here and goes no
//! further than the inbox and the [`HandoverIntake`](steno_core::HandoverIntake)
//! that admits it into the pipeline; nothing in this crate opens an outbound
//! connection.
//!
//! - [`HandoverService`]: the entry point, what the host and the CLI hold.
//!   Pairing, the device list, revocation, the listener and the receipt
//!   stream. Nothing else constructs the listener. The host and the CLI
//!   run [`HandoverService::checkpoint_store`] before they build it
//!   ([`StoreNotSynced`] when it fails).
//! - [`HandoverConfiguration`]: how the listener binds, where partial
//!   uploads live, the pairing window and the read timeout; [`Clock`] is
//!   the one time source.
//! - [`HandoverIdentity`]: the TLS identity, minted once and kept in the
//!   [`SecretStore`](steno_core::SecretStore).
//! - [`PairingPayload`]: what the QR code shows; [`base64url`] is its
//!   encoding of the fingerprint and the secret.
//! - [`pinning`]: the trust rule the phone applies, in Rust, for a client
//!   that talks to the listener (the tests).
//! - [`server`]: the TLS listener, the per-connection HTTP/1.1 handling
//!   with the body limits and the read timeout, Bonjour; [`ServerMetrics`]
//!   is what the tests read.
//! - [`upload`]: the inbox on disk and the limits on what a phone may
//!   announce.
//! - [`wire`], [`route`], [`engine`]: the protocol core, independent of the
//!   listener, driven directly by the tests. The host reads wire values only
//!   through [`HandoverReceipt`](steno_core::HandoverReceipt) and
//!   [`PairingPayload`].
//!
//! The contract with the phone is `mobile/modules/steno-link/src/wire.ts`;
//! `tests/wire_contract.rs` reads it and holds every name here against it.

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
pub use identity::{HandoverIdentity, IdentityError};
pub use pairing::{PairingPayload, PairingPayloadError};
pub use server::ServerMetrics;
pub use service::{HandoverService, ListenerState, StoreNotSynced};
