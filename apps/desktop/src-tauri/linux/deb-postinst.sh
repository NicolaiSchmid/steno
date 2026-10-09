#!/bin/sh
# Steno's .deb postinst: has every running systemd user manager reload its
# units, so the stop timeout drop-ins this package installs under
# /usr/lib/systemd/user reach a Steno that is running now, autostarted
# before the upgrade, and not only one started after the next login
# (apps/desktop/src-tauri/src/stop_timeout.rs). It skips a user whose
# autostart entry is gone unless their autostart unit is known to be
# stopped (`inactive` or `failed`): with the entry gone, the reload would
# unload a running unit, and the session's end would then stop the app
# without the SIGTERM that saves its recording. It never fails the
# install.
set -u

unit='app-steno\x2ddesktop@autostart.service'
entry=.config/autostart/steno-desktop.desktop

[ "${1:-}" = configure ] || exit 0
[ -d /run/systemd/system ] || exit 0
command -v systemctl >/dev/null 2>&1 || exit 0

# `--user -M <user>@` reaches that user's manager from root (systemd 248
# and later); an older systemctl fails, and nothing reloads.
systemctl list-units --type=service --state=running --plain --no-legend 'user@*.service' 2>/dev/null \
  | while read -r manager _; do
    uid=${manager#user@}
    uid=${uid%.service}
    user=$(getent passwd "$uid" | cut -d: -f1)
    home=$(getent passwd "$uid" | cut -d: -f6)
    [ -n "$user" ] || continue
    if [ ! -e "$home/$entry" ]; then
      # Empty when the call fails; the user is then skipped, as for a running unit.
      state=$(systemctl --user -M "$user@" is-active "$unit" 2>/dev/null)
      case $state in
        inactive | failed) ;;
        *) continue ;;
      esac
    fi
    systemctl --user -M "$user@" daemon-reload 2>/dev/null || true
  done

exit 0
