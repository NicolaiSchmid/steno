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
| `handover/` | `test-identity.p12` and `.der`: a test-only P-256 TLS identity (`CN=Steno test identity`, password in `Tests/StenoHandoverTests/Support/TestIdentity.swift`), generated once with openssl; `test-identity.key.der`, its private key as SEC1 DER (`openssl pkcs12 -nocerts -nodes \| openssl ec -outform DER`), which the Rust import's keychain test stores the way `IdentityKeychain.store` does (`IdentityFixtureTests` checks it is the certificate's key); `other-identity.p12` and `.der`, a second one (`CN=Steno other test identity`, same password, generated the same way), for a keychain that holds another identity (`IdentityGuardTests`); `third-identity.p12` and `.der`, a third (`CN=Steno third test identity`, same password, made with LibreSSL with the same legacy encryption), for two identities under the current label while `other-identity` is the staged or the minted one (`IdentityItemsTests`); loaded only by the test targets, never by a product module |
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

Three more from the same encoder with the same settings, for the salvage of
a phone recording whose recorder was killed, so it has no `moov`
(`crates/steno-audio/tests/codec_salvage.rs`). They keep the writer's
layout whole, the 57 KB reserve of zeros included (it compresses to a few
bytes), since that layout is what the salvage reads; each holds a few KB of
packets.

- `speech-44k1-avaudiofile.m4a`: 1.6 s (70 560 frames) of synthetic speech
  (`say -v Samantha "One, two, three, four."`, its first 1.2 s), four clicks
  and a 440 Hz tone over a little noise, written by `AVAudioFile` in
  4 096-frame pieces and closed: 71 packets.
- `speech-44k1-avaudiofile-killed.m4a`: the same written by a process that
  stops after 49 152 frames without closing the file and is then SIGKILLed:
  the `ftyp`, zeros to byte 57 344, and the finished file's first 48 packets
  byte for byte. Apple's encoder now and then encodes a run differently, so
  a killed file is kept only once it is the finished file's prefix.
- `silence-44k1-avaudiorecorder-killed.m4a`: `AVAudioRecorder` SIGKILLed 4 s
  into a recording over SSH, where the microphone gives zeros: the `ftyp`,
  zeros to byte 57 344 and 126 packets of silence, four bytes each.

The tools, built with `swiftc -O` on macOS 26 and run on Forge:

```swift
// rec.swift
// AVAudioRecorder with the phone's settings (mobile/src/features/recorder/recording-options.ts).
// rec <out.m4a> <seconds>: records, then stops cleanly. seconds 0: records until killed.
import AVFoundation
import Foundation
let url = URL(fileURLWithPath: CommandLine.arguments[1])
let seconds = Double(CommandLine.arguments[2])!
try? FileManager.default.removeItem(at: url)
let r = try AVAudioRecorder(url: url, settings: [AVFormatIDKey: kAudioFormatMPEG4AAC, AVSampleRateKey: 44_100,
    AVNumberOfChannelsKey: 1, AVEncoderBitRateKey: 64_000, AVEncoderAudioQualityKey: 96])
print("record", r.prepareToRecord(), r.record()); fflush(stdout)
if seconds > 0 {
    Thread.sleep(forTimeInterval: seconds)
    r.stop(); print("stopped"); fflush(stdout)
} else {
    while true { Thread.sleep(forTimeInterval: 1) }
}
```

```swift
// enc.swift
// Encodes <in>[,<in>...] (any file AVAudioFile reads, 44.1 kHz mono float) to <out.m4a> through AVAudioFile with the
// phone's settings. enc <in> <out.m4a> [hangAfterFrames]: with a frame count, stops writing there and sleeps
// without closing the file, so a SIGKILL leaves it as a killed writer would.
import AVFoundation
import Foundation
let inputs = try CommandLine.arguments[1].split(separator: ",").map { try AVAudioFile(forReading: URL(fileURLWithPath: String($0))) }
let out = URL(fileURLWithPath: CommandLine.arguments[2])
let hang = CommandLine.arguments.count > 3 ? AVAudioFramePosition(CommandLine.arguments[3])! : -1
try? FileManager.default.removeItem(at: out)
var file: AVAudioFile? = try AVAudioFile(forWriting: out, settings: [AVFormatIDKey: kAudioFormatMPEG4AAC,
    AVSampleRateKey: 44_100, AVNumberOfChannelsKey: 1, AVEncoderBitRateKey: 64_000, AVEncoderAudioQualityKey: 96],
    commonFormat: .pcmFormatFloat32, interleaved: false)
let buffer = AVAudioPCMBuffer(pcmFormat: inputs[0].processingFormat, frameCapacity: 4096)!
var written: AVAudioFramePosition = 0
for input in inputs {
while input.framePosition < input.length {
    try input.read(into: buffer)
    if hang >= 0 && written + AVAudioFramePosition(buffer.frameLength) > hang {
        print("hang", written); fflush(stdout)
        while true { Thread.sleep(forTimeInterval: 1) }
    }
    try file!.write(from: buffer)
    written += AVAudioFramePosition(buffer.frameLength)
}
}
file = nil
print("closed", written); fflush(stdout)
```

```swift
// fix.swift
// fix <speech.wav> <out.wav>: the fixture input, 44.1 kHz mono float: the speech (at most 1.2 s), then clicks
// (transients that force short windows), a 440 Hz tone with a noise bed, 1.6 s in all.
import AVFoundation
let rate = 44_100.0
let speech = try AVAudioFile(forReading: URL(fileURLWithPath: CommandLine.arguments[1]))
let sb = AVAudioPCMBuffer(pcmFormat: speech.processingFormat, frameCapacity: AVAudioFrameCount(speech.length))!
try speech.read(into: sb)
var s = Array(UnsafeBufferPointer(start: sb.floatChannelData![0], count: min(Int(sb.frameLength), Int(1.2 * rate))))
var seed: UInt64 = 7
func noise() -> Float { seed = seed &* 6364136223846793005 &+ 1442695040888963407; return Float(Int64(bitPattern: seed >> 11) % 2_000_000) / 1_000_000 - 1 }
for _ in 0..<4 { s.append(contentsOf: [0.9, -0.8, 0.6]); s.append(contentsOf: [Float](repeating: 0, count: Int(0.05 * rate))) }
while s.count < Int(1.6 * rate) { let t = Double(s.count) / rate; s.append(Float(0.3 * sin(2 * .pi * 440 * t)) + 0.02 * noise()) }
let format = AVAudioFormat(commonFormat: .pcmFormatFloat32, sampleRate: rate, channels: 1, interleaved: false)!
let out = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: AVAudioFrameCount(s.count))!
out.frameLength = AVAudioFrameCount(s.count)
s.withUnsafeBufferPointer { out.floatChannelData![0].update(from: $0.baseAddress!, count: s.count) }
let url = URL(fileURLWithPath: CommandLine.arguments[2]); try? FileManager.default.removeItem(at: url)
var file: AVAudioFile? = try AVAudioFile(forWriting: url, settings: format.settings, commonFormat: .pcmFormatFloat32, interleaved: false)
try file!.write(from: out); file = nil
print("frames", s.count)
```

```sh
# killrun.sh
#!/bin/zsh
# killrun.sh <seconds> <out.m4a> <command...>: starts the command in the background, SIGKILLs it (by the PID
# from $!) <seconds> after the start, or after 60 s without the output file growing, or never if it exits.
zmodload zsh/datetime
secs=$1; out=$2; shift 2
"$@" > $out.log 2>&1 &
pid=$!
start=$EPOCHREALTIME; last=-1; changed=$EPOCHREALTIME
while true; do
  sleep 0.05
  now=$EPOCHREALTIME
  if ! kill -0 $pid 2>/dev/null; then echo "exited by itself"; break; fi
  if (( now - start >= secs )); then kill -9 $pid; echo "killed $pid at $(( now - start )) s"; break; fi
  size=$(stat -f %z $out 2>/dev/null || echo 0)
  if (( size != last )); then last=$size; changed=$now; fi
  if (( now - changed >= 60 )); then kill -9 $pid; echo "no progress for 60 s, killed $pid"; break; fi
done
wait $pid 2>/dev/null
echo "size $(stat -f %z $out 2>/dev/null)"
```

The commands:

```sh
lockf -k ~/.steno-live-audio.lockfile ./killrun.sh 4 \
  silence-44k1-avaudiorecorder-killed.m4a ./rec silence-44k1-avaudiorecorder-killed.m4a 0
say -v Samantha -o short.aiff "One, two, three, four."
afconvert -f WAVE -d LEF32@44100 -c 1 short.aiff short.wav
./fix short.wav fixture.wav
./enc fixture.wav speech-44k1-avaudiofile.m4a
./killrun.sh 3 speech-44k1-avaudiofile-killed.m4a \
  ./enc fixture.wav speech-44k1-avaudiofile-killed.m4a 49152
```

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
