#!/usr/bin/env python3
"""Summarise one or more .xcresult bundles for the CI step summary.

Mirrors the package job's rule: totals plus every skipped test's name and
message, and a non-zero exit when a skip message names no STENO_* opt-in
switch or when no bundle produced results. Uses `xcrun xcresulttool get
test-results` (Xcode 16+).
"""

import json
import os
import subprocess
import sys


def xcresulttool(*args):
    out = subprocess.run(
        ["xcrun", "xcresulttool", "get", "test-results", *args, "--compact"],
        check=True,
        capture_output=True,
        text=True,
    ).stdout
    return json.loads(out)


def walk(node, path, cases):
    node_type = node.get("nodeType", "")
    name = node.get("name", "")
    here = path + [name] if name and node_type not in ("Test Plan", "Unit test bundle", "UI test bundle") else path
    if node_type == "Test Case":
        details = [
            child.get("name", "")
            for child in node.get("children", [])
            if child.get("nodeType") in ("Failure Message", "Skipped", "Expected Failure", "Repetition")
            or child.get("result") in ("Skipped", "Failed")
        ]
        cases.append(("/".join(here), node.get("result", ""), " ".join(d for d in details if d)))
        return
    for child in node.get("children", []):
        walk(child, here, cases)


def main(paths):
    totals = {"tests": 0, "passed": 0, "failed": 0, "skipped": 0}
    skipped, failed, missing = [], [], []
    for path in paths:
        if not os.path.isdir(path):
            missing.append(path)
            continue
        summary = xcresulttool("summary", "--path", path)
        totals["tests"] += int(summary.get("totalTestCount", 0))
        totals["passed"] += int(summary.get("passedTests", 0))
        totals["failed"] += int(summary.get("failedTests", 0))
        totals["skipped"] += int(summary.get("skippedTests", 0))
        tree = xcresulttool("tests", "--path", path)
        cases = []
        for node in tree.get("testNodes", []):
            walk(node, [], cases)
        for name, result, detail in cases:
            if result == "Skipped":
                skipped.append((name, detail))
            elif result == "Failed":
                failed.append((name, detail))

    lines = ["## macOS app tests", ""]
    lines.append("| Tests | Passed | Failed | Skipped |")
    lines.append("|---|---|---|---|")
    lines.append(f'| {totals["tests"]} | {totals["passed"]} | {totals["failed"]} | {totals["skipped"]} |')
    lines.append("")
    if failed:
        lines.append("### Failed")
        lines.extend(f"- `{name}`: {detail}" for name, detail in failed)
        lines.append("")
    if skipped:
        lines.append("### Skipped")
        lines.extend(f"- `{name}`: {detail or '(no message)'}" for name, detail in skipped)
        lines.append("")
    for path in paths:
        state = "missing" if path in missing else "read"
        lines.append(f"Source: `{path}` ({state})")
    text = "\n".join(lines) + "\n"
    print(text)
    step_summary = os.environ.get("GITHUB_STEP_SUMMARY")
    if step_summary:
        with open(step_summary, "a", encoding="utf-8") as handle:
            handle.write(text)

    bad = [name for name, detail in skipped if "STENO_" not in detail]
    if bad:
        print("::error::skipped tests whose message names no STENO_* opt-in switch: " + ", ".join(bad))
        return 1
    if len(missing) == len(paths):
        print("::error::no xcresult bundle was produced")
        return 1
    if totals["tests"] == 0:
        print("::error::no tests ran")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
