#!/usr/bin/env bash
# Builds `steno-speech-sidecar` in release and stages it as the `externalBin`
# that `tauri.release.conf.json` declares:
# `apps/desktop/src-tauri/binaries/steno-speech-sidecar-<host triple>[.exe]`
# (ignored by git). The bundler strips the triple and installs it beside the
# app's binary. Run it before `tauri build --config tauri.release.conf.json`.
# Prints the staged paths.
#
# On Windows both binaries import `DirectML.dll`, which the NSIS installer
# would not pick up on its own (leaving the older copy in System32 to load),
# so it is staged too and `tauri.release.windows.conf.json` installs it.
#
#   apps/desktop/scripts/stage-sidecar.sh
#
# Honours CARGO_TARGET_DIR and RUSTFLAGS.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
triple="$(rustc -vV | sed -n 's/^host: //p')"
test -n "$triple" || { echo "::error::rustc -vV printed no host triple" >&2; exit 1; }
exe=""
case "$triple" in *-windows-*) exe=".exe" ;; esac

cargo build --release --locked -p steno-speech-sidecar --manifest-path "$root/Cargo.toml"

built="${CARGO_TARGET_DIR:-$root/target}/release/steno-speech-sidecar$exe"
staged="$root/apps/desktop/src-tauri/binaries/steno-speech-sidecar-$triple$exe"
test -f "$built" || { echo "::error::$built is missing after the build" >&2; exit 1; }
mkdir -p "$(dirname "$staged")"
cp "$built" "$staged"
echo "$staged"

if [[ -n "$exe" ]]; then
  # Beside the binary, or wherever ort's build left it when a restored
  # cache kept its build script from running again.
  dll="$(find "$(dirname "$built")" -name DirectML.dll -print -quit)"
  test -n "$dll" || { echo "::error::no DirectML.dll under $(dirname "$built"); if ONNX Runtime no longer loads it, drop it here and in tauri.release.windows.conf.json" >&2; exit 1; }
  staged_dll="$(dirname "$staged")/DirectML.dll"
  cp "$dll" "$staged_dll"
  echo "$staged_dll"
fi
