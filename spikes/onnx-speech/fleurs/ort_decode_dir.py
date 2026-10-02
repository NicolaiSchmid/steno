#!/usr/bin/env python3
"""Decode every 16 kHz wav in a folder with the numpy TDT greedy loop on onnxruntime
(bypassing sherpa-onnx's recognizer), one whole-file encoder pass per wav. Writes
<out>/<name>.<tag>.json in the onnx-speech-spike shape (asr.text, wall_s, rtfx,
load_1min_before) so score_fleurs.py can read it with --ort. Spike F; the loop is
spike E's export/ort_decode.py without the diagnostics.

Usage: ort_decode_dir.py --dir <model-dir> --files fp32|int8 --corpus <wav-dir> --out <dir> --tag ort-fp32 [--threads 4]
"""
import argparse, json, os, sys, time
from pathlib import Path

import kaldi_native_fbank as knf
import numpy as np
import onnxruntime as ort
import soundfile as sf

ap = argparse.ArgumentParser()
ap.add_argument("--dir", required=True)
ap.add_argument("--files", default="fp32", choices=["int8", "fp32"])
ap.add_argument("--corpus", required=True)
ap.add_argument("--out", required=True)
ap.add_argument("--tag", default="ort-fp32")
ap.add_argument("--threads", type=int, default=4)
ap.add_argument("--limit", type=int, default=0)
ap.add_argument("--nemo-features", action="store_true", help="sherpa-onnx offline-stream.cc settings: preemph 0, snip_edges false, 0-8000 Hz")
a = ap.parse_args()

corpus = Path(a.corpus).resolve(); out = Path(a.out).resolve(); out.mkdir(parents=True, exist_ok=True)
suf = ".int8.onnx" if a.files == "int8" else ".onnx"
so = ort.SessionOptions(); so.intra_op_num_threads = a.threads; so.inter_op_num_threads = 1
os.chdir(a.dir)  # external encoder.weights resolves against the CWD
t0 = time.time()
enc = ort.InferenceSession(f"encoder{suf}", so, providers=["CPUExecutionProvider"])
dec = ort.InferenceSession(f"decoder{suf}", so, providers=["CPUExecutionProvider"])
joi = ort.InferenceSession(f"joiner{suf}", so, providers=["CPUExecutionProvider"])
print(f"models loaded in {time.time()-t0:.1f}s (ort {ort.__version__})", file=sys.stderr)
meta = enc.get_modelmeta().custom_metadata_map
vocab_size = int(meta["vocab_size"]); blank = vocab_size - 1
pred_rnn_layers = int(meta["pred_rnn_layers"]); pred_hidden = int(meta["pred_hidden"])
id2token = {}
for line in open("tokens.txt", encoding="utf-8"):
    t, i = line.rsplit(" ", 1); id2token[int(i)] = t
enc_in = [x.name for x in enc.get_inputs()]; dec_in = [x.name for x in dec.get_inputs()]; joi_in = [x.name for x in joi.get_inputs()]


def features(audio: np.ndarray) -> np.ndarray:
    opts = knf.FbankOptions(); opts.frame_opts.dither = 0.0; opts.frame_opts.remove_dc_offset = False
    opts.frame_opts.window_type = "hann"; opts.mel_opts.low_freq = 0; opts.mel_opts.high_freq = 0.0
    opts.frame_opts.snip_edges = not a.nemo_features; opts.frame_opts.preemph_coeff = 0.0 if a.nemo_features else 0.97
    if a.nemo_features: opts.mel_opts.high_freq = 8000.0
    opts.mel_opts.num_bins = 128; opts.mel_opts.is_librosa = True
    fb = knf.OnlineFbank(opts); fb.accept_waveform(16000, audio.tolist()); fb.input_finished()
    feats = np.stack([fb.get_frame(i) for i in range(fb.num_frames_ready)])
    if meta["normalize_type"] == "per_feature":
        feats = (feats - feats.mean(0, keepdims=True)) / (feats.std(0, keepdims=True, ddof=1) + 1e-5)
    return feats.T[None].astype(np.float32)


def run_dec(tok, s0, s1):
    o = dec.run(None, {dec_in[0]: np.array([[tok]], np.int32), dec_in[1]: np.array([1], np.int32), dec_in[2]: s0, dec_in[3]: s1})
    return o[0], o[2], o[3]


def decode(audio: np.ndarray) -> str:
    x = features(audio)
    enc_out, enc_len = enc.run(None, {enc_in[0]: x, enc_in[1]: np.array([x.shape[2]], dtype=np.int64)})
    s0 = np.zeros((pred_rnn_layers, 1, pred_hidden), np.float32); s1 = s0.copy()
    dec_out, s0n, s1n = run_dec(blank, s0, s1)
    ans, t, T, symbols_here = [], 0, int(enc_len[0]), 0
    while t < T:
        logits = joi.run(None, {joi_in[0]: enc_out[:, :, t:t + 1], joi_in[1]: dec_out})[0].squeeze()
        idx = int(np.argmax(logits[:vocab_size])); skip = int(np.argmax(logits[vocab_size:]))
        if idx != blank:
            ans.append(idx); s0, s1 = s0n, s1n; dec_out, s0n, s1n = run_dec(idx, s0, s1)
            # TDT: a non-blank token with duration 0 stays on this frame (several tokens per
            # frame); spike E's ort_decode.py forced skip >= 1 here and dropped tokens.
            symbols_here += 1
            if skip == 0 and symbols_here < 10:
                continue
            t += max(skip, 1) if symbols_here >= 10 else skip
        else:
            t += max(skip, 1)
        symbols_here = 0
    return "".join(id2token[i] for i in ans).replace("▁", " ").strip()


wavs = sorted(corpus.glob("*.wav"))
if a.limit: wavs = wavs[: a.limit]
print(f"files: {len(wavs)}", file=sys.stderr)
for w in wavs:
    audio, sr = sf.read(w, dtype="float32"); assert sr == 16000
    if audio.ndim > 1: audio = audio[:, 0]
    load = os.getloadavg()[0]
    t1 = time.time(); text = decode(audio); wall = time.time() - t1
    audio_s = len(audio) / 16000
    rep = {"file": w.name, "tag": a.tag, "audio_s": audio_s, "asr": {"text": text, "wall_s": wall, "rtfx": audio_s / wall if wall else None, "threads": a.threads, "load_1min_before": load, "model": f"{Path(a.dir).name} {a.files} (ort {ort.__version__}, whole file)"}}
    (out / f"{w.stem}.{a.tag}.json").write_text(json.dumps(rep, indent=1, ensure_ascii=False))
    print(f"{w.name} {audio_s:.1f}s wall {wall:.1f}s rtfx {audio_s/wall:.1f} words {len(text.split())} load {load:.1f}", file=sys.stderr)
print("ALLDONE", file=sys.stderr)
