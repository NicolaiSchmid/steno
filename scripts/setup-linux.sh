#!/usr/bin/env bash
# The Debian and Ubuntu packages the Tauri shell (apps/desktop) needs to
# build and to run headless in CI: WebKitGTK 4.1 and its GTK stack, the
# tray indicator library, librsvg for icons, OpenSSL and pkg-config, plus
# Xvfb and ImageMagick for apps/desktop/scripts/smoke-linux.sh. The audio
# crate's PipeWire backend needs the libpipewire headers and libclang (its
# bindings are generated at build time), and its live tests the daemon,
# its command-line tools and WirePlumber (scripts/pipewire-headless.sh).
# Used by rust-ci.yml on ubuntu-latest; run it once on a fresh Ubuntu
# machine. On NixOS, use the nix-shell apps/desktop/README.md names instead,
# plus pipewire, wireplumber and dbus for the audio crate, with
# LIBCLANG_PATH pointing at llvmPackages.libclang's lib directory.
# Plan: .plans/2026-10-02-rust-core-and-tauri-shell.md ("Repository setup").
set -euo pipefail

packages=(
  build-essential
  curl
  file
  libayatana-appindicator3-dev
  libgtk-3-dev
  librsvg2-dev
  libssl-dev
  libwebkit2gtk-4.1-dev
  libxdo-dev
  pkg-config
  wget
  # The headless smoke: a virtual X server and `import` for the screenshots;
  # the smoke and the lost-display check: xdotool to find the panels by
  # their titles and to start a recording.
  xvfb
  imagemagick
  xdotool
  # steno-audio's PipeWire backend: headers and bindgen's libclang.
  libclang-dev
  libpipewire-0.3-dev
  # Its live tests: a private daemon, pw-cli, pw-dump, pw-link,
  # pw-metadata and pw-play, WirePlumber and the session bus WirePlumber
  # 0.4 needs.
  dbus
  pipewire
  pipewire-bin
  wireplumber
)

if ! command -v apt-get >/dev/null 2>&1; then
  echo "setup-linux: apt-get not found; this script is for Debian and Ubuntu" >&2
  exit 1
fi

sudo=""
if [[ "$(id -u)" != "0" ]]; then
  sudo="sudo"
fi

$sudo apt-get update
$sudo env DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends "${packages[@]}"
