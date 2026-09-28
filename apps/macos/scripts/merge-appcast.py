#!/usr/bin/env python3
"""Merge one release's Sparkle appcast into the rolling appcast.

The release workflow generates an appcast for the release it just built
(`make-appcast.sh`, one <item>). This script folds that item into the
rolling `appcast.xml` on the `appcast` branch, which every installed build
reads:

- items are ordered newest first by `sparkle:version` (the build number);
- an item with the same `sparkle:version` as an existing one replaces it;
- `--channel <name>` adds `<sparkle:channel>` to the new items, so a
  pre-release is only offered to builds that allow that channel;
- the list is capped at `--max-items` (default 20).

Usage: merge-appcast.py <rolling.xml> <release.xml> <out.xml> [--channel beta] [--max-items 20]

`rolling.xml` may not exist yet (the first release); the output is then the
release appcast, with the channel applied. Standard library only.
"""

import argparse
import os
import sys
import xml.etree.ElementTree as ET

SPARKLE_NS = "http://www.andymatuschak.org/xml-namespaces/sparkle"
SPARKLE = "{%s}" % SPARKLE_NS


def sparkle_version(item):
    element = item.find(SPARKLE + "version")
    if element is None or not (element.text or "").strip():
        raise SystemExit("::error::an appcast item has no sparkle:version")
    text = element.text.strip()
    try:
        return int(text)
    except ValueError:
        raise SystemExit("::error::sparkle:version %r is not an integer" % text)


def set_channel(item, channel):
    existing = item.find(SPARKLE + "channel")
    if existing is None:
        existing = ET.SubElement(item, SPARKLE + "channel")
    existing.text = channel


def load_channel(path):
    tree = ET.parse(path)
    root = tree.getroot()
    channel = root.find("channel")
    if channel is None:
        raise SystemExit("::error::%s has no <channel>" % path)
    return tree, channel


def main(argv):
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("rolling")
    parser.add_argument("release")
    parser.add_argument("out")
    parser.add_argument("--channel", default=None)
    parser.add_argument("--max-items", type=int, default=20)
    args = parser.parse_args(argv)

    ET.register_namespace("sparkle", SPARKLE_NS)

    _, release_channel = load_channel(args.release)
    new_items = release_channel.findall("item")
    if not new_items:
        raise SystemExit("::error::%s carries no <item>" % args.release)
    if args.channel:
        for item in new_items:
            set_channel(item, args.channel)

    # Without a rolling file the release appcast is the template; its own
    # items are dropped below as duplicates of the new ones.
    tree, channel = load_channel(
        args.rolling if os.path.exists(args.rolling) else args.release
    )
    existing = channel.findall("item")
    for item in existing:
        channel.remove(item)

    new_versions = {sparkle_version(item) for item in new_items}
    merged = list(new_items) + [
        item for item in existing if sparkle_version(item) not in new_versions
    ]
    merged.sort(key=sparkle_version, reverse=True)
    merged = merged[: args.max_items]
    for item in merged:
        channel.append(item)

    ET.indent(tree, space="    ")
    tree.write(args.out, encoding="utf-8", xml_declaration=True)
    with open(args.out, "a", encoding="utf-8") as handle:
        handle.write("\n")
    print("%s: %d item(s), newest build %d" % (args.out, len(merged), sparkle_version(merged[0])))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
