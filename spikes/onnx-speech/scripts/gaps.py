#!/usr/bin/env python3
"""Where does the ONNX transcript lose CoreML words? For each CoreML segment, count
ONNX words whose timestamp falls inside it; list segments with under half coverage,
with the ONNX chunk that covers that time and its word count and text language.

Usage: gaps.py <baseline-dir> <spike-out-dir> <tag> <file-prefix>
"""
import json, sys
from pathlib import Path

def main():
    base, out, tag, prefix = Path(sys.argv[1]), Path(sys.argv[2]), sys.argv[3], sys.argv[4]
    f = next(p for p in out.glob(f"{prefix}*.{tag}.json") if not p.name.startswith("score."))
    stem = f.name[: -len(f".{tag}.json")]
    rep = json.loads(f.read_text())["asr"]
    words = rep["words"]
    segs = rep.get("stages", {}).get("segment_info", [])
    ref = json.loads((base / f"{stem}.parakeet-v3.json").read_text())
    lost = 0
    print(f"{stem[:8]}: {len(ref)} CoreML segments, {len(words)} ONNX words, {len(segs)} ONNX chunks")
    for s in ref:
        n_ref = len(s["text"].split())
        n_hyp = sum(1 for w in words if s["start"] - 0.5 <= w["start"] <= s["end"] + 0.5)
        if n_ref >= 5 and n_hyp < 0.5 * n_ref:
            lost += n_ref - n_hyp
            cover = [c for c in segs if c["start"] <= s["end"] and c["end"] >= s["start"]]
            cv = " ".join(f"[{c['start']:.0f}-{c['end']:.0f} {c['cut']} {c['words']}w {c['lang']}]" for c in cover) or "NO CHUNK"
            print(f"  ref {s['start']:7.1f}-{s['end']:7.1f} {s.get('language','?')} {n_ref:3d}w -> onnx {n_hyp:3d}w  chunks {cv}")
    print(f"  lost in low-coverage segments: {lost}")

if __name__ == "__main__":
    main()
