#!/usr/bin/env bash
# Checks the token check of apps/macos/scripts/bump-homebrew-cask.sh, which
# desktop-release.yml's `publish` runs for a stable release: without
# HOMEBREW_TAP_TOKEN it prints a notice, touches no tap and exits 0, so the
# optional step never fails a release that stands; with the token it clones
# the tap over HTTPS with it. A stub `git` stands in for the tap and a stub
# `shasum` for the hash. ReleaseScriptsTests runs the whole bump against a
# local tap on macOS. rust-ci.yml runs this on Linux.
set -euo pipefail

script="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../macos/scripts" && pwd)/bump-homebrew-cask.sh"
scratch="$(mktemp -d)"
trap 'rm -rf "$scratch"' EXIT
failures=0

fail() {
  echo "FAIL: $1"
  failures=$((failures + 1))
}

mkdir -p "$scratch/bin"
cat > "$scratch/bin/git" <<'SH'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$STUB_LOG"
exit 1
SH
cat > "$scratch/bin/shasum" <<'SH'
#!/usr/bin/env bash
printf '%064d  %s\n' 0 "$3"
SH
chmod +x "$scratch/bin/git" "$scratch/bin/shasum"
: > "$scratch/Steno_0.11.0_aarch64.dmg"

# bump <token>: runs the script; its status in $status, its output in
# $output, the git calls in git.log.
bump() {
  : > "$scratch/git.log"
  status=0
  env -u HOMEBREW_TAP_URL PATH="$scratch/bin:$PATH" STUB_LOG="$scratch/git.log" HOMEBREW_TAP_TOKEN="$1" \
    bash "$script" 0.11.0 "$scratch/Steno_0.11.0_aarch64.dmg" > "$scratch/output" 2>&1 || status=$?
  output="$(cat "$scratch/output")"
}

bump ''
[[ "$status" == 0 ]] || fail "without a token the script exits $status: $output"
[[ "$output" == '::notice::HOMEBREW_TAP_TOKEN is not set; '* ]] || fail "without a token the script says: $output"
[[ ! -s "$scratch/git.log" ]] || fail "without a token the script ran git: $(cat "$scratch/git.log")"

bump token
[[ "$status" != 0 ]] || fail "a clone that failed was not an error: $output"
[[ "$(cat "$scratch/git.log")" == 'clone --quiet --depth 1 --branch main https://x-access-token:token@github.com/NicolaiSchmid/homebrew-tap.git '*/tap ]] \
  || fail "with a token the script clones: $(cat "$scratch/git.log")"

if ((failures > 0)); then
  echo "bump-homebrew-cask: $failures failed"
  exit 1
fi
echo "bump-homebrew-cask: ok"
