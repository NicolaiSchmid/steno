# Spike F: absolute WER on FLEURS German (CoreML Parakeet, WhisperKit, own fp32 ONNX)

Status: measured 2026-10-01, time-boxed to about 75 minutes. Parent:
`.plans/2026-10-01-cross-platform-speech-stack.md` (WP1b, reading gate G1). Previous:
`.plans/spikes/2026-10-01-spike-own-export.md` (our fp32 export and its 11.5 % mean
disagreement with CoreML). Code: `spikes/onnx-speech/fleurs/` (data builder, Forge run
script, batch ORT decoder, scorer, README with the exact commands).

## Question

Spikes D and E only measured how much the ONNX path *disagrees* with the CoreML
transcript (11.5 % mean for our fp32 export). The CoreML encoder is a 6-bit
palettised approximation, so that number says nothing about which side is wrong. Against
human references on public German speech: what is the absolute word error rate (WER) of
(a) Parakeet TDT 0.6B v3 on CoreML through the Swift app's pipeline, (b) WhisperKit
large-v3-turbo on CoreML, (c) our fp32 ONNX export of Parakeet v3 on CPU through the
spike-D harness? Is (c) within about 1 WER point of (a)?

## Data and licence

FLEURS (`google/fleurs`, Conneau et al. 2022), configuration `de_de`, split `test`,
licence CC-BY-4.0. Read speech: native speakers reading FLoRes-101 Wikipedia sentences
(news, travel, science), one or two sentences per utterance, studio-quiet, one speaker per
file. The `transcription` field is the lowercased, punctuation-free reference; digits are
kept as digits (57 of the 300 references contain one), abbreviations stay as written
(`u. a`, `n chr.`, `ph-wert`).

| Set | Files | Audio | Reference words | How |
|---|---:|---:|---:|---|
| `utt/` | 300 | 67.8 min (mean 13.6 s, 4 to 47 s) | 6578 | first 300 rows of the split, 16 kHz mono Int16 WAV + `<name>.ref.txt` |
| `cat/` | 10 | 71.2 min including the gaps (6.1 to 8.7 min each) | 6578 | the same 300 in groups of 30 with 0.7 s of silence between utterances; references joined with spaces |

`cat/` exists because Steno transcribes long recordings and the chunker (VAD, 25 s
target, 1.5 s overlap, LCS merge) is part of what we measure for the ONNX path; `utt/`
is the plain per-utterance benchmark. The whole `de_de` archive (2 GB) was downloaded on
atlas in Docker (`python:3.11`, `datasets 2.21`, `soundfile`, `librosa`), converted by
`spikes/onnx-speech/fleurs/make_fleurs_de.py`, and rsynced to Forge
`~/steno-spikes/fleurs/`. No private recordings were used; nothing from
`~/steno-calibration/` was read or written.

This is read, clean, single-speaker speech. Absolute WERs here are a floor; Steno's
meeting audio (spike E's 7eb51e56) is far harder for every engine.

## Setup

| Item | Value |
|---|---|
| Host | Forge, M4 Pro (10 cores, 24 GB), macOS 26.7 |
| (a) CoreML Parakeet | `steno dev bakeoff <set> --engines parakeet-v3 whisperkit-large-v3-turbo`, binary `~/steno-calibration/repo/.build/release/steno` (main as of this morning), models in `~/Library/Application Support/Steno/Models` (FluidAudio Parakeet v3 CoreML); the app's own chunking and language lanes |
| (b) WhisperKit | same bake-off run, `whisperkit-large-v3-turbo` CoreML |
| (c) own fp32 ONNX | spike-D harness `~/steno-spikes/chunker/target/release/onnx-speech-spike` (sherpa-rs-sys 0.6.8, sherpa-onnx 1.12.9, bundled onnxruntime 1.17.1), `--asr-dir ~/steno-spikes/models-own/fp32 --asr-files fp32 --chunker vad --target-seconds 25 --slid-dir models/sherpa-onnx-whisper-tiny --vote --threads 4`, CPU provider. Tag `f-fp32` |
| (c') glue-free fp32 | `spikes/onnx-speech/fleurs/ort_decode_dir.py`: spike E's numpy TDT greedy loop on onnxruntime 1.30.0 (uv venv, Python 3.12) with kaldi-native-fbank features, one whole-file encoder pass per wav, no chunker, 4 threads. Tag `ort-fp32` |
| Scoring | `spikes/onnx-speech/fleurs/score_fleurs.py` over the raw per-file JSON of every engine, one normaliser for reference and hypothesis: NFC, lowercase, `%` to `prozent`, anything that is not a letter or digit is a word boundary. "Mean WER" is the mean of per-file WERs; "pooled" is total errors over total reference words. The Swift bake-off's own `report.md` (which additionally folds umlauts) agrees with the scorer to 0.1 point on every row |
| Sequencing | the steps ran one after another under `nohup` (`spikes/onnx-speech/fleurs/run-forge.sh cat utt ort`), never concurrently |

Measurement conditions: Forge shared with other users; 1-minute load 1.9 to 5.6 across
the bake-off and harness steps and up to 9 during the ORT loop, recorded per step in
the Load column of each table. No engine produced an error during the measurement; the
`utt/` harness pass was stopped on purpose after 151 files (see below).

## Results

### `cat/` (10 files of about 7 minutes, the chunker in the loop)

| Engine | Files | Mean WER | Median | Pooled | S/D/I | Mean RTFx | Load (1-min, mean) |
|---|---:|---:|---:|---:|---|---:|---:|
| (a) CoreML Parakeet v3 | 10 | **5.5 %** | 5.1 % | 5.4 % | 259/46/51 | 392 | 2.2 to 2.6 (uptime) |
| (b) WhisperKit large-v3-turbo | 10 | **6.0 %** | 5.7 % | 6.0 % | 234/105/53 | 20.2 | 2.2 to 2.6 (uptime) |
| (c) own fp32 ONNX, harness | 10 | **5.3 %** | 5.0 % | 5.2 % | 250/55/39 | 19.9 | 5.1 |
| (c') own fp32 ONNX, ORT loop, whole file | 10 | 64.4 % (broken, see below) | 64.6 % | 64.3 % | 2980/873/379 | 12.6 | 6.2 |

With digits spelled out as German number words on both sides (`--numbers`): (a) 5.3 %,
(b) 6.0 %, (c) 5.1 %, (c') 64.3 %; every row moves by 0.0 to 0.2 points and the order does
not change. With the Swift bake-off's umlaut folding: 5.5 / 5.9 / 5.3 %. The
`--numbers` rows in this report are as reported by the run and were not re-verified:
`num2words` was not installed in the venv when the verification pass ran on 2026-10-02.

Per file (raw normalisation):

| File | Ref words | (a) CoreML | (b) WhisperKit | (c) fp32 harness | (c') fp32 ORT |
|---|---:|---:|---:|---:|---:|
| fleurs-de-cat-01 | 681 | 5.6 % | 4.1 % | 5.3 % | 64.3 % |
| fleurs-de-cat-02 | 702 | 5.1 % | 5.3 % | 6.6 % | 67.0 % |
| fleurs-de-cat-03 | 783 | 4.2 % | 5.5 % | 3.7 % | 59.0 % |
| fleurs-de-cat-04 | 657 | 6.1 % | 5.9 % | 4.1 % | 63.9 % |
| fleurs-de-cat-05 | 590 | 7.3 % | 5.9 % | 7.6 % | 64.9 % |
| fleurs-de-cat-06 | 649 | 5.1 % | 11.9 % | 4.5 % | 61.5 % |
| fleurs-de-cat-07 | 607 | 5.1 % | 6.1 % | 4.1 % | 68.7 % |
| fleurs-de-cat-08 | 643 | 5.1 % | 7.0 % | 6.4 % | 66.4 % |
| fleurs-de-cat-09 | 665 | 4.5 % | 3.0 % | 4.7 % | 65.9 % |
| fleurs-de-cat-10 | 601 | 6.5 % | 5.2 % | 5.8 % | 62.9 % |
| **mean** | 6578 | **5.5 %** | **6.0 %** | **5.3 %** | 64.4 % |

Peak RSS of the harness with the fp32 model: 3.4 GB; model load 5.4 s. The harness cut
every file into 20 to 25 segments, at `long-pause` boundaries (the 0.7 s gaps) apart
from 18 `pause` cuts and one energy cut across the ten files; every lane voted `de`;
two files raised a flag, and on cat-02 the vote changed 2 segments. The chunker was
never the problem on this material.

### `utt/` (300 single utterances, no chunker decisions to speak of)

The harness was stopped after 151 of 300 files (2 to 4 s per file, most of it the SLID
pass; see below), so its row is over the first 151 utterances and the CoreML rows are
given both over all 300 and over the same 151.

| Engine | Files | Mean WER | Median | Pooled | S/D/I | Mean RTFx | Load (1-min, mean) |
|---|---:|---:|---:|---:|---|---:|---:|
| (a) CoreML Parakeet v3 | 300 | **5.7 %** | 3.6 % | 5.4 % | 260/51/45 | 150 | 1.9 to 5.6 (uptime) |
| (b) WhisperKit large-v3-turbo | 300 | **4.5 %** | 0.0 % | 4.3 % | 209/27/46 | 11.0 | 1.9 to 5.6 (uptime) |
| (a) CoreML Parakeet v3, same 151 | 151 | **5.9 %** | 3.7 % | 5.6 % | 139/33/20 | 152 | |
| (b) WhisperKit large-v3-turbo, same 151 | 151 | **4.5 %** | 0.0 % | 4.2 % | 104/16/24 | 11.6 | |
| (c) own fp32 ONNX, harness | 151 | **5.7 %** | 3.0 % | 5.3 % | 132/32/19 | 5.2 | 4.8 |

With `--numbers`: (a) 5.5 % (300) and 5.6 % (151), (b) 4.4 % and 4.3 %, (c) 5.4 %.

The harness had no WAV-reader or chunker failure on 4 to 47 s inputs (each file became
one VAD segment with a `tail` cut). Its RTFx on short files is dominated by the SLID
pass (24 whisper-tiny windows, 2 to 3 s per file regardless of length) and by the
per-call setup, so the `utt/` RTFx of (c) is not a fair speed number; `cat/` is.

### What the errors are

Reading the alignments on `fleurs-de-cat-06` (the file where WhisperKit is at 11.9 %):
all three engines share the same reference artefacts (`ph-wert` split at the hyphen,
`u. a` against `unter anderem`, `n chr.` against `nach christus`, `beida` against a
transliteration), the two Parakeets differ on proper names (`martillis` vs
`martellis`, `davie`/`davy` for `davey`) and on one or two content words each
(`mehr`/`mär` for `mehrheit`, `tau` for `haut`), and WhisperKit deleted one 50-word
stretch outright (two whole utterances, the same drop-out pattern spike E saw on the
team call). WhisperKit's 105 deletions on `cat/` against 27 on `utt/` are that pattern:
it is the best engine on isolated sentences and loses to both Parakeets as soon as the
input is a long recording. CoreML and fp32 Parakeet are the same model making the same
kind of mistakes in different places: 259 vs 250 substitutions, 46 vs 55 deletions,
51 vs 39 insertions.

## Normalisation caveat

FLEURS references are lowercased and stripped of punctuation but keep digits and
abbreviations; the engines apply inverse text normalisation (`unter anderem`, `nach
Christus`, digits for numbers). The scorer treats every non-alphanumeric character as a
word boundary (so `ph-wert`, `ph wert` and `pH-Wert` agree, and `u.a.` becomes two
tokens `u a` that never match `unter anderem`), maps `%` to `prozent`, and with
`--numbers` spells digit tokens out in German on both sides with `num2words`
(cardinals; `1990` becomes `eintausendneunhundertneunzig`, which does not match a
reference `neunzehnhundertneunzig`; `1,5` becomes `eins komma fünf`). Since both
sides are normalised identically and the references carry digits, `--numbers` only
matters where one side wrote a number as a word and the other as digits; it moves every
engine by at most 0.2 points, the same way, and the tables above quote the raw number
as primary. A reference-side expansion of abbreviations would lower every engine by a
few tenths equally; it was not done.

## Conclusion

**Yes: our fp32 ONNX export is within one WER point of CoreML Parakeet in absolute
terms, and on this data it is marginally better, not worse.** On the ten
meeting-length files (chunker included) fp32 ONNX scores 5.3 % mean WER against 5.5 %
for CoreML Parakeet; on the single utterances 5.7 % against 5.9 % over the same 151
files (CoreML is 5.7 % over all 300). The 11.5 %
"disagreement with CoreML" of spike E was therefore two implementations of one model
each making about 5 % of errors in different places, not a defective ONNX path; the
palettised CoreML encoder is, if anything, the slightly lossier side. Gate G1 ("mean WER
vs CoreML under 8 %") measured the wrong thing; restated as absolute WER against human
references, the ONNX path passes with margin (parity within 0.2 to 0.4 points).

WhisperKit large-v3-turbo is the best engine on isolated sentences (4.5 % against 5.7
and 5.7) and the worst on long recordings (6.0 % against 5.5 and 5.3) because it
drops whole stretches of speech; for Steno's workload (long recordings) both Parakeets
beat it, and the Parakeets run at RTFx 20 (ONNX, 4 CPU threads) to 400 (CoreML) against
WhisperKit's 20.

What this does not settle: this is clean read speech by one speaker; the ranking on
noisy multi-speaker meeting audio with code-switching (spike E's corpus) is not
measured against truth anywhere. The int8 exports were not scored here; spike E's 4 to
5 points of extra disagreement for int8 are now expected to be real extra errors, and a
FLEURS run of the int8 and of a calibrated static int8 would say by how much.

### The ORT loop (c') is broken, and that changes a spike E claim

The "glue-free" number could not be produced. `ort_decode_dir.py`, which is spike E's
`export/ort_decode.py` loop run over a folder, scores 64.4 % on `cat/` and 64.8 % on the
first 60 `utt/` files, the same with sherpa-onnx's exact feature settings
(`--nemo-features`: pre-emphasis 0, `snip_edges` false, 0 to 8000 Hz) and the same after
correcting the loop's TDT duration handling (a non-blank token with duration 0 must stay
on its frame; the spike E loop forced an advance). The text is recognisable German with
syllables dropped and foreign fillers inserted: `Für besten Aussichten auf Hong, sollten
Sie die Insel verlassen zum, gübergenden, Fr Kowowlun fahren... ¡Fr.? Ja.` for `für die
besten aussichten auf hongkong sollten sie die insel verlassen und zum gegenüberliegenden
ufer von kowloon fahren`. It runs at RTFx 31 to 48 on the short files (4 threads, load 3.8 to 6.1), so speed is not
the issue. Word counts are 85 to 90 % of the reference, which is why spike
E, which judged the loop by word count only (67 to 72 words on its probe window), took it
for a working decoder. Its conclusion that the zero-token window "decodes normally
through onnxruntime directly" and therefore that the defect is in sherpa-onnx's glue is
unverified: the loop it used does not produce a correct transcript anywhere, so it
cannot tell a glue bug from a decoder bug. The sherpa-onnx recognizer, with all its
faults, is the only ONNX decode of this export that has been shown to be accurate.
Finding the bug in the loop (`spikes/onnx-speech/export/ort_decode.py`: feature
extraction against NeMo's preprocessor, or
the decoder state hand-off) is the first task of WP2 if the sidecar is to own the decode
loop, and it should be validated against FLEURS, not against word counts.
