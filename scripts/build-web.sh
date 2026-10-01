#!/usr/bin/env bash
# Build the web UI (apps/macos/web) and copy its `dist/` into the app bundle
# as `Web/`. Runs as an Xcode pre-build script phase of the Steno target
# (apps/macos/project.yml) and standalone from CI.
#
#   scripts/build-web.sh              build with pnpm, then gate and copy
#   scripts/build-web.sh --copy-only  gate and copy an existing dist/
#
# CI builds the web UI in its own step and sets STENO_WEB_PREBUILT=1, which
# turns the Xcode phase into --copy-only so xcodebuild never reaches the npm
# registry. Xcode script phases start with a minimal PATH, so the usual pnpm
# and Node locations are appended. Failures are prefixed with `error:` so
# Xcode and apps/macos/scripts/xcodebuild-quiet.sh surface the line.
# Plan: .plans/2026-09-29-macos-webview-ui.md, Decision 8.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
web="$root/apps/macos/web"
dist="$web/dist"
mode="${1:-build}"

if [[ "${STENO_WEB_PREBUILT:-}" == "1" && -f "$dist/index.html" ]]; then
  mode="--copy-only"
fi

export PATH="$PATH:/opt/homebrew/bin:/usr/local/bin:$HOME/Library/pnpm:$HOME/.local/share/pnpm:$HOME/.nix-profile/bin:/run/current-system/sw/bin"

fail() {
  echo "error: build-web: $*" >&2
  exit 1
}

if [[ "$mode" != "--copy-only" ]]; then
  if command -v pnpm >/dev/null 2>&1; then
    pnpm_cmd=(pnpm)
  elif command -v corepack >/dev/null 2>&1; then
    pnpm_cmd=(corepack pnpm)
  elif [[ -f "$dist/index.html" ]]; then
    echo "warning: build-web: pnpm not found; using the existing $dist" >&2
  else
    fail "pnpm is not on PATH and $dist does not exist. Install Node 24 and pnpm 11 (corepack enable), or run 'pnpm --dir apps/macos/web build' first."
  fi
  if [[ -n "${pnpm_cmd:-}" ]]; then
    (cd "$web" && "${pnpm_cmd[@]}" install --frozen-lockfile && "${pnpm_cmd[@]}" build) \
      || fail "pnpm build failed in $web"
  fi
fi

[[ -f "$dist/index.html" ]] || fail "$dist/index.html is missing after the build."

# The bundle must not reach the network (connect-src 'none') and must not
# carry the mock bridge or the recorded fixtures. `pnpm build` already runs
# both checks; a copied dist is checked here so a stale or foreign dist (a
# screens build, say) never reaches the bundle unchecked.
if [[ "$mode" == "--copy-only" ]]; then
  command -v node >/dev/null 2>&1 || fail "node is not on PATH; the bundle gate needs it."
  (cd "$web" && node scripts/check-offline.mjs) || fail "dist contains a fetchable URL (see above)."
  (cd "$web" && node scripts/check-bundle.mjs) || fail "dist contains the mock bridge or a fixture (see above)."
fi

# Inside Xcode, copy into the product's Resources; standalone, just report.
if [[ -n "${BUILT_PRODUCTS_DIR:-}" && -n "${UNLOCALIZED_RESOURCES_FOLDER_PATH:-}" ]]; then
  target="$BUILT_PRODUCTS_DIR/$UNLOCALIZED_RESOURCES_FOLDER_PATH/Web"
  rm -rf "$target"
  mkdir -p "$target"
  cp -R "$dist"/. "$target"/
  echo "build-web: copied $(find "$target" -type f | wc -l | tr -d ' ') files into $target"
else
  echo "build-web: $dist is ready ($(find "$dist" -type f | wc -l | tr -d ' ') files)"
fi
