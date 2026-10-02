# Spike D: pause-aligned chunker and language voting for Parakeet on ONNX

Status: measured 2026-10-01, time-boxed to about 90 minutes. Parent:
`.plans/2026-10-01-cross-platform-speech-stack.md` (decisions 1 and 2, gate G1).
Baseline and setup: `.plans/spikes/2026-10-01-spike-onnx-speech.md`. Code:
`spikes/onnx-speech/` (extended in place).

## Question

Spike B measured 17.9 % word disagreement between Parakeet TDT 0.6b v3 int8 on
ONNX (CPU, 20 s quiet-point chunks) and the same model on CoreML (FluidAudio). How
much of that disappears with (a) a VAD-driven, pause-aligned chunker with overlap
merge and (b) language voting at lane and segment level? Gate G1: mean under 8 %,
no file over 15 %.

## Setup

| Item | Value |
|---|---|
| Machine | Forge, M4 Pro (10 cores, 24 GB), macOS 26.7, no other agents; 1-minute load recorded before each file (3 to 5 throughout, from system daemons) |
| Build | `sherpa-rs-sys` 0.6.8 with sherpa-onnx v1.12.9 prebuilt, as in spike B; `CARGO_BUILD_JOBS=4`; cold build 38 s; one new crate dependency, `whatlang` 0.16 (text language detection, pure Rust) |
| ASR | `sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8`, greedy search, CPU provider, 4 threads (10 threads for the final row) |
| VAD | `silero_vad.onnx` (630 KB) from the sherpa-onnx `asr-models` release, through `SherpaOnnxCreateVoiceActivityDetector`: threshold 0.5, `min_silence_duration` 0.25 s, `min_speech_duration` 0.1 s, window 512 samples, `max_speech_duration` 120 s, buffer 180 s. One pass over the whole file, 512 samples at a time, then `Flush`; the speech segments become speech regions and the gaps between them are the pauses |
| SLID | `sherpa-onnx-whisper-tiny` (multilingual, int8 encoder and decoder, 116 MB archive) through `SherpaOnnxCreateSpokenLanguageIdentification`; the 1.12.9 C API has it |
| Text language | `whatlang::detect` on the rendered segment text; only segments with at least 4 words are judged; `deu`/`eng` mapped to `de`/`en` |
| Corpus and references | the seven 10-minute 16 kHz clips and the CoreML Parakeet and WhisperKit texts from the baseline bake-off, scored with `spikes/onnx-speech/scripts/score.py` (lowercase, punctuation stripped) |

## What was built

All in `spikes/onnx-speech/src/`, behind flags so the stage 0 path is byte-identical
to spike B's.

- `vad.rs`: silero VAD wrapper returning sorted, merged speech regions in samples.
- `chunker.rs`: pause-aligned layout. From the first speech region, each chunk
  targets `--target-seconds` (default 25). Inside a window of `--search-seconds`
  (4 s) either side of the target the longest VAD pause wins and the cut lands at its
  midpoint; with no pause in the window the quietest 100 ms frame wins (energy
  fallback). A pause of at least `--long-pause-seconds` (3 s) before the window
  closes ends the chunk early at the pause start plus 0.25 s and the next chunk
  starts 0.25 s before the following speech, so long silence is never decoded. A
  chunk start that lands in a pause snaps forward to the next speech. Consecutive
  chunks share `--overlap-seconds` (1.5 s). Every chunk is clamped to 190 s (the
  export aborts at 2500 encoder frames, about 200 s) and to at least 2 s.
- `merge.rs`: the LCS overlap merger ported from `spikes/coreml-rs/src/pipeline.rs`
  (`merge_windows`, `merge_by_midpoint`, splice-safe pieces, monotonic timestamps).
  Tokens are sherpa-onnx pieces with absolute seconds instead of vocabulary ids with
  frame indices; a match is the same piece within `overlap / 2` seconds. Also a
  piece-sequence similarity (normalised LCS) used by the voting.
- `lang.rs`: SLID wrapper; `sample_windows` picks `--slid-windows` (24) windows of
  `--slid-window-seconds` (9 s) spread evenly over speech time, not file time; the
  vote keeps every language with at least 12 % of the windows and at least two
  votes (falls back to the plurality language). `text_lang` wraps whatlang.
- `main.rs`: `transcribe_vad` runs the stages and records per-stage wall time,
  segment counts, cut kinds, SLID votes, lane set, flagged and changed counts and a
  per-segment record in the JSON (`asr.stages`). `--vote` enables stage 3: a segment
  whose text language is outside the lane set is re-decoded at both boundaries
  shifted by `--vote-shift-seconds` (2 s) earlier and later; the three candidates are
  ranked by (language in the lane set, agreement = summed similarity with the other
  two, text-language confidence). An out-of-set candidate never replaces the
  original, so when no candidate is in set the segment is left alone.
- `scripts/run-chunker-forge.sh`: the matrix below. `scripts/score.py` gained the
  segment, decoded-seconds, lane and flagged/changed columns and prints the maximum
  WER next to the mean. `scripts/diff_words.py` classifies the remaining word
  differences (compound split or join, shared stem, other substitution, deletion or
  insertion runs) for the analysis below; its verbose mode is for local inspection
  only.

## Deviations from the brief and what failed

- **No token confidence.** `SherpaOnnxOfflineRecognizerResult` in 1.12.9 has
  `text`, `tokens`, `timestamps`, `json`, `lang`, `emotion`, `event` and no
  per-token score, so the "low mean confidence" flag could not be implemented. The
  only segment-level flag is text language outside the lane set; the "highest
  confidence" tie-break uses whatlang's confidence on the candidate text, which is a
  text-detection score, not an acoustic one, and is ranked after agreement.
- **Segments shorter than four words are never flagged** (whatlang is unreliable
  below that), so a two-word English aside inside a German lane is not touched, and a
  flipped segment whose candidates all come back under four words keeps the original.
- **Empty-decode recovery was added mid-spike** (stage 3b below) after stage 1
  showed two consecutive chunks on one file decoding to zero tokens. It is a second
  flag in the same voting pass: a chunk with at least 3 s of VAD speech and fewer
  than 0.4 words per speech second is re-decoded at the two shifted boundaries and as
  two halves split at the longest pause near the middle; the candidate with the most
  words wins among those whose language is in the lane set or undecidable.
- Nothing crashed and no verbatim error appeared during this spike: the build was
  clean apart from a dead-code warning, the VAD and SLID loaded first time, and the
  190 s clamp was never reached (longest chunk under 35 s). The first `score.py`
  had a latent glob bug (`*.<tag>.json` also matched its own `score.<tag>.json` on a
  second run: `AttributeError: 'list' object has no attribute 'get'`); fixed.
- The 1-minute load during the 15 s-target run and the 10-thread run was 7 to 12
  rather than 3 to 6, from indexing of the fresh output files and one 5 s
  incremental rebuild; wall times in those two rows are upper bounds. Texts are
  deterministic: the stage 0 re-run reproduced spike B's 20 s transcripts word for
  word on every file.

## Results

All rows: Parakeet TDT 0.6b v3 int8, greedy, CPU provider, Forge. WER is against
the named engine's text; S/D/I against CoreML Parakeet. "Decoded s" is the audio
the encoder actually saw (chunks including overlap); "Load" is the 1-minute average
before the file. Wall time includes VAD, SLID and extra decodes, not model load.

### Stage 0: quiet-point splitter, 20 s chunks, 4 threads (spike B baseline, re-run)

| File | Wall s | RTFx | Load | Words ONNX/CoreML/WK | WER vs CoreML (S/D/I) | WER vs WhisperKit |
|---|---:|---:|---:|---|---:|---:|
| 2d7b9aba | 41.85 | 14.3 | 3.3 | 601/609/553 | 3.4% (7/11/3) | 13.0% |
| 5ea9e7e8 | 41.99 | 14.3 | 4.1 | 1065/1094/1080 | 8.2% (37/41/12) | 12.4% |
| 7eb51e56 | 42.14 | 14.2 | 4.8 | 1703/2022/1319 | 41.4% (447/355/36) | 79.2% |
| 83bb1859 | 41.20 | 14.6 | 5.3 | 1149/1151/1010 | 9.8% (55/30/28) | 24.9% |
| bfbeef67 | 41.82 | 14.3 | 5.3 | 569/610/608 | 19.2% (50/54/13) | 22.9% |
| c0cd3671 | 42.45 | 14.1 | 5.6 | 1366/1614/1515 | 21.4% (75/259/11) | 29.0% |
| f5fd585d | 40.76 | 14.7 | 5.1 | 596/612/595 | 22.2% (76/38/22) | 32.8% |
| mean | 41.74 | 14.4 | | | **18.0%** (max 41.4%) | 30.6% |

Identical text to spike B (17.9 % there was the same numbers before rounding).

### Stage 1: VAD chunker, 25 s target, 1.5 s overlap, LCS merge, 4 threads

| File | Segments | Speech s | Decoded s | Cuts pause/long/energy | Wall s | RTFx | Load | Words ONNX/CoreML/WK | WER vs CoreML (S/D/I) | WER vs WhisperKit |
|---|---:|---:|---:|---|---:|---:|---:|---|---:|---:|
| 2d7b9aba | 13 | 177 | 225 | 5/6/1 | 17.51 | 34.3 | 5.5 | 583/609/553 | 6.1% (9/27/1) | 13.9% |
| 5ea9e7e8 | 26 | 326 | 544 | 17/8/0 | 39.83 | 15.1 | 5.3 | 1078/1094/1080 | 8.3% (43/32/16) | 13.2% |
| 7eb51e56 | 27 | 325 | 472 | 6/20/0 | 34.46 | 17.4 | 5.4 | 1782/2022/1319 | 35.9% (407/279/39) | 78.8% |
| 83bb1859 | 25 | 378 | 526 | 16/8/0 | 37.80 | 15.9 | 5.7 | 1126/1151/1010 | 8.6% (46/39/14) | 22.9% |
| bfbeef67 | 17 | 187 | 315 | 5/11/0 | 23.00 | 26.1 | 6.0 | 598/610/608 | 16.1% (50/30/18) | 19.4% |
| c0cd3671 | 26 | 453 | 612 | 22/3/0 | 44.12 | 13.6 | 5.6 | 1393/1614/1515 | 19.3% (73/230/9) | 26.8% |
| f5fd585d | 20 | 158 | 323 | 8/11/0 | 23.55 | 25.5 | 5.1 | 580/612/595 | 21.1% (63/49/17) | 29.6% |
| mean | 22.0 | 286 | 431 | | 31.47 | 21.1 | | | **16.5%** (max 35.9%) | 29.2% |

The energy fallback fired once in 154 cuts; every other cut landed in a VAD pause.
VAD costs 0.9 to 1.9 s per file. Skipping long silence cuts decoded audio to 72 % of
the file on average and wall time by a quarter (RTFx 14.4 to 21.1). Peak RSS 2.8 GB.

### Stage 1 at other target lengths, 4 threads

| Target | Segments (mean) | Decoded s (mean) | Wall s (mean) | RTFx | Load | WER vs CoreML mean | max | WER vs WhisperKit mean |
|---|---:|---:|---:|---:|---|---:|---:|---:|
| 15 s | 32.7 | 438 | 45.45 | 14.9 | 7.7 to 12.7 | 16.1% | 36.3% | 29.1% |
| 25 s | 22.0 | 431 | 31.47 | 21.1 | 5.1 to 6.0 | 16.5% | 35.9% | 29.2% |
| 30 s | 19.4 | 430 | 37.05 | 18.0 | 6.2 to 7.8 | **15.2%** | 36.4% | 27.9% |

Per file at 15 s: 3.8 / 7.0 / 36.3 / 8.4 / 15.2 / 19.6 / 22.5 %; at 30 s: 4.9 / 6.8 /
36.4 / 8.8 / 15.1 / 11.0 / 23.5 % (file order as above). The 30 s mean is better only
because c0cd3671 happens to get boundaries that avoid its empty region (11.0 % against
19.3 % and 19.6 %); the other six files move by at most 1.4 points between targets.
Chunk length is no longer the first-order variable it was with the quiet-point cutter.

### Stage 2: lane language set (SLID, 24 windows of 9 s, whisper tiny int8)

| File | Given truth | CoreML per-segment labels (words) | SLID votes | Lane set | Agrees |
|---|---|---|---|---|---|
| 2d7b9aba | en + de mixed | en 570, de 18 | en 23, de 1 | en | with CoreML; the German share is 3 % of words and 1 of 24 windows, under the 12 % floor |
| 5ea9e7e8 | de | de 1088 | de 23, id 1 | de | yes |
| 7eb51e56 | en + de mixed | en 1945 | en 20, ko 3, ja 1 | en, ko | with CoreML on English; `ko` is a SLID error on noisy audio that the 12 % floor let through |
| 83bb1859 | de | de 1132, en 6 | de 24 | de | yes |
| bfbeef67 | de | de 602 | de 23, sv 1 | de | yes |
| c0cd3671 | de | de 1600 | de 24 | de | yes |
| f5fd585d | de | de 607, en 4 | de 23, en 1 | de | yes |

SLID takes 1.9 to 3.2 s per file at 4 threads. The five German lanes are
unambiguous. On the two team calls SLID and the CoreML reference both say English
with little or no German, against the given truth; either the German turns are short
or the Mac transcript anglicised them too, which this spike cannot tell apart. A
stray vote for an unrelated language (`ko`, `id`, `sv`, `ja`) appears on four of
seven files; production should restrict the set to languages the product supports.

### Stage 3: segment voting, 25 s target, 4 threads

3a is language flagging only (as specified); 3b adds the empty-decode flag.

| File | 3a flagged/changed | 3b flagged (lang/empty) | 3b changed (lang/empty) | Extra decodes | 3b wall s | RTFx | Load | Words | WER vs CoreML (S/D/I) | WER vs WhisperKit |
|---|---|---|---|---:|---:|---:|---:|---|---:|---:|
| 2d7b9aba | 1/0 | 1/0 | 0/0 | 2 | 20.64 | 29.1 | 3.9 | 583 | 6.1% (9/27/1) | 13.9% |
| 5ea9e7e8 | 0/0 | 0/0 | 0/0 | 0 | 41.71 | 14.4 | 4.5 | 1078 | 8.3% (43/32/16) | 13.2% |
| 7eb51e56 | 0/0 | 0/0 | 0/0 | 0 | 38.63 | 15.5 | 4.8 | 1782 | 35.9% (407/279/39) | 78.8% |
| 83bb1859 | 0/0 | 0/0 | 0/0 | 0 | 41.79 | 14.4 | 5.2 | 1126 | 8.6% (46/39/14) | 22.9% |
| bfbeef67 | 1/0 | 1/0 | 0/0 | 2 | 36.82 | 16.3 | 5.6 | 598 | 16.1% (50/30/18) | 19.4% |
| c0cd3671 | 0/0 | 0/2 | 0/1 | 8 | 71.42 | 8.4 | 5.9 | 1432 | 17.2% (78/191/9) | 24.7% |
| f5fd585d | 2/1 | 2/0 | 1/0 | 4 | 40.65 | 14.8 | 6.6 | 577 | 20.9% (61/51/16) | 30.3% |
| mean 3a | 4/1 | | | 8 | 34.89 | 18.6 | | | 16.5% (max 35.9%) | 29.3% |
| mean 3b | | 6 (4/2) | 2 (1/1) | 16 | 41.67 | 16.1 | | | **16.2%** (max 35.9%) | 29.0% |

Language flags: four segments in 154. Two were German text that whatlang called
Afrikaans (one re-decode came back as German and was taken, moving that file from
21.1 % to 20.9 %); one was a 7-word English aside in a German lane whose shifted
candidates were too short to judge, kept; one was a 13-word German opening on the
English-majority team call where all three candidates agreed, kept. Language flips
of whole chunks, which spike B saw with the quiet-point cutter, did not occur with
pause-aligned 25 s chunks.

Empty flags: the two chunks at 25 to 48 s and 47 to 69 s of c0cd3671 (21 s and 17 s
of VAD speech, zero tokens). The second recovered through the split candidate (39
words); the first returned zero tokens from all four candidates. A probe afterwards
(`--probe`, decodes of arbitrary ranges):

| Range of c0cd3671 | gain 1.0 | gain 0.3 | gain 3.0 (clipped) |
|---|---:|---:|---:|
| 25 to 48 s | 0 words | 0 | 78 |
| 25 to 36.5 s, 36.5 to 48 s, 30 to 48 s | 0 / 0 / 0 | 0 / 0 / 0 | 41 / 37 / 65 |
| 20 to 48 s | 78 | 79 | 77 |
| 10 to 48 s | 62 | 85 | 78 |
| 25 to 60 s | 117 | 119 | 117 |

The audio is ordinary (RMS 0.10, peak 0.95, no clipping); the CoreML reference has
about 150 words there. The int8 export produces nothing for any window that starts
between 25 and 37 s and ends before about 50 s, and produces normal text as soon as
the window is extended either way or the signal is scaled up. Not segmentation:
boundary shifts of 2 s and halving do not escape it, extending the window by 5 to
12 s does. This spike read it as a quantisation defect of the export; spike E
reproduced it with the fp32 export (`.plans/spikes/2026-10-01-spike-own-export.md`),
so it is not quantisation, and after spike F its cause is open. FluidAudio's
recovery policies (`spikes/coreml-rs/src/pipeline.rs`: full-length encoder,
padded preprocessor, trimmed tail) exist for the same symptom on CoreML.

### Final configuration at 10 threads (stage 3b, 25 s)

| File | Wall s | RTFx | Load | WER vs CoreML |
|---|---:|---:|---:|---:|
| 2d7b9aba | 19.44 | 30.9 | 6.5 | 6.1% |
| 5ea9e7e8 | 37.19 | 16.1 | 8.3 | 8.3% |
| 7eb51e56 | 33.51 | 17.9 | 9.5 | 35.9% |
| 83bb1859 | 34.21 | 17.5 | 10.6 | 8.6% |
| bfbeef67 | 26.42 | 22.7 | 12.0 | 16.1% |
| c0cd3671 | 50.71 | 11.8 | 11.5 | 17.2% |
| f5fd585d | 25.11 | 23.9 | 11.6 | 20.9% |
| mean | 32.37 | 20.1 | | 16.2% |

Same text as 4 threads on every file. 10 threads buy 22 % wall time over 4 threads
under a load of 7 to 12 (a clean 10-thread number is still owed, as in spike B).
Peak RSS 2.8 GB; 3.2 GB with SLID loaded.

### What the remaining 16 % is

Word-level classes of the stage 3b differences against CoreML (counts of words):

| File | CoreML words | Compound split/join | Shared stem (inflection) | Other single substitution | Deletions in runs of 5+ | Other deletions | Insertions in runs of 5+ | Other insertions |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| 2d7b9aba | 609 | 0 | 0 | 14 | 12 | 10 | 0 | 1 |
| 5ea9e7e8 | 1094 | 5 | 4 | 33 | 5 | 30 | 5 | 9 |
| 7eb51e56 | 2022 | 2 | 6 | 360 | 235 | 71 | 72 | 24 |
| 83bb1859 | 1151 | 2 | 7 | 41 | 7 | 30 | 6 | 6 |
| bfbeef67 | 610 | 2 | 8 | 56 | 9 | 12 | 0 | 11 |
| c0cd3671 | 1614 | 6 | 6 | 82 | 157 | 25 | 0 | 2 |
| f5fd585d | 612 | 0 | 5 | 63 | 10 | 40 | 0 | 10 |

Three buckets:

1. **One file nobody agrees on.** 7eb51e56 is 36 % against CoreML and 79 % against
   WhisperKit; CoreML and WhisperKit disagree with each other by 50 % on it, and
   SLID hears noise. It contributes 2.8 points of the 16.2 % mean on its own. The
   CoreML reference is not trustworthy enough there to measure against.
2. **Scattered single-word substitutions on every German file**: 33 to 82 per file
   (inflection endings, article and filler swaps, near-homophones, spellings of
   product and person names, anglicised versus German spellings), plus single
   deletions and insertions of short function words and fillers. These are 6 to 12
   points per file and are the int8 model disagreeing with the fp16 CoreML model on
   the same audio, chunk boundaries far away. No chunker or voting touches them.
3. **The empty-decode defect**: 157 words in runs on c0cd3671 after recovery, about
   10 points on that file.

## Verdict against G1 (mean under 8 %, no file over 15 %)

**Fails.** Best mean 15.2 % (chunker only, 30 s target) or 16.2 % for the full
pipeline at 25 s; five of seven files are over 15 % at some stage and three stay
over 15 % everywhere. Against the 18.0 % baseline:

| Stage | Mean WER vs CoreML | Max | Mean WER vs WhisperKit | RTFx (4 threads) |
|---|---:|---:|---:|---:|
| 0: quiet-point 20 s | 18.0% | 41.4% | 30.6% | 14.4 |
| 1: VAD chunker 25 s + LCS merge | 16.5% | 35.9% | 29.2% | 21.1 |
| 1 at 15 s / 30 s | 16.1% / 15.2% | 36.3% / 36.4% | 29.1% / 27.9% | 14.9 / 18.0 |
| 2: lane set (prior only) | no text change | | | |
| 3a: language voting | 16.5% | 35.9% | 29.3% | 18.6 |
| 3b: plus empty-decode recovery | 16.2% | 35.9% | 29.0% | 16.1 (20.1 at 10 threads) |

The chunker is worth having: it removes the chunk-length sensitivity (16.1 to
16.5 % across 15 to 30 s against 17.9 to 22.1 % before), cuts a quarter of the wall
time by skipping silence, removes the 200 s abort by construction, and the LCS
merge produced no visible seam damage (the energy fallback fired once in 154 cuts).
Language voting is not worth much on this corpus: pause-aligned chunks do not flip
language, so there is nothing to vote on, and the lane prior's only use was to
reject a text-detector confusion. The two findings that matter for the plan:

- The remaining disagreement is model-level (bucket 2) and the zero-token window
  (bucket 3). Both pointed at decision 3 of the parent plan, our own export, as the
  next variable, and the chunker and recovery code here is the harness spikes E and F
  measured it with. Excluding the untrustworthy file, the floor this pipeline reaches
  with the stock export is 12.9 % mean, still above the gate as it stood.
- Any ONNX pipeline needs an empty-decode recovery that *extends* the window
  (FluidAudio-style policies), not only shifts it; the 2 s shifts and halving that
  were specified here did not escape the defect, a 5 to 12 s extension did.

Not done in the time box: a clean 10-thread timing, the window-extension recovery
policy, restricting SLID to an allowlist, and whisper cross-engine voting on
flagged segments (nothing was flagged that it would have helped).

