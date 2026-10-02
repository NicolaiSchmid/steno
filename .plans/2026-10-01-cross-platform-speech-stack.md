# Cross-platform speech stack: the ONNX path to Mac-quality transcripts

Status: direction agreed 2026-10-01; spikes D, E and F done the same day. Gate G1
(transcript quality) passed; G2 (idle laptop speed), G3 (diarization) and G4 (GPU) are
open. Follows `.plans/2026-10-01-cross-platform-spikes.md`, which owns the question, the
method, the baseline and spikes A to C. This plan widens the scope plan's "Windows,
Linux" non-goal only if the gates pass; until then it is spike work under `spikes/` and
no product target depends on it.

## Problem

Spike B (umbrella plan, results) showed what Linux and Windows would get from Parakeet
TDT v3 through sherpa-onnx on CPU: 18 times slower than CoreML on the same laptop,
17.9 % word disagreement with the Mac transcript, 2.5 to 3 GB peak memory, a process
abort on long audio, and a diarizer that returns 3 to 16 speakers for one. The causes
are separable, and none of them is the model's weights.

## How we got here

Spike D built the VAD chunker and language voting and failed the original G1
(agreement with the CoreML transcript, mean under 8 %, no file over 15 %) at 16.2 %
mean. Spike E exported the model ourselves; fp32 reached 11.5 % and still failed. Spike F
scored every engine against human references (FLEURS German) and found that the
"disagreement" was two implementations each about 5 % wrong in different places, with
the CoreML encoder, a 6-bit palettised approximation, the slightly lossier side. G1
measured the wrong thing and was restated as absolute WER; in that form the ONNX path
passes. Along the way spike D blamed a window that decodes to zero tokens on int8
quantisation, spike E reproduced it with fp32 and blamed sherpa-onnx's recognizer glue,
and spike F showed the loop spike E used as a control is itself broken, so the cause of
that window is open. The decisions below state the current position; the three reports
keep the interim reasoning.

## Decisions

1. **A custom silence chunker shared by every platform.** VAD (silero, shipped with
   sherpa-onnx) marks speech; segments target 20 to 30 s and cut at the longest pause
   within a search window either side of the target, never inside speech, with an
   energy-minimum fallback; 1 to 2 s overlap merged by the LCS merger ported from
   `spikes/coreml-rs/src/pipeline.rs`. The same chunker fronts the ONNX encoder and, in a
   Rust future, the CoreML one, so chunking stops being a source of drift. Measured in
   spike D: it removed the chunk-length sensitivity, skipped silence for a quarter of the
   wall time, and the merge produced no visible seam damage. Two corrections from spike
   E: the stock export's cap is 5000 encoder frames (400 s), not 200 s, and our export
   carries 800 s, so the chunker's 190 s clamp can be relaxed; attention memory grows with
   the square of the window (13 GB peak for 600 s), so 25 to 60 s segments stay the
   design. Empty-decode recovery must extend the window by 5 to 12 s, not shift it.
2. **Language by voting, kept as a guard, not a quality lever.** Lane level: spoken
   language identification on a few dozen windows sampled across the lane gives the
   lane's language set as a prior, restricted to the languages the product supports
   (SLID voted for unrelated languages on four of seven files). Segment level: a decode
   whose text language is outside the lane set is re-decoded at shifted boundaries and
   the candidates vote. Measured in spike D: pause-aligned chunks do not flip language,
   so voting changed nothing on the corpus; the prior's one use was to reject a text
   detector confusion. Cross-engine voting with Whisper stays a later option.
3. **Our own fp32 model export.** Parakeet TDT 0.6B v3 is CC-BY-4.0 and sherpa-onnx
   publishes the export script; `spikes/onnx-speech/export/` holds our adaptation with a
   10000-frame position table. Attribution goes in the app and the download. fp32 is the
   shipping variant: it is no slower than int8 on the M4 Pro at 4 threads (RTFx 18 to 20)
   and every dynamic int8 variant costs 4 to 5 points of agreement on German. Cost: 2.6 GB
   on disk and 3.2 to 3.4 GB peak RSS against 0.7 GB and 2.6 GB for int8. If download
   size matters, a calibrated static int8 or an fp16-weights variant is the next
   experiment; neither was tried.
4. **Speed, in this order.** VAD plus parallel segments across sessions; incremental
   transcription during the meeting at low priority with a thread cap, so only the last
   segment remains at meeting end (a pipeline change, not a live transcript UI; battery
   and call quality are measured before it ships); GPU where it exists: DirectML on
   Windows (any DirectX 12 GPU, integrated included), CUDA on Linux for NVIDIA, and
   whisper.cpp with Vulkan as a second `SpeechEngine` for cross-vendor Linux GPUs.
   Measured so far on the M4 Pro only: RTFx 18 to 20 at 4 threads for the full pipeline;
   10 threads bought 22 % of wall time under load. A clean 10-thread number and the idle
   laptop number (G2) are still owed.
5. **Inference in a sidecar process.** ONNX Runtime errors are C++ exceptions that abort
   through the FFI, and the 2 to 3 GB working set should be released after processing.
   The sidecar speaks the same JSON convention as the bridge, is spawned per job, and is
   killed on timeout. It also isolates the GPU driver. Open inside this decision: whether
   the sidecar drives ONNX Runtime directly through the `ort` crate with our own feature
   extraction and TDT greedy loop (the loop `spikes/coreml-rs/src/decoder.rs` already has
   for CoreML, which reproduces FluidAudio's decode frame for frame), or wraps the
   sherpa-onnx recognizer. Direct ORT would give one chunker, one decoder, one merger and
   two tensor backends (CoreML on the Mac, ONNX Runtime elsewhere) and would remove the
   dynamic-library and C++-exception problems of the binding. Against it: the sherpa-onnx
   recognizer is so far the only ONNX decode of this export shown to be accurate; the
   numpy loop from spike E scores 64 % WER on FLEURS, so any own loop must be validated
   against FLEURS, not word counts. WP2 settles this.
6. **Diarization rebuilt on the matching embedding.** WeSpeaker ResNet34-LM from the
   sherpa-onnx assets (the family FluidAudio uses), segmentation and embedding through
   the sherpa-onnx C API, our own clustering plus the refinement pass from
   `.plans/2026-09-29-speaker-calibration.md`, recalibrated on the full Forge corpus
   against `truth.json`.

## Gates

| Gate | Measure | Pass | State |
|---|---|---|---|
| G1 transcript quality | Absolute WER against human references (FLEURS German test) on the same machine | Within 1 point of CoreML Parakeet | **Passed 2026-10-01** (spike F): own fp32 ONNX 5.3 % vs CoreML 5.5 % on ten 7-minute files with the chunker in the loop, 5.7 % vs 5.9 % on 151 single utterances |
| G2 idle laptop speed | RTFx of the full ONNX pipeline on an idle Linux laptop (not atlas under load) | A 60-minute meeting finishes under 3 minutes, or under 1 minute with incremental transcription | Open |
| G3 diarization | Speaker counts on the full seven calls vs `truth.json` | All five 1:1 calls = 1, group calls within 1 of FluidAudio | Open |
| G4 GPU | RTFx with DirectML on an integrated GPU and CUDA on a discrete one | At least 3x the same machine's CPU figure | Open; no machine |

G1 was originally agreement with the CoreML transcript on the seven calls (mean under
8 %, no file over 15 %); spikes D and E measured 16.2 % and 11.5 % against it, and spike
F showed why that metric could not pass (see "How we got here"). What G1 does not
settle: FLEURS is clean read speech by one speaker; the ranking on noisy multi-speaker
meeting audio with code-switching is not measured against truth anywhere.

If G2 and G3 pass and there are real users on Linux or Windows, the scope plan's
non-goal is amended by a plan that names the platforms and the shell (Rust core and
Tauri, with CoreML speech on the Mac through the spike C route). Until then there is no
platform plan.

## Work packages

Done, 2026-10-01, all on `spikes/onnx-speech` with Forge as the measurement host:

| WP | Spike | Report | Result |
|---|---|---|---|
| WP0 | D | `.plans/spikes/2026-10-01-spike-chunker-voting.md` | VAD chunker, LCS merge, lane and segment language voting. Mean agreement with CoreML 18.0 % to 15.2 to 16.5 %; chunk-length sensitivity and the long-audio abort gone; a quarter less wall time. Voting changed nothing. Found the zero-token window on one file |
| WP1 | E | `.plans/spikes/2026-10-01-spike-own-export.md` | Own export, fp32 and three int8 variants, measured with the WP0 harness. fp32 11.5 % mean (max 22.3 %) at RTFx 18.4; int8 variants 15.2 to 16.3 %. Zero-token window reproduces with fp32, so it is not quantisation. On the disputed file CoreML and fp32 agree per minute; WhisperKit is the outlier |
| WP1b | F | `.plans/spikes/2026-10-01-spike-fleurs-wer.md` | Absolute WER on FLEURS German (300 utterances, 68 minutes, plus ten concatenated 7-minute files). Table below. G1 restated and passed. The spike E control loop is broken (64.4 % WER), so the zero-token cause is open |

| Engine (spike F) | Long files, mean WER | Utterances, mean WER |
|---|---:|---:|
| Own fp32 ONNX, CPU, VAD chunker | 5.3 % | 5.7 % (151 files) |
| CoreML Parakeet v3 (the app's pipeline) | 5.5 % | 5.9 % (same 151), 5.7 % (all 300) |
| WhisperKit large-v3-turbo | 6.0 % | 4.5 % |

WhisperKit wins on isolated sentences and loses on long recordings, where it drops
whole stretches; for Steno's workload both Parakeets beat it.

Open:

- WP2: sidecar process with the JSON job protocol, length guards, timeout, memory
  release; crash tests. First task: settle decision 5 by validating an own TDT decode
  loop against FLEURS (find the bug in the spike E loop: feature extraction against the
  NeMo preprocessor, or the decoder state hand-off), and check whether it or sherpa-onnx
  1.12.15 clears the zero-token window on c0cd3671 (worth about 7 points on that file).
  Relax the 190 s clamp to the measured cap.
- WP3: diarization on WeSpeaker ResNet34-LM with our clustering and refinement;
  calibration run on Forge (G3).
- WP4: GPU providers (DirectML, CUDA) behind a runtime probe with CPU fallback;
  whisper.cpp Vulkan engine (G4). Needs a Windows machine with an integrated GPU and a
  Linux machine with NVIDIA; neither exists in the current fleet.
- WP5: incremental transcription during recording, with the battery and call-quality
  measurement.
- G2 measurement on an idle Linux laptop, and a clean 10-thread timing on Forge, owed
  since spikes B and D.

Each package is its own PR with its report; no package is product code until the gates
pass and the platform plan exists.
