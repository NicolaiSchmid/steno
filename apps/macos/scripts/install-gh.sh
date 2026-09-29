#!/usr/bin/env bash
# Installs the GitHub CLI for the release workflow from the cli/cli release
# zip, pinned by version and SHA-256 per architecture, cached under
# $RUNNER_TEMP so a self-hosted runner (Forge, no brew formula installed)
# downloads it once. GitHub-hosted macOS images ship `gh`, and any `gh` on
# PATH is used as is: the workflow only needs `gh release`, which has been
# stable across versions. Puts the bin directory on GITHUB_PATH under
# Actions, otherwise prints the path.
set -euo pipefail

VERSION="2.101.0"
# From gh_${VERSION}_checksums.txt on the release
# (https://github.com/cli/cli/releases/tag/v${VERSION}).
SHA256_ARM64="e4303e39d8f07141c4bad4b99b01079f05029c59b27076e8fbc825c985ecdd8b"
SHA256_AMD64="a6fd66c88e2f07d6e4e058173db341d07dd74d58cf8f19ae668293d2bb614ca3"

if command -v gh >/dev/null 2>&1; then
  echo "gh already on PATH: $(gh --version | head -n 1)"
  exit 0
fi

case "$(uname -m)" in
  arm64 | aarch64) arch="arm64"; sha256="$SHA256_ARM64" ;;
  x86_64) arch="amd64"; sha256="$SHA256_AMD64" ;;
  *) echo "::error::no pinned gh build for $(uname -m)" >&2; exit 1 ;;
esac

root="${RUNNER_TEMP:-${TMPDIR:-/tmp}}/gh-${VERSION}"
zip="$root/gh_${VERSION}_macOS_${arch}.zip"
mkdir -p "$root"
if [ ! -f "$zip" ]; then
  url="https://github.com/cli/cli/releases/download/v${VERSION}/gh_${VERSION}_macOS_${arch}.zip"
  echo "downloading $url"
  curl -fsSL --retry 3 -o "$zip" "$url"
fi

# Verified on every run, cached or fresh, before anything from the zip runs.
if ! echo "$sha256  $zip" | shasum -a 256 -c - >/dev/null 2>&1; then
  echo "::error::gh zip checksum mismatch (want $sha256, got $(shasum -a 256 "$zip" | cut -d' ' -f1)); refusing to unpack" >&2
  rm -f "$zip"
  exit 1
fi

binary="$(find "$root" -type f -name gh -path '*/bin/*' | head -n 1 || true)"
if [ -z "$binary" ]; then
  unzip -oq "$zip" -d "$root"
  binary="$(find "$root" -type f -name gh -path '*/bin/*' | head -n 1 || true)"
fi
if [ -z "$binary" ]; then
  echo "::error::gh binary not found in the release zip" >&2
  exit 1
fi

chmod +x "$binary"
bin_dir="$(dirname "$binary")"
if [ -n "${GITHUB_PATH:-}" ]; then
  echo "$bin_dir" >> "$GITHUB_PATH"
fi
echo "$("$binary" --version | head -n 1) at $bin_dir"
