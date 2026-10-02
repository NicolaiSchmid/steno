#!/usr/bin/env python3
"""numpy-only TDT greedy decode (spike E diagnostic) of one 16 kHz wav through an ONNX Parakeet export,
with diagnostics (NaN count, logits range, first decode steps). Adapted from
sherpa-onnx scripts/nemo/parakeet-tdt-0.6b-v2/test_onnx.py."""
import argparse, time
import kaldi_native_fbank as knf
import numpy as np
import onnxruntime as ort
import soundfile as sf

ap = argparse.ArgumentParser()
ap.add_argument("--dir", required=True)
ap.add_argument("--files", default="int8", choices=["int8", "fp32"])
ap.add_argument("--wav", required=True)
ap.add_argument("--threads", type=int, default=4)
ap.add_argument("--ddof", type=int, default=1)
ap.add_argument("--no-librosa", action="store_true")
ap.add_argument("--dither", type=float, default=0.0)
ap.add_argument("--dc", action="store_true")
ap.add_argument("--high-freq", type=float, default=0.0)
ap.add_argument("--no-snip", action="store_true")
ap.add_argument("--preemph", type=float, default=0.97)
a = ap.parse_args()
suf = ".int8.onnx" if a.files == "int8" else ".onnx"
so = ort.SessionOptions(); so.intra_op_num_threads = a.threads; so.inter_op_num_threads = 1
import os; os.chdir(a.dir)
enc = ort.InferenceSession(f"encoder{suf}", so, providers=["CPUExecutionProvider"])
dec = ort.InferenceSession(f"decoder{suf}", so, providers=["CPUExecutionProvider"])
joi = ort.InferenceSession(f"joiner{suf}", so, providers=["CPUExecutionProvider"])
meta = enc.get_modelmeta().custom_metadata_map
vocab_size = int(meta["vocab_size"]); blank = vocab_size - 1
pred_rnn_layers = int(meta["pred_rnn_layers"]); pred_hidden = int(meta["pred_hidden"])
id2token = {}
for line in open("tokens.txt", encoding="utf-8"):
    t, i = line.rsplit(" ", 1); id2token[int(i)] = t

audio, sr = sf.read(a.wav, dtype="float32"); assert sr == 16000
if audio.ndim > 1: audio = audio[:, 0]
opts = knf.FbankOptions(); opts.frame_opts.dither = a.dither; opts.frame_opts.remove_dc_offset = a.dc
opts.frame_opts.window_type = "hann"; opts.mel_opts.low_freq = 0; opts.mel_opts.high_freq = a.high_freq; opts.frame_opts.snip_edges = not a.no_snip; opts.frame_opts.preemph_coeff = a.preemph; opts.mel_opts.num_bins = 128
opts.mel_opts.is_librosa = not a.no_librosa
fb = knf.OnlineFbank(opts); fb.accept_waveform(16000, audio.tolist()); fb.input_finished()
feats = np.stack([fb.get_frame(i) for i in range(fb.num_frames_ready)])
if meta["normalize_type"] == "per_feature":
    feats = (feats - feats.mean(0, keepdims=True)) / (feats.std(0, keepdims=True, ddof=a.ddof) + 1e-5)
x = feats.T[None].astype(np.float32)
t0 = time.time()
enc_out, enc_len = enc.run(None, {enc.get_inputs()[0].name: x, enc.get_inputs()[1].name: np.array([x.shape[2]], dtype=np.int64)})
print(f"ort {ort.__version__} enc_out {enc_out.shape} len {enc_len} nan {int(np.isnan(enc_out).sum())} inf {int(np.isinf(enc_out).sum())} absmax {float(np.abs(np.nan_to_num(enc_out)).max()):.2f} encoder {time.time()-t0:.1f}s")
s0 = np.zeros((pred_rnn_layers, 1, pred_hidden), np.float32); s1 = s0.copy()
def run_dec(tok, s0, s1):
    o = dec.run(None, {dec.get_inputs()[0].name: np.array([[tok]], np.int32), dec.get_inputs()[1].name: np.array([1], np.int32), dec.get_inputs()[2].name: s0, dec.get_inputs()[3].name: s1})
    return o[0], o[2], o[3]
dec_out, s0n, s1n = run_dec(blank, s0, s1)
ans, steps, t, T = [], [], 0, enc_out.shape[2]
while t < T:
    logits = joi.run(None, {joi.get_inputs()[0].name: enc_out[:, :, t:t+1], joi.get_inputs()[1].name: dec_out})[0].squeeze()
    idx = int(np.argmax(logits[:vocab_size])); skip = int(np.argmax(logits[vocab_size:]))
    if len(steps) < 12: steps.append((t, idx, skip, round(float(logits[idx]), 1), round(float(logits[blank]), 1)))
    if skip == 0: skip = 1
    if idx != blank:
        ans.append(idx); s0, s1 = s0n, s1n; dec_out, s0n, s1n = run_dec(idx, s0, s1)
    t += skip
text = "".join(id2token[i] for i in ans).replace("▁", " ").strip()
print(f"tokens {len(ans)} words {len(text.split())} chars {len(text)} total {time.time()-t0:.1f}s")
print("first steps (t, token, skip, best_logit, blank_logit):", steps)
