# Steno: the Mac windows as a local web app with Steno's own design language

Status: proposal, 2026-09-29, revised 2026-09-30 (third draft). Triggered by the review of rc.3
("still horrible", "still just a gray blob", Settings clipping the window). The first draft
imitated macOS chrome in CSS and was rejected as "fake macOS"; the second kept system SwiftUI
chrome with a web reading pane and was found too plain. The owner then had the Jamie binary
analysed, saw that Jamie is a web app in a WebKit view with its own design language, and chose
that path with one condition: everything is served locally, and the product works offline apart
from the summary call.

Binding context: [`2026-09-24-initial-scope.md`](2026-09-24-initial-scope.md) (scope, audio never
leaves the device, the LLM client sends text only) and
[`2026-09-25-macos-app-and-release.md`](2026-09-25-macos-app-and-release.md) (app target
boundaries, view models, CI and release).

This plan **amends** the scope plan's stack row and **supersedes in part**
[`2026-09-28-macos-visual-redesign.md`](2026-09-28-macos-visual-redesign.md) (layout steps 3 to 13
and the Mac side of the token mirror),
[`2026-09-28-settings-redesign.md`](2026-09-28-settings-redesign.md) (the SwiftUI layout spec; the
sections, subtitles, copy, view models and update feed stand) and the window rows of the macOS
app plan. All four files carry a note at the top. The direction mockup is
[`docs/design/webview-mockup/steno-main.html`](../docs/design/webview-mockup/steno-main.html)
(append `?dark`).

## Goal

The main window, the Settings window and the onboarding window are one React application with
Steno's own design language, rendered by `WKWebView` inside the existing Swift app and served
from the app bundle. It does not imitate macOS and it does not pretend to be a website. The
Swift side keeps everything that touches the operating system, the audio path, the database,
Sparkle and the iPhone. The app works with the network cable pulled: recording, transcription,
diarisation, export to the vault, every screen. Only the summary call to the configured LLM
needs a connection, and that call stays in `StenoLLM` as it is today.

The second goal is the loop: the UI builds, renders and screenshots on Linux in seconds with
Playwright, so design iterates without a Mac build, and macOS-version layout bugs cannot happen
in a layout engine Steno controls.

## Findings

Why this direction, from the 2026-09-29 and 2026-09-30 rounds and the Jamie analysis:

- The grey blob was the design-craft layer (alpha veils, hairlines, achromatic accent) painted
  over SwiftUI at low contrast, and the rc.3 defects were custom layout fighting
  `NavigationSplitView`. Each fix cost a Forge round trip because hosted CI runs macOS 15 and
  the owner runs macOS 26.
- Imitated chrome reads as fake. The first draft's mockups drew traffic lights, glass, switches
  and segmented controls in CSS and looked like a Linux theme of macOS. System chrome with a web
  pane (second draft) looked like Notes. Neither is the product the owner wants.
- Jamie 5.7.21 (their public GitHub releases repo, 2026-09-22) is Tauri 2.10 in Rust with wry
  over the system WebKit, no Electron, no Swift, no SwiftUI. Its UI is Next.js and React with
  Tailwind CSS v4, PP Neue Montreal and Inter, Lucide icons, cmdk, Slate, in a hidden-title-bar
  window with an `NSVisualEffectView` behind the web view. The main window loads their hosted
  web app from `app.meetjamie.ai`; only helper pages are bundled. A separate C++ sidecar captures
  audio with CoreAudio taps and ScreenCaptureKit and streams it to their servers. Jamie looks
  designed because it commits to one identity and never imitates AppKit.
- Expo has no macOS platform and `react-native-macos` lags upstream by six minor versions; the
  web target is the way to reuse the React and Tailwind stack that works for the iPhone app.
- `WKWebView` gives Steno what Jamie gets from wry with none of the remote dependency: a
  `WKURLSchemeHandler` serves the bundle with no port and no network, `-apple-system` and bundled
  woff2 fonts both work, `prefers-color-scheme` follows the window appearance, scrolling, text
  selection and find are native, and the bridge is `WKScriptMessageHandlerWithReply`.

## Non-goals

- No imitation of macOS controls or materials in CSS. No fake glass, no fake switches.
- No remote UI, no CDN, no web fonts from the network, no analytics. `connect-src 'none'`.
- No Electron, Tauri, Node at runtime or a localhost HTTP server. One process, WebKit from the
  system, assets from the bundle through a URL scheme handler.
- No product change: same windows, sections, data and editable surfaces. Copy may improve where
  a step says so.
- The menu bar item, the floating recording bubble and the detection prompt stay SwiftUI. They
  are small, animation-heavy and accepted; they adopt the web language's colours and type in a
  later plan if the seam shows.
- No shared component package with `mobile/` yet; the Mac web app shares the token names, the
  typeface and the motion tokens by convention. A workspace package is a later plan.
- No support below macOS 15, no Intel, unchanged.

## Decisions

1. **Architecture: Swift shell, web pixels.** `apps/macos/Steno` keeps `AppController`,
   `AppEnvironment`, every view model, `RecordingController`, the panels, the menu bar item,
   Sparkle, the handover listener and the commands. A new `Steno/Web/` group holds the host:
   `WebWindowView` (`NSViewRepresentable` around one `WKWebView`), `WebBridge` (message
   runtime), `AppSchemeHandler` (serves the bundle) and one `*Bridge.swift` per window that maps
   view model state to snapshots and commands to view model calls. The three `Window` scenes stay
   with `.windowStyle(.hiddenTitleBar)`; their content becomes `WebWindowView` at a route. The
   `Settings` scene becomes `Window(id: "settings")` at `/settings`, fixed 760 by 520, opened by
   `⌘,` through `AppCommands` and by `window.open("settings", section)` from the page. Onboarding
   is `/onboarding`. Traffic lights are the only native pixels in the three windows; the page
   leaves them a 52 pt inset. The window background colour is set from the page's canvas token
   on appearance change so resizing never flashes.

2. **Design language.** Steno's, derived from what already exists, not from macOS and not from
   Jamie. The mockup fixes it (second pass, accepted as direction on 2026-09-30):
   - Structure: a charcoal sidebar that is the app icon's plate (`#1A1A1D` to `#121214`, white
     text at 92, 56 and 34 percent), warm paper for the meeting list (`#F6F4EF`), a near-white
     canvas for reading (`#FDFCFA`). Dark: `#101012`, `#17171A`, `#1D1D21`. The three columns
     are three materials, so the eye knows where it is without borders doing the work.
   - Type: Geist, the phone app's typeface, for the interface at 13, 12 and 11; Geist Mono with
     tabular numerals for timestamps and durations; Instrument Serif for the column title, the
     meeting title (44) and the summary section headings (24), which gives the reading side an
     editorial voice without touching the controls. All three are SIL Open Font License and are
     bundled as woff2.
   - Colour: a warm achromatic ladder with real contrast (light ink `#1C1B18`, `#6B675E`,
     `#9D988C`; lines `#E8E4DC`, `#D3CEC3`). One accent, the icon's live green (`#62B06F`,
     `#3F8A4C`), for recording and confirmation states only; warning `#8A6512` on `#D4A72C` at
     16 percent; destructive `#D05252`. People carry a fixed muted palette (`#B3573E`,
     `#B9862A`, `#5F7F6B`, `#6D6F8E`, `#A56A8A`) assigned per person, so avatars and task owners
     are the only saturated marks on the page. Meeting kinds are a coloured dot, not a chip.
   - Surfaces: the Record control is a white capsule with a live dot and a real shadow on the
     plate; selection is a raised white card on the paper; tasks are cards on the canvas;
     popovers carry a real shadow. Radii 6, 10, 14 and full. No alpha veils under 6 percent, no
     hairlines under 1 px.
   - Components: Steno's own, built on Radix primitives for accessibility (menus, popovers,
     dialogs, selects, tooltips, tabs), styled with Tailwind 4, Lucide icons at 1.6 stroke.
     Underline tabs with counts, pill filters, avatar stacks, a callout for setup notices, a
     search field with its shortcut shown.
   - Motion: the phone app's tokens ported to CSS (`150 ms` functional, `120 ms` exit, `250 ms`
     entrance, one spatial spring), `prefers-reduced-motion` honoured.
   - Copy unchanged in tone: no developer vocabulary, sentences not labels.

3. **Served locally, offline by construction.** Production loads `steno-app://app/index.html`
   through a `WKURLSchemeHandler` from the bundled `Web/` folder. Not `file://` (relative URLs and
   CSP behave badly) and not a localhost HTTP server (opens a port to every process on the Mac
   and needs a server to keep alive). Content security policy: `default-src 'none'; script-src
   'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; font-src 'self';
   connect-src 'none'`. The navigation delegate cancels every navigation that is not the app
   scheme; external links go through `system.openURL`. Fonts, icons and all assets are in the
   bundle. In Debug, `STENO_WEB_DEV_URL` loads the Vite dev server with hot reload inside the
   real window and the policy admits that origin. The web view has no file, keychain or network
   access; the privacy rule and the offline rule are enforced by the platform, not by review.

4. **Web stack.** `apps/macos/web/`: React 19, TypeScript, Vite, Tailwind 4, Radix primitives,
   Lucide, Biome as the only linter and formatter with the `mobile/` rules (tabs, double quotes,
   sorted imports and classes), Vitest with Testing Library, Playwright for screenshots, pnpm 11
   and Node 24 as in `mobile/`, own lockfile, `packageManager` pinned. `pnpm check` runs lint,
   typecheck and tests. Files kebab-case, components PascalCase, functional components only.
   Tokens live in `src/theme.css` with the names from `mobile/global.css` where they overlap.

5. **Bridge.** Web to Swift through `WKScriptMessageHandlerWithReply`: `bridge.call(method,
   params)` returns a promise that resolves with the reply or rejects with a typed error. Swift
   to web through `evaluateJavaScript("steno.emit(...)")`, one call per coalesced change. The
   contract is `Codable` structs in `Steno/Web/BridgeContract.swift` as the source of truth and
   hand-written TypeScript types in `web/src/bridge/contract.ts`. `BridgeContractTests` records
   every message type to `apps/macos/web/fixtures/bridge/` under `STENO_RECORD_FIXTURES=1` and
   asserts the fixtures still decode otherwise; the web tests parse the same files through zod.
   A drift fails both sides.

6. **State model: snapshots in, commands out.** The page holds no domain state. Each window
   bridge observes its view models with `withObservationTracking` and emits full snapshots per
   topic, coalesced to the next run loop turn; `recording.level` is throttled to 20 Hz on the
   main actor. Nothing on the audio thread changes. Commands call the existing view model
   methods, so the view model tests keep their value and no logic is re-implemented in
   TypeScript.

   | Topic | Source | Contents |
   |---|---|---|
   | `app` | `AppController` | version, appearance, deep links (requested meeting or section), setup banner state, iPhone sync status |
   | `recording` | `RecordingController` via `RecordingControlPresentation` | state, elapsed, mode, device, level at 20 Hz, auto-stop countdown, denied permissions |
   | `progress` | `ProcessingProgressModel` | queue and per-meeting stage, fraction, estimate |
   | `meetings.list` | `MeetingListViewModel` | filters, counts, tags, grouped rows with preview and speakers, selection |
   | `meeting.detail` | `MeetingDetailViewModel`, `SpeakersViewModel` | header facts, speaker rows with options and playback state, tags, templates, retention, summary sections, transcript turns, tasks, notes, export status, error |
   | `settings.<section>` | the six Settings view models, `SettingsOverviewViewModel` | fields, statuses, errors, sidebar subtitles; the pairing QR as PNG data |
   | `onboarding` | `OnboardingViewModel` | page, permission steps, setup steps |

   Commands mirror the view models' public methods one to one (`meetings.setFilter`,
   `meetings.delete`, `meeting.selectSpeaker`, `meeting.setKeepAudio`, `meeting.reexport`,
   `meeting.saveScratchpad` with the debounce kept in the view model, `speakers.options(query)`
   with a reply, `speakers.play` and `speakers.stop` on `AVAudioPlayer`, `recording.start`,
   `recording.stop`, `settings.general.setLaunchAtLogin` and so on). Native surfaces the page
   cannot draw: `ui.openPanel` (`NSOpenPanel` for folders), `ui.confirmDestructive`
   (`NSAlert` for deleting a meeting or a recording; everything else is an in-page dialog),
   `system.openURL`, `system.revealInFinder`, `system.openSystemSettings(kind)`,
   `updates.check`, `window.open(main | settings | onboarding, section?)`,
   `window.close(onboarding)`. Menu bar commands reach the page as events (`ui.focusSearch` for
   ⌘F, `recording.toggle` for ⌘⇧R). The UI-test launch flags in `UITestScenario.swift` and the
   preview seed are unchanged and drive the fixtures.

7. **Build.** `scripts/build-web.sh` runs `pnpm install --frozen-lockfile` and `pnpm build` in
   `apps/macos/web/` and copies `dist/` into the app resources as `Web/`. `project.yml` runs it as
   a pre-build script with `dist/` as output and a clear failure when `pnpm` is missing, so Xcode
   reruns it when web sources change. CI installs Node 24 and pnpm on the macOS jobs as
   `mobile-cd.yml` does. Forge needs both.

8. **Tests and review evidence.** `web-ci.yml` on `ubuntu-latest`: `pnpm check`, `pnpm build`,
   then Playwright renders every window and state in light and dark at 960 by 600 and 1200 by 760
   against the fixture bridge, asserts that the page made zero network requests, and uploads the
   PNGs as `web-screens`. That artifact replaces the `ui-smoke` attachments as the layout
   evidence. The macOS `ui-smoke` job keeps one XCUITest per window that launches the app in the
   UI-test environment and waits for `app.ready`; `WebShellTests` loads the bundled page in an
   in-process `WKWebView` with a fake bridge and asserts through JavaScript that the fixture
   meeting renders, that an `https:` navigation is cancelled and that `fetch` is blocked. The
   SwiftUI accessibility identifiers become `data-testid` attributes asserted by Playwright and
   `WebShellTests`; XCUITest is not relied on inside the web view (WebKit exposes HTML by role
   and label, not identifier). Unit tests for the bridges cover snapshot shape and command
   routing.

9. **Offline verification.** One Playwright run and one `WebShellTests` run execute with
   networking disabled at the process level; both must pass unchanged. The web bundle is grepped
   for absolute `http` and `https` URLs and must contain none. The existing `AudioNeverLeaves`
   style check covers the Swift side as before.

10. **Rollout, window by window, each PR deleting what it replaces.** rc.4 ships from `main`
    first with the three merged SwiftUI fixes. Then WP0 to WP5 below. The release after WP5 is
    0.10.0.

11. **Deviation rule.** Anything that turns out to need a private API, a network origin, a
    localhost port, a second process or a CSS imitation of a macOS control is recorded here as a
    deviation before it is written.

## Implementation steps

WP0, scaffold and contract (no visible change):
1. `apps/macos/web/` with the stack in Decision 4, `theme.css` with the tokens in Decision 2,
   bundled Geist and Geist Mono, the component set from the mockup with stories rendered by
   Playwright; `pnpm check` green; `web-ci.yml`.
2. `BridgeContract.swift`, `contract.ts`, fixtures and `BridgeContractTests`.
3. `scripts/build-web.sh`, the `project.yml` script phase, Node and pnpm on the macOS CI jobs
   and on Forge.

WP1, host:
4. `AppSchemeHandler` (bundle files, CSP headers), `WebBridge`, `WebWindowView`, the navigation
   delegate, the Debug dev-server switch, the window background colour follow. A Debug-only menu
   item opens an empty web window to prove the pipeline on macOS 15 and 26 (Forge view-tree dump
   as in PR #134). Spike in the same PR: first-paint behaviour and what XCUITest sees.

WP2, main window:
5. Sidebar, list and detail in React from the mockup; `MainWindowBridge`; `Main/`, `Design/`
   symbols only the windows used, `SpeakerPicker` and `SetupBanner` deleted in the same PR;
   Playwright screens and `WebShellTests` cover the states the deleted views covered.

WP3, Settings:
6. The six sections in the web language (sidebar of sections with subtitles, form cards with
   Steno's controls); `SettingsBridge`; the `Settings` scene becomes a `Window`;
   `Settings/*View*.swift` and `SettingsComponents.swift` deleted; `SettingsRedesignTests`
   reduced to the view model parts.

WP4, onboarding:
7. Pages from `OnboardingView.swift` rebuilt; `OnboardingBridge`; SwiftUI onboarding deleted.

WP5, cleanup:
8. `Theme.swift` trimmed to the menu bar and panel needs, `ThemeTokensTests` reduced, README and
   `AGENTS.md` workspace table updated, deviations recorded here, version 0.10.0.

## Verification

- `pnpm check` and `web-ci.yml` green; `web-screens` reviewed for every PR from WP2 on.
- `swift format lint --strict`, `app` and `ui-smoke` green; `WebShellTests` pass on macOS 15 CI
  and on Forge (macOS 26).
- The offline runs in Decision 9 pass; the bundle contains no absolute network URL.
- The owner reviews rc.4 before WP2, and a 0.10.0 pre-release after WP3.

## Open questions

- Forge tooling: Node 24 and pnpm 11 before WP1 merges; the flake can pin both.
- Whether the menu bar popover should follow into the web language once the components exist.
  Deliberately left native here.
- `drawsBackground` on `WKWebView` is key-value coded; it is the established way to a
  non-flashing transparent web view on macOS and Steno ships outside the App Store. If a macOS
  release removes it, the window background colour follow in Decision 1 is the fallback.
- Geist is licensed under the SIL Open Font License, so bundling is fine; the licence file ships
  in `Web/fonts/`.
