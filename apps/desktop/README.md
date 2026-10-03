# Steno desktop shell

The Tauri 2 shell for macOS, Linux and Windows: three windows and two
floating panels around the web UI in `apps/macos/web`, a tray icon, and the
`bridge_call` command the web app's Tauri transport talks to. WP3 and WP8 of
`.plans/2026-10-02-rust-core-and-tauri-shell.md`. The shell holds no logic;
until WP6b wires the real host, the default `fixture-host` cargo feature
answers the bridge from the recorded fixtures in
`apps/macos/web/fixtures/bridge/`, so the whole UI runs on every platform
before any pipeline exists. Paths below are from the repository root.

## What the shell owns

Everything the Swift app does outside its three windows, per OS:

| | macOS | Linux | Windows |
|---|---|---|---|
| Tray (`tray.rs`) | Menu bar extra with a template icon | Status notifier item (libayatana-appindicator) | Notification area icon |
| Floating panels (`panels.rs`) | Non-activating `NSPanel`s on every space (`tauri-nspanel`) | Always-on-top undecorated windows, under XWayland on a Wayland session (see below) | Always-on-top undecorated windows |
| Launch at login (`autostart.rs`) | Launch Agent | `~/.config/autostart` entry | Run registry key |
| Updates (`updater.rs`) | signed manifest per lane | same | same |
| Permissions (`permissions.rs`) | microphone TCC status and prompt; system audio and calendar deferred to the host's probes | unknown (nothing to query before capture; the portal asks when the stream opens) | unknown (the privacy switch decides at capture time) |
| Deep links (`deep_links.rs`) | `steno:` through `CFBundleURLTypes`, written into the bundle by the deep-link plugin from `plugins.deep-link` | the `.deb`'s desktop entry from `linux/` (`Exec=… %u`, the `x-scheme-handler/steno` MIME type); an AppImage and a debug build register at start | registry (debug builds register at start) |
| Single instance | Unix socket | session bus name (skipped without a session bus, as in the headless smoke) | named mutex |
| Dialogs (`dialogs.rs`) | `NSOpenPanel` sheet | GTK file chooser | common item dialog |

Secrets are not the shell's: the keyring `SecretStore` (login Keychain,
Secret Service, Credential Manager) lives in `steno-services` (#173,
WP6b), which the host reads the API key through.

The tray menu is the Swift menu bar popover's controls: Record (Stop
recording while recording, with the recorder's words between), Record in
person, Open Steno, Settings, Launch at login, Check for Updates, Quit.
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
through `panels::dismiss_prompt` (WP6b wires the detection controller to
both).

On a Wayland session the shell runs under XWayland. GTK 3 on Wayland can
neither place a window nor keep it above the others, and it reports no
moves, so the panels would not float, would not stay where they are put,
and would never save the anchor. When `WAYLAND_DISPLAY` and `DISPLAY` are
both set and `GDK_BACKEND` is not, `main` allows GDK only its `x11`
backend before Tauri initialises GTK (`display.rs`). That setting stays
inside the process, so a browser the shell opens still starts on Wayland.
Every start logs one `display:` line naming the backend. A `GDK_BACKEND`
set before launch always wins: `GDK_BACKEND=wayland steno-desktop` runs
natively on Wayland, with panels that neither stay on top nor keep their
place. A Wayland session without XWayland (no `DISPLAY`) runs on Wayland
too, since X11 would not open there.

The bridge methods the shell answers itself, beside `window.*` and
`system.openURL`: `system.openSystemSettings` (the pane per OS),
`settings.general.openLoginItems`, `settings.general.setLaunchAtLogin`
(then forwarded to the host so its snapshot follows), `updates.check`, and
the three folder panels (`settings.recording.chooseFolder`,
`settings.export.chooseVault`, `onboarding.chooseVault`): the shell shows
the panel, tells the host the choice as `{ "path": … }` under the same
method name, and replies `reply.chosenPath` itself; a cancelled panel
replies `{}` and the host hears nothing.

Deep links: the Swift Mac app registers no scheme (`steno://pair/…` is the
iPhone's, the link the pairing QR code doubles as), so the scheme is new
here. `steno://meeting/<uuid>` opens the main window on the meeting (the
host selects it; until WP6b the fixture host only brings main forward),
`steno://settings[/<section>]` opens Settings on the section; scheme and
host read in any case, the section as the contract spells it; a pairing
link is logged and ignored. A link that reaches a window before its page
has mounted (a cold launch) waits in `windows::Pages` and is published on
the page's `page.ready`.

## Run

`cargo tauri dev` from `apps/desktop/src-tauri` (`pnpm dlx
@tauri-apps/cli@2.12.1 dev`, the version the workflows pin, or `cargo
install tauri-cli --version 2.12.1`) starts the Vite dev server and the
shell together.
Without the CLI, start the dev server yourself and build the shell:

```sh
pnpm --dir apps/macos/web install --frozen-lockfile
pnpm --dir apps/macos/web dev &
cargo build -p steno-desktop && target/debug/steno-desktop
```

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
(`cargo tauri build`, or `.github/workflows/desktop-release.yml` on a
manual trigger) keep the fixture host until WP6b flips the default, so
they show the whole UI with synthetic data on every platform.

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
then quits (see above). The icons in `icons/` come from `cargo tauri
icon` over the Swift app icon
(`apps/macos/Steno/Resources/Assets.xcassets/AppIcon.appiconset/icon_512x512@2x.png`),
the tray's template mark in `icons/tray/` is drawn by hand. The identifier
stays `uno.schmid.steno.desktop` so the shell installs beside the Swift
app; WP9 changes it to `uno.schmid.steno.mac` for the cutover.

Updates are signed: `plugins.updater.pubkey` is the public half of a key
pair from `cargo tauri signer generate`. The private half is never in the
repository; it is the GitHub secret `TAURI_SIGNING_PRIVATE_KEY` (with
`TAURI_SIGNING_PRIVATE_KEY_PASSWORD` when it has one), which WP9 adds
before the first signed release. Without the secret, `createUpdaterArtifacts`
is switched off through the configuration merge in the workflows, because a
public key without a private key fails the build. The lane follows the
installed version as it did with Sparkle: a pre-release reads the `beta`
manifest first, a release the stable one only (`updater.rs`).

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
paths until Cargo's `trim-paths` stabilises; the release workflow sets
`RUSTFLAGS=--remap-path-prefix` so they read `steno/…` instead.

## Test

`cargo test -p steno-desktop` covers the fixture table against `index.json`
and the mock transport, the window specs and routes, the typed `window.open`
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
and the folder choosers' replies. `cargo test -p steno-desktop
--no-default-features` runs the same without the fixture host. In the web
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
| 1 | No `page.ready` from main; or `page.ready` but no snapshot: no bridge host is wired (a build with `--no-default-features` fails here until WP6b, and the message says so); or the meeting was lost or published before the page listened; or no tray; or a panel or main that did not do as above |
| 2 | At once, when `n` is not a positive number |

`apps/desktop/scripts/smoke-linux.sh [binary] [seconds]` runs that under
`xvfb-run` and, when ImageMagick is present (`magick` or `convert`), captures
the Xvfb root and one crop per window and per panel into
`apps/desktop/screens/` (ignored by git; CI uploads it as the
`desktop-smoke-screens` artifact). With `STENO_SMOKE_DPI=120` it runs Xvfb
at that resolution, where WebKitGTK's pixel ratio is 1.25 and the panels
must still fit their pills. The windows carry only fixture data,
which is synthetic. Xvfb has no compositor, so the panels' transparent
corners render black there; a desktop shows them rounded. Xvfb has no
tray host either; a smoke run stands in for one, so the built tray counts
and the run checks the close rule a desktop with a tray gets.

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
| `apps/desktop/src-tauri/Cargo.toml` | Crate `steno-desktop`, binary `steno-desktop`; features `fixture-host` (default on) and `custom-protocol` (embeds the bundle, see Build). Dependency versions come from the workspace table in `Cargo.toml` |
| `apps/desktop/src-tauri/tauri.conf.json` | `frontendDist` is the web app's `dist/`; no `version`, so Tauri takes the crate's; `beforeDevCommand` and `beforeBuildCommand` run `pnpm dev` and `pnpm build` with `cwd` `../../macos/web`: the CLI runs them from `src-tauri`, the directory holding this file, which is also where `frontendDist` (`../../macos/web/dist`) resolves from; `csp` lets the page load only its own scripts, styles, fonts and images, and `connect-src` only the IPC origins (`ipc:`, `http://ipc.localhost`), so nothing the page does reaches the network; `plugins` carries the `steno` scheme and the updater's public key and stable endpoint; `bundle` the six installer targets (see Bundles); no windows are declared, `windows.rs` and `panels.rs` create them |
| `apps/desktop/src-tauri/tauri.linux.conf.json`, `linux/steno-desktop.desktop` | Merged on Linux: the `steno-desktop` product name for the package, and the desktop entry template (see Bundles) |
| `apps/desktop/src-tauri/Info.plist`, `Entitlements.plist` | Merged into the macOS bundle: the TCC purpose strings and the Bonjour service, verbatim from `apps/macos/project.yml`; the audio-input and calendars entitlements |
| `apps/desktop/src-tauri/build.rs`, `apps/desktop/src-tauri/placeholder/` | Points `frontendDist` at the placeholder page when the web `dist/` is missing, so a debug `cargo build` works on a bare checkout (a release build fails instead); then `tauri_build::build()` |
| `apps/desktop/src-tauri/src/main.rs` | Wires the plugins (single instance first, autostart, deep link, dialog, opener, updater, `tauri-nspanel` on macOS), the managed state, the one menu handler, the tray and the windows; hides the main window on close and keeps the process while a tray stands, ends it otherwise; a dragged panel's anchor, a destroyed window's page, and the Dock's reopen. `display.rs`, on Linux: the GDK backend (XWayland on a Wayland session) |
| `apps/desktop/src-tauri/src/tray.rs`, `menu.rs`, `actions.rs`, `recording.rs` | The tray menu and icon, the macOS menu bar, the actions behind their items, the recorder state the shell follows |
| `apps/desktop/src-tauri/src/panels.rs`, `panel_geometry.rs` | The two floating panels and the one content rule, the macOS `NSPanel` conversion; the anchor, frames and size validation as plain values |
| `apps/desktop/src-tauri/src/autostart.rs`, `updater.rs`, `permissions.rs`, `deep_links.rs`, `dialogs.rs` | One module per service (see What the shell owns); each is plain rules the tests cover over a plugin or OS call |
| `apps/desktop/src-tauri/src/windows.rs` | The three windows with the Swift sizes: main 1120 by 720 (minimum 960 by 600) at `#/main`, Settings 960 by 640 (minimum 760 by 520) at `#/settings`, onboarding fixed 560 by 620 at `#/onboarding`. Main opens at start; the others on `window.open`, focused when already open. New windows from the page are denied. `Pages` holds the requests a window is owed until its page mounts |
| `apps/desktop/src-tauri/src/bridge.rs` | `bridge_call(method, params)` and the `steno:event` emitter, scoped to the calling window; a finished `onboarding` snapshot closes the onboarding window, a `recording` snapshot to main moves the tray and the panels. `window.open` (typed: one of the six sections, a UUID meeting id), `window.close` (the onboarding window, from itself), `system.openURL` (`https:` and `mailto:` only) and the WP8 methods listed above are the shell's; everything else goes to the host. `panel_call(action, params)` is the panels' own command |
| `apps/desktop/src-tauri/src/host.rs`, `fixtures.rs` | The fixture host: the fixtures `index.json` lists, embedded with `include_str!`; every topic's snapshot on `page.ready`; replies as `mock-transport.ts` gives them (`speakers.options.reply`, `reply.confirm` and `reply.chosenPath` for the alerts and folder panels, `null` otherwise); a deep link as the `app` snapshot with the request set, then the clean one |
| `apps/desktop/src-tauri/src/navigation.rs` | Navigation policy: the app origin and, in a dev build, the Vite dev server; everything else is cancelled |
| `apps/desktop/src-tauri/src/smoke.rs`, `apps/desktop/scripts/smoke-linux.sh` | The headless smoke CI runs under Xvfb |
| `apps/desktop/src-tauri/capabilities/default.json`, `panels.json` | `core:event:allow-listen` and `allow-unlisten` for the three windows, the one core IPC the page uses; the panels get the same plus `core:window:allow-start-dragging` for `data-tauri-drag-region`; `bridge_call` and `panel_call` are app commands and native capabilities are reached through them |
| `.github/workflows/desktop-release.yml`, `apps/desktop/scripts/release-matrix.sh` | Manual trigger: the six bundles on the three platforms as workflow artifacts, unsigned (WP9 signs, notarises and publishes); the `platforms` input is filtered by `release-matrix.sh` (tested in Rust CI by `release-matrix.test.sh`) |

## Not here yet

Signing, notarisation, the GitHub release and the updater manifests are
WP9, as is `cargo deny`; the release workflow stops at unsigned bundles.
The host's half of the WP8 seams is WP6b, which wires `steno-host` (#170)
in place of the fixture host: the detection controller raising the prompt
(`panels::set_prompt`) and hearing of its dismissal
(`panels::dismiss_prompt`); the host's `LoginItem`, `Permissions`,
`Updater` and `Opener` traits implemented over `autostart`, `permissions`,
`updater` and `dialogs::reveal`, whose `LoginItemStatus` and
`UpdateOutcome` give way to the host's own; and the folder panels, which
`steno-host` asks for through its `choose_folder` callback where this
shell shows the panel itself and forwards `{ "path": … }`. The host may
treat the main window as always present: a close hides it, or ends the
process when no tray stands, so publishing to it never fails for want of a
window. Launch at login is a Launch Agent, not `SMAppService`; WP9 has to
retire the Swift registration at cutover so the user does not get two
login items (the plan's parity list). The macOS menu bar has no Record
menu yet (`⌘⇧R` and Record In Person are the tray's and the sidebar's),
and no Find Meetings (`⌘F`). Updates are checked only when asked (the
tray's item, Settings), where Sparkle checks daily on its own. On macOS
the system audio permission has no status API; the audio crate's probe
(WP5) records it and until then it reads `unknown`. The panels are
re-tuned on the Mac once they run there beside the Swift ones (the plan's
risk list). Linux and Windows keep their native title bar; macOS gets the
overlay title bar the Swift windows have. The page's traffic light inset
is a design question for the other two platforms. On Linux, WebKitGTK
leaks one shared-memory file descriptor per destroyed webview that lived
longer than about 250 ms (29 to 107 fds over 70 Settings open/close
cycles; wry/WebKitGTK level, not the shell), so long sessions with many
Settings opens should be watched until
[#160](https://github.com/NicolaiSchmid/steno/issues/160) is resolved. On
Linux a panel keeps a 5 px resize border that Tauri gives every
undecorated resizable window. The pinned size holds, but the border shows
a resize cursor and swallows a press, so a drag that starts on the outer
5 px does not move the panel; no control sits there.
