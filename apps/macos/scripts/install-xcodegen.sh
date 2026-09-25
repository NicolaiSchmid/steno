#!/usr/bin/env bash
# Installs xcodegen for CI from the GitHub release zip, cached under
# $RUNNER_TEMP so a self-hosted runner (Forge, no brew formula installed)
# downloads it once. Puts its bin directory on GITHUB_PATH when running under
# Actions, otherwise prints the path.
set -euo pipefail

VERSION="${XCODEGEN_VERSION:-2.46.0}"

if command -v xcodegen >/dev/null 2>&1; then
  echo "xcodegen $(xcodegen --version) already on PATH"
  exit 0
fi

root="${RUNNER_TEMP:-${TMPDIR:-/tmp}}/xcodegen-${VERSION}"
binary="$(find "$root" -type f -name xcodegen -path '*/bin/*' 2>/dev/null | head -n 1 || true)"

if [ -z "$binary" ]; then
  mkdir -p "$root"
  url="https://github.com/yonaskolb/XcodeGen/releases/download/${VERSION}/xcodegen.zip"
  echo "downloading $url"
  curl -fsSL --retry 3 -o "$root/xcodegen.zip" "$url"
  unzip -oq "$root/xcodegen.zip" -d "$root"
  binary="$(find "$root" -type f -name xcodegen -path '*/bin/*' | head -n 1)"
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
