# Test fixtures

Shared by every test target through `Fixtures.url(_:)` in
`Sources/StenoCore/Testing/`. Folder names are lowercase because APFS is
case-insensitive by default.

| Folder | Contents |
|---|---|
| `audio/` | Generated 16 kHz mono Int16 WAV files, each under ten seconds, listed in `MANIFEST.sha256` |
| `speech/` | `say` output committed once (German `Anna`, English `Samantha`, 16 kHz mono Int16, under ten seconds) with a `.ref.txt` per file and its own `MANIFEST.sha256`, checked by `SpeechFixtureTests`; never regenerated because `say` output changes with macOS releases |
| `transcripts/` | `[RawSegment]` and `[TranscriptSegment]` samples with invented text |
| `templates/` | The bundled summary templates as `StenoJSON`, one golden per template |
| `exports/` | The `MeetingExport` golden that is `meeting.json` |
| `meetings/` | The synthetic `MeetingExport` every adapter golden is rendered from (`StenoAdaptersTests/Support/FixtureMeeting.swift` is its source) |
| `llm/` | StenoLLM: `transcripts/` (`MeetingExport` values of `LLMFixtures`, compared as values), `text/` (token estimate input), `prompts/` (golden prompts), `responses/` (canned server bodies) |
| `handover/` | `test-identity.p12` and `.der`: a test-only P-256 TLS identity (`CN=Steno test identity`, password in `Tests/StenoHandoverTests/Support/TestIdentity.swift`), generated once with openssl; loaded only by the test targets, never by a product module |
| `snapshots/schema/` | `sqlite_master` dump per migration version |
| `snapshots/summary/` | `SummaryMarkdown.render` output for the sample export |
| `snapshots/e2e/` | The files `StenoEndToEndTests` finds in its temp vault; each workstream that replaces a fake updates it in the same PR |
| `snapshots/obsidian/` | One golden per adapter renderer output and variant; `VERSION` pins `ArtifactRenderer.version` to a SHA-256 over the goldens, so a golden change without a version bump fails `RendererVersionTests` |
| `snapshots/` (other) | Golden outputs of later workstreams' renderers |

No real meeting audio, ever. Audio is generated: `steno dev fixtures generate
--out Tests/Fixtures` runs `FixtureGenerator` (seeded SplitMix64 noise, integer
phase accumulators, Int16 WAV), which is byte-identical on every machine.
`FixtureManifestTests` regenerates every case into a temporary directory and
compares the hashes with `MANIFEST.sha256` and the bytes with the committed
files; running the tests with `STENO_UPDATE_SNAPSHOTS=1` rewrites the files
and the manifest instead. A changed fixture or manifest in a PR is a reviewed
change that names the case it adds or alters.

Goldens compared through `Snapshot.assert` are rewritten by the same switch. A
mismatch writes the actual bytes beside the golden as `<name>.actual`
(ignored by git, uploaded by CI) so a run on another machine can be inspected.
