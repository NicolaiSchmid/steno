# Steno desktop shell

The Tauri 2 shell for macOS, Linux and Windows: three windows around the web
UI in `apps/macos/web`, and the `bridge_call` command its Tauri transport
talks to. WP3 of `.plans/2026-10-02-rust-core-and-tauri-shell.md`. The shell
holds no logic; until WP6 wires the real host, the default `fixture-host`
cargo feature answers the bridge from the recorded fixtures in
`apps/macos/web/fixtures/bridge/`, so the whole UI runs on every platform
before any pipeline exists.

## Build

`cargo tauri dev` from `apps/desktop` (the `@tauri-apps/cli` or
`cargo install tauri-cli`) starts the Vite dev server and the shell together.
Without the CLI:

```sh
# The web UI first; without it a debug build embeds a placeholder page.
pnpm --dir apps/macos/web install --frozen-lockfile && pnpm --dir apps/macos/web build
cargo build -p steno-desktop
```

A build without the `custom-protocol` feature loads `devUrl` (the Vite dev
server on 5173), whatever the profile; that is Tauri's dev build. To run a
debug binary against the embedded bundle (what the smoke does), drop the dev
URL through Tauri's own configuration merge:

```sh
TAURI_CONFIG='{"build":{"devUrl":null}}' cargo build -p steno-desktop
```

A release embeds the bundle only with the feature, which `cargo tauri build`
turns on:

```sh
cargo build -p steno-desktop --release --features custom-protocol
```

A release build with the web `dist/` missing fails in `build.rs`, so no
release bundle carries the placeholder. Release packaging also passes
`--no-default-features` until WP6 flips the `fixture-host` default.

### Linux prerequisites

Debian and Ubuntu: `scripts/setup-linux.sh` installs the packages (the
list is in the script); CI runs it on `ubuntu-latest`.

NixOS: no system packages; build and run inside a shell with the libraries:

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

Windows and macOS need nothing beyond the Rust toolchain (WebView2 ships
with Windows 11; the `.ico` the Windows resource needs is in `icons/`).

## Layout

| Path | What lives there |
|---|---|
| `src-tauri/Cargo.toml` | Crate `steno-desktop`, binary `steno-desktop`; features `fixture-host` (default on) and `custom-protocol` (embeds the bundle, see Build) |
| `src-tauri/tauri.conf.json` | `frontendDist` is the web app's `dist/`; `beforeDevCommand` and `beforeBuildCommand` run `pnpm dev` and `pnpm build` in `apps/macos/web`; no windows are declared here, `windows.rs` creates them |
| `src-tauri/build.rs`, `src-tauri/placeholder/` | Points `frontendDist` at the placeholder page when the web `dist/` is missing, so a debug `cargo build` works on a bare checkout (a release build fails instead); then `tauri_build::build()` |
| `src-tauri/src/main.rs` | Wires the plugins, the managed state and the windows; keeps the process alive on macOS when the last window closes, as the Swift menu bar app does |
| `src-tauri/src/windows.rs` | The three windows with the Swift sizes: main 1120 by 720 (minimum 960 by 600) at `#/main`, Settings 960 by 640 (minimum 760 by 520) at `#/settings`, onboarding fixed 560 by 620 at `#/onboarding`. Main opens at start; the others on `window.open`, focused when already open |
| `src-tauri/src/bridge.rs` | `bridge_call(method, params)` and the `steno:event` emitter, scoped to the calling window; a finished `onboarding` snapshot closes the onboarding window. `window.open`, `window.close` and `system.openURL` (`https:` and `mailto:` only) are the shell's; everything else goes to the host |
| `src-tauri/src/host.rs`, `fixtures.rs` | The fixture host: the fixtures `index.json` lists, embedded with `include_str!`; every topic's snapshot on `page.ready`; replies as `mock-transport.ts` gives them (`speakers.options.reply`, `reply.confirm` and `reply.chosenPath` for the alerts and folder panels, `null` otherwise); a deep link as the `app` snapshot with the request set, then the clean one |
| `src-tauri/src/navigation.rs` | Navigation policy: the app origin and, in a dev build, the Vite dev server; everything else is cancelled |
| `src-tauri/src/smoke.rs`, `scripts/smoke-linux.sh` | The headless smoke CI runs under Xvfb |
| `src-tauri/capabilities/default.json` | `core:event:allow-listen` and `allow-unlisten` for the three windows, the one core IPC the page uses; `bridge_call` is an app command and native capabilities are reached through it |

## Smoke

`STENO_SMOKE_SECONDS=<n> steno-desktop` opens all three windows side by side,
waits `n` seconds and exits 0 when the main window sent `page.ready`, 1 when
it did not, 2 at once when `n` is not a positive number.
`scripts/smoke-linux.sh [binary] [seconds]` runs that under `xvfb-run` and,
when ImageMagick is present, captures the Xvfb root and one crop per window
into `apps/desktop/screens/` (ignored by git; CI uploads it as the
`desktop-smoke-screens` artifact). The windows carry only fixture data,
which is synthetic.

## Tests

`cargo test -p steno-desktop` covers the fixture table against `index.json`
and the mock transport, the window specs and routes, the URL and navigation
policies, the deep-link snapshots and the smoke's switches;
`tauri-transport.test.ts` in the web app covers the page's half of the wire.

## Not here yet

Tray, floating panels, deep links, autostart, the updater, the keyring and
single instance follow in WP8 of the plan. Until the tray lands there, Linux
and Windows quit when the last window closes; macOS keeps running, as the
Swift app does. The identifier is `uno.schmid.steno.desktop` so the shell
installs beside the Swift app; WP9 changes it to `uno.schmid.steno.mac` for
the cutover. Linux and Windows keep their native title bar; macOS gets the
overlay title bar the Swift windows have. The page's traffic light inset is a
WP8 design question for the other two platforms.
