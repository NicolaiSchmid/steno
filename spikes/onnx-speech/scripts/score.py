#!/usr/bin/env python3
"""Score ONNX spike transcripts against the Swift bake-off (CoreML Parakeet and WhisperKit).

Usage: score.py <baseline-dir> <spike-out-dir> <tag> [<tag2> ...]
Prints a Markdown table and writes <spike-out-dir>/score.<tag>.json.
"""
import json, re, sys, unicodedata
from pathlib import Path


def normalise(text: str) -> list[str]:
    text = unicodedata.normalize("NFC", text).lower()
    text = re.sub(r"[^\w\s]", " ", text)  # strip punctuation, keep letters/digits/underscore (incl. umlauts)
    return text.split()


def wer(ref: list[str], hyp: list[str]) -> tuple[float, int, int, int]:
    """Levenshtein word error rate: (wer, subs, dels, ins)."""
    n, m = len(ref), len(hyp)
    # dp[i][j] = (cost, s, d, i)
    prev = [(j, 0, 0, j) for j in range(m + 1)]
    for i in range(1, n + 1):
        cur = [(i, 0, i, 0)] + [None] * m
        for j in range(1, m + 1):
            if ref[i - 1] == hyp[j - 1]:
                cur[j] = prev[j - 1]
            else:
                sub = prev[j - 1]; dele = prev[j]; ins = cur[j - 1]
                best = min((sub[0] + 1, 0, sub), (dele[0] + 1, 1, dele), (ins[0] + 1, 2, ins), key=lambda t: t[0])
                c, kind, src = best
                cur[j] = (c, src[1] + (kind == 0), src[2] + (kind == 1), src[3] + (kind == 2))
        prev = cur
    c, s, d, i = prev[m]
    return (c / n if n else 0.0), s, d, i


def baseline_text(path: Path) -> str:
    segs = json.loads(path.read_text())
    return " ".join(s.get("text", "") for s in segs)


def main():
    base = Path(sys.argv[1]); out = Path(sys.argv[2]); tags = sys.argv[3:]
    for tag in tags:
        rows = []
        for f in sorted(p for p in out.glob(f"*.{tag}.json") if not p.name.startswith("score.")):
            stem = f.name[: -len(f".{tag}.json")]
            rep = json.loads(f.read_text())
            if not rep.get("asr"):
                continue
            hyp = normalise(rep["asr"]["text"])
            row = {"file": stem, "tag": tag, "hyp_words": len(hyp), "rtfx": rep["asr"]["rtfx"], "wall_s": rep["asr"]["wall_s"], "threads": rep["asr"]["threads"], "load": rep["asr"]["load_1min_before"], "rss_mb": rep["asr"]["peak_rss_mb_after"]}
            for eng, key in (("parakeet-v3", "coreml"), ("whisperkit-large-v3-turbo", "whisperkit")):
                bp = base / f"{stem}.{eng}.json"
                if bp.exists():
                    ref = normalise(baseline_text(bp))
                    w, s, d, i = wer(ref, hyp)
                    row[f"wer_{key}"] = w; row[f"{key}_words"] = len(ref); row[f"{key}_sdi"] = (s, d, i)
            st = rep["asr"].get("stages")
            if st:
                row["segments"] = st["segments"]; row["decoded_s"] = st["decoded_s"]; row["speech_s"] = st["speech_s"]
                row["lane_langs"] = st["lane_langs"]; row["slid_votes"] = st["slid_votes"]
                row["flagged"] = st["flagged"]; row["changed"] = st["changed"]; row["cuts"] = st["cuts"]
                row["flagged_lang"] = st.get("flagged_lang", 0); row["flagged_empty"] = st.get("flagged_empty", 0); row["changed_lang"] = st.get("changed_lang", 0); row["changed_empty"] = st.get("changed_empty", 0)
            if rep.get("diarization"):
                row["speakers"] = {str(r["threshold"]): r["num_speakers"] for r in rep["diarization"]["runs"]}
                row["diar_wall_s"] = rep["diarization"]["runs"][0]["wall_s"] if rep["diarization"]["runs"] else None
            rows.append(row)
        (out / f"score.{tag}.json").write_text(json.dumps(rows, indent=1))
        print(f"\n### {tag}\n")
        staged = rows and "segments" in rows[0]
        seg_hdr = " Segments | Decoded s | Lane (votes) | Flagged/changed |" if staged else ""
        seg_sep = "---:|---:|---|---:|" if staged else ""
        print(f"| File |{seg_hdr} Wall s | RTFx | Threads | Load | Peak RSS MB | Words (ONNX/CoreML/WK) | WER vs CoreML (S/D/I) | WER vs WhisperKit |")
        print(f"|---|{seg_sep}---:|---:|---:|---:|---:|---|---:|---:|")
        for r in rows:
            sdi = "/".join(map(str, r.get("coreml_sdi", ("-", "-", "-"))))
            seg = ""
            if staged:
                votes = " ".join(f"{k}:{v}" for k, v in sorted(r["slid_votes"].items(), key=lambda kv: -kv[1])) or "-"
                lane = ",".join(r["lane_langs"]) or "-"
                seg = f" {r['segments']} | {r['decoded_s']:.0f} | {lane} ({votes}) | {r['flagged']}/{r['changed']} |"
            print(f"| {r['file'][:8]} |{seg} {r['wall_s']:.2f} | {r['rtfx']:.1f} | {r['threads']} | {r['load']:.1f} | {r['rss_mb']:.0f} | {r['hyp_words']}/{r.get('coreml_words','-')}/{r.get('whisperkit_words','-')} | {r.get('wer_coreml', float('nan'))*100:.1f}% ({sdi}) | {r.get('wer_whisperkit', float('nan'))*100:.1f}% |")
        if rows:
            import statistics as st
            seg = f" {st.mean(r['segments'] for r in rows):.1f} | {st.mean(r['decoded_s'] for r in rows):.0f} | | {sum(r['flagged'] for r in rows)}/{sum(r['changed'] for r in rows)} |" if staged else ""
            print(f"| mean |{seg} {st.mean(r['wall_s'] for r in rows):.2f} | {st.mean(r['rtfx'] for r in rows):.1f} | | | {max(r['rss_mb'] for r in rows):.0f} (max) | | {st.mean(r.get('wer_coreml',0) for r in rows)*100:.1f}% | {st.mean(r.get('wer_whisperkit',0) for r in rows)*100:.1f}% |  max {max(r.get('wer_coreml',0) for r in rows)*100:.1f}% |")
        if rows and "speakers" in rows[0]:
            ths = list(rows[0]["speakers"].keys())
            print("\n| File | " + " | ".join(f"th {t}" for t in ths) + " | diar wall s |")
            print("|---|" + "---:|" * (len(ths) + 1))
            for r in rows:
                print(f"| {r['file'][:8]} | " + " | ".join(str(r['speakers'].get(t, '-')) for t in ths) + f" | {r.get('diar_wall_s') or 0:.2f} |")


if __name__ == "__main__":
    main()
