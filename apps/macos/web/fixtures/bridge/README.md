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
