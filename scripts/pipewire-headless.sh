#!/usr/bin/env bash
# Runs a command against a private, headless PipeWire daemon and WirePlumber
# with the test devices crates/steno-audio/tests/pipewire.rs expects:
#
#   steno-test-sink     stereo null sink, the default output
#   steno-test-sink-2   stereo null sink, a second output to switch to
#   steno-test-mic      mono virtual source, the default input
#
# Everything lives in a temporary XDG runtime, config and state directory,
# so neither the daemon nor WirePlumber touches the user's own session or
# remembers anything afterwards. No D-Bus: the session bus is disabled, and
# the modules that want it log an error and carry on.
#
#   scripts/pipewire-headless.sh cargo test -p steno-audio --test pipewire \
#     -- --ignored --test-threads=1
#
# Needs `pipewire`, `wireplumber`, `pw-cli` and `pw-play` on PATH (Ubuntu:
# pipewire, pipewire-bin, wireplumber; Nix: pipewire, wireplumber). Used by
# rust-ci.yml on ubuntu-latest.
set -euo pipefail

if [[ $# -eq 0 ]]; then
  echo "usage: $0 <command> [args...]" >&2
  exit 2
fi
for tool in pipewire wireplumber pw-cli pw-play; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "pipewire-headless: $tool not found" >&2
    exit 1
  fi
done

root="$(mktemp -d "${TMPDIR:-/tmp}/steno-pipewire.XXXXXX")"
pids=()
cleanup() {
  for pid in "${pids[@]}"; do
    kill "$pid" 2>/dev/null || true
  done
  wait 2>/dev/null || true
  command rm -rf "$root"
}
trap cleanup EXIT

export XDG_RUNTIME_DIR="$root/runtime"
export XDG_CONFIG_HOME="$root/config"
export XDG_STATE_HOME="$root/state"
export PIPEWIRE_RUNTIME_DIR="$XDG_RUNTIME_DIR"
export DBUS_SESSION_BUS_ADDRESS="disabled:"
unset PIPEWIRE_REMOTE
mkdir -p "$XDG_RUNTIME_DIR" "$XDG_CONFIG_HOME/pipewire/pipewire.conf.d" "$XDG_STATE_HOME"
chmod 700 "$XDG_RUNTIME_DIR"

# The devices, created by the daemon itself at start. The priorities make
# WirePlumber choose steno-test-sink and steno-test-mic as the defaults.
cat >"$XDG_CONFIG_HOME/pipewire/pipewire.conf.d/steno-test-devices.conf" <<'EOF'
context.objects = [
  { factory = adapter
    args = {
      factory.name = support.null-audio-sink
      node.name = steno-test-sink
      node.description = "Steno test sink"
      media.class = Audio/Sink
      audio.position = [ FL FR ]
      priority.session = 2000
      priority.driver = 2000
      monitor.channel-volumes = true
    }
  }
  { factory = adapter
    args = {
      factory.name = support.null-audio-sink
      node.name = steno-test-sink-2
      node.description = "Steno test sink 2"
      media.class = Audio/Sink
      audio.position = [ FL FR ]
      priority.session = 1000
      priority.driver = 1000
      monitor.channel-volumes = true
    }
  }
  { factory = adapter
    args = {
      factory.name = support.null-audio-sink
      node.name = steno-test-mic
      node.description = "Steno test microphone"
      media.class = Audio/Source/Virtual
      audio.position = [ MONO ]
      priority.session = 2000
    }
  }
]
EOF

pipewire >"$root/pipewire.log" 2>&1 &
pids+=($!)
for _ in $(seq 1 100); do
  [[ -S "$XDG_RUNTIME_DIR/pipewire-0" ]] && break
  sleep 0.05
done
if [[ ! -S "$XDG_RUNTIME_DIR/pipewire-0" ]]; then
  echo "pipewire-headless: the daemon did not start" >&2
  cat "$root/pipewire.log" >&2
  exit 1
fi

wireplumber >"$root/wireplumber.log" 2>&1 &
pids+=($!)
# WirePlumber publishes the defaults once it has looked at the devices.
for _ in $(seq 1 100); do
  if pw-cli info default 2>/dev/null | grep -q metadata ||
    pw-dump 2>/dev/null | grep -q '"default.audio.sink"'; then
    break
  fi
  sleep 0.1
done

set +e
"$@"
status=$?
set -e
if [[ $status -ne 0 ]]; then
  echo "pipewire-headless: the command failed ($status); daemon logs follow" >&2
  tail -n 40 "$root/pipewire.log" "$root/wireplumber.log" >&2 || true
fi
exit $status
