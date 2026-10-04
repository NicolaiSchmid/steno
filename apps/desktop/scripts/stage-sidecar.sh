#!/usr/bin/env bash
# Builds `steno-speech-sidecar` in release and puts it where the Tauri
# bundler looks for the `externalBin` that `tauri.release.conf.json`
# declares: `apps/desktop/src-tauri/binaries/steno-speech-sidecar-<target
# triple>` (plus `.exe` on Windows). The bundler strips the triple and
# installs the binary beside the app's own, where
# `SidecarConfig::beside_current_exe` (crates/steno-speech) looks for it.
# The triple is the host's, the one a `tauri build` without `--target`
# builds for. Run it before `tauri build --config tauri.release.conf.json`;
# the directory is ignored by git. Prints the staged paths.
#
# ONNX Runtime is linked statically, but its Windows build imports
# `DirectML.dll` (the app's binary and the sidecar both do), which `ort`
# copies beside the binaries it builds. The MSI picks up every DLL there on
# its own and the NSIS installer does not, which would leave the older
# copy in System32 to load, so on Windows the DLL is staged as well and
# `tauri.release.windows.conf.json` installs it beside the app for both.
#
#   apps/desktop/scripts/stage-sidecar.sh
#
# Honours CARGO_TARGET_DIR (the self-hosted runners keep the target
# directory outside the workspace) and RUSTFLAGS, which the release job
# sets for both builds so they share one dependency cache.
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
  cp "$dll" "$(dirname "$staged")/DirectML.dll"
  echo "$(dirname "$staged")/DirectML.dll"
fi
