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
# remembers anything afterwards. A private session bus comes up beside them
# when `dbus-daemon` is installed: WirePlumber 0.4 (Ubuntu 24.04) exits
# without one. Without `dbus-daemon` the bus is disabled, which WirePlumber
# 0.5 survives (its D-Bus modules log an error and are skipped).
#
#   scripts/pipewire-headless.sh cargo test -p steno-audio --test pipewire \
#     -- --ignored --test-threads=1 --nocapture
#
# Needs `pipewire`, `wireplumber`, `pw-cli`, `pw-dump`, `pw-link`,
# `pw-metadata` and `pw-play` on PATH, `stdbuf` and `timeout` (coreutils),
# and `dbus-daemon` for WirePlumber 0.4 (Ubuntu: pipewire, pipewire-bin,
# wireplumber, dbus; Nix: pipewire, wireplumber, dbus). Used by rust-ci.yml
# on ubuntu-latest.
set -euo pipefail

if [[ $# -eq 0 ]]; then
  echo "usage: $0 <command> [args...]" >&2
  exit 2
fi
for tool in pipewire wireplumber pw-cli pw-dump pw-link pw-metadata pw-play stdbuf timeout; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "pipewire-headless: $tool not found" >&2
    exit 1
  fi
done

# Keep TMPDIR short: the bus socket lives under it, and a Unix socket path
# holds at most 107 bytes.
root="$(mktemp -d "${TMPDIR:-/tmp}/steno-pipewire.XXXXXX")"
# The processes this script started, and only those, are the ones it ends.
dbus_pid=""
daemon_pids=()
# Ends the given processes: a TERM, 2 s to go, then a KILL.
terminate() {
  if [[ $# -eq 0 ]]; then
    return 0
  fi
  kill "$@" 2>/dev/null || true
  # `kill -0` succeeds while any of them is alive.
  for _ in $(seq 1 20); do
    kill -0 "$@" 2>/dev/null || break
    sleep 0.1
  done
  # Only while one is still alive: a KILL to gone processes could reach a
  # reused PID.
  if kill -0 "$@" 2>/dev/null; then
    kill -9 "$@" 2>/dev/null || true
  fi
  wait "$@" 2>/dev/null || true
}
cleanup() {
  terminate "${daemon_pids[@]}"
  if [[ -n "$dbus_pid" ]]; then
    terminate "$dbus_pid"
  fi
  command rm -rf "$root"
}
# A signal ends the script but not the command it runs: send it to the
# script's process group (as Ctrl-C and CI's cancel do), not to its PID.
trap cleanup EXIT

export XDG_RUNTIME_DIR="$root/runtime"
export XDG_CONFIG_HOME="$root/config"
export XDG_STATE_HOME="$root/state"
export PIPEWIRE_RUNTIME_DIR="$XDG_RUNTIME_DIR"
unset PIPEWIRE_REMOTE
mkdir -p "$XDG_RUNTIME_DIR" "$XDG_CONFIG_HOME/pipewire/pipewire.conf.d" "$XDG_STATE_HOME"
chmod 700 "$XDG_RUNTIME_DIR"

if command -v dbus-daemon >/dev/null 2>&1; then
  dbus-daemon --session --fork --nopidfile \
    --address="unix:path=$XDG_RUNTIME_DIR/bus" --print-pid=3 3>"$root/dbus.pid"
  dbus_pid="$(cat "$root/dbus.pid")"
  export DBUS_SESSION_BUS_ADDRESS="unix:path=$XDG_RUNTIME_DIR/bus"
else
  export DBUS_SESSION_BUS_ADDRESS="disabled:"
fi

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

# Starts the daemon and WirePlumber and waits until WirePlumber published
# the defaults; false when either did not come up.
start_daemons() {
  pipewire >"$root/pipewire.log" 2>&1 &
  daemon_pids=("$!")
  for _ in $(seq 1 100); do
    [[ -S "$XDG_RUNTIME_DIR/pipewire-0" ]] && break
    sleep 0.05
  done
  if [[ ! -S "$XDG_RUNTIME_DIR/pipewire-0" ]]; then
    echo "pipewire-headless: the daemon did not start" >&2
    cat "$root/pipewire.log" >&2
    return 1
  fi

  wireplumber >"$root/wireplumber.log" 2>&1 &
  local wireplumber_pid=$!
  daemon_pids+=("$wireplumber_pid")
  # WirePlumber publishes the defaults once it has looked at the devices.
  for _ in $(seq 1 100); do
    # Not `grep -q`: it stops reading at the first match, pw-dump dies of
    # SIGPIPE, and pipefail fails the test. The time limit: a daemon that
    # hangs must not hold the run.
    if timeout 2 pw-dump 2>/dev/null | grep '"default.audio.sink"' >/dev/null; then
      return 0
    fi
    kill -0 "$wireplumber_pid" 2>/dev/null || break
    sleep 0.1
  done
  echo "pipewire-headless: WirePlumber did not publish the default devices" >&2
  tail -n 40 "$root/pipewire.log" "$root/wireplumber.log" >&2 || true
  return 1
}

# Whether a change to the `default` metadata reaches a client that bound
# it before: a daemon can come up that never sends one (about one start in
# thirty, with or without Steno), and every device-change test then fails.
# A watcher binds, a probe key is written, and the watcher must print it
# within 2 s. Every pw-metadata call has a time limit, so a daemon that
# hangs fails the probe instead of holding the run.
broadcasts_metadata() {
  local log="$root/probe.log" watcher seen="" bound=""
  # Emptied here: the watcher's own redirect may come after the first grep,
  # which would then read the last start's log.
  : >"$log"
  stdbuf -oL pw-metadata -m -n default >"$log" 2>&1 &
  watcher=$!
  # Bound once it printed the defaults the daemon already holds.
  for _ in $(seq 1 50); do
    if grep "key:'default.audio.sink'" "$log" >/dev/null; then
      bound=1
      break
    fi
    sleep 0.1
  done
  if [[ -z "$bound" ]]; then
    terminate "$watcher"
    return 1
  fi
  if timeout 2 pw-metadata -n default 0 steno.probe "$$" >/dev/null 2>&1; then
    for _ in $(seq 1 20); do
      if grep "key:'steno.probe'" "$log" >/dev/null; then
        seen=1
        break
      fi
      sleep 0.1
    done
  fi
  terminate "$watcher"
  timeout 2 pw-metadata -n default -d 0 steno.probe >/dev/null 2>&1 || true
  [[ -n "$seen" ]]
}

for try in 1 2 3; do
  start_daemons || exit 1
  broadcasts_metadata && break
  if [[ $try -eq 3 ]]; then
    echo "pipewire-headless: the daemon sends no metadata changes after 3 starts" >&2
    tail -n 40 "$root/pipewire.log" "$root/wireplumber.log" >&2 || true
    exit 1
  fi
  echo "pipewire-headless: the daemon sends no metadata changes; restarting it (start $try)" >&2
  terminate "${daemon_pids[@]}"
  daemon_pids=()
  command rm -f "$XDG_RUNTIME_DIR/pipewire-0" "$XDG_RUNTIME_DIR/pipewire-0.lock" \
    "$XDG_RUNTIME_DIR/pipewire-0-manager" "$XDG_RUNTIME_DIR/pipewire-0-manager.lock"
done

set +e
"$@"
status=$?
set -e
if [[ $status -ne 0 ]]; then
  echo "pipewire-headless: the command failed ($status); daemon logs follow" >&2
  tail -n 40 "$root/pipewire.log" "$root/wireplumber.log" >&2 || true
fi
exit "$status"
