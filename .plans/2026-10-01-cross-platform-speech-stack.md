# Cross-platform speech stack: the ONNX path to Mac-quality transcripts

Status: direction agreed 2026-10-01; spikes D, E and F done the same day. Gates G1
(transcript quality) and G3 (diarization, WP4d of the Rust port plan) passed; G2 (idle
laptop speed) and G4 (GPU) are open. Follows `.plans/2026-10-01-cross-platform-spikes.md`,
which owns the question, the method, the baseline and spikes A to C. This plan does not
widen the scope plan's "Windows, Linux" non-goal; a later platform plan does if G2 and G3
pass. Until then it is spike work under `spikes/` and no product target depends on it.
Next: WP2 (sidecar and decode loop) and the G2 measurement (WP1c).

## Problem

Spike B (umbrella plan, results) showed what Linux and Windows would get from Parakeet
TDT 0.6B v3 through sherpa-onnx on CPU: 18 times slower than CoreML on the same laptop
(RTFx 13 against 233; RTFx is audio seconds divided by wall seconds, higher is faster),
17.9 % word error rate (WER) against the Mac transcript, 2.5 to 3 GB peak memory, a
process abort on long audio, and a diarizer that returns 3 to 16 speakers for one. The
causes are separable, and none of them is the model's weights.

## Why G1 was restated

Spike D built the VAD chunker and language voting and failed the original G1
(disagreement with the CoreML transcript, mean under 8 %, no file over 15 %) at 16.2 %
mean. Spike E exported the model ourselves; fp32 reached 11.5 % and still failed. Spike F
scored every engine against human references (FLEURS German) and found that the
disagreement was two implementations each about 5 % wrong in different places; the
CoreML encoder (a 6-bit palettised approximation) and our fp32 export are within noise
of each other. G1 measured the wrong thing and was restated as absolute WER; in that
form the ONNX path passes. The decisions below state the current position; the three
reports keep the interim reasoning.

## Decisions

1. **A custom silence chunker shared by every platform.** VAD (silero, shipped with
   sherpa-onnx) marks speech; segments target 20 to 30 s and cut at the longest pause
   within a search window either side of the target, never inside speech, with an
   energy-minimum fallback; 1 to 2 s overlap merged by the LCS merger ported from
   `spikes/coreml-rs/src/pipeline.rs`. Why: spike D showed the cut point, not the model,
   was the first-order variable (WP0 row), and one chunker in front of the ONNX encoder
   and, in a Rust future, the CoreML one stops chunking being a source of drift. Rules
   out: fixed-length or quiet-point chunking. Guards: the segment clamp is 60 s, derived
   from memory (attention grows with the square of the window; 13 GB peak at 600 s,
   spike E), not from the export's position table (400 s stock, 800 s ours), which is a
   cap and not the guard; empty-decode recovery extends the window by 5 to 12 s rather
   than shifting it.
2. **Language by voting, kept as a guard, not a quality lever.** Lane level: spoken
   language identification on a few dozen windows sampled across the lane gives the
   lane's language set as a prior, restricted to the languages the product supports.
   Segment level: a decode whose text language is outside the lane set is re-decoded at
   shifted boundaries and the candidates vote. Why: pause-aligned chunks do not flip
   language, so voting changed nothing on the corpus (WP0 row), while the prior caught
   the one text-detector confusion and SLID voted for unrelated languages on four of
   seven files, which the allowlist removes. Rules out: further investment in voting;
   cross-engine voting with Whisper stays a later option, not a planned package.
3. **Our own fp32 model export.** Parakeet TDT 0.6B v3 is CC-BY-4.0 and sherpa-onnx
   publishes the export script; `spikes/onnx-speech/export/` holds our adaptation with a
   10000-frame position table, and attribution goes in the app and the download. Why:
   fp32 is no slower than dynamic int8 on the M4 Pro at 4 threads and every dynamic int8
   variant costs 4 to 5 points more disagreement with CoreML on German (WP1 row). Rules
   out: dynamic int8 as the shipping variant. Cost: 2.6 GB on disk and 3.2 to 3.4 GB peak
   RSS against 0.7 GB and 2.6 GB for int8, which is why G2 measures peak RSS; if download
   size or that figure demands it, a calibrated static int8 or an fp16-weights variant is
   the next experiment (neither tried; WP1c).
4. **Speed, in this order.** VAD plus parallel segments across sessions; then
   incremental transcription during the meeting at low priority with a thread cap, so
   only the last segment remains at meeting end (a pipeline change, not a live transcript
   UI; battery and call quality are measured before it ships, WP5); then GPU where it
   exists: DirectML on Windows (any DirectX 12 GPU, integrated included), CUDA on Linux
   for NVIDIA, and whisper.cpp with Vulkan as a second `SpeechEngine` for cross-vendor
   Linux GPUs (WP4). Why: skipping silence already bought a quarter of the wall time, the
   segments are independent, and every CPU figure so far is from the M4 Pro (WP0 and WP1
   rows); the idle laptop number is G2 (WP1c). Rules out: the ONNX Runtime CoreML
   provider on the Mac (eight times slower than CPU at 13 GB RSS, spike B); the Mac keeps
   CoreML Parakeet.
5. **Inference in a sidecar process for the ONNX engine off the Mac.** The sidecar
   speaks the same JSON convention as the bridge, is spawned per job, is killed on
   timeout and isolates the GPU driver; the Mac app stays one process with CoreML speech
   in-process, as `.plans/2026-09-29-macos-webview-ui.md` requires. Why: ONNX Runtime
   errors are C++ exceptions that abort through the FFI, and the 2 to 3 GB working set
   should be released after processing. Rules out: in-process inference off the Mac.

   Implemented by WP4c of `.plans/2026-10-02-rust-core-and-tauri-shell.md`:
   `crates/steno-speech-sidecar` with `SidecarSpeechEngine` in `crates/steno-speech`.
   The child is spawned on demand, serves both lanes of a job and is to be stopped
   by `release()` after it once WP6b calls it; the JSON headers follow the bridge convention and the audio
   crosses the pipe as raw `f32`. On macOS the sidecar is a fallback behind a setting,
   CoreML in-process stays the default.

   Open (WP2): whether the sidecar drives ONNX Runtime directly through the `ort` crate
   with our own feature extraction and TDT greedy loop, or wraps the sherpa-onnx
   recognizer. The CoreML loop in `spikes/coreml-rs/src/decoder.rs` is shared only above
   the joint: the CoreML joint emits an argmax token and a duration bin, while the ONNX
   joiner emits logits that the loop must split into vocabulary and duration ranges
   (`spikes/onnx-speech/export/ort_decode.py`), and that split is unvalidated; the only
   own ONNX loop so far scores 64 % WER on FLEURS (spike F), and the CoreML loop's
   frame-for-frame match is inferred from word timings where the text already agrees, at
   8.7 % disagreement with the Swift transcript. Driving ONNX Runtime directly would give
   one chunker, one decoder, one merger and two tensor backends (CoreML on the Mac, ONNX
   Runtime elsewhere) and would remove the dynamic-library and C++-exception problems of
   the binding for ASR; VAD (decision 1) would still go through the sherpa-onnx C API
   until ported in its turn, while segmentation and embedding already run through `ort`
   (decision 6). Against it:
   the sherpa-onnx recognizer is so far the only ONNX decode of this export shown to be
   accurate, so any own loop is validated against FLEURS, not word counts. The window of
   c0cd3671 that decodes to zero tokens belongs to the same package: spike D blamed int8
   quantisation, spike E reproduced it with fp32 and blamed the sherpa-onnx recognizer,
   and spike F showed the loop spike E used as a control is itself broken, so its cause
   is open.
6. **Diarization rebuilt on the matching embedding.** Segmentation (pyannote 3.0) and
   embedding (WeSpeaker ResNet34-LM), the sherpa-onnx model files run through `ort` with
   Steno's own WeSpeaker fbank front end rather than the sherpa-onnx C API, our own
   clustering plus the refinement pass from `.plans/2026-09-29-speaker-calibration.md`,
   calibrated on the full Forge corpus against `truth.json`. Why: the stock sherpa-onnx
   diarizer returned 3 to 16 speakers for one at every threshold (spike B), where
   FluidAudio, with a different embedding and clustering, returns one. Rules out: the
   stock sherpa-onnx diarizer. As built in WP4d of the Rust port plan: ResNet34-LM, the
   first embedding measured, passed G3, so ERes2Net (the stock diarizer's embedding in
   spike B) and CAM++ were not measured. The pyannote segmentation gate and the embedding
   licence are confirmed before any product build.

## Gates

| Gate | Measure | Pass | State |
|---|---|---|---|
| G1 transcript quality | Absolute WER against human references (FLEURS German test) on the same machine | Within 1 point of CoreML Parakeet | **Passed 2026-10-01 on arm64** (spike F): own fp32 ONNX 5.3 % vs CoreML 5.5 % on ten 7-minute files with the chunker in the loop, 5.7 % vs 5.9 % on 151 single utterances. x86 run owed with G2 (WP1c): spike B showed x86 and arm64 transcripts differ |
| G2 idle laptop speed | RTFx and peak RSS of the full ONNX pipeline at 4 threads on an idle x86 Linux laptop of the class users have (8 cores or fewer, 16 GB or less; not atlas under load) | RTFx 20 or better, which is a 60-minute meeting in 3 minutes, and peak RSS no higher than the 3.4 GB measured on the M4 Pro. Incremental transcription (WP5) is measured separately and does not count towards G2 | Open (WP1c). RTFx 20 is what the M4 Pro reaches at 4 threads, so an x86 laptop may miss it; a miss re-sets the gate from the measurement rather than waving it through |
| G3 diarization | Speaker counts on the full seven calls vs `truth.json` | All five 1:1 calls = 1; the two group calls within 1 of the count in `truth.json` (up to 7). FluidAudio's 2 on both is not the target | **Passed 2026-10-03** (PR #164, WP4d of the Rust port plan): both backends 7 of 7 at every cut from 0.20 to 0.60; 0.32 kept as derived from FluidAudio's 0.8 rather than fitted |
| G4 GPU | RTFx with DirectML on an integrated GPU and CUDA on a discrete one | At least 3x the same machine's CPU figure | Open; no machine. DirectML is implemented for the encoder behind a probe with the CPU as the fallback, off by default (WP10b of `.plans/2026-10-02-rust-core-and-tauri-shell.md`); CUDA is not started |

G1 was originally disagreement with the CoreML transcript on the seven calls (mean
under 8 %, no file over 15 %); spikes D and E measured 16.2 % and 11.5 % against it, and
spike F showed why that metric could not pass (see "Why G1 was restated"). What G1 does
not settle: FLEURS is clean read speech by one speaker; the ranking on noisy
multi-speaker meeting audio with code-switching is not measured against truth anywhere.

If G2 and G3 pass and there are real users on Linux or Windows, the scope plan's
non-goal is amended by a plan that names the platforms and the shell (Rust core and
Tauri, with CoreML speech on the Mac through the spike C route). Until then there is no
platform plan.

## Work packages

Done, 2026-10-01, all on `spikes/onnx-speech` with Forge as the measurement host:

| WP | Spike | Report | Result |
|---|---|---|---|
| WP0 | D | `.plans/spikes/2026-10-01-spike-chunker-voting.md` | VAD chunker, LCS merge, lane and segment language voting. Mean disagreement with CoreML down from 18.0 % to 15.2 to 16.5 %; chunk-length sensitivity (16.1 to 16.5 % across 15 to 30 s targets) and the long-audio abort gone; a quarter less wall time (RTFx 14.4 to 21.1 at 4 threads; 10 threads bought 22 % of wall time under load, clean figure owed). Voting changed nothing; the merge produced no visible seam damage. Found the zero-token window on one file |
| WP1 | E | `.plans/spikes/2026-10-01-spike-own-export.md` | Own export, fp32 and three int8 variants, measured with the WP0 harness. fp32 11.5 % mean (max 22.3 %) at RTFx 18.4, no slower than int8 at 4 threads; int8 variants 15.2 to 16.3 % (4 to 5 points more disagreement). Zero-token window reproduces with fp32, so it is not quantisation. On the disputed file CoreML and fp32 agree per minute; WhisperKit is the outlier |
| WP1b | F | `.plans/spikes/2026-10-01-spike-fleurs-wer.md` | Absolute WER on FLEURS German (300 utterances, 68 minutes, plus ten concatenated 7-minute files). Table below. G1 restated and passed. The spike E control loop is broken (64.4 % WER), so the zero-token cause is open |

| Engine (spike F) | Long files, mean WER | Utterances, mean WER |
|---|---:|---:|
| Own fp32 ONNX, CPU, VAD chunker | 5.3 % | 5.7 % (151 files) |
| CoreML Parakeet v3 (the app's pipeline) | 5.5 % | 5.9 % (same 151), 5.7 % (all 300) |
| WhisperKit large-v3-turbo | 6.0 % | 4.5 % |

WhisperKit wins on isolated sentences and loses on long recordings, where it drops
whole stretches; for Steno's workload both Parakeets beat it.

Open:

- WP1c: the G2 measurement on an idle x86 Linux laptop (RTFx and peak RSS at 4
  threads), the x86 FLEURS run that completes G1, and a clean 10-thread timing on Forge,
  owed since spikes B and D. If the memory figure is too high for the laptop class, the
  calibrated static int8 or fp16-weights experiment from decision 3 follows here.
- WP2: sidecar process with the JSON job protocol, length guards, timeout, memory
  release; crash tests. First task: settle decision 5 by validating an own TDT decode
  loop against FLEURS (find the bug in the spike E loop: the logits split at the joiner,
  feature extraction against the NeMo preprocessor, or the decoder state hand-off), and
  check whether it or sherpa-onnx 1.12.15 clears the zero-token window on c0cd3671
  (worth about 7 points on that file). Replace the 190 s clamp with the 60 s
  memory-derived clamp from decision 1; the position-table cap is not the guard.
- WP3: diarization with our clustering and refinement, WeSpeaker ResNet34-LM as the
  first embedding; segmentation gate and embedding licence confirmed first; calibration
  run on Forge (G3). Moved to the Rust port plan as WP4d on 2026-10-02
  (`.plans/2026-10-02-rust-core-and-tauri-shell.md`), where ResNet34-LM passed G3 and
  ERes2Net was not measured (decision 6).
- WP4: GPU providers (DirectML, CUDA) behind a runtime probe with CPU fallback;
  whisper.cpp Vulkan engine (G4). Needs a Windows machine with an integrated GPU and a
  Linux machine with NVIDIA; neither exists in the current fleet. DirectML is built
  (WP10b of the Rust port plan) and waits for the measurement; CUDA and the Vulkan
  engine are open.
- WP5: incremental transcription during recording, with the battery and call-quality
  measurement.

Each package is its own PR with its report; no package is product code until the gates
pass and the platform plan exists.
