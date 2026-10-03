//! The JSON contract between the host and the web UI: topics, methods,
//! snapshots, params and the envelope. Swift (`Sources/StenoBridge`) is the
//! source of truth until cutover; the fixtures in
//! `apps/macos/web/fixtures/bridge/` are the oracle both sides encode to, and
//! `tests/fixtures.rs` proves every one of them decodes here and re-encodes
//! byte for byte. Plan: `.plans/2026-10-02-rust-core-and-tauri-shell.md`.
//!
//! Type mapping from Swift: `Int`/`Int64` are `i64`, `Double` is `f64`,
//! `Date` is `chrono::DateTime<Utc>` written as `2026-09-29T12:48:00.000Z`,
//! `UUID` is `uuid::Uuid` written upper case, a nil optional is an omitted
//! key, `String` enums are `steno_core::string_enum!` enums with the same
//! raw values. The date and UUID codecs are `steno_core::json`'s; this
//! crate adds only the pretty printer the fixtures are written with
//! ([`json`]).
//!
//! Naming: a top-level Swift type keeps its name without the `Bridge` prefix
//! (`BridgeMeetingSource` is [`MeetingSource`]). A type nested in a snapshot,
//! `Outer.Inner`, becomes `<Topic><Inner>`, where the topic word is the
//! snapshot's short name, one per topic: `App`, `Recording`, `Progress`,
//! `List` (`meetings.list`), `Detail` (`meeting.detail`), `General`,
//! `Recording` again for `settings.recording` ([`RecordingLevel`] belongs to
//! the live topic, [`RecordingDevice`] to the settings one), `Transcription`,
//! `Summaries`, `Export`, `Phone` (`settings.iphone`), `Onboarding`. A doubly
//! nested `Outer.Mid.Inner` becomes `<Topic><Mid><Inner>`
//! (`PhoneSettingsSnapshot.Listener.State` is [`PhoneListenerState`]). The
//! exception is a nested name Swift declares as a `typealias` of a
//! `StenoCore` type: it keeps the core name and no topic word, so
//! [`MeetingState`], [`RetentionMode`], [`SpeakerAssignmentKind`] and
//! [`TaskPriority`]; the last two are `steno_core`'s own enums re-exported.
//!
//! To add a method: the variant and raw value in [`BridgeMethod`]
//! (`envelope.rs`); its params or reply type in `commands.rs`; one line in
//! `bridge_host!` (`dispatcher.rs`), which yields the host method and the
//! route, and without which the crate does not compile; the fixture,
//! `index.json` and the `FIXTURES` row in `tests/fixtures.rs`; `contract.ts`
//! on the web side, which that test compares. To add a topic: the variant in
//! [`BridgeTopic`]; the snapshot type with its `impl Snapshot` in
//! `snapshots.rs` or `settings.rs`; the fixture and the `FIXTURES` row
//! (`every_topic_has_a_snapshot_fixture` fails until the fixture exists).
//!
//! Errors: [`BridgeError`] is the contract's error and nothing more; its
//! `From<steno_core::StoreError>` impl (`envelope.rs`) puts the store's
//! errors on the contract's codes once, so a host returns them with `?`
//! rather than mapping them by hand.
//!
//! Every public item is re-exported at the root; the modules are the table
//! of contents.

pub mod commands;
pub mod dispatcher;
pub mod envelope;
pub mod json;
pub mod settings;
pub mod snapshots;

pub use commands::*;
pub use dispatcher::*;
pub use envelope::*;
pub use settings::*;
pub use snapshots::*;
