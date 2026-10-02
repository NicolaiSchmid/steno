# Steno desktop shell

The Tauri 2 shell for macOS, Linux and Windows: three windows and two
floating panels around the web UI in `apps/macos/web`, a tray icon, and the
`bridge_call` command the web app's Tauri transport talks to. WP3 and WP8 of
`.plans/2026-10-02-rust-core-and-tauri-shell.md`. The shell holds no logic;
until WP6 wires the real host, the default `fixture-host` cargo feature
answers the bridge from the recorded fixtures in
`apps/macos/web/fixtures/bridge/`, so the whole UI runs on every platform
before any pipeline exists. Paths below are from the repository root.

## What the shell owns

Everything the Swift app does outside its three windows, per OS:

| | macOS | Linux | Windows |
|---|---|---|---|
| Tray (`tray.rs`) | Menu bar extra with a template icon | Status notifier item (libayatana-appindicator) | Notification area icon |
| Floating panels (`panels.rs`) | Non-activating `NSPanel`s on every space (`tauri-nspanel`) | Always-on-top undecorated windows | Always-on-top undecorated windows |
| Launch at login (`autostart.rs`) | Launch Agent | `~/.config/autostart` entry | Run registry key |
| Updates (`updater.rs`) | signed manifest per lane | same | same |
| Secrets (`secrets.rs`) | login Keychain | Secret Service over D-Bus | Credential Manager |
| Permissions (`permissions.rs`) | microphone TCC status and prompt; system audio and calendar deferred to the host's probes | granted (the portal asks at capture time) | granted (WASAPI asks nothing) |
| Deep links (`deep_links.rs`) | `steno:` through `Info.plist` | `.desktop` MIME type (debug builds register at start) | registry (debug builds register at start) |
| Single instance | Unix socket | session bus name (skipped without a session bus, as in the headless smoke) | named mutex |
| Dialogs (`dialogs.rs`) | `NSOpenPanel` sheet | GTK file chooser | common item dialog |

The tray menu is the Swift menu bar popover's controls: Record (Stop
recording while recording, with the recorder's words between), Record in
person, Open Steno, Settings, Launch at login, Check for Updates, Quit.
Recorder commands are bridge methods sent through the main window
(`actions.rs`), so the shell has no recorder logic of its own; it follows
the recorder off the `recording` snapshots the host publishes to that
window (`recording.rs`). The queue and recent rows of the Swift popover are
the main window's.

The panels are the web app's `#/panel/bubble` and `#/panel/prompt` routes
(`apps/macos/web/src/windows/panels/`), two webviews that hang from one
anchor (top centre of the frame, 8 pt under the main screen's top edge by
default, saved to `panel-anchor.json` in the app config directory when the
user drags one). One rule decides what shows: a busy recorder wins, else a
pending detection prompt, else nothing. The page measures its pill and
reports the size through the `panel_call` command; the shell sizes the
window from it. The prompt's X and that size report are the only two
things `panel_call` carries; everything else the panels do goes through the
bridge (`recording.stop`, `recording.keepGoing`, `recording.start`,
`window.open`). The host raises and clears the prompt through
`panels::set_prompt` (WP6b wires the detection controller).

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
here. `steno://meeting/<uuid>` opens the main window on the meeting,
`steno://settings[/<section>]` opens Settings on the section; a pairing
link is logged and ignored.

## Run

`cargo tauri dev` from `apps/desktop/src-tauri` (the `@tauri-apps/cli` or
`cargo install tauri-cli`) starts the Vite dev server and the shell together.
Without the CLI, start the dev server yourself and build the shell:

```sh
pnpm --dir apps/macos/web install --frozen-lockfile
pnpm --dir apps/macos/web dev &
cargo build -p steno-desktop && target/debug/steno-desktop
```

Every build without the `custom-protocol` feature loads `devUrl` (the Vite
dev server on 5173), whatever the profile; that is Tauri's dev build. Set
`STENO_SMOKE_SECONDS` to open all three windows at once (see Smoke).

## Build

The embedded bundle is what ships. Build the web UI first, then the shell
with the feature, which `cargo tauri build` turns on by itself:

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
`.AppImage` on Linux, `.msi` and NSIS on Windows; the icons in `icons/`
come from `cargo tauri icon` over the Swift app icon
(`apps/macos/Steno/Resources/Assets.xcassets/AppIcon.appiconset/icon_512x512@2x.png`),
the tray's template mark in `icons/tray/` is drawn by hand. The identifier
stays `uno.schmid.steno.desktop` so the shell installs beside the Swift
app; WP9 changes it to `uno.schmid.steno.mac` for the cutover (and the
keyring service with it, so the API key the Swift app stored is read).

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

Panic messages in a release binary carry the build host's source paths
until Cargo's `trim-paths` stabilises; WP8's release job can set
`RUSTFLAGS=--remap-path-prefix` meanwhile.

## Test

`cargo test -p steno-desktop` covers the fixture table against `index.json`
and the mock transport, the window specs and routes, the typed `window.open`
and `window.close` params and who may close what, the URL and navigation
policies, the deep-link snapshots and the smoke's switches and verdicts,
and every WP8 module's rules: the tray's ids, labels and tooltip per
recorder state, the panels' anchor (default, drag, screen loss, JSON),
the one content rule, the prompt query, the login item states, the update
lanes, the keyring store over the crate's in-memory mock, the permission
kinds and panes per OS, the `steno:` link grammar, and the folder
choosers' replies. `cargo test -p steno-desktop --no-default-features` the
same without the fixture host. In the web app, `tauri-transport.test.ts`
covers the page's half of the wire and `src/windows/panels/*.test.tsx` the
two panels.

## Smoke

`STENO_SMOKE_SECONDS=<n> steno-desktop` opens all three windows side by
side and both floating panels under Settings (the prompt naming a made-up
app), waits `n` seconds, hides the panels again and reports:

| Exit | When |
|---|---|
| 0 | The main window sent `page.ready`, at least one snapshot reached it, the tray was built, and both panels were visible before the hide and hidden after it |
| 1 | No `page.ready` from main; or `page.ready` but no snapshot: no bridge host is wired (a build with `--no-default-features` fails here until WP6, and the message says so); or no tray; or a panel that did not show or hide |
| 2 | At once, when `n` is not a positive number |

`apps/desktop/scripts/smoke-linux.sh [binary] [seconds]` runs that under
`xvfb-run` and, when ImageMagick is present (`magick` or `convert`), captures
the Xvfb root and one crop per window and per panel into
`apps/desktop/screens/` (ignored by git; CI uploads it as the
`desktop-smoke-screens` artifact). The windows carry only fixture data,
which is synthetic. Xvfb has no compositor, so the panels' transparent
corners render black there; a desktop shows them rounded.

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
| `apps/desktop/src-tauri/Info.plist`, `Entitlements.plist` | Merged into the macOS bundle: the TCC purpose strings and the Bonjour service, verbatim from `apps/macos/project.yml`; the audio-input and calendars entitlements |
| `apps/desktop/src-tauri/build.rs`, `apps/desktop/src-tauri/placeholder/` | Points `frontendDist` at the placeholder page when the web `dist/` is missing, so a debug `cargo build` works on a bare checkout (a release build fails instead); then `tauri_build::build()` |
| `apps/desktop/src-tauri/src/main.rs` | Wires the plugins (single instance first, autostart, deep link, dialog, opener, updater, `tauri-nspanel` on macOS), the managed state, the tray and the windows; keeps the process alive on every platform when the last window closes, as the Swift menu bar app does; a dragged panel's anchor and the Dock's reopen |
| `apps/desktop/src-tauri/src/tray.rs`, `actions.rs`, `recording.rs` | The tray menu and icon, the actions behind its items, the recorder state the shell follows |
| `apps/desktop/src-tauri/src/panels.rs` | The two floating panels, their anchor and the one content rule; the macOS `NSPanel` conversion |
| `apps/desktop/src-tauri/src/autostart.rs`, `updater.rs`, `secrets.rs`, `permissions.rs`, `deep_links.rs`, `dialogs.rs` | One module per service (see What the shell owns); each is plain rules the tests cover over a plugin or OS call |
| `apps/desktop/src-tauri/src/windows.rs` | The three windows with the Swift sizes: main 1120 by 720 (minimum 960 by 600) at `#/main`, Settings 960 by 640 (minimum 760 by 520) at `#/settings`, onboarding fixed 560 by 620 at `#/onboarding`. Main opens at start; the others on `window.open`, focused when already open. New windows from the page are denied |
| `apps/desktop/src-tauri/src/bridge.rs` | `bridge_call(method, params)` and the `steno:event` emitter, scoped to the calling window; a finished `onboarding` snapshot closes the onboarding window, a `recording` snapshot to main moves the tray and the panels. `window.open` (typed: one of the six sections, a UUID meeting id), `window.close` (the onboarding window, from itself), `system.openURL` (`https:` and `mailto:` only) and the WP8 methods listed above are the shell's; everything else goes to the host. `panel_call(action, params)` is the panels' own command |
| `apps/desktop/src-tauri/src/host.rs`, `fixtures.rs` | The fixture host: the fixtures `index.json` lists, embedded with `include_str!`; every topic's snapshot on `page.ready`; replies as `mock-transport.ts` gives them (`speakers.options.reply`, `reply.confirm` and `reply.chosenPath` for the alerts and folder panels, `null` otherwise); a deep link as the `app` snapshot with the request set, then the clean one |
| `apps/desktop/src-tauri/src/navigation.rs` | Navigation policy: the app origin and, in a dev build, the Vite dev server; everything else is cancelled |
| `apps/desktop/src-tauri/src/smoke.rs`, `apps/desktop/scripts/smoke-linux.sh` | The headless smoke CI runs under Xvfb |
| `apps/desktop/src-tauri/capabilities/default.json`, `panels.json` | `core:event:allow-listen` and `allow-unlisten` for the three windows, the one core IPC the page uses; the panels get the same plus `core:window:allow-start-dragging` for `data-tauri-drag-region`; `bridge_call` and `panel_call` are app commands and native capabilities are reached through them |
| `.github/workflows/desktop-release.yml` | Manual trigger: the six bundles on the three platforms as workflow artifacts, unsigned (WP9 signs, notarises and publishes) |

## Not here yet

Signing, notarisation, the GitHub release and the updater manifests are
WP9; the release workflow stops at unsigned bundles. The host's half of the
WP8 seams is WP6b: the detection controller raising the prompt
(`panels::set_prompt`), the General snapshot reading `autostart::status`
and `Updates::last`, the onboarding and Settings permission rows calling
`permissions::state` and `request`, the folder choice arriving as
`{ "path": … }`, the reveal methods calling `dialogs::reveal`, the LLM
client reading `secrets::KeyringSecretStore` through
`secrets::secret_blocking`. On macOS the system audio permission has no
status API; the audio crate's probe (WP5) records it and until then it
reads `unknown`. The panels are re-tuned on the Mac once they run there
beside the Swift ones (the plan's risk list). Linux and Windows keep their
native title bar; macOS gets the overlay title bar the Swift windows have.
The page's traffic light inset is a design question for the other two
platforms. On Linux, WebKitGTK leaks one
shared-memory file descriptor per destroyed webview that lived longer than
about 250 ms (29 to 107 fds over 70 Settings open/close cycles; wry/WebKitGTK
level, not the shell), so long sessions with many Settings opens should be
watched until [#160](https://github.com/NicolaiSchmid/steno/issues/160) is
resolved.
