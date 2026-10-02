# Cross-platform spikes: can Rust and Tauri carry Steno to Linux and Windows?

Status: spikes A to C measured 2026-10-01; position below. This file owns the question,
the method, the shared baseline and the results of spikes A to C. The follow-up plan
`.plans/2026-10-01-cross-platform-speech-stack.md` owns the decisions, the gates, the work
packages and spikes D to F. Each spike has its own report in `.plans/spikes/`. Nothing here
changes the scope plan (`.plans/2026-09-24-initial-scope.md`), which still lists Windows and
Linux as v1 non-goals; the web UI plan (`.plans/2026-09-29-macos-webview-ui.md`) still rules
out Tauri for the Mac app.

## Question

The Mac windows are a bundled React app since the web UI plan
(`.plans/2026-09-29-macos-webview-ui.md`). That made the UI portable but left the
product's core Apple-only: CoreAudio process taps for capture (`Sources/StenoAudio`,
4.9k lines), CoreML speech and diarization via FluidAudio and WhisperKit
(`Sources/StenoSpeech`, 2.3k lines) and the Swift shell (`apps/macos/Steno`, 11.4k
lines). `StenoCore`, `StenoBridge`, `StenoLLM`, `StenoAdapters` and most of
`StenoHandover` (about 18.6k lines) already build on Linux.

Candidate end states discussed:

1. Swift daemon speaking the bridge protocol over a socket, Tauri shell on Linux and
   Windows, the Swift shell kept on the Mac. Keeps the Swift code, adds a second
   language and toolchain for good.
2. Rust core and Tauri shell on every platform, CoreML speech on the Mac, ONNX speech
   elsewhere, the web UI unchanged. One stack, one build, but a rewrite.

Three unknowns gate option 2. Spikes A to C answer them with numbers; spikes D to F
(speech-stack plan) follow up on what B found.

## Spikes

| Spike | Question | Report |
|---|---|---|
| A | Can Rust do the two-lane tap + mic capture with Speex echo cancellation on macOS, allocation-free on the audio thread, at the Swift spike's quality? | `.plans/spikes/2026-10-01-spike-rust-capture.md` |
| B | How do Parakeet TDT v3 and pyannote through sherpa-onnx on CPU (Apple Silicon and x86 Linux) compare with the CoreML pipeline in agreement and speed? | `.plans/spikes/2026-10-01-spike-onnx-speech.md` |
| C | Can a Rust process drive FluidAudio's CoreML Parakeet models and reproduce the Swift transcript at the Swift speed? | `.plans/spikes/2026-10-01-spike-coreml-rust.md` |
| D, E, F | Chunker and language voting, own ONNX export, absolute WER on FLEURS German | `.plans/2026-10-01-cross-platform-speech-stack.md` (work packages) |

Spike code lives under `spikes/` (`capture-rs`, `coreml-rs`, `onnx-speech`) and is not
part of any product target.

## Shared method

- Host: Forge (MacBook Pro Mac16,1, M4 Pro, 10 cores, 24 GB, macOS 26.7, Xcode 27,
  Rust 1.99). Linux proxy for spike B: atlas (AMD Ryzen 7 7700, 8 cores, Linux x86_64).
- Corpus: seven 10-minute clips of the remote-party lane of real calls (16 kHz mono
  Int16), five German 1:1 calls and two team calls. Third-party content, kept on Forge
  and never committed.
- Baseline: `steno dev bakeoff` at commit `756c2cc` with the production models on an
  idle machine.

| Engine (CoreML, Swift) | Wall s per 10-min file | RTFx | Mean RTFx |
|---|---:|---:|---:|
| parakeet-v3 | 1.9 to 5.5 | 110 to 316 | 233 |
| whisperkit-large-v3-turbo | 20 to 46 | 13 to 29 | 21.5 |

RTFx is audio seconds divided by wall seconds; higher is faster. Agreement between
engines is measured as word error rate of one transcript against the other after
lowercasing, stripping punctuation and collapsing whitespace. For speech the CoreML
Parakeet transcript is the reference, since the ONNX model has the same weights.

## Results

### Spike A: Rust capture on macOS

Feasible, and the port is close to line for line. `objc2-core-audio` exposes every
HAL call `Sources/StenoAudio` uses (tap description, aggregate device, IOProc,
property reads); 1,518 lines of Rust reproduce the Swift backend's layout resolution,
48 kHz aggregate, 512-frame callbacks and 50 and 108 frame latencies exactly. The
audio-thread path has no lock and no allocation, proven by a counting global
allocator rather than by discipline; the IOProc took at most 13.7 µs of a 10.67 ms
budget. SpeexDSP vendored through `cc` gives echo-return-loss figures identical to
the Swift CSpeex build on the synthetic fixtures. Clean build 4 s, 16 crates, 788 KB.

Not shown: live levels and lane alignment. TCC hands zero-filled buffers to processes
started over SSH, for the Swift binary and the Rust binary alike, with no error
status. That is the test rig, not Rust. Two environment findings worth a check in the
Swift app: denial is silent (the `systemLaneSilent` detection stays necessary), and
the aggregate's IOProc did not start until another client opened the output device.

Risks: `hal.rs` is 346 lines of `unsafe` FFI whose mistakes are crashes; the tap API
is young and the generated bindings lag Xcode; there is no usable Speex crate; the
device-change rebuild path is not ported.

### Spike B: ONNX on CPU through sherpa-onnx

Transcription works, diarization does not, and neither is close to the Mac.

| Measure | CoreML Parakeet (Swift) | ONNX Parakeet int8, M4 Pro CPU, 4 threads | WhisperKit turbo (Swift) |
|---|---:|---:|---:|
| Wall s per 10-minute file | 1.9 to 5.5 | 43 to 56 | 20 to 46 |
| Mean RTFx | 233 | 12 to 13 | 21.5 |
| WER against CoreML Parakeet text | reference | 17.9 % (3.4 to 41 %) | 21.3 % |
| Peak RSS | | 2.5 to 3.0 GB | |

Same weights do not give the same text: int8 kernels, per-chunk language detection
that flips German to English, and no voice activity detection. The exported encoder
aborts the process on long audio (a C++ exception crosses the FFI; spike B read the cap
as about 200 s, spike E measured it at 400 s), so production needs VAD-driven
segmentation. The ONNX Runtime CoreML provider was eight times slower than CPU at 13 GB
RSS, so ONNX offers no acceleration on the Mac. Linux numbers from atlas were taken at
load 30 to 90 and are upper bounds only (RTFx 2.6 to 7.8); an idle laptop measurement is
still missing. Spikes D to F took the agreement and segmentation findings further; the
speech-stack plan has the outcome.

The stock sherpa-onnx diarizer (pyannote segmentation 3.0, ERes2Net embeddings,
agglomerative clustering) returned 3 to 16 speakers for single-speaker calls at every
threshold, where FluidAudio returns exactly one. Reproducing FluidAudio's quality
means exporting the community-1 embedding, porting the clustering and Steno's
refinement pass, and recalibrating on the corpus: weeks, not a spike.

Packaging risks: 640 MB of models, dynamic libraries without an rpath in the
published crate, bindgen needing libclang on every build host, CC-BY attribution for
Parakeet, gated pyannote weights.

### Spike C: FluidAudio's CoreML models from Rust

Yes. `objc2-core-ml` loads the app's own `.mlmodelc` bundles; the encoder runs from
Rust at the same steady-state latency as from Swift (25.5 ms per 15 s window on all
compute units, identical output length). A 1,061-line port of the pipeline
(chunking, preprocessor, encoder, TDT greedy decode, overlap merge, word timings)
transcribes the corpus at mean RTFx 211 single-threaded against Swift's 233 with four
workers, and word timings agree to 0.01 s where the text agrees.

WER against the Swift transcript is 8.7 % overall (2.7 to 15.1 % per file). Every
difference traced is a FluidAudio heuristic around the decoder that the time box did
not port: silence-aligned window starts, seam-gap repair, the empty-window retry gate
and inverse text normalisation. Estimated cost of parity: about 1,500 more lines and
two to three weeks, plus a parity harness against FluidAudio output, because
FluidAudio changes these heuristics in most releases.

## Decision

Rust is not the obstacle. Capture ports mechanically with provable real-time safety
(spike A), and the Mac speech path keeps its models and its Neural Engine speed from
Rust at a bounded, measured cost (spike C). If Steno were being started today for three
platforms, Rust and Tauri would be the right stack.

The obstacle is what Linux and Windows would get, and that is the same in either
architecture (Swift daemon or Rust core): the non-Apple speech stack is the gating work,
and the rewrite only pays for itself once that work is worth doing. Spike B put the gap
at WhisperKit-class speed and WhisperKit-class disagreement with the Mac transcript,
near 3 GB of memory, and no usable speaker labels. The speech-stack plan took that gap
on the same evening; its state is the current position on quality:

| Question from spike B | State (see the speech-stack plan) |
|---|---|
| Transcript quality on CPU | Settled. Gate G1 passed: our own fp32 export scores 5.3 % WER on FLEURS German long files against 5.5 % for CoreML Parakeet |
| Speed on an idle Linux laptop | Open (gate G2); every Linux number so far is from a loaded desktop |
| Diarization | Open (gate G3); needs the embedding export, our clustering and a calibration run |
| GPU | Open (gate G4); no suitable machine in the fleet |

Position:

1. No Rust rewrite now. Nothing in the spikes makes the Swift app worse, and the
   rewrite's benefit is entirely conditional on shipping other platforms. If G2 and G3
   pass and there are real users on Linux or Windows, the next plan is the Rust core and
   Tauri shell, with CoreML speech on the Mac through the spike C route and a parity
   harness against FluidAudio output as its first deliverable.
2. The spike crates under `spikes/` are evidence, not product code; no target depends
   on them.
3. Independently of platforms, move the view models and window bridges out of
   `apps/macos/Steno` into the Swift package behind `BridgeHost`. It makes the host
   logic testable on Linux today and keeps every option open.
4. Follow up in the Swift app on the two capture environment findings from spike A:
   silent TCC denial and the IOProc waiting for an output client.
