# Spike F: absolute WER on FLEURS German

Scripts for `.plans/spikes/2026-10-01-spike-fleurs-wer.md`. No audio or model files
live here; the data is rebuilt from Hugging Face and the models are the ones already on
Forge (`~/steno-calibration/repo` for the CoreML engines, `~/steno-spikes/models-own/fp32`
for the own ONNX export, `~/steno-spikes/chunker` for the spike-D harness).

## Data (atlas, Docker)

FLEURS (`google/fleurs`, config `de_de`, split `test`, CC-BY-4.0), first 300 utterances
as 16 kHz mono Int16 WAV plus `<name>.ref.txt` from the `transcription` field, and ten
concatenations of 30 utterances with 0.7 s of silence between them.

```sh
mkdir -p /tmp/steno-spikes/fleurs && cp make_fleurs_de.py /tmp/steno-spikes/fleurs/
docker run --rm -v /tmp/steno-spikes/fleurs:/work -w /work -e HF_HOME=/work/.hf python:3.11 \
  sh -c 'pip install -q "datasets<3" soundfile librosa numpy num2words && python make_fleurs_de.py /work --n 300'
rsync -a --exclude .hf /tmp/steno-spikes/fleurs/ nschmid10049@forge:steno-spikes/fleurs/
```

`datasets<3` keeps the script-based loader that FLEURS still needs; `librosa` is its
audio decoder. The whole `de_de` archive (2 GB, all splits) is downloaded once into
`.hf/`. Output: `utt/` (300 files), `cat/` (10 files), `manifest.json` with FLEURS ids,
durations and the raw (cased, punctuated) transcriptions.

## Measurement (Forge)

The ONNX decode loop needs a Python with onnxruntime; Forge has `uv`:

```sh
cd ~/steno-spikes && uv venv --python 3.12 fleurs-venv && uv pip install --python fleurs-venv/bin/python onnxruntime kaldi_native_fbank soundfile numpy
cp fleurs/run-forge.sh fleurs/ort_decode_dir.py ~/steno-spikes/fleurs/   # already there after the rsync above
nohup sh fleurs/run-forge.sh cat utt ort > fleurs/run.log 2>&1 &
```

`run-forge.sh` runs, per set, `steno dev bakeoff <set> --engines parakeet-v3 whisperkit-large-v3-turbo`
(writes `<set>/bakeoff/`) and the harness
`onnx-speech-spike --asr-dir ~/steno-spikes/models-own/fp32 --asr-files fp32 --chunker vad --target-seconds 25 --threads 4 --slid-dir ... --vote`
(writes `<set>/harness/<name>.f-fp32.json`), then `ort_decode_dir.py` on `cat/`
(numpy TDT loop straight on onnxruntime, whole file, no chunker; writes
`<set>/ort/<name>.ort-fp32.json`). `uptime` before and after each step is in `run.log`;
the harness and the ORT loop also record the 1-minute load per file.

## Scoring (anywhere with Python 3.11+, `num2words` only for `--numbers`)

```sh
rsync -a --exclude '*.wav' nschmid10049@forge:steno-spikes/fleurs/cat/ results/cat/
python score_fleurs.py results/cat --per-file \
  --bakeoff results/cat/bakeoff parakeet-v3 --bakeoff results/cat/bakeoff whisperkit-large-v3-turbo \
  --harness results/cat/harness f-fp32 --ort results/cat/ort ort-fp32
python score_fleurs.py results/cat --numbers ...      # digits spelled out in German on both sides
python score_fleurs.py results/cat --fold-umlauts ... # the Swift bake-off's ae/oe/ue/ss folding
python score_fleurs.py utt --first 151 ...             # same subset for every column when one engine did not finish
```

One normaliser for every engine and the reference: NFC, lowercase, `%` to `prozent`,
everything that is not a letter or digit becomes a word boundary. Mean WER is the
mean of per-file WERs (what `report.md` prints); pooled WER is total errors over total
reference words.

## Known state of `ort_decode_dir.py`

The numpy TDT loop (spike E's `export/ort_decode.py`) scores about 65 % WER on this
data with either feature setting (`--nemo-features` switches to sherpa-onnx's
pre-emphasis 0, `snip_edges` false, 0 to 8000 Hz) and with the duration-0 handling
corrected; see the report. Treat its output as a diagnostic, not a transcript, until the
loop is fixed and re-scored here.
