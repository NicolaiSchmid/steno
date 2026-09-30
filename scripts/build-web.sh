#!/usr/bin/env bash
# Build the web UI (apps/macos/web) and copy its `dist/` into the app bundle
# as `Web/`. Runs as an Xcode pre-build script phase of the Steno target
# (apps/macos/project.yml) and standalone from CI.
#
#   scripts/build-web.sh            build with pnpm, then copy
#   scripts/build-web.sh --copy-only  copy an existing dist/ (CI builds it first)
#
# Xcode script phases start with a minimal PATH, so the usual pnpm and Node
# locations are appended. When pnpm is missing and there is no dist/ the
# script fails with a clear message instead of a half-built app.
# Plan: .plans/2026-09-29-macos-webview-ui.md, Decision 8.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
web="$root/apps/macos/web"
dist="$web/dist"
mode="${1:-build}"

export PATH="$PATH:/opt/homebrew/bin:/usr/local/bin:$HOME/Library/pnpm:$HOME/.local/share/pnpm:$HOME/.nix-profile/bin:/run/current-system/sw/bin"

if [[ "$mode" != "--copy-only" ]]; then
  if command -v pnpm >/dev/null 2>&1; then
    pnpm_cmd=(pnpm)
  elif command -v corepack >/dev/null 2>&1; then
    pnpm_cmd=(corepack pnpm)
  else
    if [[ -f "$dist/index.html" ]]; then
      echo "build-web: pnpm not found; using the existing $dist" >&2
    else
      echo "build-web: pnpm is not on PATH and $dist does not exist." >&2
      echo "build-web: install Node 24 and pnpm 11 (corepack enable), or run 'pnpm --dir apps/macos/web build' first." >&2
      exit 1
    fi
  fi
  if [[ -n "${pnpm_cmd:-}" ]]; then
    (cd "$web" && "${pnpm_cmd[@]}" install --frozen-lockfile && "${pnpm_cmd[@]}" build)
  fi
fi

if [[ ! -f "$dist/index.html" ]]; then
  echo "build-web: $dist/index.html is missing after the build." >&2
  exit 1
fi

# The bundle must not reach the network: fail on any absolute http(s) URL
# other than XML namespaces inside SVG.
if grep -rEho 'https?://[^"'"'"' )<>]+' "$dist" | grep -vE '^https?://www\.w3\.org/' | head -1 | grep -q .; then
  echo "build-web: dist contains an absolute network URL:" >&2
  grep -rEho 'https?://[^"'"'"' )<>]+' "$dist" | grep -vE '^https?://www\.w3\.org/' | sort -u >&2
  exit 1
fi

# Inside Xcode, copy into the product's Resources; standalone, just report.
if [[ -n "${BUILT_PRODUCTS_DIR:-}" && -n "${UNLOCALIZED_RESOURCES_FOLDER_PATH:-}" ]]; then
  target="$BUILT_PRODUCTS_DIR/$UNLOCALIZED_RESOURCES_FOLDER_PATH/Web"
  rm -rf "$target"
  mkdir -p "$target"
  cp -R "$dist"/. "$target"/
  echo "build-web: copied $(find "$target" -type f | wc -l | tr -d ' ') files into $target"
else
  echo "build-web: built $dist ($(find "$dist" -type f | wc -l | tr -d ' ') files)"
fi
