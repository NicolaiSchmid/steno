# Spike E: own ONNX export of Parakeet TDT 0.6B v3 (fp32 and int8)

Status: measured 2026-10-01, time-boxed to about 2 hours. Parent:
`.plans/2026-10-01-cross-platform-speech-stack.md` (decision 3, gate G1). Verdict
below is against G1 as first defined (agreement with CoreML); spike F restated G1 as
absolute WER and the ONNX path passed. Previous
spikes: `.plans/spikes/2026-10-01-spike-onnx-speech.md` (setup),
`.plans/spikes/2026-10-01-spike-chunker-voting.md` (the harness and the 16 % floor
this spike starts from). Code: `spikes/onnx-speech/export/` (export scripts and
README with the exact commands), `spikes/onnx-speech/src/main.rs` (harness change),
`spikes/onnx-speech/scripts/run-own-export-forge.sh` and `words_per_minute.py`.

## Question

Spike D left 16.2 % mean word disagreement between the stock sherpa-onnx int8 export
of Parakeet TDT 0.6B v3 and the CoreML transcript, and blamed what remained on the
model level: scattered single-word substitutions on German, one file no engine agrees
on, and a window that decodes to zero tokens. Does our own export, fp32 and int8,
close that gap? Gate G1: mean word error rate (WER) vs CoreML under 8 %, no file over
15 %.

## Setup

| Item | Value |
|---|---|
| Export host | atlas (Ryzen 7 7700, 12 threads, 50 GB). Docker `python:3.11`, CPU torch wheel. `torch 2.14.1+cpu`, `nemo_toolkit[asr] 3.0.0`, `onnx 1.23.1`, `onnxruntime 1.30.0` (quantiser), `onnxsim 0.7.3` (unused). The sherpa-onnx recipe pins `onnx 1.17.0`, `onnxruntime 1.17.1`, `numpy<2`; the newer stack worked unchanged because NeMo pins the legacy TorchScript exporter (`dynamo=False`) and writes IR version 8, opset 17, the same as the stock export |
| Checkpoint | `nvidia/parakeet-tdt-0.6b-v3` (`parakeet-tdt-0.6b-v3.nemo`, 2.4 GB, CC-BY-4.0), downloaded in about 20 s |
| Recipe | `k2-fsa/sherpa-onnx` `scripts/nemo/parakeet-tdt-0.6b-v3/export_onnx.py` (Apache-2.0) adapted as `spikes/onnx-speech/export/export_parakeet_v3.py`; metadata keys identical to the stock export plus `max_frames` and `exported_by` |
| Measurement host | Forge, M4 Pro (10 cores, 24 GB), macOS 26.7. Harness `spikes/onnx-speech` as left by spike D, `sherpa-rs-sys` 0.6.8 with sherpa-onnx v1.12.9 and its bundled onnxruntime 1.17.1, `CARGO_BUILD_JOBS=4`, incremental rebuild 2.3 s. Stock models in `~/steno-spikes/onnx/models/`, own exports under `~/steno-spikes/models-own/<variant>/`, transferred with rsync over the tailnet |
| Configuration | spike D's final pipeline for every row: VAD chunker, 25 s target, 1.5 s overlap, LCS merge, SLID lane prior, language and empty-decode voting (stage 3b), greedy search, CPU provider, 4 threads. Scored with `scripts/score.py` against the CoreML Parakeet and WhisperKit texts of the baseline bake-off |
| Control | the stock int8 re-run with the rebuilt harness reproduced spike D's stage 3b text word for word on all seven files |

Measurement conditions: Forge 1-minute load 8 to 14 during the fp32 and int8 rows
(Spotlight indexing the fresh 2.4 GB files and the SSH log tail) and 5 to 7 during the
stock row, so the fp32 and int8 wall times are upper bounds; the Load column records
the average before each file. atlas, used for the export only, ran at load 22 to 55
from other users.

### Harness change

`--asr-dir <dir>` and `--asr-files int8|fp32` select the model directory and the file
set (`*.int8.onnx` or `*.onnx`). The fp32 encoder is 2.4 GB, over protobuf's 2 GB
limit, so it is saved with all initializers in one external file `encoder.weights`.
sherpa-onnx 1.12.9 reads each model file into memory and creates the ORT session
from the buffer (`offline-transducer-nemo-model.cc`, `ReadFile` then
`Ort::Session(env, data, len)`), so ORT has no model path and resolves the external
location against the process working directory. The harness therefore changes into
the model directory while the recognizer is created and restores the CWD afterwards.
With that, the fp32 model loaded first time (4.63 s, 2.0 GB RSS after load against
2.9 s and 2.6 GB for int8).

## The export

| Variant | Encoder | Decoder | Joiner | Total on disk | How |
|---|---:|---:|---:|---:|---|
| stock int8 (release `asr-models`, 2025-08) | 652 MB | 11.8 MB | 6.4 MB | 670 MB | sherpa-onnx recipe, ORT 1.17.1 `quantize_dynamic` |
| own fp32 | 83 MB graph + 2435 MB `encoder.weights` | 47 MB | 25 MB | 2590 MB | NeMo `export()`, re-saved with one external file |
| own int8 | 693 MB | 11.8 MB | 6.4 MB | 711 MB | recipe quantisation: `quantize_dynamic`, QUInt8 encoder weights, QInt8 decoder and joiner, per-tensor |
| own int8-pc | 695 MB | 11.8 MB | 6.4 MB | 713 MB | same with `per_channel=True` |
| own int8-noattn | 1071 MB | 11.8 MB | 6.4 MB | 1089 MB | per-tensor, the 192 `self_attn` MatMuls (of 289) excluded; only feed-forward MatMuls quantised |

Export time on the loaded atlas: model load 20 s, encoder trace 55 s, decoder and
joiner 5 s, fp32 save 27 s, each encoder quantisation 32 to 50 s; the whole script
4 minutes. Static (calibrated) quantisation was not attempted: it needs encoder
inputs from the NeMo preprocessor for a calibration set and `quantize_static` with
a calibration reader, about an hour of work, and the results below made it moot.

### The position table and the "2500-frame cap"

NeMo's `RelPositionalEncoding` holds a table of `2 * pos_emb_max_len - 1` rows
(`pos_emb_max_len = 5000` for this checkpoint, so 9999 rows of 1024 fp32, 41 MB,
baked into the graph as a `Constant` node) and the forward slices
`pe[:, center - T : center + T - 1]` for `T` encoder frames. For `T > 5000` the
start index goes negative, Python wraps it, and the slice comes out 2500 rows long
instead of `2T - 1`; the attention `Add` then fails with spike B's
"Attempting to broadcast an axis by a dimension other than 1. 2500 by 7500" (7500
frames = 600 s). The cap is therefore 5000 encoder frames = 400 s, not 2500 = 200 s
as spike B inferred and spike D's 190 s clamp assumed. Verified on Forge with the
stock export: a 0 to 400 s probe decodes (947 words), 0 to 401 s aborts with that
error. The own fp32 export with a 10000-frame table decodes 0 to 401 s (1010 words)
and the whole 600 s file in one window (1549 words), at 13.2 GB peak RSS for the
600 s window.

The table is enlarged before tracing with `encoder.set_max_audio_length(N)`. The own
exports carry `N = 10000` (19999 rows, 82 MB, the 41 MB difference between stock and
own int8 encoder sizes). The slice itself is dynamic in the graph, so the table is
the only limit; a fully dynamic table would mean computing the sinusoids in-graph,
which is not needed for a chunked pipeline. Attention memory grows with `T^2`, so
long windows are a memory question before they are a table question.

## Results

WER against the named engine after lowercasing and stripping punctuation; S/D/I
against CoreML Parakeet. Wall time includes VAD, SLID and the extra decodes, not
model load.

### Stock int8 (control; identical text to spike D stage 3b)

| File | Wall s | RTFx | Load | Peak RSS MB | Words ONNX/CoreML/WK | WER vs CoreML (S/D/I) | WER vs WhisperKit |
|---|---:|---:|---:|---:|---|---:|---:|
| 2d7b9aba | 23.72 | 25.3 | 5.3 | 2634 | 583/609/553 | 6.1% (9/27/1) | 13.9% |
| 5ea9e7e8 | 43.06 | 13.9 | 6.0 | 2634 | 1078/1094/1080 | 8.3% (43/32/16) | 13.2% |
| 7eb51e56 | 37.51 | 16.0 | 7.5 | 2634 | 1782/2022/1319 | 35.9% (407/279/39) | 78.8% |
| 83bb1859 | 46.08 | 13.0 | 7.3 | 2634 | 1126/1151/1010 | 8.6% (46/39/14) | 22.9% |
| bfbeef67 | 33.44 | 17.9 | 6.9 | 2634 | 598/610/608 | 16.1% (50/30/18) | 19.4% |
| c0cd3671 | 61.52 | 9.8 | 7.0 | 2634 | 1432/1614/1515 | 17.2% (78/191/9) | 24.7% |
| f5fd585d | 42.91 | 14.0 | 7.2 | 2634 | 577/612/595 | 20.9% (61/51/16) | 30.3% |
| mean | 41.18 | 15.7 | | | | **16.2%** (max 35.9%) | 29.0% |

### Own fp32

| File | Wall s | RTFx | Load | Peak RSS MB | Words ONNX/CoreML/WK | WER vs CoreML (S/D/I) | WER vs WhisperKit |
|---|---:|---:|---:|---:|---|---:|---:|
| 2d7b9aba | 28.36 | 21.2 | 8.3 | 3197 | 596/609/553 | 4.8% (4/19/6) | 14.1% |
| 5ea9e7e8 | 32.74 | 18.3 | 11.8 | 3197 | 1082/1094/1080 | 4.7% (19/22/10) | 8.0% |
| 7eb51e56 | 28.41 | 21.1 | 9.7 | 3197 | 1932/2022/1319 | 22.3% (262/139/49) | 74.6% |
| 83bb1859 | 33.71 | 17.8 | 8.4 | 3197 | 1155/1151/1010 | 7.0% (41/18/22) | 21.6% |
| bfbeef67 | 27.95 | 21.5 | 12.4 | 3197 | 592/610/608 | 11.3% (33/27/9) | 15.6% |
| c0cd3671 | 61.92 | 9.7 | 13.9 | 3201 | 1484/1614/1515 | 14.1% (68/145/15) | 22.3% |
| f5fd585d | 30.97 | 19.4 | 14.3 | 3319 | 597/612/595 | 16.2% (40/37/22) | 25.0% |
| mean | 34.87 | 18.4 | | | | **11.5%** (max 22.3%) | 25.9% |

### Own int8 (recipe quantisation, per-tensor)

| File | Wall s | RTFx | Load | Peak RSS MB | Words ONNX/CoreML/WK | WER vs CoreML (S/D/I) | WER vs WhisperKit |
|---|---:|---:|---:|---:|---|---:|---:|
| 2d7b9aba | 25.74 | 23.3 | 12.0 | 2727 | 578/609/553 | 6.9% (9/32/1) | 13.7% |
| 5ea9e7e8 | 47.49 | 12.6 | 10.2 | 2728 | 1079/1094/1080 | 8.9% (46/33/18) | 13.5% |
| 7eb51e56 | 36.36 | 16.5 | 8.8 | 2733 | 1781/2022/1319 | 36.3% (416/279/38) | 78.5% |
| 83bb1859 | 41.03 | 14.6 | 7.5 | 2734 | 1126/1151/1010 | 8.8% (48/39/14) | 22.8% |
| bfbeef67 | 25.08 | 23.9 | 6.7 | 2737 | 587/610/608 | 14.9% (48/33/10) | 18.1% |
| c0cd3671 | 56.37 | 10.6 | 6.1 | 2737 | 1438/1614/1515 | 16.7% (77/184/8) | 24.2% |
| f5fd585d | 28.84 | 20.8 | 6.3 | 2737 | 582/612/595 | 21.6% (64/49/19) | 31.1% |
| mean | 37.27 | 17.5 | | | | **16.3%** (max 36.3%) | 28.8% |

The own int8 reproduces the stock export within noise: every file differs from the
stock text by 1 to 10 words (different quantiser version, ORT 1.30 against 1.17.1,
and the larger position table), and the mean is 16.3 % against 16.2 %. The recipe is
reproducible; the stock release is not a bad build of it.

### Own int8-pc (per-channel weight scales)

| File | Wall s | RTFx | Load | Peak RSS MB | Words ONNX/CoreML/WK | WER vs CoreML (S/D/I) | WER vs WhisperKit |
|---|---:|---:|---:|---:|---|---:|---:|
| 2d7b9aba | 27.85 | 21.5 | 6.8 | 2649 | 598/609/553 | 3.1% (6/12/1) | 12.1% |
| 5ea9e7e8 | 53.94 | 11.1 | 6.5 | 2649 | 1079/1094/1080 | 8.0% (39/32/17) | 12.1% |
| 7eb51e56 | 37.24 | 16.1 | 7.3 | 2649 | 1792/2022/1319 | 35.5% (416/266/36) | 78.0% |
| 83bb1859 | 41.88 | 14.3 | 6.7 | 2649 | 1126/1151/1010 | 8.7% (47/39/14) | 22.2% |
| bfbeef67 | 25.52 | 23.5 | 6.1 | 2649 | 587/610/608 | 13.6% (42/32/9) | 18.9% |
| c0cd3671 | 60.77 | 9.9 | 5.8 | 2649 | 1456/1614/1515 | 15.9% (79/168/10) | 23.5% |
| f5fd585d | 35.83 | 16.7 | 6.6 | 2649 | 587/612/595 | 21.2% (55/50/25) | 29.9% |
| mean | 40.43 | 16.2 | | | | **15.2%** (max 35.5%) | 28.1% |

### Own int8-noattn (self-attention MatMuls in fp32)

| File | Wall s | RTFx | Load | Peak RSS MB | Words ONNX/CoreML/WK | WER vs CoreML (S/D/I) | WER vs WhisperKit |
|---|---:|---:|---:|---:|---|---:|---:|
| 2d7b9aba | 29.67 | 20.2 | 6.8 | 3009 | 579/609/553 | 6.7% (9/31/1) | 13.7% |
| 5ea9e7e8 | 41.35 | 14.5 | 8.0 | 3009 | 1077/1094/1080 | 8.6% (45/33/16) | 12.9% |
| 7eb51e56 | 35.66 | 16.8 | 6.9 | 3009 | 1780/2022/1319 | 36.0% (413/278/36) | 77.9% |
| 83bb1859 | 37.83 | 15.9 | 5.9 | 3009 | 1133/1151/1010 | 8.2% (44/34/16) | 22.3% |
| bfbeef67 | 24.45 | 24.5 | 5.9 | 3009 | 588/610/608 | 13.3% (39/32/10) | 17.9% |
| c0cd3671 | 54.46 | 11.0 | 6.1 | 3009 | 1442/1614/1515 | 16.7% (79/181/9) | 24.2% |
| f5fd585d | 26.79 | 22.4 | 6.0 | 3009 | 583/612/595 | 21.1% (58/50/21) | 30.1% |
| mean | 35.75 | 17.9 | | | | **15.8%** (max 36.0%) | 28.4% |

Keeping two thirds of the MatMul weights in fp32 buys 0.5 points over the plain
int8 and costs 380 MB on disk and 280 MB of RSS; the substitutions are spread over
the feed-forward blocks too, not concentrated in attention.

### Summary per variant

| Variant | Disk | Load s | Peak RSS MB | Wall s mean | RTFx (4 threads) | WER vs CoreML mean | max | WER vs WhisperKit mean |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| stock int8 (control) | 670 MB | 2.1 to 2.9 | 2634 | 41.18 | 15.7 | 16.2% | 35.9% | 29.0% |
| own fp32 | 2590 MB | 4.6 to 5.0 | 3319 | 34.87 | 18.4 | **11.5%** | 22.3% | 25.9% |
| own int8 | 711 MB | 2.4 | 2737 | 37.27 | 17.5 | 16.3% | 36.3% | 28.8% |
| own int8-pc | 713 MB | 2.9 | 2649 | 40.43 | 16.2 | 15.2% | 35.5% | 28.1% |
| own int8-noattn | 1089 MB | 4.7 | 3009 | 35.75 | 17.9 | 15.8% | 36.0% | 28.4% |

fp32 is not slower than int8 on the M4 Pro at 4 threads: wall time is within the
load noise of the int8 rows and on three files clearly faster. ORT 1.17.1's dynamic
int8 path (`DynamicQuantizeLinear` plus `MatMulInteger` plus the rescale, 217 of them
per pass) pays for quantising activations on every call, and the arm64 fp32 GEMM is
good enough that the 4x smaller weights do not win it back. The int8 advantage is
disk and download size, not speed, on this CPU. Mean WER excluding 7eb51e56: stock
12.9 %, fp32 9.7 %, int8-pc 11.8 %.

## The three specific questions

### 1. Does the 23 s zero-token window decode with fp32?

No, and that overturns spike D's diagnosis. The 25 to 48 s window of c0cd3671
returns zero tokens from the stock int8 **and** from our fp32 on Forge, and from
the stock int8 through the same harness built on atlas (x86). It is not quantisation
and not the arm64 kernels. The same window, the same stock int8 file and the same
greedy TDT decode loop in a numpy script (`spikes/onnx-speech/export/ort_decode.py`)
against onnxruntime directly give 67 to 72 words:

| Path | ORT | Platform | Model | 25 to 48 s | 20 to 48 s |
|---|---|---|---|---:|---:|
| sherpa-onnx 1.12.9 recognizer (harness) | 1.17.1 | Forge arm64 | stock int8 | 0 words | 78 |
| sherpa-onnx 1.12.9 recognizer (harness) | 1.17.1 | Forge arm64 | own fp32 | 0 | 78 |
| sherpa-onnx 1.12.9 recognizer (harness) | 1.17.1 | atlas x86 | stock int8 | 0 | 80 |
| numpy TDT greedy decode | 1.30.0 | atlas x86 | stock int8 | 68 | |
| numpy TDT greedy decode | 1.17.1 | atlas x86 | stock int8 | 70 | |
| numpy TDT greedy decode | 1.17.1 | atlas x86 | own fp32 | 57 | |

The numpy decode is robust on this clip: biased or unbiased standard deviation in
the per-feature normalisation, librosa or Kaldi mel filters, dither on or off, DC
removal on or off, `high_freq` 8000 or Nyquist, `snip_edges` either way, pre-emphasis
0 or 0.97 all give 67 to 72 words, and the encoder output has no NaN or inf. With
sherpa-onnx's exact NeMo feature settings (`offline-stream.cc`: dither 0, no
pre-emphasis, no DC removal, Hann, `snip_edges` false, 0 to 8000 Hz, librosa mels,
per-feature normalisation) it gives 72 words. This spike therefore placed the
remaining difference inside sherpa-onnx's recognizer glue
(`offline-recognizer-transducer-nemo-impl.h` and the TDT decoder), which was not
debugged. Both the stock and the own models trip it, so in the fp32
row c0cd3671 still loses about 110 words in runs (about 7 points of its 14.1 %), and
the two empty-decode flags fire exactly as in spike D.

Correction (spike F, same day, `.plans/spikes/2026-10-01-spike-fleurs-wer.md`): the
numpy loop used as the control here produces garbled text (64.4 % WER on FLEURS)
while keeping 85 to 90 % of the word count, so its 57 to 72 words do not show a
correct decode and the attribution to the glue is unverified. What stands: the window
is not quantisation and not the arm64 kernels; its cause is open. The plan's response
(an own decode loop validated against FLEURS, or sherpa-onnx 1.12.15 re-tested on this
window) is in the parent plan under decision 5 and WP2. The window-extension recovery
from spike D stays as a guard.

### 2. Do the scattered German single-word substitutions go away with fp32?

They shrink by about a third and do not go away. Word-level classes of the
differences against CoreML (`scripts/diff_words.py`), stock int8 against own fp32:

| File | CoreML words | Other single substitutions stock / fp32 | Inflection stock / fp32 | Single deletions stock / fp32 | Single insertions stock / fp32 | Deletion runs of 5+ stock / fp32 |
|---|---:|---:|---:|---:|---:|---:|
| 2d7b9aba | 609 | 14 / 4 | 0 / 0 | 10 / 6 | 1 / 5 | 12 / 12 |
| 5ea9e7e8 | 1094 | 33 / 20 | 4 / 3 | 30 / 13 | 9 / 6 | 5 / 7 |
| 7eb51e56 | 2022 | 360 / 275 | 6 / 9 | 71 / 68 | 24 / 17 | 235 / 52 |
| 83bb1859 | 1151 | 41 / 41 | 7 / 3 | 30 / 11 | 6 / 7 | 7 / 7 |
| bfbeef67 | 610 | 56 / 36 | 8 / 2 | 12 / 8 | 11 / 6 | 9 / 15 |
| c0cd3671 | 1614 | 82 / 74 | 6 / 3 | 25 / 30 | 2 / 5 | 157 / 112 |
| f5fd585d | 612 | 63 / 40 | 5 / 6 | 40 / 24 | 10 / 18 | 10 / 11 |

A sample of the fp32 differences on one German 1:1 file, described generically:
inflection endings on nouns and adjectives; article and preposition near-homophones;
filler words and back-channels present in one transcript and not the other; two
spellings of the same product name, and the anglicised against the German spelling
of a loanword; a short back-channel rendered in English by one engine and German by
the other; dropped stutter repetitions. These are the same kinds spike D listed;
fp32 removes the ones that were purely quantisation noise and leaves the ones where
two implementations of the same fp16/fp32 model legitimately pick different
near-equal candidates. The CoreML reference is itself an fp16 model with its own
chunking and its own TDT decoder, so some of this residue is the reference moving,
not the ONNX path being wrong.

### 3. Where in time do the engines diverge on 7eb51e56?

Words per minute of audio (word start times; CoreML and WhisperKit from the
bake-off `wordTimings`, the ONNX rows from the harness):

| Minute | CoreML | WhisperKit | stock int8 | own fp32 | own int8-pc |
|---|---:|---:|---:|---:|---:|
| 0-1 | 95 | 7 | 55 | 71 | 65 |
| 1-2 | 227 | 203 | 211 | 229 | 212 |
| 2-3 | 214 | 152 | 196 | 200 | 196 |
| 3-4 | 318 | 179 | 310 | 306 | 311 |
| 4-5 | 182 | 73 | 149 | 179 | 151 |
| 5-6 | 152 | 98 | 119 | 141 | 123 |
| 6-7 | 201 | 189 | 169 | 195 | 176 |
| 7-8 | 170 | 70 | 142 | 169 | 133 |
| 8-9 | 232 | 215 | 213 | 220 | 196 |
| 9-10 | 154 | 99 | 144 | 157 | 143 |
| total | 1945 | 1285 | 1708 | 1867 | 1706 |

CoreML and our fp32 export agree within 1 to 10 % in every minute but the first
(totals 1945 and 1867); the stock and own int8 exports are 10 to 40 % low in the
same minutes, worst where the audio is noisiest (minutes 0 to 1, 4 to 6, 7 to 8). WhisperKit is the
outlier: 7 words in the first minute, 73 and 70 in minutes 4 to 5 and 7 to 8, 1285
in total, with its drop-outs in the same minutes where the Parakeets are dense. On
this noisy mixed-language team call the untrustworthy engine is WhisperKit, and the
22 % fp32 figure against CoreML is a real but smaller disagreement (275 single
substitutions and 52 deleted words in runs) between two Parakeet implementations.

## Licensing

Parakeet TDT 0.6B v3 is CC-BY-4.0 (NVIDIA). Shipping an export derived from it
requires attribution in the app and the download: the model name, NVIDIA as the
author, a link to the Hugging Face model card and to the CC-BY-4.0 licence text, and
a note that the ONNX files are a converted form. The export recipe is Apache-2.0
(Xiaomi, sherpa-onnx); the adapted script in `spikes/onnx-speech/export/` keeps that
notice. Nothing in this spike changes the position that audio never leaves the device.

## Verdict against G1 (mean under 8 %, no file over 15 %)

**Fails, by less.** Our fp32 export takes the mean from 16.2 % to 11.5 % and
the worst file from 35.9 % to 22.3 %; three files are now under 8 % (4.7, 4.8, 7.0)
and only two are over 15 % (7eb51e56 at 22.3 %, f5fd585d at 16.2 %), against four
over 15 % with the stock export. No int8 variant moves the needle: the recipe
reproduces the stock numbers (16.3 %), per-channel scales gain one point (15.2 %),
keeping attention in fp32 gains half a point (15.8 %). Quantisation of this model
with `quantize_dynamic` costs about 4 to 5 points of agreement on German, whatever
the knobs; a calibrated static int8 was not tried and is the only quantisation
experiment left.

The specific findings change the parent plan more than the numbers do:

1. Spike D's "int8 export defect" (the window that decodes to zero tokens) is not in
   the export. It reproduces with fp32, on x86 and arm64, through sherpa-onnx 1.12.9.
   The control that seemed to clear it is itself broken (correction above), so where
   the defect lives is open. Fixing it is worth about 7 points on c0cd3671 and would
   put the fp32 mean near 10.5 %.
2. The 400 s cap of the stock export (not 200 s) is a position-table size and the
   own export carries 800 s; the chunker's 190 s clamp can be relaxed, though the
   `T^2` attention memory (13 GB peak for a 600 s window) means 25 to 60 s segments
   stay the right design.
3. 7eb51e56 is a file where CoreML and our fp32 agree minute by minute and
   WhisperKit drops two thirds of several minutes; its 22 % is two Parakeet
   implementations disagreeing on noisy mixed-language audio, not a broken ONNX
   path, and CoreML is not a reference anyone should hold the ONNX path to at 15 %
   there.

With the fp32 export, the zero-token window fixed, and 7eb51e56 scored against a
reference both engines can agree on, G1's mean is within reach (about 9 to 10 % on
the six trustworthy files today, before that fix) and the per-file ceiling of
15 % would hold on all six. The remaining 4 to 5 points over "identical" are two
implementations of one model picking different near-equal German word forms, which
decision 3 already accepts ("identical text is not a goal").

## What remains

As written at the close of this spike; spike F settled G1 by absolute WER the same
evening and overtook the first and third items.

- Decide whether the ONNX engine drives onnxruntime directly from Rust instead of
  through sherpa-onnx's recognizer: the numpy decode loop is small and a sidecar (WP2)
  would own the sessions anyway, but it must first be shown to decode correctly. Test
  sherpa-onnx 1.12.15 on the zero-token window if the wrapper is kept.
- Re-score c0cd3671 once the defect is gone; the fp32 row is at 14.1 % with about
  7 points of empty-decode damage.
- 7eb51e56: measure against our fp32 export as the reference for the ONNX path,
  or drop it from the gate set; no engine, CoreML included, is a trustworthy
  reference on it.
- fp32 costs 2.6 GB on disk and 3.2 GB peak RSS against 0.7 GB and 2.6 GB for int8,
  for a faster and better transcript on this CPU. If the download size matters, a
  calibrated static int8 or an fp16-weights-with-cast variant is the next
  experiment; an fp16 release exists for v2 in the sherpa-onnx assets and would be
  the cheaper first test.
- A clean 10-thread timing and an idle-laptop measurement (G2) are still owed from
  spikes B and D.
