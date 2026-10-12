# Bridge fixtures

Recorded bridge messages, one JSON file each, listed in `index.json`:

- `<topic>.json`: the snapshot the host emits for that topic, for example
  `meetings.list.json`.
- `<method>.reply.json`: the reply to a command that has one, for example
  `speakers.options.reply.json`.
- `params.<name>.json`, `envelope.*.json`, `reply.<name>.json`: the wire
  shapes of command parameters, message envelopes and generic replies.

The Swift side writes these files. `BridgeContractTests` records every
message type here under `STENO_RECORD_FIXTURES=1` and, in a normal run,
asserts that the existing files still decode. The web tests and the
Playwright screens read the same files through the mock transport
(`src/bridge/mock-transport.ts`: topics become snapshots, `*.reply.json`
become command replies). A drift between the two contracts fails both sides.
Do not edit these files by hand, and do not format them: Biome skips them.

Two fixtures have no Swift sample: `onboarding.import.json`, the Tauri
app's import step, which the Swift app never shows, and
`meeting.detail.keyWithheld.json`, a meeting whose summary was skipped while
that import withheld the API key, which the Swift app never skips. They were
written in the recorder's format, so they are the exception to the rule
above: edit them by hand, keeping that format.
`crates/steno-host/tests/parity.rs` (`onboarding_import`,
`meeting_detail_key_withheld`) proves the host produces them, and
`crates/steno-bridge/tests/fixtures.rs` pins their bytes. They are listed
last in `index.json`, and `BridgeFixturesTests.rustOnly` keeps them there
when the Swift side records.
