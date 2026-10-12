#!/usr/bin/env bash
# Checks apps/desktop/scripts/handoff-item.sh with a stub generate_appcast
# that writes the item Sparkle's does, and signs it only for the right key,
# and an `appcast` branch on a local bare origin: the flags and the key on
# stdin, the item's own check, the dry-run merge against the branch, and
# the pinned tarball checksum. rust-ci.yml runs it on Linux.
set -euo pipefail

script="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/handoff-item.sh"
scratch="$(mktemp -d)"
trap 'rm -rf "$scratch"' EXIT
failures=0

fail() {
  echo "FAIL: $1"
  failures=$((failures + 1))
}

git_quiet() { git -c init.defaultBranch=main -c user.name=test -c user.email=test@example.com "$@"; }

# An origin with the appcast branch as the release workflow left it: two
# Swift candidates on the beta channel, the highest build 542.
appcast_item() {
  cat <<XML
        <item>
            <title>$2</title>
            <sparkle:version>$1</sparkle:version>
            <sparkle:shortVersionString>$2</sparkle:shortVersionString>
            <enclosure url="https://github.com/NicolaiSchmid/steno/releases/download/v$2/Steno-$2.dmg" length="1" type="application/octet-stream" sparkle:edSignature="c2ln" />
            <sparkle:channel>beta</sparkle:channel>
        </item>
XML
}
git_quiet init --quiet --bare "$scratch/origin.git"
git_quiet init --quiet "$scratch/seed"
cat > "$scratch/seed/appcast.xml" <<XML
<?xml version='1.0' encoding='utf-8'?>
<rss xmlns:sparkle="http://www.andymatuschak.org/xml-namespaces/sparkle" version="2.0">
    <channel>
        <title>Steno</title>
$(appcast_item 542 0.10.0-rc.2)
$(appcast_item 530 0.10.0-rc.1)
    </channel>
</rss>
XML
git_quiet -C "$scratch/seed" add appcast.xml
git_quiet -C "$scratch/seed" commit --quiet -m appcast
git_quiet -C "$scratch/seed" push --quiet "$scratch/origin.git" HEAD:refs/heads/appcast
git_quiet clone --quiet "$scratch/origin.git" "$scratch/checkout" 2>/dev/null

# The stub: records its arguments and stdin, checks that the folder holds
# only the image, and writes the item for STUB_VERSION and STUB_BUILD;
# signed only when the key on stdin is `right-key`.
cat > "$scratch/generate_appcast" <<'STUB'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$@" > "$STUB_LOG.args"
key="$(cat)"
printf '%s' "$key" > "$STUB_LOG.key"
prefix="" out="" interval=""
while [[ $# -gt 1 ]]; do
  case "$1" in
    --download-url-prefix) prefix="$2"; shift 2 ;;
    --phased-rollout-interval) interval="$2"; shift 2 ;;
    -o) out="$2"; shift 2 ;;
    --ed-key-file | --link | --maximum-deltas) shift 2 ;;
    *) echo "generate_appcast stub: unknown $1" >&2; exit 2 ;;
  esac
done
archives=("$1"/*)
[[ ${#archives[@]} -eq 1 && "${archives[0]}" == *.dmg ]] || { echo "the folder holds ${archives[*]}" >&2; exit 3; }
signature=""
[[ "$key" != right-key ]] || signature=' sparkle:edSignature="c2ln"'
cat > "$out" <<XML
<?xml version="1.0" standalone="yes"?>
<rss xmlns:sparkle="http://www.andymatuschak.org/xml-namespaces/sparkle" version="2.0">
    <channel>
        <title>Steno</title>
        <item>
            <title>$STUB_VERSION</title>
            <pubDate>Tue, 13 Oct 2026 10:12:00 +0200</pubDate>
            <link>https://github.com/NicolaiSchmid/steno/releases</link>
            <sparkle:version>$STUB_BUILD</sparkle:version>
            <sparkle:shortVersionString>$STUB_VERSION</sparkle:shortVersionString>
            <sparkle:minimumSystemVersion>15.0</sparkle:minimumSystemVersion>
            <sparkle:hardwareRequirements>arm64</sparkle:hardwareRequirements>
            <sparkle:phasedRolloutInterval>$interval</sparkle:phasedRolloutInterval>
            <enclosure url="$prefix${archives[0]##*/}" length="21000000" type="application/octet-stream"$signature/>
        </item>
    </channel>
</rss>
XML
STUB
chmod +x "$scratch/generate_appcast"

mkdir -p "$scratch/dist"
printf 'image\n' > "$scratch/dist/Steno_0.11.0_aarch64.dmg"
printf 'not the image\n' > "$scratch/dist/other.txt"

# run <name> <version> <build> [<key>]: the script against the stub and
# the origin; prints stdout and stderr together. The stub writes <version>,
# or STUB_WRITES when set.
run() {
  local name="$1"
  STUB_LOG="$scratch/$name" STUB_VERSION="${STUB_WRITES:-$2}" STUB_BUILD="$3" \
    SPARKLE_PRIVATE_KEY="${4-right-key}" STENO_GENERATE_APPCAST="$scratch/generate_appcast" \
    STENO_REPO_ROOT="$scratch/checkout" GITHUB_REPOSITORY=NicolaiSchmid/steno \
    "$script" "$2" "$3" "$scratch/dist/Steno_0.11.0_aarch64.dmg" "$scratch/$name-out" 2>&1
}
refused() {
  local name="$1" expected="$2" output
  shift 2
  if output="$(run "$name" "$@")"; then
    fail "$name passed: $output"
  elif [[ "$output" != *"$expected"* ]]; then
    fail "$name failed without \"$expected\": $output"
  fi
}

if output="$(run good 0.11.0 3400)"; then
  [[ -s "$scratch/good-out/appcast.xml" ]] || fail "no appcast.xml in the out directory"
  [[ "$output" == *"ok: the handoff item for 0.11.0 (build 3400)"* ]] || fail "the item check did not run: $output"
  [[ "$output" == *"build 3400 is above build 542"* ]] || fail "the build check did not run: $output"
  [[ "$output" == *"item(s), newest build 3400"* ]] || fail "the dry-run merge did not run: $output"
  args="$(cat "$scratch/good.args")"
  for flag in $'--ed-key-file\n-' \
    $'--download-url-prefix\nhttps://github.com/NicolaiSchmid/steno/releases/download/v0.11.0/' \
    $'--maximum-deltas\n0' $'--phased-rollout-interval\n86400'; do
    [[ "$args" == *"$flag"* ]] || fail "generate_appcast did not get ${flag//$'\n'/ }: $args"
  done
  [[ "$(cat "$scratch/good.key")" == right-key ]] || fail "the key did not reach generate_appcast's stdin"
  [[ "$args" != *right-key* ]] || fail "the key is on generate_appcast's command line"
  [[ "$(git -C "$scratch/origin.git" rev-list --count refs/heads/appcast)" == 1 ]] || fail "the dry run pushed to the branch"
else
  fail "the good run failed: $output"
fi

refused wrong-key 'carries no sparkle:edSignature' 0.11.0 3400 other-key
refused no-key 'SPARKLE_PRIVATE_KEY missing' 0.11.0 3400 ''
refused low-build 'build 542 is not above build 542' 0.11.0 542
STUB_WRITES=0.11.0-rc.9 refused other-version "sparkle:shortVersionString '0.11.0-rc.9', expected '0.11.0'" 0.11.0 3400

# The tarball's checksum is pinned: another file is refused before
# anything runs.
printf 'not Sparkle\n' > "$scratch/fake.tar.xz"
if output="$(STENO_SPARKLE_URL="file://$scratch/fake.tar.xz" SPARKLE_PRIVATE_KEY=right-key STENO_REPO_ROOT="$scratch/checkout" \
  "$script" 0.11.0 3400 "$scratch/dist/Steno_0.11.0_aarch64.dmg" "$scratch/fake-out" 2>&1)"; then
  fail "a tarball with another checksum passed: $output"
elif [[ "$output" != *"::error::Sparkle-2.10.0.tar.xz has the SHA-256"*"expected c2bf58aa8387266ac179357b1415d6f2635f044da8be41042af32425dae6da0c"* ]]; then
  fail "a tarball with another checksum failed without saying so: $output"
fi

if ((failures > 0)); then
  echo "handoff-item: $failures failed"
  exit 1
fi
echo "handoff-item: ok"
