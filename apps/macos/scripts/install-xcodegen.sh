#!/usr/bin/env bash
# Installs xcodegen for CI from the GitHub release zip, pinned by version and
# SHA-256, cached under $RUNNER_TEMP so a self-hosted runner (Forge, no brew
# formula installed) downloads it once. An `xcodegen` already on PATH is used
# only when it is the pinned version, so Forge cannot drift from what the
# hosted runner runs. Puts the bin directory on GITHUB_PATH under Actions,
# otherwise prints the path.
set -euo pipefail

VERSION="2.46.0"
# `sha256sum xcodegen.zip` of the release asset; GitHub reports the same
# digest on the release (`gh api repos/yonaskolb/XcodeGen/releases/tags/$VERSION`).
SHA256="4d9e34b62172d645eed6457cac13fc222569974098ef4ee9c3368bedf0196806"

if command -v xcodegen >/dev/null 2>&1; then
  found="$(xcodegen --version 2>/dev/null | sed -n 's/^Version: //p')"
  if [ "$found" = "$VERSION" ]; then
    echo "xcodegen $VERSION already on PATH"
    exit 0
  fi
  echo "xcodegen on PATH is '${found:-unknown}', want $VERSION; installing the pinned release"
fi

root="${RUNNER_TEMP:-${TMPDIR:-/tmp}}/xcodegen-${VERSION}"
zip="$root/xcodegen.zip"
mkdir -p "$root"
if [ ! -f "$zip" ]; then
  url="https://github.com/yonaskolb/XcodeGen/releases/download/${VERSION}/xcodegen.zip"
  echo "downloading $url"
  curl -fsSL --retry 3 -o "$zip" "$url"
fi

# Verified on every run, cached or fresh, before anything from the zip runs.
if ! echo "$SHA256  $zip" | shasum -a 256 -c - >/dev/null 2>&1; then
  echo "::error::xcodegen.zip checksum mismatch (want $SHA256, got $(shasum -a 256 "$zip" | cut -d' ' -f1)); refusing to unpack" >&2
  rm -f "$zip"
  exit 1
fi

binary="$(find "$root" -type f -name xcodegen -path '*/bin/*' | head -n 1 || true)"
if [ -z "$binary" ]; then
  unzip -oq "$zip" -d "$root"
  binary="$(find "$root" -type f -name xcodegen -path '*/bin/*' | head -n 1 || true)"
fi
if [ -z "$binary" ]; then
  echo "::error::xcodegen binary not found in the release zip" >&2
  exit 1
fi

chmod +x "$binary"
bin_dir="$(dirname "$binary")"
if [ -n "${GITHUB_PATH:-}" ]; then
  echo "$bin_dir" >> "$GITHUB_PATH"
fi
echo "xcodegen $("$binary" --version) at $bin_dir"
