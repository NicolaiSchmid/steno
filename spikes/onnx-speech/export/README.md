# Own ONNX export of Parakeet TDT 0.6B v3 (spike E)

Scripts that produced the exports measured in
`.plans/spikes/2026-10-01-spike-own-export.md`. No model files live here; the
outputs are 0.7 to 2.5 GB per variant and stay under `~/steno-spikes/models-own/`
on the measurement machine.

The checkpoint `nvidia/parakeet-tdt-0.6b-v3` is CC-BY-4.0 (NVIDIA). Any build that
ships an export derived from it must attribute NVIDIA and link the licence.

## What `export_parakeet_v3.py` does

It is the sherpa-onnx recipe (`scripts/nemo/parakeet-tdt-0.6b-v3/export_onnx.py`,
Apache-2.0) with three changes:

1. `--max-frames N` sets the encoder's relative-position table before tracing via
   `ConformerEncoder.set_max_audio_length(N)`. NeMo builds the table with
   `2 * pos_emb_max_len - 1` rows (5000 frames of 80 ms for this checkpoint) and the
   forward slices `pe[:, center - T : center + T - 1]`; for `T > 5000` the start
   index goes negative, Python wraps it, and the attention `Add` fails at run time
   with the "2500 by 7500" broadcast error from spike B. The table is baked into
   the graph as a `Constant` node (fp32, 4 KB per row), so a 10000-frame table costs
   41 MB more on disk and nothing at run time for short segments. A fully dynamic
   table would need the sinusoid computed in-graph; not done.
2. The fp32 encoder is kept. It is 2.4 GB, over protobuf's 2 GB limit, so it is
   saved with all initializers in one external file `encoder.weights`. sherpa-onnx
   reads models into memory and creates the ORT session from the buffer, so ORT
   resolves that path against the process working directory; the harness changes
   into the model directory while it builds the recognizer (`spikes/onnx-speech/src/main.rs`).
3. Three int8 variants from the same fp32 graph with `onnxruntime.quantization.quantize_dynamic`:
   `int8` (the recipe: QUInt8 encoder weights, QInt8 decoder and joiner, per-tensor),
   `int8-pc` (per-channel weight scales), `int8-noattn` (per-tensor, but the 192
   `self_attn` MatMuls stay fp32; only the feed-forward MatMuls are quantised).
   Static (calibrated) quantisation was not attempted; see the report.

`inspect_onnx.py` prints IR and opset versions, metadata, I/O shapes and large
constants of an export, for comparing with the stock release.

`ort_decode.py` is a numpy-only greedy TDT decode of one 16 kHz wav straight through
onnxruntime (no sherpa-onnx), with the feature-pipeline knobs exposed as flags and
diagnostics (NaN count, logit range, first decode steps). It is how the spike showed
that the window which decodes to zero tokens through sherpa-onnx 1.12.9 decodes
normally through onnxruntime 1.17.1 and 1.30 with the same model file:

```sh
docker exec -w /work steno-export python ort_decode.py --dir /work/out/fp32 --files fp32 --wav clip.wav
docker exec -w /work steno-export python ort_decode.py --dir /work/stock --files int8 --wav clip.wav --high-freq 8000 --no-snip --preemph 0
```

## Exact commands (atlas, Linux x86_64, Docker)

```sh
mkdir -p /tmp/steno-spikes/export && cd /tmp/steno-spikes/export
mkdir -p recipe && for f in export_onnx.py run.sh test_onnx.py; do
  curl -sSL -o recipe/$f https://raw.githubusercontent.com/k2-fsa/sherpa-onnx/master/scripts/nemo/parakeet-tdt-0.6b-v3/$f; done
curl -sSL -o recipe/generate_bpe_vocab.py https://raw.githubusercontent.com/k2-fsa/sherpa-onnx/master/scripts/nemo/generate_bpe_vocab.py
curl -sSL -o recipe/test_onnx.py https://raw.githubusercontent.com/k2-fsa/sherpa-onnx/master/scripts/nemo/parakeet-tdt-0.6b-v2/test_onnx.py
curl -sSL -o parakeet-tdt-0.6b-v3.nemo https://huggingface.co/nvidia/parakeet-tdt-0.6b-v3/resolve/main/parakeet-tdt-0.6b-v3.nemo

docker run -d --name steno-export -v /tmp/steno-spikes/export:/work -w /work python:3.11 sleep infinity
docker exec steno-export pip install torch --index-url https://download.pytorch.org/whl/cpu
docker exec steno-export pip install "nemo_toolkit[asr]" onnx onnxruntime onnxsim huggingface_hub kaldi-native-fbank soundfile librosa
cp <repo>/spikes/onnx-speech/export/*.py .
docker exec steno-export python export_parakeet_v3.py --max-frames 10000 --variants fp32,int8,int8-pc,int8-noattn
docker exec steno-export chmod -R a+rX /work/out     # the container writes as root
docker exec steno-export python inspect_onnx.py out/fp32/encoder.onnx
```

Outputs land in `out/<variant>/`. Copy a variant to the Mac with
`rsync -rt out/<variant>/ forge:~/steno-spikes/models-own/<variant>/` and measure with
`spikes/onnx-speech/scripts/run-own-export-forge.sh <variant>`; the harness takes
`--asr-dir <dir> --asr-files fp32|int8`.

Pure-ORT sanity decode of a clip (no sherpa-onnx involved):

```sh
docker exec -w /work steno-export python recipe/test_onnx.py \
  --encoder out/fp32/encoder.onnx --decoder out/fp32/decoder.onnx \
  --joiner out/fp32/joiner.onnx --tokens out/fp32/tokens.txt --wav clip.wav
```

Timings on atlas under a 1-minute load of 22 to 36: model load 20 s, encoder
trace 55 s, decoder and joiner 5 s, fp32 save 27 s, each int8 quantisation 32 to
50 s; whole script 4 min. Checkpoint download 2.4 GB in about 20 s, pip install
about 2 min with a warm cache.
