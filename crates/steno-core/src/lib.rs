//! Steno's core: the domain types, the SQLite store that shares its file
//! with the Swift app, and the settings. Plan:
//! `.plans/2026-10-02-rust-core-and-tauri-shell.md`.
//!
//! - [`model`]: the domain types, one module per Swift file in
//!   `Sources/StenoCore/Model`.
//! - [`store`]: the SQLite store, its migrations and the column encodings.
//! - [`json`]: the `StenoJSON` convention and the date and UUID codecs.
//! - [`string_enum`](mod@string_enum): the macro every Swift `String` enum is spelled with.
//! - [`paths`]: where the database lives on each platform.
//!
//! Two rules hold the crate together. It depends on nothing else of ours
//! (every other crate depends on it), so the pipeline, the CLI and the
//! shell share one definition of each type. And the database file is
//! shared with the Swift app until cutover, so every encoding mirrors it:
//! UUIDs as uppercase text, dates in GRDB's `yyyy-MM-dd HH:mm:ss.SSS` UTC
//! form in columns and ISO 8601 in JSON, JSON columns in the `StenoJSON`
//! convention (sorted keys, no escaped slashes), embeddings as
//! little-endian `f32` blobs. A schema change is one PR touching both
//! sides; `migrations/README.md` has the procedure.

pub mod json;
pub mod model;
pub mod paths;
pub mod store;
pub mod string_enum;

pub use model::*;
pub use paths::StenoPaths;
pub use store::{Store, StoreError};
pub use string_enum::UnknownCase;
