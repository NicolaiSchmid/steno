# Cross-platform speech stack: the ONNX path to Mac-quality transcripts

Status: direction agreed 2026-10-01, spikes in progress. Follows
`.plans/2026-10-01-cross-platform-spikes.md`, which measured the gap. This plan
widens the scope plan's "Windows, Linux" non-goal only if the gates below pass; until
then it is spike work under `spikes/` and no product target depends on it.

## Problem

Spike B showed what Linux and Windows would get from Parakeet TDT v3 through
sherpa-onnx on CPU: 18 times slower than CoreML on the same laptop, 17.9 % word
disagreement with the Mac transcript, 2.5 to 3 GB peak memory, a process abort on
audio over about 200 s, and a diarizer that returns 3 to 16 speakers for one. The
causes are separable, and none of them is the model's weights.

## Decisions

1. **A custom silence chunker shared by every platform.** VAD (silero, shipped with
   sherpa-onnx) marks speech; segments target 20 to 30 s and cut at the longest pause
   within a search window either side of the target, never inside speech, with an
   energy-minimum fallback; 1 to 2 s overlap merged by the LCS merger ported in
   `spikes/coreml-rs/src/pipeline.rs`. The same chunker fronts the ONNX encoder and,
   in a Rust future, the CoreML one, so chunking stops being a source of drift. It
   also removes the 200 s abort (length guard) and skips silence the encoder was
   paying for.
2. **Language by voting, at two levels.** Lane level: spoken language identification
   on a few dozen windows sampled across the whole lane, majority vote, giving the
   lane's language set as a prior (a 1:1 call is German; a team call is German and
   English). Segment level: text language detection on each decode; a segment outside
   the lane's set or with low token confidence is re-decoded at two or three shifted
   boundaries and the candidates vote by agreement and confidence. Only flagged
   segments pay for extra decodes on CPU; with a GPU, every segment can vote.
   Cross-engine voting with Whisper on flagged segments is a later option.
3. **Our own model export.** Parakeet TDT 0.6B v3 is CC-BY-4.0 and sherpa-onnx
   publishes the export script. Export fp32 and a calibrated int8 with a larger
   position table; measure both. Attribution goes in the app. Target: within about
   5 % WER of the Mac transcript; identical text is not a goal.
4. **Speed, in this order.** VAD plus parallel segments across sessions; incremental
   transcription during the meeting at low priority with a thread cap, so only the
   last segment remains at meeting end (a pipeline change, not a live transcript UI;
   battery and call quality are measured before it ships); GPU where it exists:
   DirectML on Windows (any DirectX 12 GPU, integrated included), CUDA on Linux for
   NVIDIA, and whisper.cpp with Vulkan as a second `SpeechEngine` for cross-vendor
   Linux GPUs.
5. **Inference in a sidecar process.** ONNX Runtime errors are C++ exceptions that
   abort through the FFI, and the 2 to 3 GB working set should be released after
   processing. The sidecar speaks the same JSON convention as the bridge, is spawned
   per job, and is killed on timeout. It also isolates the GPU driver.
6. **Diarization rebuilt on the matching embedding.** WeSpeaker ResNet34-LM from the
   sherpa-onnx assets (the family FluidAudio uses), segmentation and embedding through
   the sherpa-onnx C API, our own clustering plus the refinement pass from
   `.plans/2026-09-29-speaker-calibration.md`, recalibrated on the full Forge corpus
   against `truth.json`.

## Gates

| Gate | Measure | Pass |
|---|---|---|
| G1 transcript quality | Originally: WER of ONNX text vs CoreML Parakeet text on the seven 10-minute clips (mean under 8 %, no file over 15 %). Restated after spike F: absolute WER against human references (FLEURS German test) within 1 point of CoreML Parakeet on the same machine | **Passed 2026-10-01**: fp32 ONNX 5.3 % vs CoreML 5.5 % on long files, 5.7 % vs 5.9 % on utterances |
| G2 idle laptop speed | RTFx of the full ONNX pipeline on an idle Linux laptop (not atlas under load) | a 60-minute meeting finishes under 3 minutes, or under 1 minute with incremental transcription |
| G3 diarization | speaker counts on the full seven calls vs `truth.json` | all five 1:1 calls = 1, group calls within 1 of FluidAudio |
| G4 GPU | RTFx with DirectML on an integrated GPU and CUDA on a discrete one | at least 3x the same machine's CPU figure |

G1 passed on 2026-10-01 in its restated form. If G3 passes, the scope plan's non-goal is amended by a plan that names the
platforms and the shell (Rust core and Tauri, with CoreML speech on the Mac through
the spike C route). If G1 fails, the model export (decision 3) is the next variable;
if that fails too, Linux and Windows ship WhisperKit-class transcripts or nothing.

## Work packages

- WP0 (spike D, done 2026-10-01): chunker, overlap merge, lane and segment language
  voting on `spikes/onnx-speech`. Report `.plans/spikes/2026-10-01-spike-chunker-voting.md`.
  Result: G1 fails. The VAD chunker took the mean from 18.0 % to 15.2 to 16.5 %,
  removed the chunk-length sensitivity and the 200 s abort, and cut a quarter of the
  wall time; language voting changed nothing because pause-aligned chunks do not
  flip language. The remaining disagreement is model-level: scattered single-word
  int8 substitutions on every German file, one file no engine agrees on, and an int8
  export defect where a 23 s window decodes to zero tokens unless extended by 5 to
  12 s. Empty-decode recovery must extend the window, not shift it.
- WP1 (spike E, done 2026-10-01): own export of Parakeet TDT v3 measured with the
  WP0 harness. Report `.plans/spikes/2026-10-01-spike-own-export.md`. Result: own
  fp32 export reaches 11.5 % mean (max 22.3 %) against CoreML at RTFx 18.4, no
  slower than int8 on this CPU; every int8 variant stays at 15 to 16 %. The
  zero-token window reproduces with fp32 too, so it is not quantisation; spike E
  attributed it to sherpa-onnx's recognizer glue on the strength of a numpy TDT loop
  that spike F later found to produce garbled text (about 65 % WER), so the cause is
  still open. On the disputed file, CoreML and fp32 agree per minute and WhisperKit
  is the outlier.
  With fp32, the glue bypassed, and that file excluded, the six remaining files sit
  at 9.7 % mean, all under 15 %. G1 still fails on the letter.
- Consequence for decisions 3 and 5: ship our own fp32 export, and drive ONNX
  Runtime directly through the `ort` crate with our own TDT decode loop (the one
  `spikes/coreml-rs/src/decoder.rs` already has for CoreML, which reproduces
  FluidAudio's decode frame for frame), not through sherpa-onnx's recognizer. One
  chunker, one decoder, one merger, two tensor backends: CoreML on the Mac, ONNX
  Runtime elsewhere. This removes the dynamic-library and C++-exception problems
  of the sherpa-onnx binding; whether it also removes the zero-token window is the
  first thing WP2 verifies, with the spike C decoder, not the numpy loop.
- WP1b (spike F, done 2026-10-01): absolute WER against FLEURS German test
  references (CC-BY-4.0; 300 utterances, 68 minutes, plus ten concatenated 7-minute
  files with the chunker in the loop), all engines on Forge. Report
  `.plans/spikes/2026-10-01-spike-fleurs-wer.md`.

  | Engine | Long files, mean WER | Utterances, mean WER |
  |---|---:|---:|
  | Own fp32 ONNX, CPU, VAD chunker | 5.3 % | 5.7 % (151 files) |
  | CoreML Parakeet v3 (the app's pipeline) | 5.5 % | 5.9 % (same 151), 5.7 % (all 300) |
  | WhisperKit large-v3-turbo | 6.0 % | 4.5 % |

  The 11.5 % "disagreement" of spike E was two implementations each about 5 % wrong
  in different places. ONNX fp32 is as accurate as CoreML; G1 is restated as absolute
  WER and passed. WhisperKit wins on isolated sentences and loses on long recordings,
  where it drops whole stretches.
- WP2: sidecar process with the JSON job protocol, length guards, timeout, memory
  release; crash tests.
- WP3: diarization on WeSpeaker ResNet34-LM with our clustering and refinement;
  calibration run on Forge.
- WP4: GPU providers (DirectML, CUDA) behind a runtime probe with CPU fallback;
  whisper.cpp Vulkan engine. Needs a Windows machine with an integrated GPU and a
  Linux machine with NVIDIA; neither exists in the current fleet.
- WP5: incremental transcription during recording, with the battery and call-quality
  measurement.

Each package is its own PR with its report; no package is product code until the
gates pass and the platform plan exists.
