//! Steno's core: the domain types, the SQLite store that shares its file
//! with the Swift app, and the settings. Plan:
//! `.plans/2026-10-02-rust-core-and-tauri-shell.md`.
//!
//! The Swift package (`Sources/StenoCore`) stays the source of truth until
//! cutover. Every encoding here mirrors it: UUIDs as uppercase text, dates in
//! GRDB's `yyyy-MM-dd HH:mm:ss.SSS` UTC form in columns and ISO 8601 in JSON,
//! JSON columns in the `StenoJSON` convention (sorted keys, no escaped
//! slashes), embeddings as little-endian `f32` blobs.

pub mod json;
pub mod model;
pub mod paths;
pub mod store;
pub mod string_enum;

pub use model::*;
pub use paths::StenoPaths;
pub use store::{Store, StoreError};
pub use string_enum::UnknownCase;
