#!/usr/bin/env python3
"""WER of Rust transcripts against the Swift (FluidAudio) baseline.
usage: wer.py <baseline-dir> <rust-out-dir>
Normalisation: lowercase, strip punctuation, collapse whitespace."""
import glob, json, os, re, sys

def norm(text):
    text = text.lower()
    text = re.sub(r"[^\w\s]", " ", text, flags=re.UNICODE)
    return text.split()

def lev(a, b):
    prev = list(range(len(b) + 1))
    for i, x in enumerate(a, 1):
        cur = [i]
        for j, y in enumerate(b, 1):
            cur.append(min(prev[j] + 1, cur[j - 1] + 1, prev[j - 1] + (x != y)))
        prev = cur
    return prev[-1]

base_dir, rust_dir = sys.argv[1], sys.argv[2]
print("| File | Swift words | Rust words | Edits | WER vs Swift |")
print("|---|---:|---:|---:|---:|")
tot_e = tot_n = 0
for path in sorted(glob.glob(os.path.join(rust_dir, "*.rust.json"))):
    name = os.path.basename(path).replace(".rust.json", "")
    rust = norm(json.load(open(path))["text"])
    segs = json.load(open(os.path.join(base_dir, f"{name}.parakeet-v3.json")))
    ref = norm(" ".join(s["text"] for s in segs))
    e = lev(ref, rust)
    tot_e += e; tot_n += len(ref)
    print(f"| {name} | {len(ref)} | {len(rust)} | {e} | {100*e/max(1,len(ref)):.2f}% |")
print(f"| all | {tot_n} | | {tot_e} | {100*tot_e/max(1,tot_n):.2f}% |")
