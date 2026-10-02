# Steno desktop shell

The Tauri 2 shell for macOS, Linux and Windows: three windows around the web
UI in `apps/macos/web`, and the `bridge_call` command its Tauri transport
talks to. WP3 of `.plans/2026-10-02-rust-core-and-tauri-shell.md`. The shell
holds no logic; until WP6 wires the real host, the default `fixture-host`
cargo feature answers the bridge from the recorded fixtures in
`apps/macos/web/fixtures/bridge/`, so the whole UI runs on every platform
before any pipeline exists.

## Layout

| Path | What lives there |
|---|---|
| `src-tauri/Cargo.toml` | Crate `steno-desktop`, binary `steno-desktop`, feature `fixture-host` (default on) |
| `src-tauri/tauri.conf.json` | `frontendDist` is the web app's `dist/`; `beforeDevCommand` and `beforeBuildCommand` run `pnpm dev` and `pnpm build` in `apps/macos/web`; no windows are declared here, `windows.rs` creates them |
| `src-tauri/build.rs` | Generates the fixture module (`include_str!` of every file `index.json` lists); stands in for a missing web `dist/` with a placeholder page so `cargo build` works on a bare checkout; then `tauri_build::build()` |
| `src-tauri/src/windows.rs` | The three windows with the Swift sizes: main 1120 by 720 (minimum 960 by 600) at `#/main`, Settings 960 by 640 (minimum 760 by 520) at `#/settings`, onboarding fixed 560 by 620 at `#/onboarding`. Main opens at start; the others on `window.open`, focused when already open |
| `src-tauri/src/bridge.rs` | `bridge_call(method, params)` and the `steno:event` emitter, scoped to the calling window. `window.open`, `window.close` and `system.openURL` are the shell's; everything else goes to the host |
| `src-tauri/src/host.rs`, `fixtures.rs` | The fixture host: every topic's snapshot on `page.ready`, replies as `mock-transport.ts` gives them (`speakers.options.reply`, `reply.confirm` and `reply.chosenPath` for the alerts and folder panels, `null` otherwise) |
| `src-tauri/src/navigation.rs` | Navigation policy: the app origin and, in debug builds, the Vite dev server; everything else is cancelled |
| `src-tauri/src/smoke.rs`, `scripts/smoke-linux.sh` | The headless smoke CI runs under Xvfb |
| `src-tauri/capabilities/default.json` | `core:default` for the three windows, nothing more; native capabilities are reached through `bridge_call` |

## Build

```sh
# The web UI first; without it the shell embeds a placeholder page.
pnpm --dir apps/macos/web install --frozen-lockfile && pnpm --dir apps/macos/web build
cargo build -p steno-desktop
```

A debug build loads `devUrl` (the Vite dev server on 5173) instead of the
embedded `dist/`, as Tauri does for every dev build. To run a debug binary
against the embedded bundle (what the smoke does), drop the dev URL through
Tauri's own configuration merge:

```sh
TAURI_CONFIG='{"build":{"devUrl":null}}' cargo build -p steno-desktop
```

Release builds always embed. `cargo tauri dev` (the `@tauri-apps/cli` or
`cargo install tauri-cli`) from `apps/desktop` starts the Vite dev server and
the shell together.

### Linux prerequisites

Debian and Ubuntu: `scripts/setup-linux.sh` (WebKitGTK 4.1, GTK 3,
libayatana-appindicator, librsvg, OpenSSL, pkg-config, xdo; Xvfb and
ImageMagick for the smoke). CI runs it on `ubuntu-latest`.

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

## Smoke

`STENO_SMOKE_SECONDS=<n> steno-desktop` opens all three windows side by side,
waits `n` seconds and exits 0 when the main window sent `page.ready`, 1 when
it did not. `scripts/smoke-linux.sh [binary] [seconds]` runs that under
`xvfb-run` and, when ImageMagick is present, captures the Xvfb root and one
crop per window into `apps/desktop/screens/` (ignored by git; CI uploads it
as the `desktop-smoke-screens` artifact). The windows carry only fixture
data, which is synthetic.

## Tests

`cargo test -p steno-desktop`: the fixture module (every topic has a
snapshot, every fixture parses, the reply aliases match the mock transport),
the window specs and routes, the `params.window` shape, the navigation
policy and the smoke's switches. The web side's `tauri-transport.test.ts`
covers the page's half of the wire.

## Not here yet

Tray, floating panels, deep links, autostart, the updater and the keyring
follow in WP8 of the plan. Linux and Windows keep their native title bar;
macOS gets the overlay title bar the Swift windows have. The page's traffic
light inset is a WP8 design question for the other two platforms.
