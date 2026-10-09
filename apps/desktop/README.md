# Steno desktop shell

The Tauri 2 shell for macOS, Linux and Windows: three windows and two
floating panels around the web UI in `apps/macos/web`, a tray icon, and the
`bridge_call` command the web app's Tauri transport talks to. WP3 and WP8
of `.plans/2026-10-02-rust-core-and-tauri-shell.md`; WP6b puts it on the
real host. The shell holds no logic: `steno_host::Host` over the services
graph (`steno-services`: the store, the pipeline, the recorder, the
handover listener) answers the bridge. The opt-in `fixture-host` cargo
feature answers it from the recorded fixtures in
`apps/macos/web/fixtures/bridge/` instead, for UI work without a database
(`cargo run -p steno-desktop --features fixture-host`). Paths below are
from the repository root.

## What the shell owns

Everything the Swift app does outside its three windows, per OS:

| | macOS | Linux | Windows |
|---|---|---|---|
| Tray (`tray.rs`) | Menu bar extra with a template icon | Status notifier item (libayatana-appindicator) | Notification area icon |
| Floating panels (`panels.rs`) | Non-activating `NSPanel`s on every space (`tauri-nspanel`) | Always-on-top undecorated windows, under XWayland on a Wayland session (see below) | Always-on-top undecorated windows |
| Launch at login (`autostart.rs`, `packaged.rs`) | Launch Agent | `~/.config/autostart` entry naming a stable path (see Packaged installs), and systemd drop-ins for the stop timeout (`stop_timeout.rs`, see Launch at login under systemd) | Run registry key |
| Updates (`updater.rs`) | signed manifest per lane | same; off on a packaged install (see Packaged installs) | same |
| Permissions (`permissions.rs`) | microphone TCC status and prompt; system audio and calendar deferred to the host's probes | unknown (nothing to query before capture; the portal asks when the stream opens) | unknown (the privacy switch decides at capture time) |
| Deep links (`deep_links.rs`) | `steno:` through `CFBundleURLTypes`, written into the bundle by the deep-link plugin from `plugins.deep-link` | the `.deb`'s desktop entry from `linux/` (`Exec=… %u`, the `x-scheme-handler/steno` MIME type); an AppImage and a debug build register at start | registry (debug builds register at start) |
| Single instance | Unix socket | session bus name (skipped without a session bus, as in the headless smoke) | named mutex |
| Dialogs (`dialogs.rs`) | `NSOpenPanel` sheet | GTK file chooser | common item dialog |

Secrets are not the shell's: the `SecretStore` lives in `steno-services`
(the login Keychain on macOS; the Credential Manager on Windows, each
credential kept on this computer rather than with a roaming profile; the
Secret Service on Linux, or the 0600 `secrets.json` where no keyring answers
before the first move into it), which the host reads the API key through.

Every exit saves first: `App::shutdown` runs once, at most ten seconds.
It quits the pipeline (no new job starts), lets a start or a stop under
way settle, stops and saves a recording in progress and stops the
handover listener, as the Swift `applicationShouldTerminate` awaited
`AppController.shutdown`. The saved recording stays queued, and a job
the exit ends stays processing, not failed, until the next launch. The
exits reach the shutdown these ways:

- Quit in the tray's menu or the macOS menu bar, the close that ends the
  process because no tray stands, and SIGTERM, SIGINT and SIGHUP (a
  plain `kill`, Ctrl-C, a closed terminal, systemd at a shutdown) are
  exit requests, held until the shutdown has ended (`exit_request` in
  `main.rs` over `steno_services::app::ExitGate`); a second Quit
  meanwhile is held too. A signal quits the pipeline at once, before its
  request reaches the main thread. A second SIGTERM or a second SIGINT
  ends the process at once, unsaved; a SIGHUP never does. A signal the
  app inherited ignored (`nohup`, a background job's SIGINT) stays
  ignored.
- A logout on GNOME or Xfce saves before the session manager lets the
  app go: the app registers with it on the session bus (GNOME's
  `org.gnome.SessionManager`, else Xfce's `org.xfce.SessionManager`,
  which serves the same protocol under names of its own,
  `session_end.rs`). The session manager first asks whether the session
  may end, and the logout can still be called off after that (GNOME's
  confirmation dialog, another Xfce app; xfce4-session then tells the app
  nothing), so the app answers at once, records on, and saves at the end,
  which gnome-session waits about ten seconds for and xfce4-session
  seven. When xfce4-session tells the app to leave (Session settings'
  Quit Program), the app unregisters, which calls off the kill
  xfce4-session would send 15 seconds later, then saves and quits.
  xfce4-session also tells a client to leave right after it dropped it,
  when a checkpoint (Save Session) has waited a minute, usually with no
  logout to follow, and with no kill; the app then records on and
  registers again, so a later logout still reaches it. On Wayland
  xfce4-session quits right after it asked, with no end and no cancel to
  follow, so there the app saves when asked, answers, and ends with the
  display. Should the session go on after all (the app took it for a
  Wayland one wrongly), the app says so 30 seconds later and relaunches
  once the message is closed, so the user can record again.
- Where no session manager runs (KDE Plasma, wlroots desktops), the app
  follows the desktop portal's session monitor. It answers the portal's
  query at once, since the user can still call the logout off then, and
  saves when the portal reports the session ending. Plasma 6.6's portal
  serves the monitor, but nothing in Plasma 6.6 asks it yet, and the
  other portals report no end; the next point is what saves there.
- Every logout ends the display server, and so does a shutdown once
  logind goes ahead; GDK would end the process with it. The app's log
  writer sees GDK's last line and holds that exit until the save has
  ended (`display_lost.rs`), so a save that outlasts a session manager's
  or logind's wait still ends, at most ten seconds after it began. On
  KDE Plasma this is the save: ksmserver asks only XSMP clients, which
  GTK 3 is not, and on Wayland KWin closes only native Wayland windows,
  not the app's, which run under XWayland. The writer knows GDK's lines
  by GTK 3.24.52's wording; a GTK that rewords them ends the app unsaved
  again. Only a kill ends the save early: systemd's `SIGKILL` once a
  stop has waited out the unit's `TimeoutStopSec` (90 s unless the unit
  sets another; the autostart unit and GNOME's app scope set 5 s, which
  Steno's drop-ins raise to 20 s, see below), xfce4-session's `SIGKILL`
  15 seconds after it told the app to leave, which the app calls off by
  unregistering first, or a second SIGTERM.
- While a recording runs the app holds the portal's logout inhibitor
  ("A meeting is being recorded") and releases it when the recording
  stops. GNOME then lists Steno in its logout dialog, also for
  `gnome-session-quit --logout --no-prompt`, so the user can go back to
  the meeting; logging out anyway saves as above (read from
  gnome-session's and the GTK portal's source; not yet seen on a real
  GNOME session). Plasma 6.6's portal records it for its session
  monitor, which nothing in Plasma asks yet; the GTK portal outside
  GNOME (Xfce, wlroots) refuses it.
- A system shutdown or reboot on Linux saves while logind waits: the app
  holds logind's `shutdown` delay lock and releases it after the save.
  logind waits for the lock at most five seconds by default
  (`InhibitDelayMaxSec`), then goes ahead; the SIGTERM that follows waits
  for the save in progress, and so does the display closing. Sleep and
  the screen lock do not stop a recording.
- An autostarted Steno on a desktop that runs XDG autostart through
  systemd (KDE Plasma, and uwsm sessions such as Omarchy's Hyprland) is
  the unit `app-steno\x2ddesktop@autostart.service`, which
  `systemd-xdg-autostart-generator` makes from the entry with
  `TimeoutStopSec=5s`. On GNOME, gnome-session starts the entry, and
  gnome-shell an app from the dash or the app grid, in a scope of its
  own, `app-gnome-steno\x2ddesktop-<pid>.scope`, which gnome-session's
  `app-gnome-.scope.d/override.conf` gives `TimeoutStopSec=5s` too. When
  the session ends, systemd sends SIGTERM and, once the stop has waited
  out that `TimeoutStopSec`, `SIGKILL`, while the save may need ten
  seconds and the process two more to end. Two drop-ins raise both
  timeouts to 20 s (see Launch at login under systemd).
- The Dock's Quit, a logout and a shutdown on macOS reach the shell only as
  the run loop's last event, `RunEvent::Exit`, which AppKit waits for, so
  it waits for the shutdown first (`shut_down_before_exit`).
- An update's relaunch bypasses the request, so it waits for the shutdown
  first too; on Windows the installer's own exit runs it, and an install
  that fails after that ends the app once its message is closed.
- A logoff or a shutdown on Windows also arrives as `RunEvent::Exit`
  (tao answers `WM_ENDSESSION` with it), and the shutdown runs until
  Windows' end-session timeout ends the process: about five seconds,
  less than the ten above. Untested on hardware, and the logoff may end
  the speech sidecar, a console process, before `RunEvent::Exit` quits
  the pipeline, so its job could leave the meeting failed; unverified
  (WP10).

What starts the save on each Linux desktop, from the desktops' source
(GTK 3.24.52, xfce4-session 4.20.4, Plasma 6.6.5, gnome-session 50.1);
only a kill cuts it off on any of them:

| Desktop | What starts the save |
|---|---|
| GNOME (X11, Wayland) | `EndSession`, about ten seconds to answer; then the display closing holds the exit until the save has ended |
| Xfce on X11 | `EndSession`, seven seconds to answer; then the display closing holds the exit until the save has ended |
| Xfce on Wayland | `QueryEndSession`; xfce4-session quits at once, and the display closing holds the exit until the save has ended |
| Xfce's Quit Program (Session settings) | `Stop`; the app unregisters first, so xfce4-session's kill 15 seconds later does not come |
| KDE Plasma 6.6 (Wayland, X11) | the display closing |
| A desktop whose portal reports the end | the portal's ending state, then the display closing |
| wlroots and others | the display closing |
| A shutdown or reboot (all) | logind's delay lock (five seconds), then SIGTERM and the display closing, which both wait for the save |

The session clients, the portal's monitor and inhibitor and the logind
lock are tested against fakes on a private bus. The lost display ran
under Xvfb and headless sway with a recording in progress, and a real
xfce4-session 4.20.4 logout ran on X11 and on Wayland under labwc, and
its Quit Program and Save Session under a recording on X11; no
real GNOME or KDE Plasma session has run yet (before the first Linux
release). The autostart unit's stop ran under a real systemd user
manager, in a container (see Launch at login under systemd).

On Linux an exit that went through ends the process two seconds later at
the latest (`end_within` in `main.rs`): the single-instance plugin
releases its bus name at the run loop's end and waits for the bus
without a bound, so a frozen session bus would hold the exit.

The speech sidecar ignores SIGINT, SIGTERM and SIGHUP on Linux and
macOS: Ctrl-C, a closed terminal and systemd signal it with the app, and
it would otherwise end its job first. It exits within a heartbeat once
the app is gone.

On Linux, when the app runs in a unit of a systemd user manager (a
launcher's scope, the autostart service), the sidecar gets a transient
scope of its own right after its start,
`app-steno\x2dspeech\x2dsidecar-<pid>.scope`, in the app's slice and
`PartOf` the app's unit (`crates/steno-speech/src/sidecar/scope.rs`).
`systemd-oomd`, which some distributions turn on for the user's session,
kills a whole cgroup under memory pressure. With the sidecar in a cgroup
of its own, oomd takes the sidecar first and the recording goes on: the
job that was transcribing fails as after any crash of the sidecar, and
the next job starts a new one. The app's own cgroup stays a
candidate: speaker diarization runs in the app's process, so while it
runs, or under pressure that lasts after the sidecar is gone, oomd can
take the app.

The app asks the user manager for the scope over the user bus's Unix
socket in `$XDG_RUNTIME_DIR`, sending the sidecar's pid, the unit names
and the scope's fixed settings, nothing else, and waits about two seconds
for the sidecar to be in it. A start the manager has not carried out by
then is called off and the sidecar stays in the app's cgroup; a sidecar
that joined its scope just before, or that a late manager moves
afterwards, stays in its scope. Without a user manager, a user bus or a
unit (a plain shell, a container), or when the manager refuses, the
sidecar stays in the app's cgroup. Stopping the app's unit also stops the
sidecar's scope. The log says which happened: `speech sidecar in a scope
of its own`, `the speech sidecar stays in the app's cgroup`, or, when
nothing tells whether the manager moved it, `the speech sidecar stays
where the user manager put it`.

Snapshots reach the windows, the tray and the panels from the main thread
(`WindowSink` in `host.rs`): the host emits under its `publishing` lock,
the main thread can be waiting for a thread that holds it (a Stop from the
tray joins the recorder's level thread, which publishes), and the tray's
setters wait for the main thread when called from another.

The tray menu is the Swift menu bar popover's controls: Record (Stop
recording while recording, with the recorder's words between), Record in
person, Open Steno, Settings, Launch at login, Check for Updates, Quit.
Each platform names them its own way (`MenuAction::label_on`): the Mac's
"Settings…", "Check for Updates…" and "Quit Steno", the first two
without the ellipsis on Windows and Linux, "Exit Steno" on Windows; only
the Mac's menu shows shortcut hints.
Recorder commands are bridge methods sent through the main window
(`actions.rs`), so the shell has no recorder logic of its own; it follows
the recorder off the `recording` snapshots the host publishes to that
window (`recording.rs`). The queue and recent rows of the Swift popover are
the main window's. Closing the main window hides it while the tray stands,
as the Swift window closes behind the menu bar item, so the tray and the
panels always have it. Where no tray could be built, or nothing shows it,
the window closes for real and the process ends with it, since nothing
would be left to reach the app from. On Linux "shows it" means a status
notifier host: at each close the shell asks the session bus whether
`org.kde.StatusNotifierWatcher` has an owner (KDE, most desktop panels,
and GNOME only with the AppIndicator extension); once it has seen one it
stops asking, so a bus that fails one call does not turn a close into a
quit. On stock GNOME the icon is not shown and closing main quits; the
AppIndicator extension brings the tray back. An `XEmbed`-only tray is not
asked for, so there closing main also ends the app, the safe side. On macOS
the menu bar carries the shell's own menu (`menu.rs`): Quit goes through
the run loop, the Edit menu gives the pages their copy and paste shortcuts.

Every webview gets the platform before its page runs: `platform.rs` adds
`window.__STENO_PLATFORM__ = "linux"` (or `"macos"`, `"windows"`) as an
initialization script, and the page words itself and binds its keys for
it (`apps/macos/web/src/lib/platform.tsx`: "Show in File Explorer" for
"Show in Finder", "this computer" for "this Mac", Ctrl+F for ⌘F). The
host words its own sentences and lists the permissions for the same
value (`HostConfig::platform`).

The panels are the web app's `#/panel/bubble` and `#/panel/prompt` routes
(`apps/macos/web/src/windows/panels/`), two webviews that hang from one
anchor (top centre of the frame, 8 pt under the main screen's top edge by
default, saved to `panel-anchor.json` in the app config directory when the
user drags one; the geometry is `panel_geometry.rs`). One rule decides
what shows: a busy recorder wins, else a pending detection prompt, else
nothing. Each window is created once and then hidden and shown; the
prompt's is navigated to each new request, which the shell numbers when
the host raises it, so the page remounts and the countdown restarts; the X
sends that number back and dismisses only its own prompt. The page
measures its pill and reports the size in device pixels through the
`panel_call` command; the shell divides it by the window's scale factor
(WebKitGTK's pixel ratio follows the X resolution, the window's scale does
not), rounds it up to whole points, clamps it to the screen's work area
and sizes the window from it. `panel_call` is a synchronous command, so it
runs on the main thread in the order the page sent its reports, and the
last one sent is the size the window keeps (a report that is not a size is
`invalidParams`). The prompt's X
and that size report are the only two things `panel_call` carries;
everything else the panels do goes through the bridge (`recording.stop`,
`recording.keepGoing`, `recording.start`, `window.open`). The host raises
and clears the prompt through `panels::set_prompt` and hears of its X
through `panels::dismiss_prompt`; no detection controller calls either
yet (see Not here yet).

On a Wayland session the shell runs under XWayland. GTK 3 on Wayland can
neither place a window nor keep it above the others, and it reports no
moves, so the panels would not float, would not stay where they are put,
and would never save the anchor. When `WAYLAND_DISPLAY` and `DISPLAY` are
both set and `GDK_BACKEND` is not, `main` allows GDK only its `x11`
backend before Tauri initialises GTK (`display.rs`). That setting stays
inside the process, so a browser the shell opens still starts on Wayland.
Every start logs one `display:` line naming the backend. A `GDK_BACKEND`
that is a list, or holds GDK's `*`, counts as not set: it is a session's
default for every app (Omarchy exports `wayland,x11,*`), and GDK skips
the entries it is not allowed. A single `GDK_BACKEND` set before launch
always wins: `GDK_BACKEND=wayland steno-desktop` runs natively on
Wayland, with panels that neither stay on top nor keep their place. A
Wayland session without XWayland (no `DISPLAY`) runs on Wayland too,
since X11 would not open there.

On Linux the panels are titled "Steno bubble" and "Steno prompt" (on
macOS and Windows "Steno", as the main window), so a window manager's
rules can tell them from the main window; Hyprland's are below.

The bridge methods the shell answers itself, beside `window.*` and
`system.openURL`: `system.openSystemSettings` (the pane per OS),
`settings.general.openLoginItems`, `settings.general.setLaunchAtLogin`
(then forwarded to the host so its snapshot follows), `updates.check`, and
the three folder panels (`settings.recording.chooseFolder`,
`settings.export.chooseVault`, `onboarding.chooseVault`): the shell shows
the panel, tells the host the choice as `{ "path": … }` under the same
method name (the host's `choose_folder` callback answers with it,
`host::ChosenFolder`), and replies `reply.chosenPath` itself; a cancelled panel
replies `{}` and the host hears nothing.

Deep links: the Swift Mac app registers no scheme (`steno://pair/…` is the
iPhone's, the link the pairing QR code doubles as), so the scheme is new
here. `steno://meeting/<uuid>` opens the main window on the meeting (the
host selects it; the fixture host only brings main forward),
`steno://settings[/<section>]` opens Settings on the section; scheme and
host read in any case, the section as the contract spells it; a pairing
link is logged and ignored. A link that reaches a window before its page
has mounted (a cold launch) waits in `windows::Pages` and is published on
the page's `page.ready`.

### Launch at login under systemd

systemd gives the autostart unit and GNOME's app scope 5 s to stop (see
the exits above). Two drop-ins raise that to 20 s: the shutdown's ten
seconds and the process's two more, with room to spare, and still short
enough that a hung app does not hold a logout for long.

| File in `linux/` | Unit | Name in the unit's `.d` directory |
|---|---|---|
| `autostart-service-stop-timeout.conf` (`[Service]`) | `app-steno\x2ddesktop@autostart.service`, the autostart unit | `10-steno.conf`, so a drop-in of the user's own (`systemctl --user edit`) still wins |
| `gnome-scope-stop-timeout.conf` (`[Scope]`) | `app-gnome-steno\x2ddesktop-.scope`, every scope GNOME starts Steno in | `zz-steno.conf`: drop-ins apply in file name order, so it comes after gnome-session's `override.conf` |

They reach the user manager two ways:

- The `.deb` installs both under `/usr/lib/systemd/user/`
  (`bundle.linux.deb.files` in `tauri.conf.json`), and its `postinst`
  (`linux/deb-postinst.sh`) has every running user manager reload its
  units, so a Steno autostarted before an upgrade gets the 20 s too
  (`check-bundle.sh` checks all three files).
- The app writes the same files under the same names into
  `~/.config/systemd/user/`, whatever `XDG_CONFIG_HOME` says: the
  autostart entry is always under `~/.config/autostart`, so only a user
  manager that reads `~/.config` has the unit. GNOME's drop-in is
  written at every launch and never removed; the autostart unit's while
  Launch at login is on, at each launch and when it is switched on. After
  writing a file the app asks the user manager to reload its units over
  the session bus, so a running session takes the new timeout at once.
  The write leaves a mark in the support directory
  (`~/.local/share/Steno/systemd-reload-owed`) that only a reload that
  went through clears, so a reload that failed, was skipped or was cut
  off is asked for again at the next launch or switch; with nothing
  written and nothing owed the app does not reload, since each reload
  reruns every generator of the user manager (`stop_timeout.rs`). This
  covers the AppImage, and installs that turned Launch at login on
  before the drop-ins existed. A directory it cannot write is logged, and
  the unit keeps 5 s.

While the system manages the login item (`STENO_LOGIN_ITEM=managed`,
Packaged installs below), the switch writes nothing, though it shows on.
The app writes the autostart unit's drop-in only while it runs as that
unit and the unit's entry stands (an entry an earlier build wrote, or
the user's own), with the reload that applies it, and the exit that
removes an earlier build's entry removes the drop-in with it.

To check a running Steno: `systemctl --user show 'app-steno*'
'app-gnome-steno*' -p TimeoutStopUSec -p DropInPaths` shows 20 s and the
drop-in.

A reload while the autostart entry is gone unloads the running
autostart unit, and the session's end then stops the app without a
SIGTERM, before its save. So the app, while it runs as the autostart
unit, asks for a reload only while the entry stands, and the `postinst`
skips a user whose entry is gone unless their autostart unit is known
to be stopped (`inactive` or `failed`).

Outside the autostart unit, turning Launch at login off removes the
entry and the autostart unit's drop-in at once, with no reload. While
the app runs as the autostart unit, the entry stays until the app
exits, since any other reload of the user manager (a package install,
another app, `nixos-rebuild switch`) would unload the unit. Settings and
the tray show the switch off at once; the app marks the choice in the
support directory (`~/.local/share/Steno/launch-at-login-off-at-exit`)
and removes the entry and the drop-in after the exit's save. A mark
left by a kill or a crash is applied at the next launch that does not
run as the unit, so the next login still autostarts the app once; an
update's relaunch keeps it. Turning it on again before the exit clears
the mark and leaves the entry as it is: the plugin rewrites an entry by
emptying it first, and a reload that read it empty would unload the
unit. A launch as the
autostart unit that finds no entry (an older release removed it at
once, or the user did, and then an update relaunched in the unit) puts
the entry back with the mark, so the unit gets its drop-in and the reload
that applies it, and the entry goes again after the save.

The user's copies stay after the package is removed. They name only
Steno's units and change nothing once the app is gone.

On Arch and NixOS every package change reloads the user managers
(Arch's systemd package ships a pacman hook that does; so do
`nixos-rebuild switch` and `home-manager switch`), so there only the kept
entry protects a running autostart unit. Where the AUR and Nix packages
put the drop-ins is under Packaged installs.

The app logs how long each shutdown took at `warn` (`the shutdown
ended`, with `elapsed`; on Linux in the journal of the unit), so a real
machine's log shows how long a save takes. In a container limited to one
CPU, under a real systemd user manager: without the drop-ins, the
autostart unit and GNOME's scope killed a stand-in that needs 8 s to
save, 5 s after SIGTERM; with them, it finished. A debug build with 8 s
added before its save wrote the drop-ins at launch, and the running unit
and scope took 20 s. Stopped in either, it saved, and the meeting was
`queued` with its duration. It saved the same way when Launch at login
was turned off mid-recording and the user manager reloaded after that.
With the stand-in autostarted before the drop-ins existed, installing
them and running the `postinst` gave the running unit and scope 20 s,
and the session's end let it finish; without the `postinst` the unit
kept 5 s and killed it.

At a reboot the save runs inside logind's delay (above), before systemd
stops anything. On Omarchy logind waits up to 15 s (`InhibitDelayMaxSec`),
more than the save's ten; the user manager itself then gets only 5 s
(`user@.service` `TimeoutStopSec=5s`), which a drop-in for Steno's unit
cannot raise, so that path relies on the delay. Elsewhere logind waits 5 s
by default and the user manager 120 s, so a save that outlasts logind's
wait finishes under the drop-ins' 20 s. Started from a compositor key
binding without `uwsm-app`, Steno runs in the compositor's own unit
(uwsm's `wayland-wm@.service`, 10 s), where the drop-ins do not apply.

## Packaged installs

A package manager that installs Steno also updates it, and may start it at
login itself. Two environment variables tell the app (stable plan X5,
`.plans/2026-10-07-stable-promotion.md`):

| Variable | Set by | What the app does |
|---|---|---|
| `STENO_DISTRIBUTION=aur` or `=nix` | the Nix package at build time (`env`) and in its wrapper; the AUR package's `/usr/bin` wrapper | Never checks for updates: the tray's Check for Updates says "Updates come from your package manager.", and Settings > General shows that line in place of the check and the two switches. A value in the environment wins over the one the build was given (`steno_services::updates::updates_are_managed`) |
| `STENO_LOGIN_ITEM=managed` | the NixOS module, for its user service and the session (`environment.sessionVariables`) | Leaves Launch at login to the system: it never writes, rewrites or removes the autostart entry, the first launch registers nothing, the autostart unit's drop-in is written only for the unit made from an entry that stands and goes with that entry, Settings shows the switch on and locked with "Your system opens Steno when you log in and manages this setting.", and the tray's item is checked and disabled. It removes one entry, below |

With `STENO_LOGIN_ITEM=managed`, the one entry the app removes is one an
earlier build wrote: its `Exec` starts a program in `/nix/store`, or
names one of the paths in step 3 below. That entry goes at launch,
unless the app runs as the unit systemd made from it
(`app-steno\x2ddesktop@autostart.service`): then it goes when Steno
exits, after the save, since a reload without the entry unloads that
unit (see Launch at login under systemd). While Steno runs as that unit
it gives the unit its stop timeout drop-in, and removes the drop-in with
the entry. Without the variable, such an entry stays as it is; if Steno
no longer opens at login, turn Launch at login off and on again.

Without `STENO_LOGIN_ITEM=managed`, the entry the app writes on Linux
(`~/.config/autostart/steno-desktop.desktop`, the plugin's file and form)
names a path that outlives an upgrade, never `current_exe()`, which on
Nix is `<out>/bin/.steno-desktop-wrapped` in the store
(`packaged.rs`):

1. An AppImage names `$APPIMAGE`, as the plugin does.
2. `STENO_EXEC_PATH`, when a package's wrapper sets it to an absolute
   path outside `/nix/store` of a file anyone may run: for a launcher
   that is not beside the binary it runs, such as a `/usr/bin` wrapper
   over `/usr/lib/steno-desktop/`.
3. Else the first of `/usr/bin/steno-desktop`,
   `/usr/local/bin/steno-desktop`,
   `/run/current-system/sw/bin/steno-desktop`,
   `/etc/profiles/per-user/$USER/bin/steno-desktop`,
   `~/.nix-profile/bin/steno-desktop` and, with Nix's
   `use-xdg-base-directories`,
   `${XDG_STATE_HOME:-~/.local/state}/nix/profile/bin/steno-desktop` that,
   with its links resolved, lies in the directory the running binary lies
   in. Nix's wrapper and the binary it runs share `<out>/bin`, so a
   profile counts while it links to this build; the `.deb` names
   `/usr/bin/steno-desktop` itself.

A path with a control character, `%` or `\` is never named: systemd's
XDG autostart generator would skip the entry. With none of these paths,
turning Launch at login on writes nothing, the switch turns back off,
and Settings says "Opening Steno at login could not be changed." with
the reason "Steno can't open at login from where it's installed now.
Restart Steno, or install it with your package manager." The setting
is not saved. When the first launch's own registration fails, that
launch is not counted, so the next one tries again. A development
build from `target/` is such a case, and so is a running build whose
profile now links to a newer one, until Steno restarts.

What a package sets:

- **Nix** (stable plan X7, #259: `nix/package.nix`):
  `STENO_DISTRIBUTION=nix` in the build's `env` and in the wrapper. The
  module (`nix/module.nix`) sets `STENO_LOGIN_ITEM=managed` on its
  service and in `environment.sessionVariables` when it starts Steno at
  login; without the module, the app finds the profile's path itself
  (step 3). The `.deb`'s drop-ins come with the `.deb`'s tree (see The
  Nix package and the NixOS module).
- **AUR** (stable plan X6): `STENO_DISTRIBUTION=aur` in the `/usr/bin`
  wrapper, and `STENO_EXEC_PATH=/usr/bin/steno-desktop` when the wrapper
  runs a binary elsewhere (`/usr/lib/steno-desktop/`). A package whose
  `/usr/bin/steno-desktop` is the binary itself, or a link to it, needs
  no `STENO_EXEC_PATH`; it still needs `STENO_DISTRIBUTION=aur`, from a
  wrapper or from the build's environment, and both drop-ins under
  `/usr/lib/systemd/user/`.

## Hyprland

Hyprland (Omarchy, NixOS with `programs.hyprland`) ignores the
always-on-top and every-workspace hints the panels set, and focuses every
window that opens. Without rules the bubble takes the keyboard focus from
the call when a recording starts and stays on the workspace it opened
on. `src-tauri/linux/hyprland-steno.lua` holds the window rules for
Hyprland 0.55 and later, whose config is Lua: the panels float, are
pinned to every workspace, take no focus when they open or when the
pointer crosses them, and have no border, shadow or blur. The rules match
the class and the panels' titles ("Steno bubble", "Steno prompt"), never
the main window ("Steno").

Load them at the end of `~/.config/hypr/hyprland.lua` (on Omarchy, below
"Add any other personal Hyprland configuration below"):

```lua
-- the AUR package installs the file here
require("/usr/share/steno-desktop/hyprland-steno")
-- or a copy of it at ~/.config/hypr/steno.lua
require("steno")
```

Hyprland reloads its config when the file changes. A config in the older
syntax, `hyprland.conf` from Hyprland 0.53 on (or the one Home Manager
writes from `wayland.windowManager.hyprland.settings`), takes the same
rule as one line:

```
windowrule = match:class [Ss]teno-desktop, match:title Steno (bubble|prompt), float on, pin on, no_initial_focus on, no_follow_mouse on, border_size 0, no_shadow on, no_blur on
```

With the rules loaded, a recording's bubble shows as floating, pinned
and under XWayland:

```sh
hyprctl clients -j | jq '.[] | select(.class | test("steno-desktop"; "i"))
  | {title, xwayland, floating, pinned}'
```

On a `uwsm` session, as Omarchy's, start Steno from the app launcher or
with `uwsm-app -- steno-desktop`, so it runs in a scope of its own and
not in Hyprland's unit (stable plan X1). Steno runs under XWayland there
even though Omarchy sets `GDK_BACKEND=wayland,x11,*`, since a list is a
session default (above). Under native Wayland (a single
`GDK_BACKEND=wayland`, or no XWayland) Hyprland places the panels itself
and a drag is not saved; native panels on the layer-shell protocol,
which would need no rules, are later work.

## Run

`cargo tauri dev` from `apps/desktop/src-tauri` (`pnpm dlx
@tauri-apps/cli@2.12.1 dev`, the version the workflows pin, or `cargo
install tauri-cli --version 2.12.1`) starts the Vite dev server and the
shell together. Without the CLI, start the dev server yourself and build the shell:

```sh
pnpm --dir apps/macos/web install --frozen-lockfile
pnpm --dir apps/macos/web dev &
cargo build -p steno-desktop -p steno-speech-sidecar && target/debug/steno-desktop
```

Parakeet, the speech model, runs in its own process, `steno-speech-sidecar`,
on Linux and Windows. On the Mac `CoreML` runs it inside the shell, unless
`speech.json` in the support directory holds `{"onnxSidecarOnMac": true}` or
the stored engine is not Parakeet v3 (Whisper, which Settings offers, or
Parakeet Ultra or Parakeet DE, which the Swift app may have stored); then the
sidecar runs it there too. The shell reads `speech.json` once, at launch: an
edit takes effect at the next start, not at a Settings save. The support
directory is `~/Library/Application Support/Steno` on the Mac,
`$XDG_DATA_HOME/Steno` (else `~/.local/share/Steno`) on Linux and
`%APPDATA%\Steno` on Windows.

`steno-services` starts the sidecar from beside the shell's binary
(`steno_services::speech::sidecar_config`). `cargo build -p steno-desktop` and
`cargo tauri dev` do not build it: use the command above, run
`cargo build -p steno-speech-sidecar` once before `cargo tauri dev`, or run
`cargo build` at the workspace root, so that `target/debug/` holds both
binaries. Without the sidecar the windows still work, but processing a
meeting fails with "could not start" and the path it looked for.

The sidecar also needs the fp32 Parakeet export (2.6 GB, from Hugging Face)
and Silero VAD. With Parakeet v3 as the engine, Settings > Transcription
downloads both into `onnx/` under the models directory (`Models` in the
support directory, unless `STENO_MODELS_DIR` or the settings name another). A
meeting processed before then waits in the queue with "Download the speech
models in Settings" and is processed once they are installed. `steno process`
downloads them on first use. `STENO_MODELS_MIRROR` serves them from a copy
instead. The bundles carry the sidecar beside the shell (see
Release).

Every build without the `custom-protocol` feature loads `devUrl` (the Vite
dev server on 5173), whatever the profile; that is Tauri's dev build. Set
`STENO_SMOKE_SECONDS` to open all three windows and both panels at once
(see Smoke).

## Build

The embedded bundle is what ships. Build the web UI first, then the shell
with the feature, which `cargo tauri build` (CLI 2.12.1, as above) turns
on by itself:

```sh
pnpm --dir apps/macos/web install --frozen-lockfile && pnpm --dir apps/macos/web build
cargo build -p steno-desktop --release --features custom-protocol
```

A release build with the web `dist/` missing fails in `build.rs`, so no
release bundle carries the placeholder page. The bundles
(`cargo tauri build`, or `.github/workflows/desktop-release.yml`) carry the
real host; a build with `--features fixture-host` shows the whole UI with
synthetic data instead.

A bundle that transcribes off the Mac also needs the speech sidecar beside
the app (see Release); stage it and add the release configuration:

```sh
apps/desktop/scripts/stage-sidecar.sh
cd apps/desktop/src-tauri
pnpm dlx @tauri-apps/cli@2.12.1 build --config tauri.release.conf.json \
  --config '{"bundle":{"createUpdaterArtifacts":false}}'
```

(Drop the second `--config` when `TAURI_SIGNING_PRIVATE_KEY` is set.)

### Bundles and the updater key

`tauri.conf.json` bundles `.app` and `.dmg` on macOS (minimum 15.0,
`Entitlements.plist` and `Info.plist` beside the config), `.deb` and
`.AppImage` on Linux, `.msi` and NSIS on Windows. On Linux the product
name is `steno-desktop` (`tauri.linux.conf.json`), so the `.deb` is the
`steno-desktop` package and leaves `steno` to the CLI; the binary keeps
its name and the desktop entry (`linux/steno-desktop.desktop`, a template
the bundler fills for the `.deb` and the AppImage) still reads Steno,
passes a `steno:` link to the binary (`%u`) and claims the scheme. The
`.deb` depends on `libayatana-appindicator3-1` explicitly: the Tauri CLI
adds the tray's library only when it sees the `tray-icon` feature on a
crate-local `tauri` dependency, and ours is inherited from the workspace.
For the same reason the AppImage does not bundle that library; a host
without it runs the shell without a tray, and closing the main window
then quits (see above). The `.deb` also depends on PipeWire
(`libpipewire-0.3-0t64 | libpipewire-0.3-0` and `pipewire`), which the
CLI does not add: without `client.conf` (in `pipewire-bin`, which
`pipewire` pulls in) every recording fails at its start. And it depends
on `libc6 (>= 2.39)`, the newest glibc the binaries need when built on
Ubuntu 24.04, so apt refuses an older system (Debian 12, Ubuntu 22.04)
instead of installing binaries that cannot start there.
`check-bundle.sh` checks all three. The icons in `icons/` come from
`cargo tauri icon` over the Swift app icon
(`apps/macos/Steno/Resources/Assets.xcassets/AppIcon.appiconset/icon_512x512@2x.png`),
the tray's template mark in `icons/tray/` is drawn by hand. The identifier
stays `uno.schmid.steno.desktop`, so the shell keeps its own preferences
and permissions beside the Swift app until the Mac cutover changes it to
`uno.schmid.steno.mac` (`.plans/2026-10-04-mac-cutover.md`, whose step 1
decides what becomes of these installs). Both apps are `Steno.app`,
though: dragged into `/Applications`, the desktop `.dmg` replaces the
Swift app, so install it elsewhere (`~/Applications`) to keep both.

Every release bundle carries `steno-speech-sidecar` beside the app (see
"The speech sidecar" under Release), so a `.deb`, AppImage, `.msi` or NSIS
install processes a meeting once the sidecar's models are downloaded (see
Run). A Mac bundle transcribes on `CoreML`, except in the two cases under
Run that send Parakeet to the sidecar, and diarizes in the sidecar.

Updates are signed: `plugins.updater.pubkey` is the public half of a key
pair from `cargo tauri signer generate`. The private half is never in the
repository; it is the secret `TAURI_SIGNING_PRIVATE_KEY` (see Release).
With `createUpdaterArtifacts` on, a bundle build without the key fails;
Rust CI and a local build without the key turn it off through the
configuration merge. The lane follows the installed version, as Sparkle's
channel does in the Swift app: a pre-release reads the beta manifest first,
a release the stable one only (`updater.rs`; the manifests are under
Release).

Besides Check for Updates in the tray and in Settings, the update
schedule (`steno_services::updates`) checks at launch and every hour once
the last check is a day old, as Sparkle's daily check did. A check that
has not answered after 60 seconds fails with "The update check timed
out." Settings' switches are the booleans `steno.updates.automaticChecks`
(on when missing) and `steno.updates.automaticDownload` (off when
missing) in `preferences.json`; the last check time is `lastCheckAt`, RFC
3339 UTC, in `update-check.json` beside it, written only after a check
that succeeded. To make the next launch check, set it back a day or
delete the file. A found update brings up the Install and Relaunch dialog
once per version in a run, and not while a recording starts, runs or
stops: the first hourly tick after the recording ends brings it up. A yes
given once a recording has started asks again before it installs
("Installing stops and saves the recording in progress."); Not Now, the
default button, leaves the update for the next idle tick. A yes downloads
the update first and then installs and relaunches. From just before the
install, Record in the sidebar or the tray is refused with "Steno is
installing an update. You can record again once it relaunches, or if you
cancel the install." On a `.deb` install the system asks for a password
first, a second time if the first prompt is cancelled; until it is
answered or cancelled, Record stays refused, and cancelling ends the
install with an error and frees Record. A recording started during the
download puts the install off: no restart, and the dialog comes back at
the first idle tick, whose yes installs without downloading again. The
app downloads and installs by itself only when its install gate says it
is idle, and no build has that gate until P25 of
`.plans/2026-10-07-stable-promotion.md` builds it; until then automatic
downloads stay off in effect, and every install is the user's, from the
dialog. With `STENO_DISTRIBUTION` set to `aur` or
`nix` the schedule does not run, and a check makes no request and says
the package manager delivers the updates.

To run a debug binary against the embedded bundle instead of the dev server
(what the smoke does), drop the dev URL through Tauri's own configuration
merge:

```sh
TAURI_CONFIG='{"build":{"devUrl":null}}' cargo build -p steno-desktop
```

On a bare checkout without a web build, `build.rs` points `frontendDist` at
`apps/desktop/src-tauri/placeholder/` so a debug `cargo build` still goes
through; that page is only what the window shows when the dev URL is
dropped as above or the feature is on. A plain debug build loads
`localhost:5173` either way.

Panic messages in a release binary would carry the build host's source
paths until Cargo's `trim-paths` stabilises. The release workflow passes
`--remap-path-prefix` to the sidecar's and the app's builds (through
`CARGO_ENCODED_RUSTFLAGS`, so a path with a space stays one flag): the
dependency sources in the cargo home read `cargo/…`. Cargo already gives
the workspace's own sources relative paths; any absolute one reads
`steno/…`.

### The Nix package and the NixOS module

`flake.nix` builds the Linux app from the tree it is in
(`packages.x86_64-linux.steno`, in `nix/package.nix`), so every tag from
the first one with it builds as it is:

```sh
nix build github:NicolaiSchmid/steno/<tag>#steno
```

The derivation does what the release does for the `.deb`: it builds the web
UI, stages the sidecar with `stage-sidecar.sh`, runs `cargo tauri build
--config tauri.release.conf.json` without updater artifacts, and installs
the `.deb`'s tree. The differences:

- ONNX Runtime is nixpkgs' `onnxruntime`, linked dynamically
  (`ORT_LIB_LOCATION`, `ORT_PREFER_DYNAMIC_LINK=1`), in place of the build
  `ort` downloads (pyke's, 1.28.0), which the sandbox forbids.
- The tray's `dlopen` of `libayatana-appindicator3.so.1` names the library's
  store path (`postPatch`).
- Only `steno-desktop` gets the GTK wrapper (`wrapGAppsHook3`, which the
  file chooser's schemas need). The wrapper sets `STENO_DISTRIBUTION=nix`
  unless the environment has it, and execs `bin/.steno-desktop-wrapped`;
  the sidecar beside it stays the plain binary.
- `STENO_DISTRIBUTION=nix` is also in the build environment, the default
  the app is built with (see Packaged installs).
- The `.deb`'s systemd user files (the stop timeout drop-ins, P5) end up
  in `share/systemd/user/`, where stdenv moves them, with
  `lib/systemd/user` a link to it. The module links them into the user
  units; without the module, the app's own copies in
  `~/.config/systemd/user/` cover a profile install.

The crates' hashes come from `Cargo.lock`. The web UI's dependencies are
one fixed-output hash, `pnpmDeps.hash` in `nix/package.nix`: a change to
`apps/macos/web/pnpm-lock.yaml` needs a new one. Build, and copy the `got:`
hash from the mismatch.

`nixosModules.default` adds `programs.steno` (`nix/module.nix`):

```nix
# The system's flake.nix: an input at a tag,
inputs.steno.url = "github:NicolaiSchmid/steno/<tag>";
# and in nixpkgs.lib.nixosSystem's modules:
steno.nixosModules.default
{ programs.steno.enable = true; }
```

It installs the package system-wide, or for the users in
`programs.steno.users` only, and starts Steno with the graphical session as
the user service `steno.service` (`TimeoutStopSec=20s` and
`STENO_LOGIN_ITEM=managed`, which the app reads as Packaged installs says;
`programs.steno.launchAtLogin = false` turns it off). The service runs the
profile path, `/run/current-system/sw/bin/steno-desktop` or
`/etc/profiles/per-user/%u/bin/steno-desktop`, never a store path.

The session's `STENO_LOGIN_ITEM=managed` applies to every user, listed in
`programs.steno.users` or not, so list every user who runs Steno: for one
left out, a Steno from their own `nix profile` starts at no login and
removes the autostart entry it wrote before. The variable reaches a
session only from its next login (with lingering, from the next boot):
after the switch that turns the module on, log out and in, or reboot,
before starting Steno. A Steno started before then writes its own
autostart entry, which goes once the variable is in place.

A rebuild never restarts or stops `steno.service`, whatever changed
(`X-RestartIfChanged=false`, `X-StopOnRemoval=false`). The new version
starts at the next login, or at the next launch after quitting. Turning the
module or `launchAtLogin` off leaves a running Steno until it quits or the
session ends. A rebuild that changes PipeWire's units, as most nixpkgs bumps
do, restarts PipeWire. A recording in progress then reconnects with a second
or two of silence; if PipeWire is not back within about 2 s, the recording
ends and what was recorded is saved.

The module also turns on PipeWire and, as the Secret Service, GNOME Keyring,
both with `mkDefault`. The app keeps its secrets there, or in a 0600 file
where no Secret Service answers. The keyring stays off where another Secret
Service runs (Plasma's KWallet, `services.passSecretService`) and beside
another SSH agent, because the keyring brings gcr's: nixpkgs refuses it next
to `programs.ssh.startAgent`, and next to
`programs.gnupg.agent.enableSSHSupport` gcr's socket takes the session's
`SSH_AUTH_SOCK`, so SSH through gpg-agent stops working.
`services.gnome.gnome-keyring.enable = false` turns it off; the module only
sets a default. If one of these comes on after Steno has moved its secrets
into GNOME Keyring, the keyring goes off and Steno can no longer reach the
API key or this computer's phone pairing; they stay in
`~/.local/share/keyrings`. Beside an SSH agent,
`services.gnome.gnome-keyring.enable = true` with
`services.gnome.gcr-ssh-agent.enable = false` keeps the keyring. The
module links the package's systemd user files, and raises logind's
`InhibitDelayMaxSec` when `programs.steno.inhibitDelayMaxSec` is set. It
does not open the handover's port in the firewall yet.

The package is built with the flake's own pinned nixpkgs, so the system
carries a second GTK and WebKit closure. `inputs.steno.inputs.nixpkgs.follows
= "nixpkgs"` builds it with the system's nixpkgs instead, which CI does not
test.

`nix flake check` builds the package and checks its layout (the wrapper,
its GTK schemas and its `STENO_DISTRIBUTION=nix`, the sidecar beside the
real binary, no missing library, the tray's library, the `.deb`'s systemd
user files), that the build itself sets
`STENO_DISTRIBUTION=nix`, and that the release pins the same Tauri CLI. It
evaluates the module in a system-wide and a per-user system down to the
user units and the session variable, in one with `launchAtLogin = false`,
and in one each with `programs.ssh.startAgent` and with GnuPG's SSH
support.

## Release

`.github/workflows/desktop-release.yml` builds the six bundles on the three
platforms. A pushed `desktop-v<version>` tag builds all of them and
publishes; the version must be the one under `[workspace.package]` in
`Cargo.toml`, which Tauri stamps into the bundles, or the run fails before
it builds. So does a version the MSI cannot carry: WiX takes numbers only,
so `scripts/wix-version.sh` accepts `X.Y.Z` and `X.Y.Z-<label>.<N>` alone.
A manual run builds, signs and notarises the platforms it is given and
keeps the bundles as workflow artifacts. It checksums and OpenPGP-signs
them as a tag would, but keeps none of the `.asc` files, only
`SHA256SUMS` and the log of what verified (see Checksums and OpenPGP
signatures). It publishes nothing.
The `desktop-v` prefix keeps these tags apart from the Swift
app's `v*` (`release.yml`) and the mobile build tags `ios-fp-*`
(`mobile-cd.yml`).
`cargo deny check` (`deny.toml`: the licence allow list, the MPL-2.0
crates by name, advisories, sources) runs first and stops the run on any
finding.

### Cutting a release

0. Before the first release, and after a change to the workflow: the
   secrets in the table below are set, and a manual run on the branch
   passes:
   `gh workflow run desktop-release.yml --ref <branch> -f platforms=linux,windows,macos`.
   Its `desktop-release-checksums` artifact proves the checksums and
   signatures (see Checksums and OpenPGP signatures), and the run's
   summary holds the notes.
1. On `main`, set `[workspace.package] version` in `Cargo.toml`, run
   `cargo check` so `Cargo.lock` follows (CI builds with `--locked`), and
   merge both. A hyphen (`0.2.0-rc.1`) means the beta lane only.
2. Tag the merge commit:
   `git tag desktop-v<version> <merge commit> && git push origin desktop-v<version>`.
   Push one tag at a time and wait for its publish: GitHub keeps one
   waiting job per concurrency group, so a third tag cancels the second's
   waiting publish. Until the macOS job is done, start no Swift release
   and no other desktop run with macOS (see Signing).
3. Watch the Desktop release run. `publish` runs only when all three
   platforms bundled.
4. Check what the lanes serve:
   `curl -fsSL https://github.com/NicolaiSchmid/steno/releases/download/desktop-beta/latest.json | jq .version`
   (and `desktop-stable` for a release).

### When a run fails

Re-run only the failed jobs (`gh run rerun <run id> --failed`), never all
jobs: a full re-run rebuilds and replaces the release's assets while the
lanes still serve the old `latest.json`, whose signatures do not match the
new assets until **Update lanes** finishes.

- **plan**: the tag does not name the workspace version, or the MSI
  cannot carry the version. Delete the tag
  (`git push origin :refs/tags/desktop-v<version>` and
  `git tag -d desktop-v<version>`), fix the version on `main`, tag again.
- **Check secrets**: add the secret it names (see the table below).
- **Signing keychain and notarisation key**: the `.p12` or its password is
  wrong, or the certificate is not a Developer ID Application one; the
  `::error::` or `security`'s message says which.
- **cargo deny**: update the dependency, or name the crate with its reason
  in `deny.toml`.
- **Build**: a compile error Rust CI would show too; fix it on `main`,
  then delete the tag as above and tag again.
- **Notarise the disk image**: notarytool's log for the submission is in
  the step output. A notarisation failure in **Bundle** is the bundler's
  error in that step's output.
- **Check the bundles**: the `::error::` names the file or the check that
  failed.
- **Gather the assets** (in `assets`): an artifact holds a file its
  platform does not build or whose name has a character other than
  A-Z a-z 0-9 . _ + -, two files share a name, or a platform's artifact is
  missing; the `::error::` names it
  (`scripts/release-assets.sh`). Fix the bundle job, delete the tag and
  tag again.
- **Verify the updater signatures** (in `assets`):
  `TAURI_SIGNING_PRIVATE_KEY` is not the secret half of
  `plugins.updater.pubkey`, or a platform lacks a `.sig`; fix the secret,
  delete the tag and tag again (re-running only `assets` reuses the same
  bundles).
- **Checksums and signatures** (in `assets`): `LINUX_GPG_PRIVATE_KEY` is
  not the secret half of `release-signing-key.asc`, `LINUX_GPG_PASSPHRASE`
  is not its passphrase, or the key has expired; the `::error::` says
  which file did not sign or verify, or when the key expired. Fix the
  secret and re-run the failed jobs. An expired key needs a new commit,
  since a re-run checks out the tag's public key: extend the key (see
  Checksums and OpenPGP signatures), merge the new public key, then delete
  the tag and tag the new commit.
- **A job that timed out or lost its runner**: `notarize-dmg.sh` gives up
  after 45 minutes, but the bundler's own notarisation of the `.app` waits
  until the job's 90-minute timeout. Check `xcrun notarytool history` with
  the App Store Connect key, then re-run the failed jobs.
- **publish**, also one cancelled while it waited (a third tag): re-run it.
  It uploads what `assets` left in the run's `desktop-release-assets`
  artifact, which is kept 5 days; after that, re-run the `assets` job
  instead, which signs the same bundles again and runs publish after it:

  ```sh
  gh run view <run id> --json jobs --jq '.jobs[] | select(.name == "assets") | .databaseId'
  gh run rerun --job <that job id>
  ```

  An existing release is reused and its assets replaced; the lanes move as
  on the first run. GitHub allows re-runs for 30 days; after that, delete
  the tag and tag again.

### A bad release

Installed apps only take a higher version, so no lane can take a user
back. To stop the spread, put the last good version each lane served back
on it by hand. On `desktop-beta` that is the version before the bad one:

```sh
gh release download desktop-v<last good> -p latest.json --clobber
gh release upload desktop-beta latest.json --clobber
```

A bad release (no hyphen) moved `desktop-stable` too. That lane gets the
previous *release*'s manifest, never an rc's, which would offer the rc to
every stable user: the same two commands with `desktop-v<previous
release>` and `desktop-stable`. Never re-run the bad tag's publish: its
**Update lanes** moves the lanes back to it. Then fix forward with a higher
version, whose tag moves the lanes as usual.

### Secrets

| Secret | Used by | What it is |
|---|---|---|
| `TAURI_SIGNING_PRIVATE_KEY` | every platform's Bundle | The private half of `plugins.updater.pubkey`, empty password; signs every updater artifact |
| `MACOS_CERTIFICATE_P12_BASE64` | macOS signing keychain | The Developer ID Application certificate and key as a base64 `.p12` (shared with `release.yml`) |
| `MACOS_CERTIFICATE_PASSWORD` | macOS signing keychain | The `.p12`'s password |
| `ASC_KEY_ID` | macOS Bundle, `notarize-dmg.sh` | The App Store Connect API key's id |
| `ASC_ISSUER_ID` | macOS Bundle, `notarize-dmg.sh` | The key's issuer id |
| `ASC_PRIVATE_KEY` | macOS Bundle, `notarize-dmg.sh` | The key itself, the `.p8` contents |
| `LINUX_GPG_PRIVATE_KEY` | `plan`'s Check secrets, `assets`' Checksums and signatures | The armored OpenPGP secret key of `release-signing-key.asc`, passphrase-protected |
| `LINUX_GPG_PASSPHRASE` | `plan`'s Check secrets, `assets`' Checksums and signatures | Its passphrase |

`scripts/require-secrets.sh` names every missing one before anything is
built: the `plan` job checks the two OpenPGP secrets, each bundle job its
own.

Installed apps verify updates only with the `pubkey` they were built with.
To rotate the updater key, publish one release (no hyphen, and at or
above what both lanes serve, so both move to it) whose `tauri.conf.json`
carries the new public key, signed with the old private key. The `assets`
job checks the signatures against the config's key, so on that release's
commit the `pubkey=` line of **Verify the updater signatures**
(`desktop-release.yml`) is set to the old key,
`pubkey="$(base64 --decode <<< '<the old plugins.updater.pubkey value>')"`,
and the next commit reverts it. After that release, replace
`TAURI_SIGNING_PRIVATE_KEY`. An app that never installed that release
needs a manual install, and so does every app if the key is lost.

### The speech sidecar

Off the Mac, and on the Mac when the ONNX fallback is on,
`steno-speech-sidecar` runs the speech model in a child process; on every
platform it also runs the diarizer, so a Mac bundle needs it too, or every
meeting gets the fallback speakers. `SidecarConfig::beside_current_exe`
looks for it in the directory of the running binary. `tauri.release.conf.json` declares it as an `externalBin`;
`scripts/stage-sidecar.sh` builds it in release and copies it to
`src-tauri/binaries/steno-speech-sidecar-<target triple>` (ignored by
git), where the bundler finds it and installs it without the triple. It is
not in `tauri.conf.json`, because `tauri-build` then requires the file for
every build, debug and test included. Where it lands:

| Bundle | Installed at | The sidecar |
|---|---|---|
| `.app` (in the `.dmg`) | `Steno.app/Contents/MacOS/steno-desktop` | `Steno.app/Contents/MacOS/steno-speech-sidecar` |
| `.deb` | `/usr/bin/steno-desktop` | `/usr/bin/steno-speech-sidecar` |
| `.AppImage` | `usr/bin/steno-desktop` in the mounted image | `usr/bin/steno-speech-sidecar` beside it |
| `.msi` | `Steno\steno-desktop.exe` under Program Files | `steno-speech-sidecar.exe` beside it |
| NSIS `-setup.exe` | `Steno\steno-desktop.exe` under the user's `AppData\Local` | `steno-speech-sidecar.exe` beside it |

`scripts/check-bundle.sh` proves the table for each build: it unpacks each
bundle as its installer would (`dpkg-deb -x`, `--appimage-extract`, an
administrative MSI install, a silent NSIS install), finds the two binaries
side by side and starts the sidecar from there, which greets and exits
when its stdin ends.

ONNX Runtime is linked statically, so on macOS and Linux the sidecar needs
no library beside it. On Windows both binaries load `DirectML.dll`, which
the MSI picks up from the build directory on its own and the NSIS
installer does not (an NSIS install would then load the older copy in
`System32`): `stage-sidecar.sh` stages it and
`tauri.release.windows.conf.json` installs it beside the app for both.
`bundle.windows.bundleVCRuntime` puts the Visual C++ runtime
(`msvcp140.dll` and the rest) there too, so neither installer depends on
a redistributable the machine may not have.

### Signing

The release binary is built first with no secret in the environment
(`tauri build --no-bundle`), so no dependency's build script sees one;
`tauri bundle` then signs and packages it with the keys. On macOS the job
imports the Developer ID certificate into a throwaway keychain
(`scripts/signing-keychain.sh`, the Swift release's approach) and hands
its identity to the bundler; at the end it takes only that keychain off
the search list. Run only one signing job on the self-hosted Mac at a
time (no concurrency group spans the two workflows): two throwaway
keychains would hold the same Developer ID identity, and a `codesign` by
name (the Swift app's `make-dmg.sh` and Xcode export) then fails as
ambiguous. The bundler signs the sidecar, the app binary and the bundle
under the hardened runtime with `Entitlements.plist` (one file for every
item, so the sidecar carries the two entitlements without using them),
then notarises and staples the `.app` with the App Store Connect key
before it builds the image and the updater archive from it, and
`scripts/notarize-dmg.sh` notarises and staples the image.
`check-bundle.sh --signed` checks the Developer ID authority, the runtime
flag, the timestamp and the team on all three items, the entitlements,
the ticket and Gatekeeper's verdict. The Linux bundles have no signature
of their own format (no signed apt repository, no AppImage-embedded
signature); a detached OpenPGP signature beside each covers them instead
(below). Windows installers are not code-signed yet, since there is no
Windows certificate: SmartScreen warns before the first install, which the
release notes say, and updates install without asking again.

### Checksums and OpenPGP signatures

Every release also carries `SHA256SUMS`, the SHA-256 of every other file
in it but the OpenPGP signatures (`.asc`): all three platforms, the
updater `.sig` files and `latest.json`. `SHA256SUMS` and each Linux `.deb`
and `.AppImage` have a detached armored OpenPGP signature (`<file>.asc`).
The key is the
Steno release signing key, an ed25519 key whose public half is
[`apps/desktop/release-signing-key.asc`](release-signing-key.asc):

```text
Steno release signing (github.com/NicolaiSchmid/steno)
048B 5279 50E4 F609 B90E  6349 5F88 10A6 E6D4 DB46
expires 2029-10-04
```

To check a download, put `SHA256SUMS`, `SHA256SUMS.asc` and the files in
one directory and run these commands there. The release notes give the
`gpg --verify` line for each Linux bundle, and `wget` fetches the key
where `curl` is missing:

```sh
curl -fsSLO https://raw.githubusercontent.com/NicolaiSchmid/steno/desktop-v<version>/apps/desktop/release-signing-key.asc
gpg --import release-signing-key.asc
gpg --verify SHA256SUMS.asc SHA256SUMS
sha256sum --check --ignore-missing SHA256SUMS
gpg --verify steno-desktop_<version>_amd64.deb.asc steno-desktop_<version>_amd64.deb
```

Each `gpg --verify` must report "Good signature" from the fingerprint
above, and `sha256sum` must print OK for every file you downloaded; the
warning that the key is not certified by a trusted signature is expected.
On Windows, in PowerShell,
`(Get-FileHash .\<installer>).Hash -eq '<its hash in SHA256SUMS>'` must
print `True`.

Only the `assets` job holds the secret key (`LINUX_GPG_PRIVATE_KEY`,
`LINUX_GPG_PASSPHRASE`): `plan` learns only whether the two secrets are
set, and the bundle jobs and `publish` never see them.
`scripts/release-assets.sh` first gathers each platform's files from its
own artifact only, so no other bundle job can put a `.deb` or `.AppImage`
up for signing. `scripts/release-signatures.sh` imports the key into a
throwaway `GNUPGHOME` under `$RUNNER_TEMP`, passes the passphrase through
a file there (loopback pinentry), signs with the committed key's
fingerprint only, and removes the directory and its agent on exit. It then
verifies every signature with `gpgv` against a keyring holding only the
committed public key; a signature that does not verify fails the run
before anything is published. An expired key fails the run before
anything is signed, and the script warns 90 days before.
`scripts/release-notes.sh` writes the release notes with the fingerprint
and the commands above.

On a tag the signed set is the `desktop-release-assets` artifact (kept 5
days) that `publish` uploads. A manual run signs and verifies the same
way, but its `desktop-release-checksums` artifact (kept 3 days) holds only
`SHA256SUMS` and `signatures.txt`, the script's log of what it signed and
verified: a signature on a build that is never published stays on the
runner.

Before the key expires, extend it where the secret key is kept, then
commit the new public key and the new date in the block above, and
replace the secret:

```sh
gpg --quick-set-expire 048B527950E4F609B90E63495F8810A6E6D4DB46 3y
gpg --armor --export 048B527950E4F609B90E63495F8810A6E6D4DB46 > apps/desktop/release-signing-key.asc
gpg --armor --export-secret-keys 048B527950E4F609B90E63495F8810A6E6D4DB46 | gh secret set LINUX_GPG_PRIVATE_KEY
```

The fingerprint stays, so old releases still verify. To replace the key
(lost or compromised), generate a new one
(`gpg --quick-generate-key 'Steno release signing (github.com/NicolaiSchmid/steno)' ed25519 sign 3y`),
commit its public half and the new fingerprint here and in
`scripts/release-notes.test.sh`, replace both secrets, and say in the
next release's notes that the key changed; for a compromised key, also
publish its revocation certificate. Releases signed with the old key keep
verifying against its public half in the history at their tag.

### Publishing, on a tag

One GitHub release per tag carries every bundle, the `.sig` of each
updater artifact (`.app.tar.gz`, `.AppImage`, `.deb`, `.msi`,
`-setup.exe`), `latest.json` (`scripts/updater-manifest.sh`),
`SHA256SUMS` and the `.asc` signatures. The `assets` job assembles them on
every run, a manual one included: it gathers each platform's artifact
(`scripts/release-assets.sh`) and checks each `.sig` against
`plugins.updater.pubkey`, so a signing key that is not the config key's
other half fails the release instead of every user's next update. It
then writes the manifest, the checksums and the OpenPGP signatures (see
above). `publish` uploads that set with the notes from
`scripts/release-notes.sh`. The manifest then goes to the rolling release of
each lane, `desktop-stable` and `desktop-beta`, and the lane's tag moves to
the release commit. Each lane only moves forward: the stable lane takes a
release (no hyphen), the beta lane every version, each only when the
version is at or above the one the lane serves (`scripts/updater-lanes.sh`,
SemVer precedence). A rerun of a tag moves the same lanes again; an older
tag or a hotfix on an older line leaves a lane where it is. Every desktop
release is a GitHub pre-release and never "latest": until the Mac cutover
(`.plans/2026-10-04-mac-cutover.md`) the "latest release" that the
repository README, the site and the Homebrew cask point at is the Swift
app's.

## Test

`cargo test -p steno-desktop` covers the window specs and routes, the typed `window.open`
and `window.close` params and who may close what, the URL and navigation
policies, the deep-link snapshots and the smoke's switches and verdicts,
and every WP8 module's rules: the tray's ids, labels and tooltip per
recorder state, the panels' geometry (the anchor's default, drag, screen
loss and JSON, the probe before measuring, which size reports are
accepted and how they are clamped), the one content rule, the prompt
query and its numbering, the window requests a page is owed before it
mounts, when the main window hides on close and when the process ends,
the login item states, the update lanes, the permission panes per OS,
the `steno:` link grammar and its case rules, the Linux desktop entry,
the folder choosers' replies and the host's chosen folder, the alert's
buttons, and the exit rules: an exit request runs the shutdown once and
exits after it, a second Quit meanwhile is held, a close behind a tray
runs nothing, which repeated signal forces the exit, an ignored signal
reads as ignored; the window sink delivers on the main thread, in emit
order, without the emit waiting. `cargo test -p steno-desktop
--features fixture-host` runs the same with the fixture host, plus the
fixture table against `index.json` and the mock transport; Rust CI runs
both. In the web
app, `tauri-transport.test.ts` covers the page's half of the wire and
`src/windows/panels/*.test.tsx` the two panels.

## Smoke

`STENO_SMOKE_SECONDS=<n> steno-desktop` opens all three windows side by
side and both floating panels under Settings (the prompt naming a made-up
app), asks main for a meeting before its page has mounted (as a cold
launch's deep link does), waits `n` seconds, then checks the panels,
raises a second prompt, hides the panels, closes main and reports:

| Exit | When |
|---|---|
| 0 | The main window sent `page.ready`, at least one snapshot reached it, the meeting reached it after its `page.ready`, the tray was built, both panels were visible at the size their page reported and kept it when asked for 40 points more, the prompt's window took the second prompt, both panels hid, and closing main hid it and kept it |
| 1 | No `page.ready` from main; or `page.ready` but no snapshot: no bridge host answered; or the meeting was lost or published before the page listened; or no tray; or a panel or main that did not do as above; or, on Linux in a run as the autostart unit, Launch at login turned off that changed the entry at once or set no mark, or, while the system manages the login item, changed the login item at all |
| 2 | At once, when `n` is not a positive number |

`apps/desktop/scripts/smoke-linux.sh [binary] [seconds]` runs that under
`xvfb-run` and, when ImageMagick is present (`magick` or `convert`), captures
the Xvfb root and one crop per window and per panel into
`apps/desktop/screens/` (ignored by git; CI uploads it as the
`desktop-smoke-screens` artifact). With `STENO_SMOKE_DPI=120` it runs Xvfb
at that resolution, where WebKitGTK's pixel ratio is 1.25 and the panels
must still fit their pills. The windows carry what the host's database
holds: nothing on CI's fresh runner, synthetic data with `--features
fixture-host`. Xvfb has no compositor, so the panels' transparent
corners render black there; a desktop shows them rounded. Xvfb has no
tray host either; a smoke run stands in for one, so the built tray counts
and the run checks the close rule a desktop with a tray gets.

After the run the script checks the stop timeout drop-ins, then runs the
smoke once more in the same `HOME`, which must write nothing and reload
only for a reload still owed. Every launch names the binary in
`STENO_EXEC_PATH`, resolved to an absolute path, since a build under
`target/` has no path that outlives an upgrade. Then it runs the smoke
as the autostart unit, in a throwaway `HOME` and a cgroup named after
the unit, below a delegated `systemd-run --user` scope. The first run,
with `~/.config/autostart` unwritable, must fail to restore the entry,
ask for no reload and leave it owed. A launch outside the unit then
counts the first launch and writes the entry, which must name the binary
whole; the script removes it and its drop-in. The next run must restore
the entry, marked, reload, keep it through Launch at login turned on
again, and remove it and its drop-in after the shutdown's line. The last
ends as an update's relaunch (`STENO_SMOKE_RELAUNCH=1`, which ends
through the relaunch's shutdown without restarting) and must keep the
entry, the mark and the drop-in. Then three runs while the system
manages the login item, each with an earlier build's store entry: the
first must remove it at launch and leave the autostart unit's drop-in
alone; the second, as the unit, must keep the entry while it runs, give
the unit its drop-in with a reload, and remove both after the shutdown's
line; the third, as the unit, ends as a relaunch and must keep both.
Without a user manager that starts the scope, or as a root that can
write to the unwritable directory, it skips the runs as the unit, unless
`STENO_REQUIRE_UNIT_SMOKE` is set, as in CI. Its last launch is over a
database it cannot open, which must refuse (exit 3) and leave an entry
marked to go in place.

`scripts/pipewire-headless.sh apps/desktop/scripts/lost-display-linux.sh
[binary] [seconds]` checks end to end that a recording is saved when the
display goes away: it starts the shell on an Xvfb server of its own with
a fresh `HOME`, starts a recording with the record shortcut (xdotool),
ends the server after `seconds` (4 by default) and fails unless the app
logged its save and the store holds the meeting `queued` with a
duration. CI's Linux job runs it after the smoke; outside CI it needs
`Xvfb`, `xdotool` and `python3` on the `PATH` besides the smoke's setup.

`apps/desktop/scripts/smoke-macos.sh [binary] [seconds]` runs the smoke on
a Mac, in the logged-in session (the windows show on its screen for those
seconds) and with a fresh `HOME`; CI's macOS job runs it. A panel there is
not resizable at all, so the run checks that instead of asking for 40
points more: AppKit's minimum and maximum hold only against the user's
resizing, and a size set from code goes through.

## Prerequisites

### Debian and Ubuntu

`scripts/setup-linux.sh` installs the packages (the list is in the script);
CI runs it on `ubuntu-latest`.

### NixOS

No system packages; build and run inside a shell with the libraries:

```sh
nix-shell -p pkg-config webkitgtk_4_1 gtk3 libsoup_3 openssl libayatana-appindicator \
  librsvg glib cairo pango gdk-pixbuf atk --run 'cargo build -p steno-desktop'
```

For the headless smoke add `xvfb-run imagemagick mesa libglvnd` to that list
and point WebKitGTK at Mesa's software EGL, which a server without
`/run/opengl-driver` does not expose:

```sh
MESA=$(nix-build '<nixpkgs>' -A mesa --no-out-link)
INDICATOR=$(nix-build '<nixpkgs>' -A libayatana-appindicator --no-out-link)
export __EGL_VENDOR_LIBRARY_DIRS=$MESA/share/glvnd/egl_vendor.d
export LD_LIBRARY_PATH=$MESA/lib:$INDICATOR/lib
apps/desktop/scripts/smoke-linux.sh target/debug/steno-desktop 12
```

The tray loads `libayatana-appindicator3.so.1` at run time (the crate
`dlopen`s it), hence the second path; without it the shell logs that the
tray library is missing and runs without a tray, and the smoke fails on
the tray check.

### Windows and macOS

Nothing beyond the Rust toolchain. WebView2 ships with Windows 11; the
`.ico` the Windows resource needs is in `apps/desktop/src-tauri/icons/`.

## Layout

| Path | What lives there |
|---|---|
| `apps/desktop/src-tauri/Cargo.toml` | Crate `steno-desktop`, binary `steno-desktop`; features `fixture-host` (opt-in) and `custom-protocol` (embeds the bundle, see Build). Dependency versions come from the workspace table in `Cargo.toml` |
| `apps/desktop/src-tauri/tauri.conf.json` | `frontendDist` is the web app's `dist/`; no `version`, so Tauri takes the crate's; `beforeDevCommand` and `beforeBuildCommand` run `pnpm dev` and `pnpm build` with `cwd` `../../macos/web`: the CLI runs them from `src-tauri`, the directory holding this file, which is also where `frontendDist` (`../../macos/web/dist`) resolves from; `csp` lets the page load only its own scripts, styles, fonts and images, and `connect-src` only the IPC origins (`ipc:`, `http://ipc.localhost`), so nothing the page does reaches the network; `plugins` carries the `steno` scheme and the updater's public key and stable endpoint; `bundle` the six installer targets (see Bundles); no windows are declared, `windows.rs` and `panels.rs` create them |
| `apps/desktop/src-tauri/tauri.linux.conf.json`, `linux/steno-desktop.desktop`, `linux/*-stop-timeout.conf` | Merged on Linux: the `steno-desktop` product name for the package, and the desktop entry template (see Bundles); the stop timeout drop-ins for the autostart unit and GNOME's scope (see Launch at login under systemd) |
| `apps/desktop/src-tauri/Info.plist`, `Entitlements.plist` | Merged into the macOS bundle: the TCC purpose strings and the Bonjour service, verbatim from `apps/macos/project.yml`; the audio-input and calendars entitlements |
| `apps/desktop/src-tauri/build.rs`, `apps/desktop/src-tauri/placeholder/` | Points `frontendDist` at the placeholder page when the web `dist/` is missing, so a debug `cargo build` works on a bare checkout (a release build fails instead); then `tauri_build::build()` |
| `apps/desktop/src-tauri/src/main.rs` | Wires the plugins (single instance first, autostart, deep link, dialog, opener, updater, `tauri-nspanel` on macOS), the managed state, the one menu handler, the tray and the windows; hides the main window on close and keeps the process while a tray stands, ends it otherwise; every exit through the shutdown (`exit_request`, `RunEvent::Exit`, SIGTERM, SIGINT, SIGHUP); a dragged panel's anchor, a destroyed window's page, and the Dock's reopen. `display.rs`, on Linux: the GDK backend (XWayland on a Wayland session). `session_end.rs`, on Linux: the GNOME and Xfce session client, the portal's session monitor and logout inhibitor, and logind's shutdown lock. `display_lost.rs`, on Linux: the log writer that saves before GDK ends the process for a lost display. `stop_timeout.rs`, on Linux: the systemd drop-ins for the stop timeout |
| `apps/desktop/src-tauri/src/tray.rs`, `menu.rs`, `actions.rs`, `recording.rs` | The tray menu and icon, the macOS menu bar, the actions behind their items and their words per platform, the recorder state the shell follows |
| `apps/desktop/src-tauri/src/platform.rs` | The initialization script that tells every page its platform |
| `apps/desktop/src-tauri/src/panels.rs`, `panel_geometry.rs` | The two floating panels and the one content rule, the macOS `NSPanel` conversion; the anchor, frames and size validation as plain values |
| `apps/desktop/src-tauri/src/autostart.rs`, `updater.rs`, `permissions.rs`, `deep_links.rs`, `dialogs.rs` | One module per service (see What the shell owns); each is plain rules the tests cover over a plugin or OS call |
| `apps/desktop/src-tauri/src/packaged.rs`, `packaged/linux.rs` | The Linux autostart entry's stable path, and the login item a package leaves to the system (see Packaged installs) |
| `apps/desktop/src-tauri/src/windows.rs` | The three windows with the Swift sizes: main 1120 by 720 (minimum 960 by 600) at `#/main`, Settings 960 by 640 (minimum 760 by 520) at `#/settings`, onboarding fixed 560 by 620 at `#/onboarding`. Main opens at start; the others on `window.open`, focused when already open. On Linux a closed Settings or onboarding window is kept, without its page, and loads afresh when opened again (`Kept`, against the fd leak of a destroyed webview). New windows from the page are denied. `Pages` holds the requests a window is owed until its page mounts |
| `apps/desktop/src-tauri/src/bridge.rs` | `bridge_call(method, params)` and the `steno:event` emitter, scoped to the calling window; a finished `onboarding` snapshot closes the onboarding window, a `recording` snapshot to main moves the tray and the panels. `window.open` (typed: one of the six sections, a UUID meeting id), `window.close` (the onboarding window, from itself), `system.openURL` (`https:` and `mailto:` only) and the shell's own methods listed above are the shell's; everything else goes to the host. `panel_call(action, params)` is the panels' own command |
| `apps/desktop/src-tauri/src/host.rs` | The real host: `steno_host::Host` over `steno_services::build`, the window sink that delivers on the main thread, the `Opener`, the alert and the chosen folder, the exits' `shutdown_action` |
| `apps/desktop/src-tauri/src/fixtures.rs` (and `host.rs` under the feature) | With `--features fixture-host`, the fixture host: the fixtures `index.json` lists, embedded with `include_str!`; every topic's snapshot on `page.ready`; replies as `mock-transport.ts` gives them (`speakers.options.reply`, `reply.confirm` and `reply.chosenPath` for the alerts and folder panels, `null` otherwise); a deep link as the `app` snapshot with the request set, then the clean one |
| `apps/desktop/src-tauri/src/navigation.rs` | Navigation policy: the app origin and, in a dev build, the Vite dev server; everything else is cancelled |
| `apps/desktop/src-tauri/src/smoke.rs`, `apps/desktop/scripts/smoke-linux.sh`, `smoke-macos.sh` | The smoke CI runs under Xvfb on Linux and in the runner's session on macOS |
| `apps/desktop/scripts/lost-display-linux.sh` | Ends the display server under a recording and fails unless the app saved it first; CI's Linux job runs it |
| `apps/desktop/src-tauri/capabilities/default.json`, `panels.json` | `core:event:allow-listen` and `allow-unlisten` for the three windows, the one core IPC the page uses; the panels get the same plus `core:window:allow-start-dragging` for `data-tauri-drag-region`; `bridge_call` and `panel_call` are app commands and native capabilities are reached through them |
| `.github/workflows/desktop-release.yml`, `apps/desktop/scripts/release-matrix.sh` | The six bundles on the three platforms, signed and notarised on macOS, checksummed and, for Linux, OpenPGP-signed in the `assets` job, published with the updater manifests on a `desktop-v*` tag (see Release); the `platforms` input of a manual run is filtered by `release-matrix.sh` (tested in Rust CI by `release-matrix.test.sh`) |
| `apps/desktop/src-tauri/tauri.release.conf.json`, `tauri.release.windows.conf.json`, `apps/desktop/scripts/stage-sidecar.sh`, `check-bundle.sh` | The sidecar as an `externalBin`, its staging, and the check that every bundle installs it beside the app (see Release); Rust CI bundles a `.deb` and runs the check |
| `apps/desktop/scripts/signing-keychain.sh`, `notarize-dmg.sh`, `require-secrets.sh`, `wix-version.sh`, `updater-manifest.sh`, `updater-lanes.sh`, `release-assets.sh`, `release-signatures.sh`, `release-notes.sh` | The release job's macOS keychain, the image's notarisation, the secrets guard, the MSI version, `latest.json`, the lanes a release moves, the assets gathered from each platform's artifact, `SHA256SUMS` and the OpenPGP signatures, and the release notes (each `.sh` with a `.test.sh` is tested in Rust CI) |
| `apps/desktop/release-signing-key.asc` | The public half of the release signing key that signs `SHA256SUMS` and the Linux bundles (see Checksums and OpenPGP signatures) |

## Not here yet

The Mac cutover (the bundle id, the Sparkle handoff, the Swift app's
removal) is planned in `.plans/2026-10-04-mac-cutover.md`; until it
lands the desktop app installs beside the Swift app on the Mac. WP6b
filled the host's half of the WP8 seams except four; S4 of
`.plans/2026-10-07-stable-promotion.md` later filled the updater and the
QR half of the fourth, which leaves the detection controller, the
permissions and the clip player (the plan's "Pipeline and services
(WP6b)" list gives each one's reason and what closes it): the detection
controller (WP5) is not ported, so nothing raises the prompt
(`panels::set_prompt`) and its X (`panels::dismiss_prompt`) tells no
one; the host's `Permissions` stay the services' fake (all granted),
because `permissions` answers `unknown` off the Mac and for the Mac's
system audio, which the host's onboarding opener counts as missing, so
onboarding would open at every launch until the audio probe (WP5) and a
rule for `unknown` land; and the clip player is a fake, which needs an
audio output and follows the stable release. The host may treat the main
window as always present: a close hides it, or ends the process when no
tray stands, so publishing to it never fails for want of a window.
Launch at login is a Launch Agent, not `SMAppService`; the cutover has
to retire the Swift registration so the user does not get two login
items (the plan's parity list, `.plans/2026-10-04-mac-cutover.md`). The
macOS menu bar has no Record menu yet (`⌘⇧R` and Record In Person are
the tray's and the sidebar's), and no Find Meetings (`⌘F`). On macOS the
system audio permission has no status API; the audio crate's probe (WP5)
records it and until then it reads `unknown`. The panels are re-tuned on
the Mac once they run there beside the Swift ones (the plan's risk
list). Linux and Windows keep their native title bar; macOS gets the
overlay title bar the Swift windows have, and only there does the page
leave the traffic lights their inset (`titleBarInset` in
`apps/macos/web/src/lib/platform.tsx`). On Linux every destroyed webview
leaks one shared-memory file descriptor
([#160](https://github.com/NicolaiSchmid/steno/issues/160)): wry's IPC
handler holds the webview it belongs to, a reference cycle (webview, its
user content manager, the handler, the webview), so the view is never
finalised and WebKitGTK never frees its memfd. The app works around it
rather than fixing it: no window is destroyed there while the app runs.
Main and the panels hide, and a closed Settings or onboarding window is
kept, drops its page (`about:blank`) and loads its route afresh, on the
section asked for, when opened again (`windows.rs`). Over 70 Settings
open and close cycles under Xvfb the shell's file descriptors stay at 69
to 72, where destroying the window took them from 61 to 128. The cycle
itself remains in wry and is not yet reported there.

On Linux a panel keeps a 5 px resize border that Tauri gives every
undecorated resizable window. The pinned size holds, but the border
shows a resize cursor and swallows a press, so a drag that starts on the
outer 5 px does not move the panel; no control sits there.
