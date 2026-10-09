#!/bin/sh
# /usr/bin/steno-desktop in the AUR package steno-desktop-bin. The binary
# and its speech sidecar live together in /usr/lib/steno-desktop, since the
# app starts the sidecar from beside its own binary.
#
# STENO_DISTRIBUTION=aur: pacman delivers the updates, so the in-app updater
# stays off and Settings says so. STENO_EXEC_PATH: the path the autostart
# entry names, this file, which outlives every upgrade.
export STENO_DISTRIBUTION=aur
export STENO_EXEC_PATH=/usr/bin/steno-desktop
exec /usr/lib/steno-desktop/steno-desktop "$@"
