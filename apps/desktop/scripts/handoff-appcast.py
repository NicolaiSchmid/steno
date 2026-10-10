#!/usr/bin/env python3
"""The Sparkle handoff's reads and the one write of desktop-release.yml.

The Swift app's last update is one item without a channel on the `appcast`
branch (stable plan D8, "The Sparkle handoff"). Five subcommands, each over
an appcast file:

  check-build <appcast.xml> <build>
      Exit 1 with an ::error:: unless <build> is above every
      `sparkle:version` in the file, the element or the enclosure's
      attribute: Sparkle orders by build number alone, so a lower one is
      never offered. `plan` runs it before any bundle is built.

  has-handoff-item <appcast.xml>
      Print `true` when an item carries no `sparkle:channel`, `false`
      otherwise. `publish` writes it as the output the `handoff` job reads.

  has-build <appcast.xml> <build>
      Print `true` when an item carries `sparkle:version` <build>, `false`
      otherwise. The `handoff` job runs it, so a re-run after the item
      reached the branch keeps the branch's item and its date.

  check-item <appcast.xml> <version> <build> <enclosure URL>
      Exit 1 with an ::error:: unless the file holds exactly the handoff
      item `generate_appcast` should have written for this release: one
      item, `sparkle:version` <build>, `sparkle:shortVersionString`
      <version>, no channel, the phased rollout interval of D8 (86400 s),
      minimum system 15.0, `arm64` only, and the enclosure <enclosure URL>
      (the release's disk image) with a length and a `sparkle:edSignature`
      (`generate_appcast` skips signing without an error when the key does
      not match the app's).

  stamp-pubdate <appcast.xml> <unix time>
      Set the one item's `<pubDate>` to <unix time> in the form
      `generate_appcast` writes (`EEE, dd MMM yyyy HH:mm:ss ZZ`,
      en_US_POSIX, here in UTC: `Tue, 13 Oct 2026 09:00:00 +0000`), in
      place and leaving every other byte. Sparkle counts the phased
      rollout's groups from it, parsed with SUAppcastItem's
      `E, dd MMM yyyy HH:mm:ss Z` (en_US), and reads any other form as no
      date, which offers the item to every group at once. The `handoff` job
      runs it at the approval.

apps/desktop/scripts/handoff-appcast.test.sh checks it; rust-ci.yml runs
that. Standard library only.
"""

import datetime
import email.utils
import re
import sys
import xml.etree.ElementTree as ET

SPARKLE = "{http://www.andymatuschak.org/xml-namespaces/sparkle}"
ROLLOUT_INTERVAL = "86400"


def fail(message):
    print("::error::" + message, file=sys.stderr)
    sys.exit(1)


def items(path):
    try:
        root = ET.parse(path).getroot()
    except (OSError, ET.ParseError) as error:
        fail("%s is not an appcast: %s" % (path, error))
    channel = root.find("channel")
    if channel is None:
        fail("%s has no <channel>" % path)
    return channel.findall("item")


def text(item, name):
    element = item.find(SPARKLE + name)
    return (element.text or "").strip() if element is not None else None


def builds(item):
    found = [text(item, "version")]
    enclosure = item.find("enclosure")
    if enclosure is not None:
        found.append(enclosure.get(SPARKLE + "version"))
    return [value for value in found if value]


def number(value, what):
    if not re.fullmatch(r"[0-9]+", value or ""):
        fail("%s %r is not a whole number" % (what, value))
    return int(value)


def published(path):
    return [number(value, "sparkle:version") for item in items(path) for value in builds(item)]


def the_item(path):
    found = items(path)
    if len(found) != 1:
        fail("%s holds %d items, expected the one handoff item" % (path, len(found)))
    return found[0]


def check_build(path, build):
    build = number(build, "the build")
    highest = max(published(path), default=0)
    if build <= highest:
        fail(
            "build %d is not above build %d, the highest on the appcast branch; Sparkle would "
            "never offer it. Every tag needs its own version-bump commit on main" % (build, highest)
        )
    print("build %d is above build %d, the highest on the appcast branch" % (build, highest))


def has_handoff_item(path):
    print("true" if any(text(item, "channel") is None for item in items(path)) else "false")


def has_build(path, build):
    print("true" if number(build, "the build") in published(path) else "false")


def check_item(path, version, build, expected_url):
    item = the_item(path)
    expected = {
        "version": build,
        "shortVersionString": version,
        "phasedRolloutInterval": ROLLOUT_INTERVAL,
        "minimumSystemVersion": "15.0",
        "hardwareRequirements": "arm64",
    }
    for name, value in expected.items():
        if text(item, name) != value:
            fail("the handoff item has sparkle:%s %r, expected %r" % (name, text(item, name), value))
    channel = text(item, "channel")
    if channel is not None:
        fail("the handoff item has the channel %r; Swift builds on no channel would never see it" % channel)
    enclosure = item.find("enclosure")
    if enclosure is None:
        fail("the handoff item has no enclosure")
    url = enclosure.get("url", "")
    if url != expected_url:
        fail("the handoff item's enclosure is %r, expected %r" % (url, expected_url))
    if not re.fullmatch(r"[1-9][0-9]*", enclosure.get("length", "")):
        fail("the handoff item's enclosure has no length")
    if not enclosure.get(SPARKLE + "edSignature"):
        fail(
            "the handoff item carries no sparkle:edSignature: SPARKLE_PRIVATE_KEY is not the "
            "secret half of the bundle's SUPublicEDKey"
        )
    print("ok: the handoff item for %s (build %s) at %s" % (version, build, url))


PUB_DATE = re.compile(r"<pubDate>[^<]*</pubDate>")


def stamp_pubdate(path, when):
    when = datetime.datetime.fromtimestamp(number(when, "the time"), datetime.timezone.utc)
    stamp = email.utils.format_datetime(when)
    the_item(path)
    with open(path, encoding="utf-8") as handle:
        feed = handle.read()
    stamped, count = PUB_DATE.subn("<pubDate>%s</pubDate>" % stamp, feed)
    if count != 1:
        fail("%s holds %d <pubDate>, expected one" % (path, count))
    with open(path, "w", encoding="utf-8") as handle:
        handle.write(stamped)
    print("the handoff item's pubDate is %s" % stamp)


COMMANDS = {
    "check-build": (check_build, 2),
    "has-handoff-item": (has_handoff_item, 1),
    "has-build": (has_build, 2),
    "check-item": (check_item, 4),
    "stamp-pubdate": (stamp_pubdate, 2),
}


def main(argv):
    if not argv or argv[0] not in COMMANDS or len(argv) - 1 != COMMANDS[argv[0]][1]:
        fail("usage: handoff-appcast.py %s ... (see the script's docstring)" % "|".join(COMMANDS))
    command, _ = COMMANDS[argv[0]]
    command(*argv[1:])
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
