# Steno v1: audio capture (`Sources/StenoAudio`)

Status: implementation plan, written 2026-09-25, reconciled the same day.
Binding context: [`2026-09-25-v1-program.md`](2026-09-25-v1-program.md)
(`EchoCanceller`, `AudioDecoding`, `AudioAsset`, `AudioLane`) and
[`2026-09-24-initial-scope.md`](2026-09-24-initial-scope.md). Owns
`Sources/StenoAudio`, `Tests/StenoAudioTests` and the `audio-devices`,
`capture-spike`, `record` and `aec-bench` CLI commands.

API names marked "(unverified)" were seen only in blog posts or recalled
from memory, not in the AudioCap source, Apple documentation, or the
anarlog crate read for this plan. Verify against the macOS 15 SDK headers
in step 2 before relying on them.

## Goal

Capture a meeting on the Mac as two time-aligned lanes at 48 kHz, "me" from
the microphone and "them" from a Core Audio process tap of everything the
Mac plays except Steno itself, cancel the loudspeaker echo out of the mic
lane in software, write a crash-tolerant master file plus the 16 kHz mono
sidecars the pipeline consumes, implement the program's `AudioDecoding`
(decode any recording to 16 kHz lanes, mix down to AAC for export), tell the
app when another process opens the microphone, and settle the Continuity
phone-call question with a CLI spike.
Everything real-time runs without allocation or locks; everything else is
an actor.

## Non-goals

- Live transcription, level-triggered auto-stop, or any playback.
- ScreenCaptureKit capture (needs Screen Recording permission, misses
  `callservicesd`/`avconferenced`, adds a menu-bar indicator).
- Apple VoiceProcessingIO (silences the tap lane per Sauron PR #4).
- Per-application taps or an app picker. v1 taps the global mix.
- Recording the phone through the iPhone; that is the handover workstream.
- Windows, Intel, macOS 14.
- Encoding to AAC at capture time. The `audio.m4a` mixdown is produced after
  processing by `AVFoundationAudioCodec.mixdown` (pipeline step 8).

## Decisions

| Topic | Decision |
|---|---|
| Tap | `CATapDescription(stereoGlobalTapButExcludeProcesses:)` (unverified spelling; AudioCap uses the `stereoMixdownOfProcesses:` sibling) with Steno's own process object, `muteBehavior = .unmuted`, private. Global taps sit at the HAL and hear daemons such as Continuity calls. |
| Aggregate | One private aggregate: default output device as `kAudioAggregateDeviceMainSubDeviceKey` (clock master), tap in `kAudioAggregateDeviceTapListKey` with drift compensation, and the selected input device as a second entry in `kAudioAggregateDeviceSubDeviceListKey`. One IOProc then delivers both lanes in one callback, already sample-aligned by the HAL. Fallback if spike S2 fails: two IOProcs aligned by `AudioTimeStamp.mHostTime`. |
| Mic lane | Core Audio IOProc, not AVAudioEngine: AVAudioEngine cannot be retargeted to the aggregate (DGR Labs, `-10877`), its tap block is not real-time, and one callback for both lanes removes the alignment problem. Change requested below. |
| Rates | The aggregate runs at 48 kHz (`kAudioDevicePropertyNominalSampleRate`, unverified for aggregates). Master file 48 kHz Float32 CAF, sidecars 16 kHz Int16 WAV. |
| AEC | SpeexDSP MDF via the `CSpeex` SwiftPM package (sbooth/CSpeex, unverified that it exports the echo API), run at 48 kHz on the processing thread, 10 ms frames (480), 200 ms tail (9600). Far-end is the system lane of the same callback. WebRTC AEC3 and DTLN-aec are bake-off candidates only (spike S4). |
| Threads | IOProc (real-time) copies into two lock-free rings. A dedicated processing thread drains 10 ms frames, runs AEC, metering and downmix. A serial writer queue does file I/O. Actors only outside these three. |
| Permission | No status API. Onboarding runs the full tap pipeline for 500 ms and treats "prompt accepted and buffers non-zero while `afplay` plays a tone" as authorised. AudioCap's private `TCCAccessPreflight`/`TCCAccessRequest` on `kTCCServiceAudioCapture` is compiled only under `STENO_TCC_SPI` for debugging, never shipped. |
| Meeting detection | Listener on `kAudioDevicePropertyDeviceIsRunningSomewhere` for input devices, then attribution by enumerating `kAudioHardwarePropertyProcessObjectList` and reading `kAudioProcessPropertyIsRunningInput` (unverified; listeners on it reportedly never fire) and `kAudioProcessPropertyBundleID`. Poll every 2 s as a safety net. |
| Lanes | Call mode: `[.mic, .system]`. In-person: `[.mixed]`, one lane, no tap, no AEC. `AudioLane` is StenoCore's enum, shared with `TranscriptSegment.lane`. |
| Decode and mixdown | `AVFoundationAudioCodec: AudioDecoding`. `decode` returns `sidecars16k` when present, otherwise reads CAF, m4a or WAV through `AVAudioFile` + `AVAudioConverter` to 16 kHz mono per lane (channel n = lane n of the master). `mixdown` sums lanes to mono and writes AAC 64 kbps `.m4a` via `AVAssetWriter`; `.m4aAAC` inputs are copied. |

## Public API

```swift
public enum CaptureMode: Sendable, Equatable { case call, inPerson }

public struct CaptureConfiguration: Sendable, Equatable {
    public var mode: CaptureMode
    public var inputDeviceUID: String?        // nil: default input device
    public var echoCancellation: Bool         // default true in .call, ignored in .inPerson
    public var keepRawMicLane: Bool           // debug: writes mic.raw.caf next to the master
    public var outputDirectory: URL           // per-meeting folder is created inside
    public var retention: AudioRetention
}

public enum CaptureState: Sendable, Equatable {
    case idle, starting
    case recording(startedAt: Date)
    case stopping
    case failed(CaptureError)
}

public struct LaneLevel: Sendable, Equatable { public var rms: Float; public var peak: Float }  // dBFS

public struct LaneLevels: Sendable, Equatable {   // emitted at 10 Hz
    public var mic: LaneLevel
    public var system: LaneLevel?                 // nil in .inPerson
}

public struct CaptureStatistics: Sendable, Equatable {
    public var duration: TimeInterval
    public var droppedFrames: [AudioLane: Int]   // ring overruns, should be zero
    public var systemLaneSilent: Bool          // true if the tap never exceeded -80 dBFS
    public var deviceChanges: Int
}

public actor CaptureSession {
    public init(configuration: CaptureConfiguration, echoCanceller: (any EchoCanceller)?) throws
    public var state: CaptureState { get }
    public var states: AsyncStream<CaptureState> { get }
    public var levels: AsyncStream<LaneLevels> { get }
    public func start(meetingID: UUID) async throws
    public func stop() async throws -> (asset: AudioAsset, statistics: CaptureStatistics)   // format .caf48kFloat32, sidecars16k filled
}

public struct AVFoundationAudioCodec: AudioDecoding {                // program protocol; StenoCore's WAVAudioDecoder is the fake
    public init()
    public func decode(_ asset: AudioAsset) async throws -> [AudioLane: AudioBuffer16k]
    public func mixdown(_ asset: AudioAsset, to url: URL) async throws
}

public enum SystemAudioPermission {
    public static func request() async -> Bool   // full tap pipeline + tone, see Permission
    public static func microphone() async -> Bool // AVCaptureDevice.requestAccess(for: .audio)
}

public actor MeetingDetector {
    public enum Event: Sendable, Equatable {
        case microphoneOpened(bundleID: String?, pid: pid_t)
        case microphoneReleased
    }
    public init(ignoringPIDs: Set<pid_t> = [ProcessInfo.processInfo.processIdentifier])
    public var events: AsyncStream<Event> { get }
    public func start() throws
    public func stop()
}

public final class SpeexEchoCanceller: EchoCanceller, @unchecked Sendable {
    public init(sampleRate: Double, frameSize: Int) throws          // tail = 200 ms
    public init(sampleRate: Double, frameSize: Int, tailLength: Int) throws
    public func process(nearEnd: UnsafeBufferPointer<Float>, farEnd: UnsafeBufferPointer<Float>,
                        out: UnsafeMutableBufferPointer<Float>)      // no allocation, no locks
    public func reset()
}

public final class PassthroughEchoCanceller: EchoCanceller { }       // tests and .inPerson

public final class RecordingWriter: @unchecked Sendable {            // owned by the writer queue
    public init(directory: URL, lanes: [AudioLane], sampleRate: Double) throws
    public func write(_ frames: LaneFrames) throws                    // 48 kHz Float32, one call per 10 ms
    public func finish() throws -> RecordingFiles                     // master + sidecars
}

public struct RecordingFiles: Sendable, Equatable {
    public var master: URL                    // recording.caf
    public var sidecars16k: [AudioLane: URL]  // mic.16k.wav, system.16k.wav or mixed.16k.wav
    public var rawMic: URL?
    public var duration: TimeInterval
}
```

`LaneFrames` is a non-Sendable value with preallocated `UnsafeMutableBufferPointer<Float>`
per lane and the host time of the first frame. `CaptureError` covers
`tapCreationFailed(OSStatus)`, `aggregateCreationFailed(OSStatus)`,
`inputDeviceUnavailable`, `systemAudioSilent`, `deviceLost`, `writerFailed(Error)`.

## Files

```
Package.swift                                   add StenoAudio target, CSpeex dependency, StenoAudioTests
Sources/StenoAudio/
  Capture/CaptureSession.swift                  actor: state machine, owns backend, worker, writer
  Capture/CaptureConfiguration.swift            configuration, state, error, statistics types
  Capture/ProcessTap.swift                      CATapDescription -> AudioHardwareCreateProcessTap, format read, destroy
  Capture/AggregateDevice.swift                 dictionary builder, AudioHardwareCreateAggregateDevice, sample rate, destroy
  Capture/IOProcRunner.swift                    AudioDeviceCreateIOProcIDWithBlock, buffer-list demux into rings, start/stop
  Capture/CaptureBackend.swift                  protocol over ProcessTap+Aggregate+IOProc so CaptureSession is testable
  Capture/AudioObjectProperties.swift           typed read/listen helpers in the AudioCap style
  Capture/AudioDevices.swift                    input/output enumeration, UID lookup, default-device listeners
  Capture/SystemAudioPermission.swift           throwaway pipeline probe; TCC SPI behind STENO_TCC_SPI
  RealTime/LaneRingBuffer.swift                 SPSC ring, Synchronization.Atomic indices, drop counter
  RealTime/ProcessingThread.swift               drains rings in 10 ms frames: AEC, metering, downmix, hands off to writer
  RealTime/LaneAligner.swift                    host-time alignment for the two-IOProc fallback
  RealTime/LevelMeter.swift                     RMS/peak per lane, dBFS
  RealTime/Resampler48kTo16k.swift              AVAudioConverter wrapper with preallocated buffers
  AEC/SpeexEchoCanceller.swift                  CSpeex bridge, Float<->Int16 scratch, residual suppression
  AEC/PassthroughEchoCanceller.swift            copies near-end to out
  AEC/EchoMetrics.swift                         ERLE and RMS helpers shared by tests and the CLI bench
  Detection/MeetingDetector.swift               actor, debounce, event stream
  Detection/ProcessAudioActivity.swift          process object list, IsRunningInput, BundleID, PID
  Writer/RecordingWriter.swift                  ExtAudioFile CAF master, WAV sidecars, optional raw mic
  Writer/RecordingLayout.swift                  file names inside the meeting folder
  Codec/AVFoundationAudioCodec.swift            AudioDecoding: decode to 16 kHz lanes, AAC mixdown
Sources/steno/Commands/
  AudioDevicesCommand.swift                     `steno audio-devices`: devices, processes, running flags
  CaptureSpikeCommand.swift                     `steno capture-spike --seconds 10 --out DIR`: RMS per lane
  RecordCommand.swift                           `steno record --mode call|in-person --out DIR`
  AECBenchCommand.swift                         `steno aec-bench --mic --far --engine speex|passthrough`
  FixturesCommand+Audio.swift                   adds the 48 kHz cases below to core's `steno fixtures generate`
Tests/StenoAudioTests/
  LaneRingBufferTests.swift, LaneAlignerTests.swift, LevelMeterTests.swift,
  SpeexEchoCancellerTests.swift, RecordingWriterTests.swift, Resampler48kTo16kTests.swift,
  AVFoundationAudioCodecTests.swift, MeetingDetectorTests.swift, CaptureSessionTests.swift, TapIntegrationTests.swift
Tests/Fixtures/audio/                           shared folder owned by core; these cases added here
  tone-1k-48k-2s.wav, sweep-48k-3s.wav, speech-like-far-48k-6s.wav, echo-mic-48k-6s.wav, room-ir-48k.wav
```

## Steps

Each step is at most one day and ends with a reviewer-runnable check.
CI is the `macos-15` job; audio integration tests run only when
`STENO_AUDIO_TESTS=1` (program verification standard).

1. **Package wiring.** `StenoAudio` target depending on `StenoCore` and
   `CSpeex`, empty public types, `StenoAudioTests` with one test.
   Check: `swift build && swift test` green in CI; `CSpeex` links and
   `speex_echo_state_init` (unverified) resolves in a smoke test.
2. **Core Audio property layer.** `AudioObjectProperties`, `AudioDevices`,
   `ProcessAudioActivity`: read `kAudioHardwarePropertyDefaultSystemOutputDevice`,
   `kAudioHardwarePropertyDefaultInputDevice`, `kAudioDevicePropertyDeviceUID`,
   `kAudioHardwarePropertyTranslatePIDToProcessObject`, process list, bundle IDs.
   Verify every "(unverified)" name in this plan against the SDK headers and
   drop the marker. Check: `steno audio-devices` lists devices, the own
   process object ID, and processes with `IsRunning`/`IsRunningInput` flags.
3. **System lane end to end (spike S1).** `ProcessTap`, `AggregateDevice`
   (output device as main sub-device, tap as sub-tap, `TapAutoStart` true,
   private, not stacked), `IOProcRunner` with a non-nil serial queue,
   `LaneRingBuffer`. Teardown order: `AudioDeviceStop`,
   `AudioDeviceDestroyIOProcID`, `AudioHardwareDestroyAggregateDevice`,
   `AudioHardwareDestroyProcessTap`. Handle interleaved, non-interleaved and
   mono buffer lists. Check: `steno capture-spike --lanes system` while
   `afplay` loops `Tests/Fixtures/audio/tone-1k-48k-2s.wav` reports system RMS
   above -30 dBFS and a 1 kHz spectral peak; a second run after
   `tccutil reset SystemAudioCaptureRequests <bundle-id>` (unverified) prompts again.
4. **Mic lane in the same aggregate (spike S2).** Add the input device to
   `kAudioAggregateDeviceSubDeviceListKey`, demux the two input streams by
   stream index, force 48 kHz. Build the two-IOProc `LaneAligner` fallback the
   same day only if S2 is no-go. Check: `steno capture-spike` with the
   built-in mic and with AirPods reports both lanes non-silent, mic and
   system 1 kHz onsets within 5 ms of each other across a 60 s recording.
5. **Processing thread and session skeleton.** `ProcessingThread` (10 ms
   frames, `PassthroughEchoCanceller`, `LevelMeter`), `CaptureSession` state
   machine over a `CaptureBackend` protocol, `states` and `levels` streams.
   Check: `CaptureSessionTests` drive idle->starting->recording->stopping->idle
   with a fake backend; `steno record --seconds 5` prints levels at 10 Hz;
   Instruments Allocations shows zero allocations on the IOProc queue and
   the processing thread after the first second.
6. **Writer and sidecars.** `RecordingWriter` writes `recording.caf` via
   `ExtAudioFile` (Float32, 48 kHz, one channel per lane) and per-lane
   16 kHz Int16 WAV sidecars through `Resampler48kTo16k`; `finish()` returns
   `RecordingFiles`; `CaptureSession.stop()` builds the `AudioAsset`.
   Check: `RecordingWriterTests` write 2 s of distinct sines per lane and
   read them back sample-accurately; killing `steno record` with SIGKILL
   mid-recording leaves a `recording.caf` that `afinfo` reads with the
   correct duration to within one second.
7. **`AVFoundationAudioCodec`.** `decode` for `.caf48kFloat32` (per-channel
   lanes), `.m4aAAC` (one `.mixed` lane) and `.wav16kInt16`, preferring
   `sidecars16k`; `mixdown` to AAC mono. Check: `AVFoundationAudioCodecTests`
   decode a 2 s two-channel CAF fixture into two 32 000-sample lanes whose
   1 kHz peaks match the sidecar decode within 0.1 dB; a mixdown of the same
   file is an `.m4a` that `afinfo` reports as AAC mono with duration 2 s;
   `decode` of a phone-style `.m4a` fixture yields one `.mixed` lane.
8. **SpeexDSP canceller.** `SpeexEchoCanceller` with preallocated Int16
   scratch, `speex_preprocess` residual suppression, `reset()`.
   `steno aec-bench` prints ERLE and writes the processed file.
   Check: `SpeexEchoCancellerTests` on `echo-mic-48k-6s.wav` (far-end
   convolved with `room-ir-48k.wav` at 60 ms delay plus -20 dB noise) reach
   ERLE >= 20 dB after 3 s; with a near-end sweep added (double talk) the
   sweep's RMS in the output is within 3 dB of the input.
9. **AEC in the live path.** Feed the system lane frame as far-end for the
   mic lane frame of the same callback; when device latency
   (`kAudioDevicePropertyLatency` + `kAudioDevicePropertySafetyOffset`,
   unverified for aggregates) exceeds 100 ms, delay the far-end by that
   amount through a preallocated FIFO. `keepRawMicLane` writes `mic.raw.caf`.
   Check: on a Mac playing `speech-like-far-48k-6s.wav` through speakers with
   nobody talking, `steno aec-bench --mic mic.raw.caf --far recording.caf:1`
   reports ERLE >= 15 dB and the master's mic lane RMS is >= 15 dB below the
   raw mic lane.
10. **Meeting detection.** `MeetingDetector` with the IsRunningSomewhere
   listener, process attribution, 2 s debounce, 2 s poll fallback, own PID
   ignored. Check: `MeetingDetectorTests` with a fake activity source emit
   exactly one `microphoneOpened` for a flapping input and one
   `microphoneReleased` after release; manually, opening FaceTime audio
   emits `microphoneOpened(bundleID: "com.apple.FaceTime", ...)` within 3 s.
11. **In-person mode and device changes.** `.inPerson` builds an aggregate
    with the input device only, one `.mixed` lane, no AEC. Listen for default
    output/input changes and `kAudioDevicePropertyDeviceIsAlive`; on change,
    fail with `.deviceLost` and stop cleanly (rebuilding mid-meeting is v1.1).
    Check: unplugging the USB mic during `steno record` yields a readable file,
    state `.failed(.deviceLost)` and no crash; `steno record --mode in-person`
    produces `recording.caf` with one channel and `mixed.16k.wav`.
12. **Continuity spike (S3) and stress.** Run S3 below, record the outcome as
    an addendum to this file. Stress: 200 start/stop cycles, leaks and
    `AudioObjectID` counts flat. Check: addendum committed; `leaks` clean.

## Tests

- **Unit** (no devices, run in CI): ring buffer overrun accounting, aligner
  offsets from synthetic host times, level meter against known sines,
  Speex ERLE and double-talk on synthetic echo, writer round trip, resampler
  peak preservation, detector debounce with a fake source, session state
  machine with a fake backend. Fixtures generated by `steno fixtures`, all
  under ten seconds, no voices.
- **Integration** (`STENO_AUDIO_TESTS=1`, developer Mac): `TapIntegrationTests`
  starts the tap, plays `tone-1k-48k-2s.wav` with `afplay` (a separate
  process, so it is not excluded), asserts the 1 kHz peak in the system
  lane. A mic-lane integration test is feasible only with a virtual input
  device installed (BlackHole); it is gated on `STENO_VIRTUAL_INPUT_UID`
  and otherwise skipped, and it is not required for v1.
- **Manual** (one human, one Mac): a real video call on laptop speakers.
  Expect the system lane to contain only the remote party, the mic lane to
  contain the local voice with the remote party at least 15 dB down
  (measured with `aec-bench` from `mic.raw.caf`), and no audible artefacts
  on the local voice. Repeat with headphones: AEC must not damage the voice.

## Spikes

| Spike | Before step | Go | No-go consequence |
|---|---|---|---|
| S1 permission and silence | 4 | Prompt appears on first start of a signed build with `NSAudioCaptureUsageDescription` as a literal Info.plist key; after accepting, buffers are non-zero while `afplay` plays; after `tccutil reset` they are zero and the prompt returns. | Onboarding cannot detect denial; fall back to the TCC SPI in Developer ID builds and document it. |
| S2 mic in the tap aggregate | 5 | Both input streams appear in one buffer list; built-in mic and AirPods (HFP) deliver at 48 kHz; onsets aligned within 5 ms over 60 s. | Two IOProcs plus `LaneAligner`; AEC gets a 20 ms alignment jitter budget. |
| S3 Continuity call audible | 12 (result gates the phone story) | `steno capture-spike --seconds 10` during a Continuity call taken on the Mac: system lane RMS during remote speech >= -40 dBFS and >= 20 dB above the idle floor; playback intelligible. Test with the tone first so TCC denial is not mistaken for a no-go. | Phone source becomes "phone on speaker, room mic, `.mixed` lane"; `Meeting.source == .phone` keeps its meaning but capture uses `.inPerson`. Addendum to this plan plus a program log entry. |
| S4 AEC bake-off | after 9, time-boxed two days | Switch from Speex only if DTLN-aec on CoreML (models `model_128_{1,2}.onnx`, block 512, shift 128, 16 kHz, two stateful sessions per block) or WebRTC AEC3 beats Speex by >= 6 dB ERLE or clearly fewer double-talk artefacts on the manual-check recording, at < 15% of one core. | Speex stays. DTLN's fixed 16 kHz would also force the master's mic lane to be band-limited or raw; note in the addendum. |

## Real-time and privacy rules for this module

- IOProc: pointer arithmetic and `memcpy` into rings, one atomic store per
  lane, drop counter on overrun. No Swift arrays, closures, logging, actors.
- Processing thread: `Thread` with `qualityOfService = .userInteractive`,
  all buffers allocated in `init`, `EchoCanceller.process` called with
  exactly `frameSize` samples, never blocks on the writer (bounded queue,
  drop and count on overflow).
- Rings zeroed on stop so a restart never replays stale frames.
- No network access anywhere in `StenoAudio`. Files go only to
  `CaptureConfiguration.outputDirectory`.

## Needs from other workstreams

- **StenoCore**: `EchoCanceller`, `AudioDecoding`, `AudioAsset`
  (`AudioFormat`, `sidecars16k`, `mixdownURL`), `AudioLane`,
  `AudioRetention`; `Settings.audioFolder`, `defaultRetention`,
  `inputDeviceUID`; the `steno` root command and `fixtures generate`.
  Pipeline step 1 calls `AudioDecoding.decode(asset)` and step 8 calls
  `mixdown`, so no core change is needed for sidecars.
- **macOS app**: onboarding calls `SystemAudioPermission.request()` and
  `SystemAudioPermission.microphone()`; `Info.plist` contains
  `NSAudioCaptureUsageDescription` and `NSMicrophoneUsageDescription` as
  literal keys (Xcode ignores `INFOPLIST_KEY_NSAudioCaptureUsageDescription`);
  App Sandbox off, Hardened Runtime on, deployment target macOS 15;
  the prompt UI for `MeetingDetector.Event`; subscribes to `levels` for the
  menu bar indicator.
- **StenoSpeech**: nothing beyond `AudioBuffer16k` from the sidecars.

## Requested changes to the program document

Reconciled into the program document, see its log (entries 6 to 11).

## Deferred

- Rebuilding the aggregate mid-meeting after a device change (v1 fails with
  `.deviceLost` and stops cleanly).
- Per-application taps and an app picker; ScreenCaptureKit capture.
- A mic-lane integration test on CI (needs a virtual input device; the
  `STENO_VIRTUAL_INPUT_UID` gate stays optional).
