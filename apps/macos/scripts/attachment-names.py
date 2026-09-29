#!/usr/bin/env python3
"""List the attachments `xcrun xcresulttool export attachments` wrote.

Reads the export directory's `manifest.json` (Xcode 16+: one entry per test
with an `attachments` list carrying `suggestedHumanReadableName` and
`exportedFileName`) and prints one line per attachment, sorted, so the CI
step summary and a Linux checkout of the artefact name the screenshot set
without opening an image. Falls back to the directory listing when the
manifest is missing or has another shape. Never reads the images.
"""

import json
import os
import sys


def names(directory):
    manifest = os.path.join(directory, "manifest.json")
    result = []
    try:
        with open(manifest, encoding="utf-8") as handle:
            entries = json.load(handle)
        for entry in entries:
            for attachment in entry.get("attachments", []):
                human = attachment.get("suggestedHumanReadableName", "")
                exported = attachment.get("exportedFileName", "")
                result.append((human or exported, exported))
    except (OSError, ValueError, AttributeError, TypeError) as error:
        print(f"manifest not read ({error}); listing files", file=sys.stderr)
        for name in os.listdir(directory):
            if name != "manifest.json":
                result.append((name, name))
    return sorted(result)


def main(argv):
    if len(argv) != 2:
        print("usage: attachment-names.py <export-directory>", file=sys.stderr)
        return 2
    listed = names(argv[1])
    print(f"{len(listed)} attachments")
    for human, exported in listed:
        print(f"- {human}" + (f" ({exported})" if exported and exported != human else ""))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
