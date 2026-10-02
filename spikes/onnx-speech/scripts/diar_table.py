#!/usr/bin/env python3
"""Diarization speaker counts per threshold vs truth and FluidAudio (threshold 0.8).
Usage: diar_table.py <spike-out-dir> <tag>"""
import json, sys
from pathlib import Path
TRUTH = {"2d7b9aba": "group (<=7)", "c0cd3671": "group (<=7)"}
FLUID_08 = {"2d7b9aba": 2, "5ea9e7e8": 1, "7eb51e56": 1, "83bb1859": 1, "bfbeef67": 1, "c0cd3671": 2, "f5fd585d": 1}
out, tag = Path(sys.argv[1]), sys.argv[2]
rows = []
for f in sorted(out.glob(f"*.{tag}.json")):
    rep = json.loads(f.read_text()); d = rep.get("diarization")
    if not d: continue
    rows.append((f.name[:8], d))
ths = [r["threshold"] for r in rows[0][1]["runs"]] if rows else []
print(f"| File | Truth | FluidAudio @0.8 | " + " | ".join(f"ONNX th {t}" for t in ths) + " | Wall s per pass (min-max) | RTFx | Load | Peak RSS MB |")
print("|---|---|---:|" + "---:|" * len(ths) + "---|---:|---:|---:|")
for stem, d in rows:
    runs = d["runs"]; walls = [r["wall_s"] for r in runs]
    counts = " | ".join(str(r["num_speakers"]) for r in runs)
    print(f"| {stem} | {TRUTH.get(stem, '1')} | {FLUID_08[stem]} | {counts} | {min(walls):.0f}-{max(walls):.0f} | {600/ (sum(walls)/len(walls)):.1f} | {min(r['load_1min_before'] for r in runs):.0f}-{max(r['load_1min_before'] for r in runs):.0f} | {d['peak_rss_mb_after']:.0f} |")
