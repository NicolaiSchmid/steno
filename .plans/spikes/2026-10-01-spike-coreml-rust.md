# Spike: driving FluidAudio's Parakeet TDT v3 CoreML models from Rust

Date: 2026-10-01. Time box: 90 minutes. Status: complete, with precise gaps.

Question: can a Rust process load the exact `.mlmodelc` bundles the Swift app
uses today (FluidAudio 0.17.4, `parakeet-tdt-0.6b-v3`, encoder precision
`.int8`) and reproduce the Swift transcript at the Swift speed? This decides
whether a Rust rewrite could keep Neural Engine speech on the Mac while using
ONNX elsewhere.

Crate: `spikes/coreml-rs/` (source only; build on a Mac). Measurements on
Forge (Mac16,1, M4 Pro 10 cores, 24 GB, macOS 26.7, Xcode 27, Rust 1.99,
`CARGO_BUILD_JOBS=4`). Two other agents built Rust on Forge during the spike,
so the 1-minute load is recorded next to every timing.

## Short answer

Yes for the models, yes for the speed, "almost" for the transcript.

- The encoder runs from Rust at the same steady-state latency as from Swift:
  25.5 ms per 15 s window on `.all` compute units, both languages, identical
  `encoder_length`. The binding adds no measurable overhead.
- A 1,061-line Rust port of the pipeline (chunking, preprocessor, encoder,
  TDT greedy decode, merge, text and word timings) transcribes a 10-minute
  file in 2.4 to 3.5 s serially (RTFx 170 to 255) against FluidAudio's 1.9 to
  5.5 s with four parallel chunk workers (RTFx 110 to 316).
- Word timings match the Swift output to the hundredth of a second where the
  text matches. WER of the Rust text against the Swift text is 2.7 to 15.1 %
  per file, 8.7 % overall. Every residual difference traced in the time box is
  a FluidAudio heuristic around the decoder (silence-aligned window starts,
  seam-gap repair, the empty-window retry's confidence gate, inverse text
  normalisation), not the decoder itself.

## Binding: `objc2-core-ml` 0.3.2

Candidates: `objc2-core-ml` 0.3.2 with `objc2-foundation` 0.3.2 (madsmtm's
generated bindings), `cidre` 0.29 `ml` module (one maintainer, used by
fastrepl/anarlog which pins 0.15), `coreml-native` 0.2 (new safe wrapper).

Chosen: `objc2-core-ml`. It is generated from the CoreML headers, so every
class and method needed is present with the Objective-C names:
`MLModel::modelWithContentsOfURL_configuration_error`,
`MLModelConfiguration::setComputeUnits`, `MLMultiArray::initWithShape_dataType_error`
plus `shape`, `strides`, `dataPointer`, `MLFeatureValue::featureValueWithMultiArray`,
`MLDictionaryFeatureProvider::initWithDictionary_error`,
`MLFeatureProvider::featureValueForName`, `MLPredictionOptions::setOutputBackings`.
It shares the `objc2` runtime with the AppKit, AVFoundation and CoreAudio
crates a Rust Mac app would use anyway, and anarlog already depends on the
same family. `cidre` covers the same surface but its API moves between minor
versions and the feature flags are heavier; nothing it offers was needed.

Dependency build: 22 s cold on Forge. Incremental rebuild of the crate: 1 to 2 s.

API gaps hit (none blocking):

- Every call is `unsafe`; the crate wraps them in `src/coreml.rs` (164 lines).
- `MLMultiArray::dataPointer` is deprecated in favour of
  `getBytesWithHandler`, which needs `block2` closures. FluidAudio itself still
  uses `dataPointer`; the spike does too and accepts the four warnings.
- Input arrays are allocated by CoreML (`initWithShape:dataType:`) and filled
  through the pointer, so the shapes and strides are CoreML's own; outputs are
  read through `shape` and `strides` (the encoder output `[1, 1024, 188]` is
  read strided, everything else is checked contiguous).
- Rust ergonomics: `model.predict(&provider(...)?)` does not compile
  (`error[E0308]: `?` operator has incompatible types ... expected
  `MLDictionaryFeatureProvider`, found `Retained<MLDictionaryFeatureProvider>``);
  bind the provider to a local first.
- No runtime CoreML error occurred. Model load, prediction, and output
  extraction worked on the first successful compile.

## Step 1: encoder latency, Rust vs Swift

`Encoder.mlmodelc` (425 MB, "int8" label, 6-bit LUT palettised fp16 per
`config.json`), computeUnits `.all`, input `mel [1, 128, 1501]` from the
Preprocessor (cpuOnly, as FluidAudio configures it) on the first 15 s of
`2d7b9aba-system-10min.wav`, `mel_length` 1501, 20 steady-state calls. Swift
reference: `spikes/coreml-rs/swift/EncoderBench.swift` (36 lines, `swiftc -O`),
zero mel of the same shape. Load averages 2.7 to 2.9 during the four runs.

| Run | Language | Model load | First prediction | Steady min | Steady median | Steady mean | Steady max | encoder_length |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| cold cache | Rust | 15.77 s | 31.0 ms | 25.9 ms | 26.0 ms | 26.0 ms | 26.2 ms | 188 |
| cold cache | Swift | 14.66 s | 44.0 ms | 25.5 ms | 25.5 ms | 25.6 ms | 26.2 ms | 188 |
| warm cache | Rust | 0.165 s | 31.4 ms | 25.5 ms | 25.5 ms | 25.5 ms | 25.7 ms | 188 |
| warm cache | Swift | 0.111 s | 45.2 ms | 25.5 ms | 25.5 ms | 25.5 ms | 25.6 ms | 188 |

"Cold cache" is the first load after the model directory was last compiled by
another process; the ~15 s is CoreML's Neural Engine compilation, paid once per
machine and model and cached by the OS regardless of language. Steady state is
identical. Rust's first prediction is faster than Swift's; the difference is
the Swift `MLDictionaryFeatureProvider` and `NSNumber` setup on the first call
and is irrelevant at 15 s per window.

## Step 2: the full pipeline

### What was ported (from reading FluidAudio 0.17.4, revision 21493f8)

- Model contract: Preprocessor `audio_signal [1, 240000] f32`,
  `audio_length [1] i32` to `mel [1, 128, 1501]`, `mel_length`; Encoder to
  `encoder [1, 1024, 188]`, `encoder_length`; Decoder `targets [1, 1] i32`,
  `target_length`, `h_in`/`c_in [2, 1, 640]` to `decoder [1, 640, 1]`,
  `h_out`, `c_out`; JointDecisionv3 `encoder_step [1, 1024, 1]`,
  `decoder_step [1, 640, 1]` to `token_id`, `token_prob`, `duration` (bin
  index). The joint already contains the argmax, so the Rust loop never sees
  logits.
- Compute units as FluidAudio sets them: preprocessor `.cpuOnly`, encoder,
  decoder and joint `.all`.
- Chunk layout for v3 with `melChunkContext = false` (the v3 default):
  window 239,360 samples (14.96 s), overlap 32,000 (2.0 s), stride 207,360
  (12.96 s), all frame aligned to 1,280 samples (80 ms); zero padding to
  240,000 with `audio_length` set to the real length; the end-aligned final
  window (issue #747) with its suppressed warm-up prefix and the trailing
  silence search (`speechRmsFloor` 0.0005).
- `TdtDecoderV3.decodeWithTimings`: SOS priming with blank 8192, cached
  predictor output, outer loop with duration bins `[0, 1, 2, 3, 4]`, the two
  `duration == 0` fixes, the inner blank loop that skips frames without
  touching the LSTM, `maxSymbolsPerStep` 10 with forced advance,
  `maxTokensPerChunk` 150, and the last-chunk tail with
  `consecutiveBlankLimit` 5 and the three frame variations. A fresh decoder
  state per window, as `ChunkProcessor` does.
- Empty-decode recovery (#909): RMS gate 0.003 over at least 2 s, five
  length policies (`encoderFull`, `preprocessorFull`, `trimmedTail` and the
  two combinations), acceptance at two or more tokens with mean confidence
  0.7 or more.
- Overlap merge: time-tolerant LCS over the 2 s overlap, gap arbitration by
  length, splice-safe tail handling, midpoint fallback, monotonic timestamps.
- Text and word timings: pieces joined at word-start pieces (the v3 vocab
  file marks them with a leading space, not the SentencePiece "▁"; FluidAudio
  accepts both), one-frame TDT emission delay, end from token duration.

Port size: 1,061 lines of Rust (`coreml.rs` 164, `decoder.rs` 284,
`pipeline.rs` 440, `main.rs` 136, `wav.rs` 37), 36 lines of Swift, 33 lines of
Python. FluidAudio's `ASR/Parakeet` directory is 22,828 lines; the files the
Steno path exercises (`ChunkProcessor`, `TdtDecoderV3`, `AsrManager+Pipeline`,
`+Transcription`, `+TokenProcessing`, `AsrModels`, `SequenceMatcher`) are
about 5,200.

### Results on the seven corpus files

Rust is serial: one window at a time, one thread. FluidAudio's baseline used
`parallelChunkConcurrency = 4`. Run at load 4.3 rising to 5.4 (two concurrent
Rust builds on the host); the single-file run at load 2.9 is shown for scale.
Swift columns are from `~/steno-spikes/baseline-bakeoff/report.md` (idle
machine). WER: Rust text against the Swift text, both lower-cased, punctuation
stripped, whitespace collapsed.

| File | Swift wall s | Swift RTFx | Rust wall s | Rust RTFx | Windows | Rust tokens | Pre s | Enc s | Dec s | Decoder calls | Joint calls | WER vs Swift |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 2d7b9aba-system-10min.wav | 1.94 | 310 | 2.48 (2.35 at load 2.9) | 242 (255) | 47 | 947 | 0.07 | 1.26 | 1.15 | 1186 | 3329 | 5.58 % |
| 5ea9e7e8-system-10min.wav | 3.39 | 177 | 2.94 | 204 | 47 | 2147 | 0.06 | 1.22 | 1.66 | 2579 | 3698 | 2.65 % |
| 7eb51e56-system-10min.wav | 2.44 | 246 | 3.53 | 170 | 47 | 3061 | 0.06 | 1.25 | 2.22 | 3531 | 5660 | 15.13 % |
| 83bb1859-system-10min.wav | 3.63 | 165 | 2.90 | 207 | 47 | 2240 | 0.06 | 1.22 | 1.62 | 2721 | 3947 | 8.08 % |
| bfbeef67-system-10min.wav | 1.90 | 316 | 2.59 | 232 | 46 | 1253 | 0.07 | 1.20 | 1.31 | 1437 | 3429 | 8.20 % |
| c0cd3671-system-10min.wav | 1.96 | 307 | 3.44 | 174 | 47 | 3155 | 0.06 | 1.24 | 2.14 | 3729 | 4678 | 6.07 % |
| f5fd585d-system-10min.wav | 5.45 | 110 | 2.44 | 246 | 47 | 1203 | 0.06 | 1.22 | 1.16 | 1472 | 3652 | 10.29 % |
| mean | 2.96 | 233 | 2.90 | 211 | | | | | | | | 8.73 % (673 edits / 7,712 words) |

The timing columns are from the pre-recovery build; the recovery port adds
retries only on windows that decoded empty (5 to 49 retries per file, each a
preprocessor, encoder and decode pass) and changed the WER by 0.06 points
overall, because almost no retry clears the 0.7 confidence gate (see below).
A second full run at load 7.0 to 7.1 measured 5.1 to 7.4 s per file and is
not a valid timing; a clean rerun was queued to wait for load below 4 (result
appended at the end if it completed inside the time box).

Where the time goes (serial): the encoder is a fixed 1.2 s per 10-minute file
(47 windows at 25.5 ms); the decoder loop is 1.1 to 2.2 s, proportional to the
number of tokens (one Decoder call per emitted token, one Joint call per frame
step, about 0.3 ms each as CoreML round trips, not compute). The preprocessor
is 0.06 s. Four worker threads with their own `DecoderBuffers` would bring the
serial 2.4 to 3.5 s to roughly the Swift 1.9 to 5.5 s band or better; CoreML
`MLModel.prediction` is thread-safe and FluidAudio does exactly this.

### Why the text differs from Swift, with evidence

The decoder core is faithful: where the Rust and Swift texts agree, the word
timings agree to 0.01 s (a four-word German phrase at 2.80 to 4.00 s carries the same per-word
times in both), and 5ea9e7e8 differs by 29 words in 1,094. The
residue falls into four groups, all outside `TdtDecoderV3`:

1. Window layout. FluidAudio v3 uses silence-aligned chunk starts
   (`silenceAlignedChunkStarts`, a ±4 s energy search per boundary, plus the
   "would compress speech tail" rule, about 250 lines not ported); the spike
   uses regular 12.96 s strides. On 2d7b9aba the Swift text has
   three one-word greetings at 14.7 to 23.9 s; the Rust window 1
   (12.96 to 27.92 s) decodes zero tokens, and its `trimmedTail` retry
   produces 16 tokens at mean confidence 0.60, below the 0.7 gate. Swift's
   window boundary for that stretch sits elsewhere and either decodes the
   words outright or clears the gate. The same mechanism explains the dropped
   sentence at 262 to 265 s (window 21 decodes empty in Rust).
2. Seam heuristics not ported: `findContiguousMatches` as the first merge
   pass (the spike goes straight to LCS), `collapseSeamWordDuplicates`, the
   seam-word pop and re-segmentation in `mergeUsingMatches`, and the seam-gap
   repair pass (issue #758) that re-decodes gaps the merger dropped. 7eb51e56
   (159 Swift segments, the densest file) has the highest WER, consistent
   with seam effects scaling with speech density.
3. Post-processing: the Swift baseline writes `30`, the Rust port `thirty`;
   FluidAudio links `NemoTextProcessing` for inverse text normalisation.
   `fulfill` vs `fulfil` and `copilot` vs `co pilot` are the same category or
   seam re-segmentation.
4. Suppressed tokens are not tracked, so the end-aligned final window counts
   as "empty" for the recovery gate even when it decoded suppressed prefix
   tokens. Affects one window per file.

None of these needs a different binding or a different model; they are more
lines of port. The silence-aligned starts and the seam-gap repair are the two
that move WER; both are pure Swift logic over sample energy and token lists.

### Not done

- `ort` with the CoreML execution provider was not built (another spike
  covers ONNX on CPU). Note for the alternative path: `ort` 2.x exposes the
  CoreML EP, but it needs an ONNX export of Parakeet, not FluidAudio's
  `.mlmodelc`; the EP compiles ONNX to CoreML at load, falls back to CPU for
  unsupported ops, and would not share FluidAudio's model files, Neural
  Engine cache, or future model updates. It answers "ONNX on the ANE", not
  "FluidAudio's models from Rust".
- No `MLPredictionOptions.outputBackings`, no ANE-aligned buffers
  (`ANEMemoryUtils`), no `prefetchToNeuralEngine`. Encoder parity at 25.5 ms
  shows the encoder does not need them; the decoder loop might gain a little.
- No parallel windows, no streaming API, no `TdtDecoderState` carried across
  calls (Steno passes a fresh state per `transcribe` call anyway).

## Verdict

Rust can keep CoreML speech on the Mac at FluidAudio's speed with
FluidAudio's models: the binding is a thin, zero-overhead layer over the same
`MLModel` the Swift app calls, and a 1,061-line port already reproduces the
decoder output frame for frame. What Rust cannot do is call FluidAudio: the
transcript parity comes from re-implementing FluidAudio's Swift heuristics
around the decoder, and that is where the cost and the drift risk live.

Production port cost (estimate, from the code read): the remaining Steno path
in FluidAudio is about 5,200 Swift lines of which perhaps 1,500 matter for
output parity (window alignment, merge and repair, token dedup, text
normalisation). Two to three weeks for a faithful port with a parity harness
against FluidAudio output on the calibration corpus; the harness is the real
deliverable, because FluidAudio ships decoder-adjacent heuristics in most
releases (the 0.17.x `ChunkProcessor` alone cites issues #594, #683, #706,
#747, #758, #787, #803, #897, #905, #909).

Risks:

- Binding maintenance: low. `objc2-core-ml` is generated; a CoreML header
  change is a crate bump. The deprecation of `dataPointer` is the only
  foreseeable churn and has a documented replacement.
- Model updates from FluidAudio: medium. The `.mlmodelc` contract (names,
  shapes, the argmax-in-joint design) is stable across v2/v3, but FluidAudio
  changes the encoder files (`Encoder_v2.mlmodelc` int8-linear, `EncoderInt4`)
  and the download manifest; the Rust side would pin a model version and
  re-verify shapes at load.
- Decoder parity: the greedy loop is small and now ported; the heuristics
  around it are not, and FluidAudio keeps changing them. Without a parity
  harness the Rust transcript will drift from "what the Swift app produced"
  release by release. With one, divergence is a measured number.
- Memory: the Rust process mapped the same 425 MB encoder; no additional
  copies beyond one 240,000-sample input buffer and the per-thread decoder
  buffers (about 10 KB). Four workers share one `MLModel` each.
- Inverse text normalisation: FluidAudio's `NemoTextProcessing` is a
  prebuilt xcframework from `text-processing-rs`, which is itself Rust, so
  this piece exists natively.

## How to reproduce

On a Mac with the models in `~/Library/Application Support/Steno/Models/fluidaudio/parakeet-tdt-0.6b-v3`:

```
cd spikes/coreml-rs
cargo build --release
M="$HOME/Library/Application Support/Steno/Models/fluidaudio/parakeet-tdt-0.6b-v3"
./target/release/coreml-rs bench-encoder "$M" all 20 some-16k-mono.wav
swiftc -O -o encoder-bench swift/EncoderBench.swift && ./encoder-bench "$M" all 20
./target/release/coreml-rs transcribe "$M" out file1.wav file2.wav
python3 tools/wer.py <dir-with-<name>.parakeet-v3.json> out
COREML_RS_DEBUG=1 ./target/release/coreml-rs transcribe ...   # per-window and recovery trace
```

## Appendix: queued rerun under heavy load

The rerun waited 13 minutes for the 1-minute load to fall below 4 and never
got it (two other agents' Rust builds; load 13.4 at start, 19.4 at the end),
so it ran anyway. Not a valid speed measurement, but it shows where the port
is sensitive to CPU contention and where it is not:

| Measurement at load 13 to 19 | Rust | Swift |
|---|---:|---:|
| Encoder steady median (`.all`) | 27.0 ms | 27.7 ms |
| Encoder first prediction | 30.6 ms | 40.0 ms |

The encoder runs on the Neural Engine and barely moves (25.5 to 27 ms). The
full pipeline went from 2.4 to 3.5 s per file at load 3 to 5 to 5.9 to 9.4 s
at load 13 to 19, entirely in the decoder loop (`Dec s` 3.7 to 7.5 s versus
1.1 to 2.2 s): the Decoder and Joint calls are thousands of tiny CoreML round
trips whose cost is CPU scheduling, not compute. The Swift baseline was
measured on an idle machine; the same loop in Swift would suffer the same way.
WER was unchanged (8.73 % overall), as expected: timing noise does not change
the decode.
