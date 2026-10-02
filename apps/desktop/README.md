# Steno desktop shell

The Tauri 2 shell for macOS, Linux and Windows: three windows around the web
UI in `apps/macos/web`, and the `bridge_call` command its Tauri transport
talks to. WP3 of `.plans/2026-10-02-rust-core-and-tauri-shell.md`. The shell
holds no logic; until WP6 wires the real host, the default `fixture-host`
cargo feature answers the bridge from the recorded fixtures in
`apps/macos/web/fixtures/bridge/`, so the whole UI runs on every platform
before any pipeline exists. Paths below are from the repository root.

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
release bundle carries the placeholder page. Release packaging also passes
`--no-default-features` until WP6 flips the `fixture-host` default.

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
policies, the deep-link snapshots and the smoke's switches and verdicts;
`cargo test -p steno-desktop --no-default-features` the same without the
fixture host. `tauri-transport.test.ts` in the web app covers the page's
half of the wire.

## Smoke

`STENO_SMOKE_SECONDS=<n> steno-desktop` opens all three windows side by side,
waits `n` seconds and reports:

| Exit | When |
|---|---|
| 0 | The main window sent `page.ready` and at least one snapshot reached it |
| 1 | No `page.ready` from main, or `page.ready` but no snapshot: no bridge host is wired (a build with `--no-default-features` fails here until WP6, and the message says so) |
| 2 | At once, when `n` is not a positive number |

`apps/desktop/scripts/smoke-linux.sh [binary] [seconds]` runs that under
`xvfb-run` and, when ImageMagick is present (`magick` or `convert`), captures
the Xvfb root and one crop per window into `apps/desktop/screens/` (ignored
by git; CI uploads it as the `desktop-smoke-screens` artifact). The windows
carry only fixture data, which is synthetic.

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
export __EGL_VENDOR_LIBRARY_DIRS=$MESA/share/glvnd/egl_vendor.d LD_LIBRARY_PATH=$MESA/lib
apps/desktop/scripts/smoke-linux.sh target/debug/steno-desktop 12
```

### Windows and macOS

Nothing beyond the Rust toolchain. WebView2 ships with Windows 11; the
`.ico` the Windows resource needs is in `apps/desktop/src-tauri/icons/`.

## Layout

| Path | What lives there |
|---|---|
| `apps/desktop/src-tauri/Cargo.toml` | Crate `steno-desktop`, binary `steno-desktop`; features `fixture-host` (default on) and `custom-protocol` (embeds the bundle, see Build). Dependency versions come from the workspace table in `Cargo.toml` |
| `apps/desktop/src-tauri/tauri.conf.json` | `frontendDist` is the web app's `dist/`; no `version`, so Tauri takes the crate's; `beforeDevCommand` and `beforeBuildCommand` run `pnpm dev` and `pnpm build` with `cwd` `../../macos/web`: the CLI runs them from `src-tauri`, the directory holding this file, which is also where `frontendDist` (`../../macos/web/dist`) resolves from; `csp` lets the page load only its own scripts, styles, fonts and images, and `connect-src` only the IPC origins (`ipc:`, `http://ipc.localhost`), so nothing the page does reaches the network; `bundle.active` is false until WP8 packages installers; no windows are declared, `windows.rs` creates them |
| `apps/desktop/src-tauri/build.rs`, `apps/desktop/src-tauri/placeholder/` | Points `frontendDist` at the placeholder page when the web `dist/` is missing, so a debug `cargo build` works on a bare checkout (a release build fails instead); then `tauri_build::build()` |
| `apps/desktop/src-tauri/src/main.rs` | Wires the plugins, the managed state and the windows; keeps the process alive on macOS when the last window closes, as the Swift menu bar app does |
| `apps/desktop/src-tauri/src/windows.rs` | The three windows with the Swift sizes: main 1120 by 720 (minimum 960 by 600) at `#/main`, Settings 960 by 640 (minimum 760 by 520) at `#/settings`, onboarding fixed 560 by 620 at `#/onboarding`. Main opens at start; the others on `window.open`, focused when already open. New windows from the page are denied |
| `apps/desktop/src-tauri/src/bridge.rs` | `bridge_call(method, params)` and the `steno:event` emitter, scoped to the calling window; a finished `onboarding` snapshot closes the onboarding window. `window.open` (typed: one of the six sections, a UUID meeting id), `window.close` (the onboarding window, from itself) and `system.openURL` (`https:` and `mailto:` only) are the shell's; everything else goes to the host |
| `apps/desktop/src-tauri/src/host.rs`, `fixtures.rs` | The fixture host: the fixtures `index.json` lists, embedded with `include_str!`; every topic's snapshot on `page.ready`; replies as `mock-transport.ts` gives them (`speakers.options.reply`, `reply.confirm` and `reply.chosenPath` for the alerts and folder panels, `null` otherwise); a deep link as the `app` snapshot with the request set, then the clean one |
| `apps/desktop/src-tauri/src/navigation.rs` | Navigation policy: the app origin and, in a dev build, the Vite dev server; everything else is cancelled |
| `apps/desktop/src-tauri/src/smoke.rs`, `apps/desktop/scripts/smoke-linux.sh` | The headless smoke CI runs under Xvfb |
| `apps/desktop/src-tauri/capabilities/default.json` | `core:event:allow-listen` and `allow-unlisten` for the three windows, the one core IPC the page uses; `bridge_call` is an app command and native capabilities are reached through it |

## Not here yet

Tray, floating panels, deep links, autostart, the updater, the keyring and
single instance follow in WP8 of the plan. Until the tray lands there, Linux
and Windows quit when the last window closes; macOS keeps running, as the
Swift app does. The identifier is `uno.schmid.steno.desktop` so the shell
installs beside the Swift app; WP9 changes it to `uno.schmid.steno.mac` for
the cutover. Linux and Windows keep their native title bar; macOS gets the
overlay title bar the Swift windows have. The page's traffic light inset is a
WP8 design question for the other two platforms.
