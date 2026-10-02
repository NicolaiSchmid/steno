# Spike B: Parakeet TDT v3 and pyannote through sherpa-onnx on CPU

Status: measured 2026-10-01, time-boxed to about 90 minutes; two rows marked "not measured" were still running when the box closed. Parent:
`.plans/2026-10-01-cross-platform-spikes.md`. Code: `spikes/onnx-speech/`.

## Question

If Steno ran on Linux and Windows with ONNX models on CPU through sherpa-onnx from
Rust, how would transcription and speaker diarization compare with the CoreML
pipeline (FluidAudio Parakeet v3, FluidAudio pyannote-style diarization) in agreement
and speed?

## Setup that worked

| Item | Value |
|---|---|
| Crate | `sherpa-rs-sys` 0.6.8 (crates.io, 2025-10-05), features `download-binaries` only, default features off. The safe `sherpa-rs` wrapper was not used: its `TransducerRecognizer::transcribe` returns text only (no token timestamps) and its `Diarize` hard-codes `num_threads: 1` for segmentation and embedding |
| sherpa-onnx | v1.12.9 prebuilt shared libraries, downloaded by the crate's build script from the k2-fsa GitHub release (`dist.json` in the crate pins the tag). Git HEAD of sherpa-rs (857bedd, 2026-03-08) pins v1.12.15 but is unpublished |
| onnxruntime | 1.17.1 (bundled in the sherpa-onnx archive) |
| Build tools | bindgen 0.69 needs libclang: Xcode's on Forge, `nix-build '<nixpkgs>' -A libclang.lib` on atlas. cmake is pulled in by the build script but not invoked when the prebuilt download succeeds |
| Build time | atlas: 48 s cold (`cargo build --release`, 152% CPU, includes the 20 MB download). Forge: 44 s cold with `CARGO_BUILD_JOBS=4`. Incremental 1.5 s |
| Binary size | 690 KB (Linux x86_64) / 657 KB (macOS arm64) for the spike binary, plus the shared libraries it dlopens: Linux `libonnxruntime.so` 15 MB + `libsherpa-onnx-c-api.so` 5.0 MB; macOS universal2 `libonnxruntime.1.17.1.dylib` 50 MB + `libsherpa-onnx-c-api.dylib` 8.9 MB |
| Models | `sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8`: encoder 622 MB, decoder 12 MB, joiner 6.1 MB, tokens 92 KB (464 MB compressed). No non-int8 v3 asset exists in the `asr-models` release (the `.../parakeet-tdt-0.6b-v3.tar.bz2` URL returns 404), so int8 is the only option without exporting from NeMo. `sherpa-onnx-pyannote-segmentation-3-0/model.onnx` 5.8 MB (int8 1.5 MB). Embedding `3dspeaker_speech_eres2net_sv_en_voxceleb_16k.onnx` 26 MB; `wespeaker_en_voxceleb_CAM++.onnx` 28 MB tried as a second embedding |
| Diarization pipeline | sherpa-onnx `OfflineSpeakerDiarization`: pyannote segmentation 3.0 sliding windows, speaker embeddings per local speaker, agglomerative "fast clustering" with a cosine-distance threshold. Same shape as FluidAudio's community-1 pipeline (segmentation + embedding + clustering), but FluidAudio uses pyannote's own community-1 WeSpeaker embedding and a Euclidean cut on unit embeddings at 0.8; the thresholds are therefore not directly comparable, only the counts are |
| Machines | atlas: AMD Ryzen 7 7700 desktop CPU (8 cores / 12 threads visible, 50 GB), Linux x86_64. A desktop part, faster than most Linux laptops. It was heavily loaded by other users' GitHub runners and other agent sessions throughout (1-minute load 40 to 100 on a 12-thread box), so atlas timings are upper bounds. Forge: M4 Pro (10 cores, 24 GB), macOS 26.7, shared with two other agents' Rust builds (load 2 to 6) |

Thread count is `num_threads` in the sherpa-onnx model config (onnxruntime intra-op
threads). Peak RSS is `getrusage(RUSAGE_SELF).ru_maxrss`. Load is the 1-minute
average read just before each file.

## What failed

1. **Whole-file decoding is impossible with this export.** Feeding the 600 s clip to
   the offline recognizer in one stream aborts inside onnxruntime:

   ```
   [E:onnxruntime:, sequential_executor.cc:514 ExecuteKernel] Non-zero status code returned while running Add node. Name:'/layers.0/self_attn/Add_2' Status Message: /shared/onnxruntime/core/providers/cpu/math/element_wise_ops.h:540 void onnxruntime::BroadcastIterator::Init(ptrdiff_t, ptrdiff_t) axis == 1 || axis == largest was false. Attempting to broadcast an axis by a dimension other than 1. 2500 by 7500
   fatal runtime error: Rust cannot catch foreign exceptions, aborting
   ```

   The exported encoder's relative-position table is fixed at 2500 encoder frames
   (about 200 s of audio). The C++ exception crosses the FFI boundary and kills the
   process; there is no error return. The spike therefore splits the audio into
   chunks of about 60 s (default) or 20 s, cutting at the quietest 100 ms frame within
   5 s of each boundary (`split_at_quiet_points` in `spikes/onnx-speech/src/main.rs`).
   Production would need VAD-driven segmentation, as the sherpa-onnx documentation
   recommends for Parakeet.

2. **Dynamic libraries are not found at run time.** The published crate links
   dynamically and sets no rpath. On macOS: `dyld: Library not loaded:
   @rpath/libonnxruntime.1.17.1.dylib ... Reason: no LC_RPATH's found`; the libraries
   live in `~/Library/Caches/sherpa-rs/<target>/<sha>/sherpa-onnx-v1.12.9-osx-universal2-shared/lib`
   and `DYLD_LIBRARY_PATH` has to be set inside the final command (SIP strips `DYLD_*`
   from `/bin/sh`'s environment, so `nohup sh -c` loses it). On atlas the binary's
   rpath points at `~/.cache/sherpa-rs/...`, but the prebuilt `.so` needs a system
   `libstdc++.so.6` which the host lacks (`error while loading shared libraries:
   libstdc++.so.6`); `LD_LIBRARY_PATH` to a nix gcc lib fixed it. The `static` feature
   exists (and on Linux wants `RUSTFLAGS="-C relocation-model=dynamic-no-pic"`) but was
   not tried within the time box; the Rust linking question for a shippable bundle
   stays open.

3. `sherpa-onnx` 1.12.9's `SherpaOnnxOfflineRecognizerResult` has no `durations`
   field (added later), so word end times are not available from this crate version.

4. The `sherpa-rs` safe API and its `parakeet.rs` example target Parakeet v2; v3 works
   with the same `model_type = "nemo_transducer"` configuration.

## Results

All runs: Parakeet TDT 0.6b v3 int8, greedy search, `provider = cpu` unless stated.
WER is the ONNX text scored against the named engine's text after lowercasing,
stripping punctuation and collapsing whitespace (`spikes/onnx-speech/scripts/score.py`).
S/D/I are substitutions, deletions and insertions against CoreML Parakeet. "Load" is the
1-minute load average just before the file ran.

### Transcription, Forge (M4 Pro, 10 cores), CPU provider, 4 threads, 60 s chunks

| File | Wall s | RTFx | Load | Peak RSS MB | Words ONNX/CoreML/WhisperKit | WER vs CoreML (S/D/I) | WER vs WhisperKit |
|---|---:|---:|---:|---:|---|---:|---:|
| 2d7b9aba | 45.09 | 13.3 | 5.4 | 2972 | 445/609/553 | 29.6% (10/167/3) | 31.3% |
| 5ea9e7e8 | 46.59 | 12.9 | 5.6 | 2972 | 1054/1094/1080 | 9.0% (45/47/7) | 12.7% |
| 7eb51e56 | 47.12 | 12.7 | 5.9 | 2972 | 1682/2022/1319 | 43.6% (448/387/47) | 80.0% |
| 83bb1859 | 44.78 | 13.4 | 6.4 | 2972 | 1121/1151/1010 | 9.5% (53/43/13) | 23.6% |
| bfbeef67 | 45.45 | 13.2 | 6.1 | 2972 | 534/610/608 | 24.4% (57/84/8) | 26.5% |
| c0cd3671 | 46.82 | 12.8 | 7.7 | 2972 | 1552/1614/1515 | 12.7% (109/79/17) | 19.4% |
| f5fd585d | 45.14 | 13.3 | 7.0 | 2972 | 571/612/595 | 26.0% (68/66/25) | 35.5% |
| mean | 45.9 | 13.1 | | | | 22.1% | 32.7% |

Wall time is flat at about 46 s per 10-minute file whatever the speech density: the
encoder runs over silence too, unlike the VAD-gated CoreML path (1.9 to 5.5 s).

### Transcription, Forge, CPU provider, 4 threads, 20 s chunks

| File | Wall s | RTFx | Load | Peak RSS MB | Words ONNX/CoreML/WhisperKit | WER vs CoreML (S/D/I) | WER vs WhisperKit |
|---|---:|---:|---:|---:|---:|---:|---:|
| 2d7b9aba | 48.79 | 12.3 | 16.3 | 2524 | 601/609/553 | 3.4% (7/11/3) | 13.0% |
| 5ea9e7e8 | 54.63 | 11.0 | 10.6 | 2525 | 1065/1094/1080 | 8.2% (37/41/12) | 12.4% |
| 7eb51e56 | 46.48 | 12.9 | 10.1 | 2525 | 1703/2022/1319 | 41.4% (447/355/36) | 79.2% |
| 83bb1859 | 42.94 | 14.0 | 8.1 | 2525 | 1149/1151/1010 | 9.8% (55/30/28) | 24.9% |
| bfbeef67 | 45.91 | 13.1 | 7.2 | 2525 | 569/610/608 | 19.2% (50/54/13) | 22.9% |
| c0cd3671 | 54.79 | 11.0 | 7.2 | 2525 | 1366/1614/1515 | 21.4% (75/259/11) | 29.0% |
| f5fd585d | 56.17 | 10.7 | 8.0 | 2525 | 596/612/595 | 22.2% (76/38/22) | 32.8% |
| mean | 50.0 | 12.0 | | | | 17.9% | 30.6% |

Chunk length moves agreement a lot: 2d7b9aba goes from 29.6% to 3.4% (the 60 s chunks
dropped 167 words, mostly whole utterances in minutes 8 and 9), while c0cd3671 gets
worse (259 deletions at 20 s: the quiet-point cut lands inside speech). Segmentation,
not the model, is the first-order variable; production needs VAD-driven segments.

### Transcription, Forge, other configurations

| Configuration | Files | Wall s | RTFx | Load | Peak RSS MB | Note |
|---|---|---:|---:|---:|---:|---|
| CPU, 10 threads, 60 s chunks | bfbeef67, 5ea9e7e8 | not measured, run still in progress at time box (queued behind the CoreML-provider run in `spikes/onnx-speech/scripts/run-forge2.sh`; results land in `~/steno-spikes/onnx/out-forge/*.forge-t10.json` on Forge) | | | | |
| `provider = coreml`, 4 threads, 60 s chunks | 5ea9e7e8 | 379.6 | 1.6 | 8.2 | 13275 | Model load 22.8 s (2.2 s on CPU). Eight times slower than the CPU provider and 13.3 GB peak RSS: onnxruntime 1.17.1 partitions the int8 graph, runs the unsupported nodes on CPU and copies tensors back and forth. The CoreML provider is accepted without error but is unusable for this model. bfbeef67: 180.0 s, RTFx 3.3, load 5.8, peak RSS 14758 MB. |

### Transcription, atlas (Ryzen 7 7700 desktop, Linux x86_64), CPU provider, 60 s chunks

atlas was shared with other users' GitHub runners and other agent sessions for the
whole window (1-minute load 31 to 92 on 12 hardware threads), so these are upper
bounds on wall time, not laptop estimates. An idle run was not possible in the time box.

| File | Threads | Wall s | RTFx | Load | Peak RSS MB | WER vs CoreML (S/D/I) | WER vs WhisperKit |
|---|---:|---:|---:|---:|---:|---:|---:|
| 2d7b9aba | 4 | 141.5 | 4.2 | 74.1 | 2110 | 29.6% (9/168/3) | 31.1% |
| 5ea9e7e8 | 4 | 275.6 | 2.2 | 72.1 | 2110 | 9.5% (59/36/9) | 13.1% |
| 7eb51e56 | 4 | 331.9 | 1.8 | 92.4 | 2110 | 43.7% (424/426/34) | 78.2% |
| 83bb1859 | 4 | 292.5 | 2.1 | 88.9 | 2110 | 9.4% (50/43/15) | 23.6% |
| bfbeef67 | 4 | 307.1 | 2.0 | 63.3 | 2110 | 23.3% (53/80/9) | 27.0% |
| c0cd3671 | 4 | 264.6 | 2.3 | 53.0 | 2110 | 12.2% (112/69/16) | 19.3% |
| f5fd585d | 4 | 167.8 | 3.6 | 53.5 | 2110 | 25.8% (66/69/23) | 34.6% |
| mean | 4 | 254.4 | 2.6 | | | 21.9% | 32.4% |
| 2d7b9aba | 12 | 122.8 | 4.9 | 31.5 | 2111 | 29.6% | 31.1% |
| 5ea9e7e8 | 12 | 121.9 | 4.9 | 33.2 | 2157 | 9.5% | 13.1% |
| 7eb51e56 | 12 | 76.7 | 7.8 | 43.3 | 2157 | 43.7% | 78.2% |
| 83bb1859 | 12 | 127.9 | 4.7 | 32.5 | 2157 | 9.4% | 23.6% |
| bfbeef67 | 12 | 126.5 | 4.7 | 33.1 | 2157 | 23.3% | 27.0% |
| c0cd3671 | 12 | 140.6 | 4.3 | 31.7 | 2157 | 12.2% | 19.3% |
| f5fd585d | 12 | 126.4 | 4.7 | 40.7 | 2157 | 25.8% | 34.6% |
| mean | 12 | 120.4 | 5.0 | | | 21.9% | 32.4% |

The x86 and arm64 int8 transcripts of the same file differ (bfbeef67: 539 vs 534
words, token similarity 0.949; different word choices in a few compound nouns). Quantised kernels are not bit-identical across architectures, so a
Linux and a Mac build will not produce the same transcript from the same audio.

### Scale: the two CoreML engines against each other

| File | WER WhisperKit vs CoreML Parakeet |
|---|---:|
| 2d7b9aba | 11.8% |
| 5ea9e7e8 | 7.9% |
| 7eb51e56 | 50.5% |
| 83bb1859 | 19.8% |
| bfbeef67 | 18.5% |
| c0cd3671 | 15.4% |
| f5fd585d | 24.8% |
| mean | 21.3% |

So ONNX Parakeet disagrees with CoreML Parakeet (17.9% at 20 s chunks) about as much
as WhisperKit does (21.3%), despite sharing weights. 7eb51e56 is an outlier for every
pair (mixed German/English team call where the CoreML output has 700 more words than
WhisperKit). Sampled differences on bfbeef67 are mostly inflection and spelling
(verb inflection, the spelling of a company name, an anglicised vs German noun) plus a few clauses
where the ONNX chunk flipped language and produced English filler for German speech
(a two-word English aside became a full English sentence; on c0cd3671 a German
sentence introducing the meeting context came out as unrelated English filler). Parakeet v3 detects the language per utterance, so a chunk that
starts with English small talk can drag the following German with it.

### Diarization, Forge, CPU provider, 10 threads

sherpa-onnx offline diarization: pyannote segmentation 3.0 + 3D-Speaker ERes2Net
(VoxCeleb, 16 kHz) embeddings + agglomerative clustering, `num_clusters = -1` so the
threshold decides, `min_duration_on 0.3 s`, `min_duration_off 0.5 s`. One pass per
threshold (the C API recomputes segmentation and embeddings every time). Truth: five
1:1 calls have exactly one remote speaker; 2d7b9aba and c0cd3671 are group calls with
up to seven. FluidAudio's counts come from `steno dev diarize-sweep` on the same clips
at the production threshold 0.8 (its own Euclidean cut, not comparable to the
sherpa-onnx cosine threshold).

| File | Truth | FluidAudio @0.8 | ONNX th 0.7 | ONNX th 0.8 | ONNX th 0.9 | ONNX th 0.95 | Wall s per pass | RTFx | Load | Peak RSS MB |
|---|---|---:|---:|---:|---:|---:|---|---:|---:|---:|
| 2d7b9aba | group (<=7) | 2 | 6 | 6 | 6 | 6 | 43-45 | 13.7 | 6-11 | 425 |
| 5ea9e7e8 | 1 | 1 | 7 | 6 | 4 | 3 | 61-67 | 9.3 | 12-13 | 457 |
| 7eb51e56 | 1 | 1 | 6 | 4 | 3 | 3 | 62-65 | 9.4 | 15-16 | 467 |
| 83bb1859 | 1 | 1 | 8 | 8 | 5 | 4 | 75-83 | 7.7 | 13-19 | 478 |
| bfbeef67 | 1 | 1 | 8 | 6 | 5 | 3 | 51-58 | 10.9 | 14-19 | 478 |
| c0cd3671 | group (<=7) | 2 | 20 | 16 | 13 | 10 | 104-150 | 4.8 | 17-25 | 478 |
| f5fd585d | 1 | 1 | 16 | 14 | 12 | 10 | 44-49 | 12.8 | 20-23 | 478 |

At the sherpa-onnx default threshold 0.5, bfbeef67 (one speaker) came out as 15
speakers on both machines. No threshold in the sweep gets a 1:1 call down to one
speaker; the stock pipeline over-counts by 3 to 16 on this audio, while FluidAudio's
counts match truth on all five 1:1 calls and under-count the group calls. The
wespeaker CAM++ embedding was queued but not reached in the time box. FluidAudio's
own sweep (five thresholds, seven files, CoreML) took 2 min 3 s wall in total, about
3.5 s per file per threshold, against 43 to 150 s per file per threshold here.

Diarization on atlas: bfbeef67 at threshold 0.5, 4 threads, load 101: 124 s per pass
(RTFx 4.8), 15 speakers. At threshold 0.8, 12 threads, load 46: 374 s per pass (RTFx 1.6), 6 speakers, the same count as Forge at 0.8, so the over-counting is not a platform artefact. The 0.9 pass on atlas was still running at the time box (not measured).

### Memory

Peak RSS after loading the int8 Parakeet alone is 2.1 GB (Linux) to 2.5 GB (macOS);
it reaches 3.0 GB during 60 s chunks on macOS and stays at 2.1 GB on Linux.
Diarization alone peaks at 0.48 GB. Model load takes 2 to 3 s on Forge, 9 to 13 s on
the loaded atlas.

## Verdict

**Speed.** On the M4 Pro, ONNX Parakeet on CPU is 18 times slower than CoreML Parakeet
(RTFx 13 against 233) and about as fast as WhisperKit large-v3-turbo on CoreML (21.5).
A 60-minute meeting would take about 4.6 minutes to transcribe and, with the stock
pipeline, 4 to 15 more minutes to diarize, against well under a minute today. That is
usable for post-meeting processing in the background, not for the near-live summary
Steno shows at meeting end. On the Ryzen 7 7700 the measured RTFx of 2.6 to 7.8 was
taken under a load of 30 to 90 and is not a laptop number; an idle measurement is the
first thing to redo. Thread scaling on Forge was not resolved (10-thread row above).

**Agreement.** Same weights do not give the same text. After the segmentation fix to
20 s chunks the ONNX transcript disagrees with the CoreML one by 17.9% WER on average
(3.4% to 9.8% on the four cleanest files, 19% to 41% on the rest), which is the same
order as WhisperKit's 21.3% disagreement with CoreML Parakeet. The differences are
int8 quantisation, per-chunk language detection flipping German to English, and the
absence of a VAD. The first and third are fixable in the pipeline (sherpa-onnx ships
silero VAD); the quantisation and language flips are properties of the only v3 export
available.

**Diarization.** The stock sherpa-onnx pipeline is not fit for Steno's speaker lanes:
3 to 16 speakers for single-speaker calls at every threshold tried, where FluidAudio
gets exactly one. Reproducing FluidAudio's quality would mean exporting pyannote's
community-1 embedding model, reimplementing its clustering and refinement, and
calibrating against the Forge corpus again (`.plans/2026-09-29-...` speaker
calibration work). That is weeks, not a spike.

**Verdict.** ONNX on CPU is good enough for transcription as a background job on
Linux and Windows, at WhisperKit-class speed and with WhisperKit-class disagreement
from the Mac transcript, once VAD segmentation is in place. It is not good enough for
diarization as shipped. A cross-platform Steno would ship a visibly different product
on Linux and Windows: slower, differently worded transcripts, and either no speaker
labels or a diarizer that has to be rebuilt.

**Risks.**
- German accuracy: per-chunk language ID flips German to English filler; only an int8
  export of v3 exists; x86 and arm64 int8 outputs differ from each other.
- Model licensing: Parakeet TDT 0.6b v3 is CC-BY-4.0 (attribution in the app),
  sherpa-onnx is Apache-2.0, onnxruntime MIT, pyannote segmentation 3.0 is MIT but
  gated on Hugging Face (the sherpa-onnx redistribution bypasses the gate; pyannote's
  community-1 pipeline that FluidAudio mirrors is also gated), 3D-Speaker and WeSpeaker
  embeddings are Apache-2.0. Confirm the embedding licences before shipping.
- Binary and download size: 640 MB of model files plus 20 MB (Linux) or 59 MB
  (macOS universal) of shared libraries; the published crate links dynamically with
  no rpath, so packaging needs the `static` feature (untested here, Linux wants
  `-C relocation-model=dynamic-no-pic`) or an installer that lays out the libraries.
- Robustness: onnxruntime errors are C++ exceptions that cross the FFI and abort the
  process; every call needs a length guard (2500 encoder frames) before it reaches
  the model.
- Memory: 2.1 to 3.0 GB peak for ASR is high for a background job on an 8 GB laptop.
- GPU story: sherpa-rs exposes `cuda` and `directml` features (untested); the CoreML
  provider through onnxruntime is accepted but eight times slower than CPU with 13 GB RSS (table above), so ONNX offers no acceleration on the Mac either; CoreML Parakeet stays the Mac path. On Linux laptops without CUDA there is no acceleration
  path; the CPU numbers above are the product.
- Toolchain: bindgen needs libclang on every build host; the build downloads
  binaries from GitHub at build time unless `SHERPA_LIB_PATH` is vendored.
