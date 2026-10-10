#!/usr/bin/env bash
# Checks apps/desktop/scripts/handoff-appcast.py over appcasts in the form
# the `appcast` branch and `generate_appcast` write: the build number
# check, whether the branch has the handoff item or a build, the handoff
# item's own
# check (each defect on its own), and the pubDate stamp, which the test
# parses back with Sparkle's date format. rust-ci.yml runs it on Linux.
set -euo pipefail

script="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/handoff-appcast.py"
scratch="$(mktemp -d)"
trap 'rm -rf "$scratch"' EXIT
failures=0

fail() {
  echo "FAIL: $1"
  failures=$((failures + 1))
}

url='https://github.com/NicolaiSchmid/steno/releases/download/v0.11.0/Steno_0.11.0_aarch64.dmg'

# item <build> <short version> [<extra elements>]: one item as the branch
# holds a Swift release candidate's.
item() {
  cat <<XML
        <item>
            <title>$2</title>
            <pubDate>Fri, 02 Oct 2026 10:57:59 +0200</pubDate>
            <link>https://github.com/NicolaiSchmid/steno/releases</link>
            <sparkle:version>$1</sparkle:version>
            <sparkle:shortVersionString>$2</sparkle:shortVersionString>
            <sparkle:minimumSystemVersion>15.0</sparkle:minimumSystemVersion>
            <sparkle:hardwareRequirements>arm64</sparkle:hardwareRequirements>
            <enclosure url="https://github.com/NicolaiSchmid/steno/releases/download/v$2/Steno-$2.dmg" length="18928751" type="application/octet-stream" sparkle:edSignature="c2ln" />
            ${3:-}
        </item>
XML
}

# feed <file> <items...>: an appcast holding the items.
feed() {
  local file="$scratch/$1"
  shift
  {
    echo "<?xml version='1.0' encoding='utf-8'?>"
    echo '<rss xmlns:sparkle="http://www.andymatuschak.org/xml-namespaces/sparkle" version="2.0">'
    echo '    <channel>'
    echo '        <title>Steno</title>'
    printf '%s\n' "$@"
    echo '    </channel>'
    echo '</rss>'
  } > "$file"
  echo "$file"
}

# passes <why> <args...> and refused <why> <expected text> <args...>.
passes() {
  local why="$1" output
  shift
  output="$(python3 "$script" "$@" 2>&1)" || fail "$why was refused: $output"
}
refused() {
  local why="$1" expected="$2" output
  shift 2
  if output="$(python3 "$script" "$@" 2>&1)"; then
    fail "$why passed: $output"
  elif [[ "$output" != *"::error::"*"$expected"* ]]; then
    fail "$why failed without \"$expected\": $output"
  fi
}

beta='<sparkle:channel>beta</sparkle:channel>'
branch="$(feed branch.xml "$(item 542 0.10.0-rc.2 "$beta")" "$(item 530 0.10.0-rc.1 "$beta")" "$(item 244 0.9.0-rc.1 "$beta")")"

# check-build
passes 'a build above the branch' check-build "$branch" 543
refused 'the highest build again' 'build 542 is not above build 542' check-build "$branch" 542
refused 'a lower build' 'build 100 is not above build 542' check-build "$branch" 100
refused 'a build that is no number' "the build '54x' is not a whole number" check-build "$branch" 54x
attribute="$(feed attribute.xml "$(item 542 0.10.0-rc.2 "$beta")" \
  '<item><title>old</title><enclosure url="x" sparkle:version="900" length="1" /></item>')"
refused 'a build below an enclosure attribute' 'build 600 is not above build 900' check-build "$attribute" 600
passes 'a branch without items' check-build "$(feed empty.xml)" 1
refused 'a feed that is no appcast' 'is not an appcast' check-build "$(printf 'no xml' > "$scratch/garbage.xml"; echo "$scratch/garbage.xml")" 1

# has-handoff-item
[[ "$(python3 "$script" has-handoff-item "$branch")" == false ]] || fail "a branch of beta items has a handoff item"
with_item="$(feed with-item.xml "$(item 3400 0.11.0)" "$(item 542 0.10.0-rc.2 "$beta")")"
[[ "$(python3 "$script" has-handoff-item "$with_item")" == true ]] || fail "the branch's handoff item is not found"

# has-build
[[ "$(python3 "$script" has-build "$with_item" 3400)" == true ]] || fail "build 3400 is not found on the branch"
[[ "$(python3 "$script" has-build "$branch" 3400)" == false ]] || fail "build 3400 is found on a branch without it"
[[ "$(python3 "$script" has-build "$attribute" 900)" == true ]] || fail "an enclosure attribute's build is not found"

# check-item: the item generate_appcast writes for v0.11.0, then each defect.
rollout='<sparkle:phasedRolloutInterval>86400</sparkle:phasedRolloutInterval>'
handoff_item() {
  local build="${1:-3400}" short="${2:-0.11.0}" extra="${3-$rollout}" enclosure="${4:-<enclosure url=\"$url\" length=\"21000000\" type=\"application/octet-stream\" sparkle:edSignature=\"c2ln\" />}"
  cat <<XML
        <item>
            <title>$short</title>
            <pubDate>Tue, 13 Oct 2026 10:12:00 +0200</pubDate>
            <sparkle:version>$build</sparkle:version>
            <sparkle:shortVersionString>$short</sparkle:shortVersionString>
            <sparkle:minimumSystemVersion>15.0</sparkle:minimumSystemVersion>
            <sparkle:hardwareRequirements>arm64</sparkle:hardwareRequirements>
            $enclosure
            $extra
        </item>
XML
}
good="$(feed good.xml "$(handoff_item)")"
passes 'the handoff item' check-item "$good" 0.11.0 3400 "$url"
refused 'two items' 'holds 2 items' check-item "$(feed two.xml "$(handoff_item)" "$(handoff_item 3399)")" 0.11.0 3400 "$url"
refused 'another build' "sparkle:version '3399', expected '3400'" check-item "$(feed build.xml "$(handoff_item 3399)")" 0.11.0 3400 "$url"
refused 'another version' "sparkle:shortVersionString '0.11.0-rc.4', expected '0.11.0'" \
  check-item "$(feed version.xml "$(handoff_item 3400 0.11.0-rc.4)")" 0.11.0 3400 "$url"
refused 'no rollout' "sparkle:phasedRolloutInterval None, expected '86400'" \
  check-item "$(feed rollout.xml "$(handoff_item 3400 0.11.0 '')")" 0.11.0 3400 "$url"
refused 'a channel' "the channel 'beta'" \
  check-item "$(feed channel.xml "$(handoff_item 3400 0.11.0 "$rollout$beta")")" 0.11.0 3400 "$url"
sed 's|<sparkle:minimumSystemVersion>15.0|<sparkle:minimumSystemVersion>14.0|' "$good" > "$scratch/system.xml"
refused 'another minimum system' "sparkle:minimumSystemVersion '14.0', expected '15.0'" check-item "$scratch/system.xml" 0.11.0 3400 "$url"
grep -v hardwareRequirements "$good" > "$scratch/intel.xml"
refused 'no arm64 requirement' "sparkle:hardwareRequirements None, expected 'arm64'" check-item "$scratch/intel.xml" 0.11.0 3400 "$url"
refused 'another enclosure' "enclosure is 'https://example.com/Steno.dmg'" \
  check-item "$(feed url.xml "$(handoff_item 3400 0.11.0 "$rollout" '<enclosure url="https://example.com/Steno.dmg" length="1" sparkle:edSignature="c2ln" />')")" 0.11.0 3400 "$url"
refused 'no length' 'has no length' \
  check-item "$(feed length.xml "$(handoff_item 3400 0.11.0 "$rollout" "<enclosure url=\"$url\" sparkle:edSignature=\"c2ln\" />")")" 0.11.0 3400 "$url"
refused 'no signature' 'carries no sparkle:edSignature' \
  check-item "$(feed unsigned.xml "$(handoff_item 3400 0.11.0 "$rollout" "<enclosure url=\"$url\" length=\"1\" />")")" 0.11.0 3400 "$url"

# stamp-pubdate: 2026-10-13 09:00:00 UTC, the plan's example.
cp "$good" "$scratch/stamped.xml"
passes 'the stamp' stamp-pubdate "$scratch/stamped.xml" 1791882000
stamp="$(sed -n 's|.*<pubDate>\(.*\)</pubDate>.*|\1|p' "$scratch/stamped.xml")"
[[ "$stamp" == 'Tue, 13 Oct 2026 09:00:00 +0000' ]] || fail "the stamp is '$stamp'"
# Parsed back with Sparkle's format (SUAppcastItem's `EEE, dd MMM yyyy
# HH:mm:ss ZZ`, en_US_POSIX), it is the approval's time.
parsed="$(python3 -c '
import datetime, sys
when = datetime.datetime.strptime(sys.argv[1], "%a, %d %b %Y %H:%M:%S %z")
print(int(when.timestamp()))
' "$stamp")" || fail "Sparkle's format does not parse '$stamp'"
[[ "$parsed" == 1791882000 ]] || fail "'$stamp' parses as $parsed"
# Nothing else in the file changed.
changed="$(diff "$good" "$scratch/stamped.xml" | grep -c '^[<>]' || true)"
[[ "$changed" == 2 ]] || fail "the stamp changed $changed lines: $(diff "$good" "$scratch/stamped.xml" || true)"
python3 "$script" check-item "$scratch/stamped.xml" 0.11.0 3400 "$url" >/dev/null || fail "the stamped item no longer checks"
refused 'a stamp over two items' 'holds 2 items' stamp-pubdate "$scratch/two.xml" 1791882000
refused 'a time that is no number' "the time 'now' is not a whole number" stamp-pubdate "$scratch/stamped.xml" now

refused 'an unknown command' 'usage: handoff-appcast.py' sign "$good"

if ((failures > 0)); then
  echo "handoff-appcast: $failures failed"
  exit 1
fi
echo "handoff-appcast: ok"
