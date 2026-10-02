#!/usr/bin/env python3
"""Build the FLEURS German WER set (spike F).

Downloads google/fleurs de_de test (CC-BY-4.0), takes the first N utterances, writes
each as 16 kHz mono Int16 WAV + <name>.ref.txt (the `transcription` field, already
lowercased and punctuation-free) into utt/, and concatenates them in groups of
CAT_SIZE with CAT_GAP s of silence into cat/ with the references joined by spaces.

Usage (inside `python:3.11` Docker with datasets + soundfile installed):
  python make_fleurs_de.py /work [--n 300] [--cat-size 30] [--cat-gap 0.7]
"""
import argparse, json, sys
from pathlib import Path

import numpy as np
import soundfile as sf
from datasets import load_dataset, Audio

ap = argparse.ArgumentParser()
ap.add_argument("out")
ap.add_argument("--n", type=int, default=300)
ap.add_argument("--cat-size", type=int, default=30)
ap.add_argument("--cat-gap", type=float, default=0.7)
a = ap.parse_args()

out = Path(a.out); utt = out / "utt"; cat = out / "cat"
utt.mkdir(parents=True, exist_ok=True); cat.mkdir(parents=True, exist_ok=True)

ds = load_dataset("google/fleurs", "de_de", split="test", trust_remote_code=True)
ds = ds.cast_column("audio", Audio(sampling_rate=16000))
print("rows in split:", len(ds), file=sys.stderr)

manifest = []
buf, refs, group = [], [], 1
gap = np.zeros(int(16000 * a.cat_gap), dtype=np.int16)
for i in range(a.n):
    row = ds[i]
    x = row["audio"]["array"]
    assert row["audio"]["sampling_rate"] == 16000
    pcm = np.clip(np.asarray(x, dtype=np.float64) * 32767.0, -32768, 32767).astype(np.int16)
    name = f"fleurs-de-{i+1:04d}"
    ref = " ".join(row["transcription"].split())
    sf.write(utt / f"{name}.wav", pcm, 16000, subtype="PCM_16")
    (utt / f"{name}.ref.txt").write_text(ref + "\n", encoding="utf-8")
    manifest.append({"name": name, "fleurs_id": row["id"], "seconds": round(len(pcm) / 16000, 2), "words": len(ref.split()), "raw_transcription": row["raw_transcription"]})
    buf.append(pcm); refs.append(ref)
    if len(buf) == a.cat_size:
        pieces = []
        for k, p in enumerate(buf):
            if k: pieces.append(gap)
            pieces.append(p)
        cname = f"fleurs-de-cat-{group:02d}"
        sf.write(cat / f"{cname}.wav", np.concatenate(pieces), 16000, subtype="PCM_16")
        (cat / f"{cname}.ref.txt").write_text(" ".join(refs) + "\n", encoding="utf-8")
        buf, refs = [], []; group += 1
(out / "manifest.json").write_text(json.dumps(manifest, indent=1, ensure_ascii=False), encoding="utf-8")
tot = sum(m["seconds"] for m in manifest)
print(f"wrote {len(manifest)} utterances, {tot/60:.1f} min, {sum(m['words'] for m in manifest)} words; {group-1} cat files", file=sys.stderr)
