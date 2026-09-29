#!/usr/bin/env python3
"""List the attachments `xcrun xcresulttool export attachments` wrote.

Reads the export directory's `manifest.json` (Xcode 16+: one entry per test
with an `attachments` list carrying `suggestedHumanReadableName` and
`exportedFileName`), drops the `_<n>_<uuid>` Xcode appends to each name and
prints one line per attachment, sorted, so the CI step summary and a Linux
checkout of the artefact name the screenshot set without opening an image.
Falls back to the directory listing when the manifest is missing or has
another shape. Exits 1 when nothing was exported or one of the expected
names (the remaining arguments) is missing. Never reads the images.
"""

import json
import os
import re
import sys

SUFFIX = re.compile(r"_\d+_[0-9A-Fa-f-]{36}(?=\.[^.]*$|$)")


def names(directory):
    try:
        with open(os.path.join(directory, "manifest.json"), encoding="utf-8") as handle:
            manifest = json.load(handle)
        raw = [
            attachment.get("suggestedHumanReadableName") or attachment["exportedFileName"]
            for entry in manifest
            for attachment in entry["attachments"]
        ]
    except (OSError, ValueError, KeyError, TypeError) as error:
        print(f"manifest not read ({error}); listing files", file=sys.stderr)
        raw = os.listdir(directory) if os.path.isdir(directory) else []
        raw = [name for name in raw if name != "manifest.json"]
    return sorted(SUFFIX.sub("", name) for name in raw)


def main(argv):
    if len(argv) < 2:
        print("usage: attachment-names.py <export-directory> [expected-name ...]", file=sys.stderr)
        return 2
    listed = names(argv[1])
    print(f"{len(listed)} attachments")
    for name in listed:
        print(f"- {name}")
    missing = [name for name in argv[2:] if name not in listed]
    if not listed or missing:
        print("missing: " + (", ".join(missing) or "every attachment"), file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
