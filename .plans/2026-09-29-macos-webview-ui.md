# Steno: macOS windows as web UI in WKWebView

Status: proposal, 2026-09-29. Triggered by the owner's review of rc.3 ("UI is better, but still
horrible", "still just a gray blob", Settings clipping the window) and the decision that followed:
the Mac UI moves off SwiftUI to a web UI hosted in the Swift app, and it must still feel native,
Liquid Glass included.

Binding context: [`2026-09-24-initial-scope.md`](2026-09-24-initial-scope.md) (scope, audio never
leaves the device, the LLM client sends text only) and
[`2026-09-25-macos-app-and-release.md`](2026-09-25-macos-app-and-release.md) (app target
boundaries, view models, CI and release).

This plan **amends** the scope plan's stack row ("Swift, SwiftUI") and **supersedes in part**
[`2026-09-28-macos-visual-redesign.md`](2026-09-28-macos-visual-redesign.md) (layout steps 3 to 13
and the Mac side of the token mirror),
[`2026-09-28-settings-redesign.md`](2026-09-28-settings-redesign.md) (the SwiftUI layout spec; the
sections, copy, view models and update feed stand) and the window rows of the macOS app plan. All
four files carry a note at the top. The mockups that set the direction are
[`docs/design/webview-mockup/main.html`](../docs/design/webview-mockup/main.html) and
[`docs/design/webview-mockup/settings.html`](../docs/design/webview-mockup/settings.html);
open them in a browser, append `?dark` for the dark appearance.

## Goal

The main window, the Settings window and the onboarding window are one React application rendered
by `WKWebView` inside the existing Swift app. Everything that touches the operating system, the
audio path, the database, Sparkle or the iPhone stays Swift and stays where it is. The result
looks and behaves like a macOS 26 app: a floating Liquid Glass sidebar over the desktop, system
typography, the system accent colour, native menus, native switches and pop-up buttons, native
scrolling, correct light and dark appearance, Reduce Transparency and Reduce Motion honoured. On
macOS 15 the same UI runs over the classic sidebar material.

The second goal is the loop. The web UI builds, renders and screenshots on Linux in seconds with
Playwright, so a design iteration no longer needs a Mac build. The macOS 26 bugs that hosted
macOS 15 CI cannot show (the Settings split view overflow, PR #134) cannot happen in a layout
engine we control.

## Findings

Why SwiftUI is being replaced rather than fixed again, from the 2026-09-28 and 2026-09-29 rounds:

- Three layout defects in rc.3 (banner wrapping one word per line, the nav column squeezed to
  140 pt, the Settings columns overflowing the window on macOS 26) each cost a Forge round trip
  and depended on modifier order or a hard-coded height (`SettingsView.height`).
- The hosted UI smoke job runs macOS 15, so macOS 26 layout is never reviewed before the owner
  sees it. Forge (macOS 26) exists but is the release runner, not a design loop.
- The design-craft token ladder (3 percent alpha veils, hairlines, one achromatic accent) on a
  light canvas reads as grey on grey. The owner's "gray blob" verdict holds on `main` after PR
  #135. A toolkit change alone would not fix that; this plan changes the design direction too.
- Expo has no macOS platform; `react-native-macos` is at React Native 0.81 against upstream 0.87
  and Expo's own team names that lag as the blocker for desktop support. React Native on the Mac
  would still render AppKit views and inherit their layout problems. The web target is the way to
  reuse the React and Tailwind stack that works for the iPhone app.
- `NSGlassEffectView` is public AppKit in macOS 26 (styles `.regular` and `.clear`, `cornerRadius`,
  `tintColor`, `contentView`). A transparent `WKWebView` over it is the pattern hosted-content apps
  use for glass chrome; Apple's own Terminal ships "glass chrome, opaque content".
- WebKit renders native controls for `<select>` (a real `NSMenu` pop-up in `WKWebView`),
  `<input type="checkbox" switch>` (the system switch, WebKit since Safari 17.4, so macOS 14.4+),
  `<button>` and `<input type="search">`. `-apple-system` is SF Pro. CSS system colours
  (`Canvas`, `CanvasText`, `AccentColor`, the `-apple-system-*` label and fill colours) follow the
  window appearance and the user's accent. `prefers-color-scheme`, `prefers-reduced-motion` and
  `prefers-reduced-transparency` follow the system.

## Non-goals

- No Electron, Tauri or a second process. One app bundle, one process, WebKit from the system.
- No network from the web layer, ever. Content security policy and the navigation delegate block
  it; the LLM client, the Obsidian adapter and the iPhone listener stay in Swift.
- No change to the product: same windows, same sections, same copy unless a step says so, no new
  data, no editing of generated text.
- The menu bar item, the floating recording bubble and the detection prompt stay SwiftUI in this
  plan. They are small, animation-heavy and already accepted. Revisit only if the split hurts.
- No shared component library with `mobile/` yet. The phone app has three screens; sharing the
  Tailwind token names and the spacing scale is enough. A workspace package is a later plan.
- No support below macOS 15 and no Intel, unchanged.

## Decisions

1. **Architecture: Swift shell, web pixels.** `apps/macos/Steno` keeps `AppController`,
   `AppEnvironment`, every view model, `RecordingController`, the panels, the menu bar item,
   Sparkle and the commands. A new `Steno/Web/` group holds the host: `WebWindowView`
   (`NSViewRepresentable` for a container view with the glass view at the back and the web view in
   front), `WebBridge` (message runtime), `AppSchemeHandler` (serves the bundle) and one
   `*Bridge.swift` adapter per window that maps view model state to snapshots and commands to view
   model calls. The three SwiftUI `Window` scenes stay; their content becomes `WebWindowView`.
   Settings changes from the `Settings` scene to a `Window(id: "settings")` with a hidden title
   bar, fixed 760 by 520, so it gets the same glass sidebar and no `NavigationSplitView`.
   `⌘,` keeps working through `AppCommands`.

2. **Web stack.** `apps/macos/web/`: React 19, TypeScript, Vite, Tailwind 4, Biome as the only
   linter and formatter with the `mobile/` rules (tabs, double quotes, sorted imports and classes),
   Vitest with Testing Library, Playwright for screenshots. pnpm 11 and Node 24 as in `mobile/`,
   own lockfile, own `package.json` `packageManager`. `pnpm check` runs lint, typecheck and tests.
   Files kebab-case, components PascalCase, functional components only.

3. **Glass.** The web page is transparent (`drawsBackground` false on the web view,
   `underPageBackgroundColor` clear, `html { background: transparent }`). The sidebar region draws
   nothing behind its rows; the content region paints `Canvas`. Behind the web view the host
   places one glass view in the sidebar's frame: `NSGlassEffectView(style: .regular)` with a 14 pt
   corner radius on macOS 26, `NSVisualEffectView(material: .sidebar, blendingMode:
   .behindWindow)` on macOS 15. The web layer reports the sidebar frame through the bridge
   (`layout.sidebar`) whenever it changes; the host resizes the glass view. Reduce Transparency is
   handled by AppKit for the material and by `prefers-reduced-transparency` for the web side, which
   then paints an opaque sidebar fill. No private `NSGlassEffectView` variants: `.regular` only,
   so the look survives macOS updates.

4. **Native controls where WebKit has them, native surfaces through the bridge otherwise.**
   Switches are `<input type="checkbox" switch>`, choices are `<select>`, standard buttons are
   `<button>` with the default appearance, search is `<input type="search">`. Context menus,
   pull-down menus with more than a flat list, confirmations, open and save panels, "Reveal in
   Finder", opening URLs and the Sparkle check all go through bridge calls that end in `NSMenu`,
   `NSAlert`, `NSOpenPanel`, `NSWorkspace` and `SPUUpdater`. Tooltips are `title` attributes.
   Text is `-apple-system` at the system sizes (13 pt body, 11 pt secondary, 22 to 28 pt titles).
   Colours are CSS system colours plus a short semantic layer in `theme.css`; the accent is
   `AccentColor` so it follows System Settings. Icons are SF Symbols: the scheme handler serves
   `steno-app://symbol/<name>?pointSize=&weight=` as a template PNG rendered with
   `NSImage(systemSymbolName:)`, and CSS applies it as a mask so it takes `currentColor`.

5. **Bridge.** One channel each way. Web to Swift: `WKScriptMessageHandlerWithReply`, so
   `bridge.call("meeting.setSpeaker", params)` returns a promise that resolves with the reply or
   rejects with a typed error. Swift to web: `evaluateJavaScript("steno.emit(<event>)")` with the
   event serialised once per change. The contract is JSON with `Codable` structs in
   `Steno/Web/BridgeContract.swift` as the source of truth and hand-written TypeScript types in
   `web/src/bridge/contract.ts`. `BridgeContractTests` writes every message type as a fixture to
   `apps/macos/web/fixtures/bridge/` when `STENO_RECORD_FIXTURES=1` and otherwise asserts the
   fixtures still decode; the web tests parse the same files through zod schemas. A drift fails
   both sides.

6. **State model: snapshots in, commands out.** The web layer holds no domain state. Each
   window bridge observes its view models with `withObservationTracking` and emits full snapshots
   per topic (`meetings.list`, `meeting.detail`, `recording`, `progress`, `settings.general` and
   the other five sections, `onboarding`, `setup`). Snapshots are coalesced to the next run loop
   turn and, for `recording.level`, throttled to 20 Hz on the main actor; nothing on the audio
   thread changes. Commands call the existing view model methods, so the view model tests keep
   their value and nothing is re-implemented in TypeScript.

7. **Loading and isolation.** Production loads `steno-app://app/index.html` through
   `WKURLSchemeHandler` from the bundled `Web/` folder, never `file://`. Content security policy:
   `default-src 'none'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:
   blob:; font-src 'self'; connect-src 'none'`. The navigation delegate cancels every navigation
   that is not the app scheme; external links go through `system.openURL`. In Debug, when
   `STENO_WEB_DEV_URL` is set, the host loads the Vite dev server instead, with hot reload inside
   the real window, and the policy admits that origin and its websocket. Text selection, copy and
   find work as in any web view. The web view has no access to the file system, the keychain or
   the network, so the privacy rule is enforced by the platform, not by review.

8. **Design direction: a macOS 26 app, not the design-craft ladder.** The mockups define it:
   floating glass sidebar with the Record control as a capsule at the top under the traffic
   lights, a plain list column with grouped day headers and accent-filled selection, a detail
   column with a 680 pt reading measure, a large title, a segmented control for Summary,
   Transcript, Tasks and Notes, glass capsule toolbar buttons floating over the content, and
   grouped form cards in Settings. Spacing and radii keep the shared scale (4, 8, 12, 16; 6, 8,
   10, 14). `Theme.swift` shrinks to what the menu bar item and the panels still use; the
   `ThemeTokensTests` mirror check against `global.css` is dropped for the Mac.

9. **Build.** `scripts/build-web.sh` runs `pnpm install --frozen-lockfile` and `pnpm build` in
   `apps/macos/web/` and copies `dist/` to the app's resources as `Web/`. `project.yml` gets it as
   a pre-build script with `dist/` as its output, so Xcode reruns it when the web sources change
   and the script fails with a clear message when `pnpm` is missing. CI installs Node 24 and pnpm
   on the macOS jobs the way `mobile-cd.yml` does. Forge needs the same two tools; that is a
   prerequisite, see Open questions.

10. **Tests and review evidence.** `web-ci.yml` on `ubuntu-latest`: `pnpm check`, `pnpm build`,
    then Playwright renders every window in light and dark at 960 by 600 and 1200 by 760 against
    the fixture bridge and uploads the PNGs as the `web-screens` artifact. That artifact replaces
    the `ui-smoke` attachments as the review evidence for layout. The macOS `ui-smoke` job keeps
    one XCUITest per window that launches the app in the UI-test environment, waits for the web
    view to report `ready`, and asserts through `WebShellTests` (an in-process `WKWebView` with the
    bundled UI and a fake bridge) that the fixture meeting renders and that the glass view sits
    exactly under the sidebar frame. Unit tests for the bridges cover snapshot shape and command
    routing. The SwiftUI accessibility identifiers (`meeting-<uuid>`, `nav-all`, `tab-summary`,
    `settings-<section>`, the onboarding ids) become `data-testid` attributes and are asserted
    by Playwright and `WebShellTests`; XCUITest is not relied on for elements inside the web
    view because WebKit exposes HTML through `app.webViews` by role and label, not by
    identifier. WP1 spikes that once and records what XCUITest can see.

11. **Rollout, window by window, each PR deleting what it replaces.** rc.4 ships from `main`
    first with the three SwiftUI fixes, so the owner reviews the current state while this lands.
    Then WP0 to WP5 below. The release after WP5 is 0.10.0.

12. **Deviation rule.** Anything that turns out to need a private API, a network origin or a
    second process is recorded as a deviation entry here before it is written.

## Bridge contract, first cut

Topics the web layer subscribes to and the view model each maps from. Names are final; fields
follow the view models and are pinned by the fixtures in WP0.

| Topic | Source | Contents |
|---|---|---|
| `app` | `AppController` | version, appearance, permissions summary, requested section or meeting (deep links), setup banner state |
| `recording` | `RecordingController` via `RecordingControlPresentation` | state, elapsed, source, device, level at 20 Hz, auto-stop countdown, denied permissions |
| `progress` | `ProcessingProgressModel` | queue and per-meeting stage |
| `meetings.list` | `MeetingListViewModel` | filters, counts, tags, grouped rows, selection |
| `meeting.detail` | `MeetingDetailViewModel` and `SpeakersViewModel` | header facts, speaker rows with suggestions and playback state, tags, templates, retention, summary sections, transcript, tasks, notes, export status, error |
| `settings.<section>` | the six Settings view models and `SettingsOverviewViewModel` | the section's fields, statuses, errors, sidebar subtitles; the pairing QR crosses as PNG data |
| `onboarding` | `OnboardingViewModel` | page, permissions, choices |

Commands mirror the view models' public methods one to one (`meetings.setFilter`,
`meetings.delete`, `meeting.selectSpeaker`, `meeting.setKeepAudio`, `meeting.reexport`,
`meeting.saveScratchpad` with the debounce kept in the view model, `speakers.options(query)`
with a reply, `speakers.play` and `speakers.stop` (playback stays `AVAudioPlayer`),
`recording.start`, `recording.stop`, `settings.general.setLaunchAtLogin` and so on). Native
surfaces: `ui.contextMenu`, `ui.confirm`, `ui.openPanel`, `system.openURL`,
`system.revealInFinder`, `system.openSystemSettings(kind)`, `updates.check`,
`window.open(settings | onboarding | main, section?)`, `window.close(onboarding)`,
`layout.sidebar`. Events from the menu bar commands reach the page as `ui.focusSearch` (⌘F)
and the existing deep links as fields of `app`. Sheets and popovers (speaker picker,
acknowledgements, pairing QR) become in-page dialogs. The UI-test launch flags in
`UITestScenario.swift` and the preview seed are unchanged; they still drive the fixtures.

## Implementation steps

WP0, scaffold and contract (no visible change):
1. `apps/macos/web/` with Vite, React, Tailwind 4, Biome, Vitest, Playwright; `theme.css` with the
   system-colour layer; `pnpm check` green; `web-ci.yml`.
2. `BridgeContract.swift`, `contract.ts`, the fixture round trip and `BridgeContractTests`.
3. `scripts/build-web.sh`, the `project.yml` script phase, Node and pnpm on the macOS CI jobs.

WP1, host:
4. `AppSchemeHandler` (bundle files, SF Symbol endpoint, CSP headers), `WebBridge`,
   `WebWindowView` with the glass view under a transparent web view, the navigation delegate,
   the Debug dev-server switch. A Debug-only menu item opens an empty web window to prove the
   pipeline on macOS 15 and 26 (Forge dump of the view tree as in PR #134). Spike in the same
   PR: what XCUITest sees inside the web view (`app.webViews`, roles, labels) and whether a
   transparent web view over `NSGlassEffectView` renders without a first-paint flash.

WP2, main window:
5. Sidebar, list and detail in React from the mockup; `MainWindowBridge`; the SwiftUI files under
   `Main/` and the parts of `Design/` only they used are deleted in the same PR; `ui-smoke` and the
   Playwright screens updated.

WP3, Settings:
6. The six sections as grouped cards; `SettingsBridge`; the `Settings` scene becomes a `Window`;
   `Settings/*View*.swift` and `SettingsComponents.swift` deleted; `SettingsRedesignTests` reduced
   to the view model parts.

WP4, onboarding:
7. Pages from `OnboardingView.swift` rebuilt; `OnboardingBridge`; SwiftUI onboarding deleted.

WP5, cleanup:
8. `Theme.swift` trimmed to the menu bar and panel needs, `ThemeTokensTests` reduced, the README
   and `AGENTS.md` workspace table updated, deviations recorded here, version 0.10.0.

## Verification

- `pnpm check` and `web-ci.yml` green; the `web-screens` artifact reviewed for every PR from WP2 on.
- `swift format lint --strict`, `app` and `ui-smoke` green; `WebShellTests` pass on macOS 15 CI
  and on Forge (macOS 26).
- A Forge view-tree dump for each window shows the glass view frame equal to the sidebar frame
  reported over the bridge, in both appearances and with Reduce Transparency on.
- The navigation delegate test proves an `https:` navigation from the page is cancelled and that
  `connect-src 'none'` blocks `fetch`.
- `AudioNeverLeavesTests`-style check: the web bundle contains no absolute `http` or `https` URL.
- The owner reviews rc.4 before WP2, and a 0.10.0 pre-release after WP3.

## Open questions

- Forge tooling: Node 24 and pnpm 11 must be on the runner before WP1 merges. Nix is already
  used for the Swift toolchain container; the same flake can pin both.
- Whether the menu bar popover should follow in a later plan once the web components exist. The
  mockup deliberately leaves it native.
- `drawsBackground` on `WKWebView` is set through key-value coding; it is the established way to
  get a transparent web view on macOS and Steno ships outside the App Store. If a macOS release
  removes it, the fallback is an opaque sidebar fill sampled from the desktop, recorded as a
  deviation.
- SF Symbols served as raster masks scale with the backing factor; if a symbol needs multicolour
  rendering the endpoint gains a `?rendering=` parameter.
