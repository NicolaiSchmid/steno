#!/usr/bin/env bash
# Headless smoke of the Tauri shell on Linux: runs the built binary under
# xvfb-run with STENO_SMOKE_SECONDS, which makes the shell open all three
# windows side by side and both floating panels under Settings, and exit 0
# once the main window has sent page.ready, a snapshot reached it, the tray
# was built and the panels showed and hid (1 when any did not). When
# ImageMagick's `import` is available, the Xvfb root is captured near the
# end of the wait into apps/desktop/screens/ as review evidence, with one
# crop per window and per panel (`magick` from ImageMagick 7, `convert`
# from 6); the windows carry what the host's database holds (nothing on
# a fresh runner). Xvfb has no compositor, so the panels' transparent
# corners render black there.
#
#   [STENO_SMOKE_DPI=<dpi>] apps/desktop/scripts/smoke-linux.sh [path/to/steno-desktop] [seconds]
#
# Needs the web dist embedded (pnpm build in apps/macos/web before cargo
# build) and the runtime libraries the binary links; on NixOS run it inside
# the nix-shell apps/desktop/README.md names, with xvfb-run and imagemagick.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
binary="${1:-$root/target/debug/steno-desktop}"
seconds="${2:-${STENO_SMOKE_SECONDS:-12}}"
screens="$root/apps/desktop/screens"

[[ -x "$binary" ]] || { echo "smoke: $binary is not an executable" >&2; exit 2; }
[[ "$seconds" =~ ^[1-9][0-9]*$ ]] \
  || { echo "smoke: seconds must be a positive number, got \"$seconds\"" >&2; exit 2; }
command -v xvfb-run >/dev/null || { echo "smoke: xvfb-run is not on PATH" >&2; exit 2; }

mkdir -p "$screens"
export STENO_SMOKE_SECONDS="$seconds"
# Software rendering: Xvfb has no GPU and WebKitGTK's DMA-BUF path fails
# without one.
export WEBKIT_DISABLE_DMABUF_RENDERER=1
export WEBKIT_DISABLE_COMPOSITING_MODE=1
export LIBGL_ALWAYS_SOFTWARE=1
export GDK_BACKEND=x11

# 1120x720 main at the origin, Settings to its right, onboarding below.
# STENO_SMOKE_DPI sets the X resolution (Xvfb's own default otherwise);
# WebKitGTK's devicePixelRatio follows it, so 120 checks the panels at a
# ratio of 1.25.
server_args="-screen 0 2200x1500x24${STENO_SMOKE_DPI:+ -dpi $STENO_SMOKE_DPI}"
xvfb-run --auto-servernum --server-args="$server_args" bash -c '
  set -u
  "$1" & app=$!
  if command -v import >/dev/null; then
    sleep "$(( $2 > 3 ? $2 - 3 : 1 ))"
    import -window root "$3/smoke-root.png" && echo "smoke: captured $3/smoke-root.png"
    # One file per window, at the positions smoke.rs lays them out.
    # ImageMagick 7 is `magick`; 6 (the hosted Ubuntu image) is `convert`.
    crop=""
    if command -v magick >/dev/null; then crop=magick
    elif command -v convert >/dev/null; then crop=convert
    fi
    if [[ -n "$crop" ]]; then
      "$crop" "$3/smoke-root.png" -crop 1120x720+0+0 +repage "$3/main.png"
      "$crop" "$3/smoke-root.png" -crop 960x640+1160+0 +repage "$3/settings.png"
      "$crop" "$3/smoke-root.png" -crop 560x620+0+780 +repage "$3/onboarding.png"
      # The panels, where smoke.rs puts them (PANELS_X, PANEL_*_Y), with a
      # margin around each so the crop survives the page resizing the
      # window about its top centre, at 120 dpi too.
      "$crop" "$3/smoke-root.png" -crop 720x100+1040+680 +repage "$3/prompt.png"
      "$crop" "$3/smoke-root.png" -crop 720x100+1040+780 +repage "$3/bubble.png"
      echo "smoke: cropped main, settings, onboarding, prompt and bubble with $crop"
    fi
  fi
  wait "$app"
' _ "$binary" "$seconds" "$screens"
