# Test fixtures

Shared by every test target through `Fixtures.url(_:)` in
`Sources/StenoCore/Testing/`. Folder names are lowercase because APFS is
case-insensitive by default.

| Folder | Contents |
|---|---|
| `audio/` | Generated 16 kHz mono Int16 WAV files, each under ten seconds, listed in `MANIFEST.sha256`; and the compressed files of the Rust decoder's phone path, committed once and not in the manifest (listed below the table) |
| `speech/` | `say` output committed once (German `Anna`, English `Samantha`, plus `Daniel` in `two-speakers-mf`, 16 kHz mono Int16, under ten seconds) with a `.ref.txt` per file and its own `MANIFEST.sha256`, checked by `SpeechFixtureTests`; never regenerated because `say` output changes with macOS releases |
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
| `snapshots/platforms/` | The Rust folder note's goldens for a call recorded on Windows (`windows/`) and Linux (`linux/`): copies of `obsidian/`'s goldens of the same name with "Windows call" or "Linux call" where the info line says "Mac call". Read by the Rust tests only. They sit outside `obsidian/`, so `RendererVersionTests` does not hash them. A change to `obsidian/folder-note-plain-utc.md` or `obsidian/folder-note-wikilink-berlin.md` is copied here by hand; the Rust renderer tests fail until it is |
| `snapshots/macos/` | The macOS app's four detail tabs (Summary, Transcript, Tasks, Scratchpad) as text lines for the sample meeting, from `TabText` in `apps/macos`; checked by `TabTextSnapshotTests` in the app's hostless test bundle |
| `snapshots/` (other) | Golden outputs of later workstreams' renderers |

The compressed audio fixtures, read by the Rust tests through
`CARGO_MANIFEST_DIR/../../Tests/Fixtures`, not `Fixtures.url`. The first five
come from ffmpeg 8.1.1 (libavformat 62.12.101), which reproduces them byte for
byte; the sixth is the first with three packets overwritten. The encoder
priming the Rust tests trim (1 024 samples, from the edit list) depends on
the encoder, so another version is a reviewed change.

- `tone-440-44k1-500ms.m4a` and `.mp3`: half a second of a 440 Hz sine at 0.5,
  mono 44.1 kHz, AAC-LC and MP3 at 96 kbps:
  `ffmpeg -f lavfi -i "aevalsrc=0.5*sin(2*PI*440*t):s=44100:d=0.5" -c:a aac -b:a 96k tone-440-44k1-500ms.m4a`,
  and the same with `-c:a libmp3lame` for `.mp3`.
- `tone-440-44k1-onset-200ms.m4a`: the same sine after 0.2 s of silence (from
  sample 8 820), for the AAC priming trim:
  `ffmpeg -f lavfi -i "aevalsrc='if(gte(n,8820),0.5*sin(2*PI*440*(n-8820)/44100),0)':s=44100:d=0.5" -c:a aac -b:a 96k tone-440-44k1-onset-200ms.m4a`.
- `tone-440-1000-44k1-stereo-onset.m4a`: the same with a second channel, a
  1 kHz sine from sample 11 025:
  `ffmpeg -f lavfi -i "aevalsrc='if(gte(n,8820),0.5*sin(2*PI*440*(n-8820)/44100),0)|if(gte(n,11025),0.5*sin(2*PI*1000*(n-11025)/44100),0)':s=44100:d=0.5" -c:a aac -b:a 96k tone-440-1000-44k1-stereo-onset.m4a`.
- `tone-440-44k1-500ms-mp3.mp4`: the MP3 in an MP4 container (an edit list of
  1 105 samples): `ffmpeg -i tone-440-44k1-500ms.mp3 -c:a copy -f mp4 tone-440-44k1-500ms-mp3.mp4`.
- `tone-440-44k1-500ms-damaged.m4a`: `tone-440-44k1-500ms.m4a` with the
  payload of three of its 23 AAC packets overwritten, for the decoder's
  silence in place of a packet it cannot decode: packet 9 (bytes 2 593 to
  2 905) with zeros, packet 10 (2 906 to 3 170) with `0xAA` and packet 15
  (4 269 to 4 521) with `0x55`, which symphonia rejects as invalid data, a
  program config element and a coupling channel element. The offsets are
  the `mdat` payload's start (44) plus the `stsz` sizes before each packet:
  `python3 -c "b=bytearray(open('tone-440-44k1-500ms.m4a','rb').read()); b[2593:2906]=bytes(313); b[2906:3171]=b'\xaa'*265; b[4269:4522]=b'\x55'*253; open('tone-440-44k1-500ms-damaged.m4a','wb').write(b)"`.
- `silence-44k1-avaudiorecorder.m4a`: half a second from `AVAudioRecorder`
  (`record(forDuration: 0.5)` over SSH, where the microphone gives zeros).
- `tone-440-44k1-onset-200ms-apple.m4a`: the onset fixture's PCM written
  through `AVAudioFile` in 4 096-frame pieces.

The last two come from Apple's AAC encoder on macOS 26 with the phone's
recorder settings (AAC, 44.1 kHz mono, 64 kbps, quality 96) and are not
reproducible byte for byte. Both are cut to the recorder's layout: `ftyp`,
`moov` without `udta`, `mdat`. The `udta` (the gapless tag) and the 56 KB
`free` box the writer reserves are dropped, and the chunk offsets moved to
match. `AVAudioFile` reads 20 416 frames from the first and 22 464 from the
second, with the onset at sample 8 823; the Rust test asserts the same.

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
