#!/usr/bin/env python3
"""Score engine transcripts against FLEURS references with one normaliser (spike F).

Sources (any mix, each becomes a column):
  --bakeoff <dir> <engine>   `steno dev bakeoff` output: <name>.<engine>.json (segments with "text")
                             plus report.json (wall s, RTFx per file)
  --harness <dir> <tag>      onnx-speech-spike output: <name>.<tag>.json (asr.text, asr.rtfx, asr.load_1min_before)
  --ort <dir> <tag>          ort_decode_dir.py output, same shape as --harness

Normalisation (applied to reference and hypothesis alike): NFC, lowercase, `%`->` prozent `,
`€`->` euro `, strip everything that is not a letter or digit, collapse whitespace.
With --numbers, digit tokens are additionally spelled out in German with num2words
(`12` -> `zwölf`, `1,5` -> `eins komma fünf`, `1.000` -> `eintausend`); years are
spelled as cardinals (`1990` -> `eintausendneunhundertneunzig`), which may differ from
a reference that says `neunzehnhundertneunzig`. --fold-umlauts applies the Swift
bake-off's ae/oe/ue/ss folding.

Usage: score_fleurs.py <ref-dir> [--numbers] [--fold-umlauts] [--out score.json] (--bakeoff D E | --harness D T | --ort D T)...
"""
import argparse, json, re, statistics, sys, unicodedata
from pathlib import Path

try:
    from num2words import num2words
except ImportError:  # only needed with --numbers
    num2words = None


def normalise(text: str, numbers: bool, fold: bool) -> list[str]:
    text = unicodedata.normalize("NFC", text).lower()
    text = text.replace("%", " prozent ").replace("€", " euro ").replace("$", " dollar ")
    if numbers:
        text = re.sub(r"(?<=\d)\.(?=\d{3}\b)", "", text)  # 1.000 -> 1000
        text = re.sub(r"\d+,\d+", lambda m: " komma ".join(num2words(int(p), lang="de") for p in m.group(0).split(",")), text)
        text = re.sub(r"\d+", lambda m: " " + num2words(int(m.group(0)), lang="de") + " ", text)
    if fold:
        for a, b in (("ä", "ae"), ("ö", "oe"), ("ü", "ue"), ("ß", "ss")):
            text = text.replace(a, b)
    return [w for w in re.split(r"[^\w]+", text.replace("_", " ")) if w]


def edit_distance(ref: list[str], hyp: list[str]) -> tuple[int, int, int, int]:
    """(errors, subs, dels, ins) by word-level Levenshtein."""
    n, m = len(ref), len(hyp)
    prev = [(j, 0, 0, j) for j in range(m + 1)]
    for i in range(1, n + 1):
        cur = [(i, 0, i, 0)] + [None] * m
        for j in range(1, m + 1):
            if ref[i - 1] == hyp[j - 1]:
                cur[j] = prev[j - 1]
            else:
                cands = ((prev[j - 1][0] + 1, 0, prev[j - 1]), (prev[j][0] + 1, 1, prev[j]), (cur[j - 1][0] + 1, 2, cur[j - 1]))
                c, kind, src = min(cands, key=lambda t: t[0])
                cur[j] = (c, src[1] + (kind == 0), src[2] + (kind == 1), src[3] + (kind == 2))
        prev = cur
    return prev[m]


def load_bakeoff(d: Path, engine: str) -> dict[str, dict]:
    rep = {}
    rj = d / "report.json"
    if rj.exists():
        data = json.loads(rj.read_text())
        rows = data.get("rows", data if isinstance(data, list) else [])
        for r in rows:
            if r.get("engine") == engine:
                rep[Path(r["file"]).stem] = r
    out = {}
    for f in sorted(d.glob(f"*.{engine}.json")):
        stem = f.name[: -len(f".{engine}.json")]
        segs = json.loads(f.read_text())
        text = " ".join(s.get("text", "") for s in segs) if isinstance(segs, list) else segs.get("text", "")
        r = rep.get(stem, {})
        wall = r.get("wallSeconds"); audio = r.get("audioSeconds")
        out[stem] = {"text": text, "wall_s": wall, "rtfx": (audio / wall) if wall and audio else None, "load": None}
    return out


def load_harness(d: Path, tag: str) -> dict[str, dict]:
    out = {}
    for f in sorted(d.glob(f"*.{tag}.json")):
        if f.name.startswith("score."):
            continue
        stem = f.name[: -len(f".{tag}.json")]
        rep = json.loads(f.read_text())
        a = rep.get("asr") or {}
        out[stem] = {"text": a.get("text", ""), "wall_s": a.get("wall_s"), "rtfx": a.get("rtfx"), "load": a.get("load_1min_before")}
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("ref_dir")
    ap.add_argument("--numbers", action="store_true")
    ap.add_argument("--fold-umlauts", action="store_true")
    ap.add_argument("--out")
    ap.add_argument("--bakeoff", nargs=2, action="append", default=[], metavar=("DIR", "ENGINE"))
    ap.add_argument("--harness", nargs=2, action="append", default=[], metavar=("DIR", "TAG"))
    ap.add_argument("--ort", nargs=2, action="append", default=[], metavar=("DIR", "TAG"))
    ap.add_argument("--per-file", action="store_true", help="print the per-file table")
    ap.add_argument("--first", type=int, default=0, help="score only the first N references (sorted by name)")
    a = ap.parse_args()
    if a.numbers and num2words is None:
        sys.exit("pip install num2words")

    refs = {p.name[: -len(".ref.txt")]: p.read_text(encoding="utf-8") for p in Path(a.ref_dir).glob("*.ref.txt")}
    if a.first:
        refs = dict(sorted(refs.items())[: a.first])
    cols: list[tuple[str, dict[str, dict]]] = []
    for d, e in a.bakeoff:
        cols.append((e, load_bakeoff(Path(d), e)))
    for d, t in a.harness + a.ort:
        cols.append((t, load_harness(Path(d), t)))

    result = {"ref_dir": a.ref_dir, "numbers": a.numbers, "fold_umlauts": a.fold_umlauts, "columns": {}}
    print(f"\n### {Path(a.ref_dir).name} (numbers={'words' if a.numbers else 'raw'}, umlauts={'folded' if a.fold_umlauts else 'kept'})\n")
    print("| Engine | Files | Ref words | Mean WER | Median | Pooled WER | S/D/I | Mean RTFx | Load (mean 1-min) |")
    print("|---|---:|---:|---:|---:|---:|---|---:|---:|")
    per_file: dict[str, dict[str, float]] = {}
    for name, hyps in cols:
        rows = []
        for stem in sorted(refs):
            if stem not in hyps:
                continue
            r = normalise(refs[stem], a.numbers, a.fold_umlauts)
            h = normalise(hyps[stem]["text"], a.numbers, a.fold_umlauts)
            err, s, d, i = edit_distance(r, h)
            w = err / len(r) if r else 0.0
            rows.append({"file": stem, "ref_words": len(r), "hyp_words": len(h), "errors": err, "subs": s, "dels": d, "ins": i, "wer": w, "wall_s": hyps[stem]["wall_s"], "rtfx": hyps[stem]["rtfx"], "load": hyps[stem]["load"]})
            per_file.setdefault(stem, {})[name] = w
        if not rows:
            print(f"| {name} | 0 | | | | | | | |")
            continue
        tot_ref = sum(x["ref_words"] for x in rows); tot_err = sum(x["errors"] for x in rows)
        wers = [x["wer"] for x in rows]
        rt = [x["rtfx"] for x in rows if x["rtfx"] is not None]
        ld = [x["load"] for x in rows if x["load"] is not None]
        summ = {"files": len(rows), "ref_words": tot_ref, "mean_wer": statistics.mean(wers), "median_wer": statistics.median(wers), "pooled_wer": tot_err / tot_ref,
                "subs": sum(x["subs"] for x in rows), "dels": sum(x["dels"] for x in rows), "ins": sum(x["ins"] for x in rows),
                "mean_rtfx": statistics.mean(rt) if rt else None, "mean_load": statistics.mean(ld) if ld else None, "max_wer": max(wers), "max_file": max(rows, key=lambda x: x["wer"])["file"]}
        result["columns"][name] = {"summary": summ, "rows": rows}
        print(f"| {name} | {summ['files']} | {tot_ref} | {summ['mean_wer']*100:.1f}% | {summ['median_wer']*100:.1f}% | {summ['pooled_wer']*100:.1f}% | {summ['subs']}/{summ['dels']}/{summ['ins']} | {summ['mean_rtfx'] or float('nan'):.1f} | {summ['mean_load'] if summ['mean_load'] is not None else '-'} |")
    if a.per_file and per_file:
        names = [n for n, _ in cols]
        print("\n| File | Ref words | " + " | ".join(names) + " |")
        print("|---|---:|" + "---:|" * len(names))
        for stem in sorted(per_file):
            rw = len(normalise(refs[stem], a.numbers, a.fold_umlauts))
            print(f"| {stem} | {rw} | " + " | ".join(f"{per_file[stem][n]*100:.1f}%" if n in per_file[stem] else "-" for n in names) + " |")
    if a.out:
        Path(a.out).write_text(json.dumps(result, indent=1, ensure_ascii=False))


if __name__ == "__main__":
    main()
