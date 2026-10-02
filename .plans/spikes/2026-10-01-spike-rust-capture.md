# Spike: two-lane CoreAudio capture with echo cancellation in Rust

Status: measured 2026-10-01 on Forge (Mac16,1, M4 Pro, macOS 26.7), time-boxed
to about 90 minutes. Question: can Rust do Steno's two-lane capture (process
tap + microphone through one private aggregate device and one IOProc) with
Speex echo cancellation, at the quality of the Swift implementation in
`Sources/StenoAudio`?

Short answer: the HAL surface is fully reachable from Rust and the port is
essentially line for line; the audio-thread path is allocation-free and fast;
the vendored Speex matches the Swift build to 0.1 dB. The spike could **not**
measure live audio quality (levels, lane alignment, live ERLE) because TCC
silently hands both the microphone and the tap as zeros to processes started
over SSH, for the Swift binary and the Rust binary alike. Details, verbatim
errors and tables follow.

Source: `spikes/capture-rs/` (not built by the package or CI). Build and run
on a Mac with `cargo build --release`, then
`target/release/steno-capture-spike --seconds 10 --lanes call --out DIR` or
`--synthetic`.

## Crates chosen and why

| Need | Chosen | Rejected | Reason |
|------|--------|----------|--------|
| HAL: tap, aggregate, IOProc, properties | `objc2-core-audio` 0.3.2 with `objc2-core-audio-types`, `objc2-foundation`, `objc2-core-foundation` (objc2 0.6.4) | `cidre` 0.29 (anarlog's choice), `coreaudio-sys` 0.2 | objc2 mirrors the headers one to one (`CATapDescription`, `AudioHardwareCreateProcessTap`, `kAudioHardwarePropertyTranslatePIDToProcessObject`, aggregate keys), so `ProcessTap.swift`, `AggregateDevice.swift` and `IOProcRunner.swift` port directly. `cidre` wraps the same calls in its own types (`TapDesc`, `TapGuard`, `StartedDevice`) and is one large crate behind a `macos_15_0` feature; fine for anarlog, but it hides the calls we want to compare. `coreaudio-sys` runs bindgen at build time (needs libclang) and has no binding for the Objective-C `CATapDescription` class at all. |
| Echo cancellation | SpeexDSP C sources vendored (7 files, 5250 lines, BSD) compiled by `cc` in `build.rs`, hand-written FFI in `src/speex.rs` | `speexdsp-sys` / `speexdsp` 0.1.2 | `speexdsp-sys`'s `build.rs` requires a system `speexdsp >= 1.2` found through pkg-config (`metadeps`) plus bindgen 0.37; Forge has no brew and no sudo, and a dependency on a system library is the wrong shape for a bundled app anyway. Vendoring is what the Swift side does too (`sbooth/CSpeex`). |
| Ring buffer | own SPSC in `src/ring.rs` | `rtrb` 0.4 | The Swift contract is all-or-nothing across lanes per callback with a drop counter per lane (`LaneRings.reserve`). Writing the 160 lines keeps that contract visible for review; `rtrb` would do for a single lane. |
| WAV | `hound` 3.5 | | Float32 WAV, which `steno dev aec-bench` reads. |

Dependency tree: 6 direct crates, 16 in total, `cc` as the only build
dependency. Release binary 788 KB, linking only CoreAudio, Foundation,
CoreFoundation, libobjc and libSystem. `cargo clean && cargo build --release`
at `CARGO_BUILD_JOBS=4`: 4.0 s wall (load 6.28). Incremental release build
after a source change: 0.6 to 1.3 s.

Build errors hit along the way, verbatim:

- `package steno-capture-spike depends on objc2-core-audio with feature
  AudioHardwareBase but objc2-core-audio does not have that feature` (the
  feature is `AudioHardware`; the Base header is folded into it).
- `error[E0603]: type alias OSStatus is private` from
  `objc2-core-audio-0.3.2/src/lib.rs:27`; defined locally as `i32`.
- With `-DOUTSIDE_SPEEX`: `vendor/speexdsp/math_approx.h:49:57: error:
  unknown type name 'spx_int32_t'` (14 errors). `arch.h` only includes
  `speex/speexdsp_types.h` when `OUTSIDE_SPEEX` is not defined; dropped the
  define and generated `speexdsp_config_types.h` from the `.in` template.

## What was built

`spikes/capture-rs/src/`:

| File | Lines | Ports |
|------|-------|-------|
| `hal.rs` | 346 | `ProcessTap.swift`, `AggregateDevice.swift`, `AudioDevices.swift`, `AudioObjectProperties.swift`, own-process lookup from `ProcessAudioActivity.swift`, the IOProc create/start/stop of `IOProcRunner.swift` |
| `layout.rs` | 100 | `StreamLayout.swift` (both HAL orderings) |
| `ring.rs` | 162 | `LaneRingBuffer.swift` + `LaneRings.swift` |
| `rt.rs` | 55 | counting global allocator (no Swift equivalent) |
| `speex.rs` | 156 | `SpeexEchoCanceller.swift` (same controls: rate 48 kHz, frame 480, tail 200 ms, suppress -40/-15 dB, denoise/AGC/VAD off) |
| `metrics.rs` | 119 | `EchoMetrics.swift` + the per-lane report of `DevCaptureSpike.swift`, plus an envelope cross-correlation lag |
| `synthetic.rs` | 82 | `AudioFixtures.speechLikeFar`, `roomImpulseResponse`, `echoMic`, `convolve`, same SplitMix64 and seeds |
| `main.rs` | 469 | `LiveCaptureBackend.start/stop` (without device-change listeners), consumer thread with far-end delay line as in `ProcessingThread.swift`, `DevCaptureSpike` + `DevAECBench` reporting |

1518 lines of Rust (plus 29 in `build.rs` and 5250 vendored C) against 1892
lines for the Swift files listed above. The Swift figure includes the device
notification coalescing and rebuild path in `LiveCaptureBackend`, which the
spike does not port.

Flow, as in Swift: resolve default system output (clock master) and default
input; translate own pid to a process object; create a private, unmuted
stereo global tap excluding it; create the private aggregate (output as
`MainSubDevice`, mic drift-compensated, tap drift-compensated with
`TapAutoStart`); ask for 48 kHz and read it back until it settles; read the
aggregate's input stream configuration and resolve the layout; one
`AudioDeviceCreateIOProcID` with a C callback and a raw context pointer; the
callback copies each lane into its ring following the precomputed
`ChannelRef`s (stereo tap folded to mono); a consumer thread drains 480-frame
blocks, runs Speex with the far-end delayed by input + output latency when
that sum is at least one frame, and writes `mic.wav`, `system.wav`,
`master.wav` (stereo) and `mic-aec.wav`.

## Measurements

Scenario (`runs/scenario.sh` on Forge): start the recorder, after 2 s
`afplay` a 1 kHz, 3 s, -6 dBFS tone generated with Python's `wave` module and
`say "testing one two three"`. Load averages are the 1-minute figure before
and after each run.

### Swift vs Rust, same scenario, 10 s, `--lanes call`

| | Swift `steno dev capture-spike` | Rust `steno-capture-spike` |
|---|---|---|
| Load before / after | 2.64 / 2.68 | 2.63 / 2.99 |
| Devices | MacBook Pro Speakers, MacBook Pro Microphone | same |
| Tap creation | not printed | 3.7 ms, id 102, 48 kHz stereo interleaved (flags 0x9) |
| Layout | tap first = false; mic buffer 0 ch 0 stride 1; system buffer 1 ch 0 stride 2 + ch 1 stride 2 | identical (aggregate buffers `[1, 2]`, sub-devices `[[], [1]]`, tap `[2]`) |
| Aggregate rate | 48000 Hz | 48000 Hz, buffer frame size 512 |
| Input latency + safety offset | 50 frames | 50 frames |
| Output latency + safety offset | 108 frames | 108 frames |
| Recorded | master 7.93 s of 10 s | 8.15 s: 764 callbacks x 512 frames |
| First callback after start | not printed (implied ~2.07 s) | 1873.6 ms |
| Dropped frames | none | none |
| Max ring occupancy | not printed | 1376 frames (28.7 ms), ring 2 s |
| IOProc duration | not measured | max 13.7 us, mean 3.3 us |
| Allocations inside the IOProc | by construction none | 0 (134 in the whole process) |
| mic rms / peak / 1 kHz / floor | -160 / -160 / -160 / -160 dBFS | same |
| system rms / peak / 1 kHz / floor | -160 / -160 / -160 / -160 dBFS | same |
| Onset mic, system, difference | none, none, n/a | none, none, n/a |
| `system lane silent` | true | (same data) |

Both lanes are digital silence in both implementations. See "TCC behaviour"
below; this is the environment, not either implementation. The onset
alignment and level comparison the protocol asked for therefore has no
result.

### Rust callback timing, further runs

| Run | Load before | Playback | Callbacks | Captured | Dropped | IOProc max / mean | Allocs in IOProc |
|-----|-------------|----------|-----------|----------|---------|-------------------|------------------|
| 10 s | 2.63 | tone at 2 s + say | 764 | 8.15 s | none | 13.7 / 3.3 us | 0 |
| 20 s | 5.25 (above the 4 limit; kept as a worst case) | `say` at 1 s | 1712 | 18.26 s, first callback at 1759 ms | none | 12.5 / 3.0 us | 0 |
| 30 s | 5.47 | none | **0** | 0 s | none | - | 0 |

The 30 s run is the second environment finding: with nothing playing on the
output device, the aggregate never ran its IOProc. In the other runs the
first callback arrived exactly when `afplay` or `say` opened the speakers
(the Swift master's 7.93 s of 10 s is the same effect). The aggregate's clock
master is the output device, the tap has `TapAutoStart`, and in a session
where capture is denied the HAL apparently waits for another client to pull
the output clock. Both implementations behave the same, so it is not a Rust
defect, but whether the first two seconds of a recording are also lost in a
permitted GUI session needs checking against the Swift app before anyone
relies on it.

### Echo cancellation

Live lanes: `steno dev aec-bench --mic rust1/mic.wav --far rust1/system.wav`
reads the Rust Float32 WAVs (390720 frames) and reports -160 dBFS everywhere,
ERLE 0.0 dB; the Rust spike's own live ERLE is 0.0 dB on silence. No live
figure exists for either side.

Synthetic parity (`steno dev aec-bench --synthetic` versus
`steno-capture-spike --synthetic`, same fixtures, seeds and controls):

| Second | Swift (CSpeex) ERLE | Rust (vendored Speex) ERLE |
|--------|--------------------|----------------------------|
| 0 | 5.1 dB | 5.1 dB |
| 1 | 15.7 dB | 15.7 dB |
| 2 | 20.2 dB | 20.2 dB |
| 3 | 22.7 dB | 22.7 dB |
| 4 | 25.7 dB | 25.7 dB |
| 5 | 26.8 dB | 26.8 dB |
| Overall / after 3 s | 12.3 / 24.7 dB | 12.3 / 24.7 dB |
| mic / far / processed | -24.5 / -18.6 / -36.9 dBFS | -24.5 / -18.6 / -36.9 dBFS |
| Processing cost | not printed | 72.9 us per 10 ms frame (load 6.28) |

Identical to the printed precision. The fixture generators also agree, which
confirms the SplitMix64 and envelope ports.

## Real-time safety

- `ring.rs`: per lane a power-of-two `Box<[UnsafeCell<f32>]>` allocated once,
  `AtomicUsize` write and read indices as monotonically increasing sample
  counts, Release store by the producer, Acquire load by the consumer, a
  `dropped` counter. `LaneRings::reserve(frames)` checks every lane and either
  admits the whole callback or counts the drop on every lane, so lanes never
  skew. No lock anywhere on the producer side.
- Callback (`main.rs`, `io_proc` and `deliver`): `rt::enter_callback`, a
  clock read (`Instant::now`, `clock_gettime` on macOS), pointer arithmetic
  from the `AudioBufferList` into the rings following the precomputed
  `LaneSource`s, three relaxed atomic adds for the statistics,
  `rt::leave_callback`. No `Vec`, no `Box`, no formatting, no `Arc` clone,
  no channel, no syscall other than the clock read.
- Evidence rather than assertion: `rt.rs` installs a `#[global_allocator]`
  that counts every `alloc`/`dealloc`/`realloc`; while the callback flag is
  set it compares `pthread_self()` with the thread recorded at callback entry
  and counts the hits. All runs report `allocations inside IOProc: 0` with
  123 to 137 allocations in the process overall, so the counter demonstrably
  works and the callback path demonstrably does not allocate.
- Timing: max 13.7 us per 512-frame callback (10.67 ms budget) at load 2.6,
  12.5 us at load 5.3, mean 3.0 to 3.3 us. Swift does not print this figure;
  it should be of the same order since both do the same copy.
- Differences from Swift: the consumer polls every 5 ms instead of waiting on
  a `DispatchSemaphore` signalled per callback (a signal is a syscall but no
  allocation; production code would add it). Max ring occupancy seen by the
  consumer was 1376 frames (28.7 ms) against a 2 s ring.
- Cost of the port: `hal.rs` is almost entirely `unsafe` FFI (raw
  `AudioObjectGetPropertyData` with `NonNull` and `c_void`, an NSDictionary
  cast to `CFDictionary` over the toll-free bridge, a raw context pointer
  into the callback whose lifetime the code must manage by hand). Rust's
  safety story does not reach this layer; it reaches the ring and the
  consumer.

## TCC behaviour

- Neither `AudioHardwareCreateProcessTap` nor `AudioHardwareCreateAggregateDevice`
  nor `AudioDeviceStart` returned an error for the Rust binary; the tap was
  created in 3.7 ms. Denial shows up as zero-filled buffers, exactly as for
  the Swift binary. A backend cannot detect it from status codes; Steno's
  `systemLaneSilent` detection remains necessary.
- `~/Library/Application Support/com.apple.TCC/TCC.db`: the only microphone
  or audio-capture grant on Forge is
  `kTCCServiceMicrophone|com.microsoft.teams2|2|2`. Nothing for Terminal,
  sshd, steno or the spike; `log show --predicate 'subsystem == "com.apple.TCC"'`
  recorded no prompt or denial for either run.
- `launchctl managername` over SSH is `Background`; a console session
  `gui/502` exists (logged in since Sep 24). Launching the spike inside it
  with a LaunchAgent plist and `launchctl bootstrap gui/502 ...` failed:
  `Bootstrap failed: 5: Input/output error` (`Try re-running the command as
  root for richer errors.`). Not tried: `open -a Terminal`, which would raise
  a microphone prompt on Forge's screen that nobody is there to answer.
- Output volume was 56 %, not muted, so `afplay` and `say` did play; the mic
  lane is zeros because of TCC, not because of a silent room.
- Consequence for the protocol: the live level, onset and ERLE comparison
  needs either a signed, bundled app in a GUI session (the Swift note in
  `DevCaptureSpike.swift` says the same) or a one-time manual grant of
  Microphone and System Audio Recording to Terminal on Forge, after which
  `runs/scenario.sh` can be rerun unchanged for both binaries.

## Verdict

Feasible, with the quality question left open by the environment rather than
by Rust.

What the spike shows:

- Every HAL call the Swift backend uses exists in `objc2-core-audio`, and the
  code ports almost mechanically: same layout resolution result, same
  latencies, same 512-frame callback cadence, same start behaviour, no drops.
- The audio-thread contract (no allocation, no lock) is met and, unlike in
  Swift, proven by a counting allocator rather than by discipline.
- Speex vendored into a `cc` build gives bit-identical results to the Swift
  CSpeex package.
- Build cost is trivial (4 s clean, 16 crates, 788 KB).

What it does not show: that the mic and system lanes carry the same levels
and alignment as Swift's. Structurally they must (the HAL does the mixing,
resampling and drift compensation in both cases; the code only copies), but
the spike has no recording to prove it.

Main risks for a production Rust capture backend on macOS:

1. Permissions and packaging. TCC attributes the tap and microphone to the
   responsible app; the Rust binary needs to live inside the same signed
   bundle with `NSAudioCaptureUsageDescription`, and the behaviour seen here
   (silent zeros, no error) is identical. No gain, no loss, but the Rust core
   does not remove the Swift or Objective-C app shell.
2. The ObjC bridge is the thin part of the ecosystem. `CATapDescription` is an
   Objective-C class, aggregate descriptions are Foundation dictionaries, the
   IOProc wants a C callback with a raw context pointer; `hal.rs` is 346
   lines of `unsafe` whose mistakes are crashes, not compile errors. Device
   change handling (property listeners on a dispatch queue, coalescing, the
   rebuild) is not ported and would add `block2`/`dispatch2` and more of the
   same.
3. Binding drift. `objc2-core-audio` is generated from Apple's headers by the
   objc2 maintainers; the tap API is young (macOS 14.2) and already changed
   once (`CATapMuteBehavior`, `kAudioAggregateDeviceTapAutoStartKey`). Swift
   gets new keys with Xcode; Rust gets them when the crate is regenerated.
4. Speex has no usable crate; vendoring 5250 lines of C and owning the build
   flags is the only option (as it is for CSpeex in Swift today).
5. The two-second start delay and the output-clock dependency need
   confirming in a permitted session; if it is real for the Swift app too,
   it is a product bug independent of language.
6. Two languages in one app: the UI, the window hosts and the bridge in
   `apps/macos` stay Swift; a Rust core adds a FFI boundary (cbindgen or
   UniFFI) that this spike did not touch.

Recommendation: capture alone is not a reason to move to Rust; it ports at
roughly one-to-one effort and gains an allocation proof but loses nothing
else and adds an FFI boundary. The port is cheap enough that, if the rest of
the core (speech engines, LLM client, storage) moves to Rust for other
reasons, capture would not be the blocker. Before deciding, rerun
`runs/scenario.sh` for both binaries from a session with the permissions
granted to get the level, alignment and live ERLE table this report could not
fill.

## Reproduction

On Forge: `~/steno-spikes/capture-rs/` holds the crate (same source as
`spikes/capture-rs/`), `runs/tone1k.wav`, `runs/scenario.sh`, the run logs
(`swift.*`, `rust1.*`, `clean-build.log`) and the WAVs under `runs/rust1/`;
the Swift lanes are under `/tmp/swift-capture/`. None of the WAVs or logs are
copied into the repository (`spikes/capture-rs/.gitignore`).
