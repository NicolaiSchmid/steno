#!/usr/bin/env python3
"""Word counts per minute of audio for the CoreML and WhisperKit references and one or
more spike transcripts, to see where in time the engines diverge on a file.

Usage: words_per_minute.py <baseline-dir> <spike-out-dir> <file-stem> <tag> [<tag2> ...]
"""
import json, sys
from pathlib import Path

base, out, stem, *tags = sys.argv[1:]
base, out = Path(base), Path(out)


def ref_starts(path):
    return [w["start"] for seg in json.loads(path.read_text()) for w in seg.get("wordTimings", [])]


cols = {
    "CoreML": ref_starts(base / f"{stem}.parakeet-v3.json"),
    "WhisperKit": ref_starts(base / f"{stem}.whisperkit-large-v3-turbo.json"),
}
for tag in tags:
    d = json.loads((out / f"{stem}.{tag}.json").read_text())
    cols[tag] = [w["start"] for w in d["asr"]["words"]]

minutes = int(max(max(c) for c in cols.values() if c) // 60) + 1
print("| Minute | " + " | ".join(cols) + " |")
print("|---|" + "---:|" * len(cols))
for m in range(minutes):
    row = [sum(1 for s in c if m * 60 <= s < (m + 1) * 60) for c in cols.values()]
    print(f"| {m}-{m+1} | " + " | ".join(str(x) for x in row) + " |")
print("| total | " + " | ".join(str(len(c)) for c in cols.values()) + " |")
