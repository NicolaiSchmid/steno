#!/usr/bin/env python3
"""Classify word-level differences between a spike transcript and the CoreML baseline.

Usage: diff_words.py <baseline-dir> <spike-out-dir> <tag> [file-prefix]
Prints per-file counts by class (case/umlaut-only, compound split/join, inflection
(shared 5-char stem), other substitution, deletion run >= 5 words, other deletion,
insertion) and, with -v, the aligned opcodes for inspection (never paste into reports).
"""
import difflib, json, sys
from pathlib import Path
sys.path.insert(0, str(Path(__file__).parent))
from score import normalise, baseline_text

def classify(ref, hyp):
    c = {"compound": 0, "inflection": 0, "other-sub": 0, "del-run>=5": 0, "del": 0, "ins-run>=5": 0, "ins": 0}
    sm = difflib.SequenceMatcher(a=ref, b=hyp, autojunk=False)
    ops = []
    for tag, i1, i2, j1, j2 in sm.get_opcodes():
        if tag == "equal": continue
        r, h = ref[i1:i2], hyp[j1:j2]
        ops.append((tag, " ".join(r), " ".join(h)))
        if tag == "delete":
            c["del-run>=5" if len(r) >= 5 else "del"] += len(r)
        elif tag == "insert":
            c["ins-run>=5" if len(h) >= 5 else "ins"] += len(h)
        else:
            if "".join(r) == "".join(h):
                c["compound"] += max(len(r), len(h))
            elif len(r) == len(h) and all(a[:5] == b[:5] for a, b in zip(r, h)):
                c["inflection"] += len(r)
            else:
                n = max(len(r), len(h))
                if n >= 5: c["del-run>=5" if len(r) > len(h) else "ins-run>=5"] += n
                else: c["other-sub"] += n
    return c, ops

def main():
    base, out, tag = Path(sys.argv[1]), Path(sys.argv[2]), sys.argv[3]
    prefix = next((a for a in sys.argv[4:] if not a.startswith("-")), "")
    verbose = "-v" in sys.argv
    for f in sorted(p for p in out.glob(f"{prefix}*.{tag}.json") if not p.name.startswith("score.")):
        stem = f.name[: -len(f".{tag}.json")]
        hyp = normalise(json.loads(f.read_text())["asr"]["text"])
        ref = normalise(baseline_text(base / f"{stem}.parakeet-v3.json"))
        c, ops = classify(ref, hyp)
        print(stem[:8], len(ref), len(hyp), c)
        if verbose:
            for op in ops: print("   ", op)

if __name__ == "__main__":
    main()
