#!/bin/sh
# Steno's .deb postinst: has every running systemd user manager reload its
# units, so the stop timeout drop-ins this package installs under
# /usr/lib/systemd/user reach a Steno that is running now, autostarted
# before the upgrade, and not only one started after the next login
# (apps/desktop/src-tauri/src/stop_timeout.rs). It skips a user whose
# Steno runs as the autostart unit while the autostart entry is gone: the
# reload would unload that unit, and the session's end would then stop
# the app without the SIGTERM that saves its recording. It never fails
# the install.
set -u

unit='app-steno\x2ddesktop@autostart.service'

[ "${1:-}" = configure ] || exit 0
command -v systemctl >/dev/null 2>&1 || exit 0

systemctl list-units --type=service --state=running --plain --no-legend 'user@*.service' 2>/dev/null \
  | while read -r manager _; do
    uid=${manager#user@}
    uid=${uid%.service}
    user=$(getent passwd "$uid" | cut -d: -f1)
    home=$(getent passwd "$uid" | cut -d: -f6)
    [ -n "$user" ] || continue
    if systemctl --user -M "$user@" is-active --quiet "$unit" 2>/dev/null \
      && [ ! -e "$home/.config/autostart/steno-desktop.desktop" ]; then
      continue
    fi
    systemctl --user -M "$user@" daemon-reload 2>/dev/null || true
  done

exit 0
