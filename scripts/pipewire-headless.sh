#!/usr/bin/env bash
# Runs a command against a private, headless PipeWire daemon and WirePlumber
# with the test devices crates/steno-audio/tests/pipewire.rs expects:
#
#   steno-test-sink     stereo null sink, the default output
#   steno-test-sink-2   stereo null sink, a second output to switch to
#   steno-test-mic      mono virtual source, the default input
#
# Everything lives in a temporary XDG runtime, config and state directory
# (the runtime directory under a short path, for its sockets), so neither
# the daemon nor WirePlumber touches the user's own session or
# remembers anything afterwards. A private session bus comes up beside them
# when `dbus-daemon` is installed: WirePlumber 0.4 (Ubuntu 24.04) exits
# without one. Without `dbus-daemon` the bus is disabled, which WirePlumber
# 0.5 survives (its D-Bus modules log an error and are skipped).
#
#   scripts/pipewire-headless.sh cargo test -p steno-audio --test pipewire \
#     -- --ignored --test-threads=1 --nocapture
#
# A command it runs can kill the daemon and WirePlumber and start them
# again, as a crash or an update of the daemon would, with
# `scripts/pipewire-headless.sh --kill-daemons` and then `--start-daemons`
# (the environment it exports says where; the new daemons are ended with
# the rest when the command is done).
#
# Needs `pipewire`, `wireplumber`, `pw-cli`, `pw-dump`, `pw-link`,
# `pw-metadata` and `pw-play` on PATH, `stdbuf` and `timeout` (coreutils),
# and `dbus-daemon` for WirePlumber 0.4 (Ubuntu: pipewire, pipewire-bin,
# wireplumber, dbus; Nix: pipewire, wireplumber, dbus). Used by rust-ci.yml
# on ubuntu-latest.
set -euo pipefail

if [[ $# -eq 0 ]]; then
  echo "usage: $0 <command> [args...] | --kill-daemons | --start-daemons" >&2
  exit 2
fi
for tool in pipewire wireplumber pw-cli pw-dump pw-link pw-metadata pw-play stdbuf timeout; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "pipewire-headless: $tool not found" >&2
    exit 1
  fi
done

# The processes this script started, and only those, are the ones it ends,
# and the directories it made, which the trap removes even when the second
# `mktemp` fails.
root=""
runtime=""
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
# The daemons running now, as the last start recorded them: a command that
# started them again (`--start-daemons`) recorded its own.
recorded_daemons() {
  if [[ -s "$root/daemons.pid" ]]; then
    cat "$root/daemons.pid"
  fi
}
cleanup() {
  local recorded
  mapfile -t recorded < <(recorded_daemons)
  if [[ ${#recorded[@]} -gt 0 ]]; then
    terminate "${recorded[@]}"
  else
    terminate "${daemon_pids[@]}"
  fi
  if [[ -n "$dbus_pid" ]]; then
    terminate "$dbus_pid"
  fi
  local directory
  for directory in "${root:-}" "${runtime:-}"; do
    if [[ -n "$directory" ]]; then
      command rm -rf "$directory"
    fi
  done
}
# Starts the daemon and WirePlumber and waits until WirePlumber published
# the defaults; false when either did not come up.
start_daemons() {
  pipewire >>"$root/pipewire.log" 2>&1 &
  daemon_pids=("$!")
  record_daemons
  for _ in $(seq 1 100); do
    [[ -S "$XDG_RUNTIME_DIR/pipewire-0" ]] && break
    sleep 0.05
  done
  if [[ ! -S "$XDG_RUNTIME_DIR/pipewire-0" ]]; then
    echo "pipewire-headless: the daemon did not start" >&2
    cat "$root/pipewire.log" >&2
    return 1
  fi

  wireplumber >>"$root/wireplumber.log" 2>&1 &
  local wireplumber_pid=$!
  daemon_pids+=("$wireplumber_pid")
  record_daemons
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

# Starts the daemons until one sends metadata changes (three starts at
# most); false when none did.
start_checked() {
  local try
  for try in 1 2 3; do
    start_daemons || return 1
    broadcasts_metadata && break
    if [[ $try -eq 3 ]]; then
      echo "pipewire-headless: the daemon sends no metadata changes after 3 starts" >&2
      tail -n 40 "$root/pipewire.log" "$root/wireplumber.log" >&2 || true
      return 1
    fi
    echo "pipewire-headless: the daemon sends no metadata changes; restarting it (start $try)" >&2
    terminate "${daemon_pids[@]}"
    daemon_pids=()
    remove_sockets
  done
}

# Writes the daemons started so far where the cleanup and a later
# `--kill-daemons` find them.
record_daemons() {
  printf '%s\n' "${daemon_pids[@]}" >"$root/daemons.pid"
}

# The sockets a daemon that was ended leaves behind.
remove_sockets() {
  command rm -f "$XDG_RUNTIME_DIR/pipewire-0" "$XDG_RUNTIME_DIR/pipewire-0.lock" \
    "$XDG_RUNTIME_DIR/pipewire-0-manager" "$XDG_RUNTIME_DIR/pipewire-0-manager.lock"
}

# Kills the recorded daemons at once, as a crash would. Not waited for:
# the first ones are this script's children and stay zombies (which
# `kill -0` still finds) until its cleanup, but hold no socket after the
# KILL.
kill_daemons() {
  local recorded
  mapfile -t recorded < <(recorded_daemons)
  if [[ ${#recorded[@]} -gt 0 ]]; then
    kill -9 "${recorded[@]}" 2>/dev/null || true
  fi
  remove_sockets
}

# The two calls a command makes from inside the harness (see the top):
# they act on the harness's daemons and leave its cleanup in place.
case "$1" in
  --kill-daemons | --start-daemons)
    root="${STENO_PIPEWIRE_HEADLESS_ROOT:?$1 runs only inside $0}"
    if [[ "$1" == --kill-daemons ]]; then
      kill_daemons
    else
      start_checked
    fi
    exit
    ;;
esac

# A signal ends the script but not the command it runs: send it to the
# script's process group (as Ctrl-C and CI's cancel do), not to its PID.
trap cleanup EXIT

root="$(mktemp -d "${TMPDIR:-/tmp}/steno-pipewire.XXXXXX")"
# The sockets (the bus, `pipewire-0-manager`) live in the runtime
# directory, and a Unix socket path holds at most 107 bytes, so it gets a
# short base of its own: TMPDIR while that leaves room (the longest socket
# path is the base plus 35 bytes) and holds only characters a D-Bus
# address takes unescaped, else /tmp. A nix-shell's or a CI runner's
# TMPDIR can be far longer.
runtime_base="${TMPDIR:-/tmp}"
if [[ ${#runtime_base} -gt 64 ]] ||
  LC_ALL=C grep -q '[^A-Za-z0-9/._-]' <<<"$runtime_base"; then
  runtime_base=/tmp
fi
runtime="$(mktemp -d "$runtime_base/steno-pw.XXXXXX")"

export XDG_RUNTIME_DIR="$runtime"
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

start_checked || exit 1
export STENO_PIPEWIRE_HEADLESS_ROOT="$root"

set +e
"$@"
status=$?
set -e
if [[ $status -ne 0 ]]; then
  echo "pipewire-headless: the command failed ($status); daemon logs follow" >&2
  tail -n 40 "$root/pipewire.log" "$root/wireplumber.log" >&2 || true
fi
exit "$status"
