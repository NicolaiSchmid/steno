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

2. **Design language: T3 Code's recipe on Steno's layout.** On 2026-09-30 the owner named
   `pingdotgg/t3code` as the target look ("clean, modern, rounded, glassy"). Its recipe was read
   from `apps/web/src/index.css` and `apps/web/src/components/ui` in that repository and is
   adopted here with two Steno-specific changes: three columns instead of two, and the icon's
   live green as the accent instead of T3's blue. The mockup is `steno-main.html`
   (`?tab=summary|transcript`, `?dark`, `?menu` opens the glass actions menu).
   - Structure: a 236 pt sidebar (`sidebar` surface with a faint grain), a 320 pt meeting list
     on the `background` surface, the reading canvas on `background` with a faint grain. The
     Record control is the one primary button at the top of the sidebar, mark inside, no
     wordmark; then the Meetings filters with counts, Tags, and a footer with the paired
     iPhone card and Settings. Toolbar actions (Export, more) are glass pills floating over
     the scrolled content in a 52 pt top bar; the actions menu is a glass popover.
   - Type: the system font stack (`-apple-system, BlinkMacSystemFont, system-ui`), which is
     SF Pro in the app, and `ui-monospace` for times. No bundled fonts. Sizes 13 UI, 12 and 11
     secondary, 15 reading, 26 meeting title at 600 with tight tracking, 16 section headings.
   - Colour, light: background `#FCFCFC` (zinc-25), sidebar `#FAFAFA` (zinc-50), card and
     popover white, foreground `#27272A`, muted `#71717A`, faint `#A1A1AA`, border `#E4E4E7`,
     accent (hover) `#F4F4F5`. Dark: background `#0A0A0A`, sidebar and card the background
     mixed with 3 percent white, border white at 6 percent, accent white at 4 percent, muted
     white at 3 percent, foreground `#F5F5F5`. Primary is the live green (`#3F8A4C`, `#4C9A5A`)
     for the Record button, confirm states, checked boxes and the iPhone status dot; warning
     amber for what needs the user (unnamed speaker, missing summary); people keep a fixed
     muted palette. Meeting kinds are a coloured dot.
   - Geometry: `--radius` 10 pt with sm 6, md 8, lg 10, xl 14, 2xl 18; `--control-radius` 8 pt for
     buttons, rows and inputs; dialogs 2xl; glass pills and tag pills full. Cards are a 1 px
     border plus a 1 px 5 percent shadow. Selected rows are a white card on the surface.
   - Controls: primary button with a 1 px inner top highlight at 16 percent white and an
     extra-small shadow, hover at 90 percent, pressed scale 0.97; outline button on the popover
     surface with a 1 px 4 percent shadow (dark: a 6 percent top edge); ghost; glass. Segmented
     tabs as a pill group on the accent surface with the active tab a raised white pill.
     Switch, checkbox, select, menu, popover, dialog, tooltip and toast from the same recipe.
   - Glass: surface at 80 percent over a 12 pt blur with saturation 1.14 (dark 16 pt, 1.08), a
     10 percent foreground border, menus and popovers with a `0 16px 40px -18px` shadow at 55
     percent (dark 80 percent), dialog backdrops at 60 percent with a 4 pt blur. Falls back to
     an opaque surface when `backdrop-filter` is unavailable.
   - Motion: open and close at 200 ms with scale 0.98 and opacity through Base UI's starting
     and ending styles; the drawer curve `cubic-bezier(0.32, 0.72, 0, 1)` for surfaces that
     slide in; status indicators duty-cycled with stepped keyframes; nothing repaints
     continuously; panel motion defaults to none; `prefers-reduced-motion` honoured.
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

4. **Web stack.** `apps/macos/web/`: React 19, TypeScript, Vite, Tailwind 4, Base UI
   (`@base-ui/react`, the headless primitives T3 Code uses) with `class-variance-authority` and
   `tailwind-merge`, Lucide, Biome as the only linter and formatter with the `mobile/` rules
   (tabs, double quotes, sorted imports and classes), Vitest with Testing Library, Playwright for
   screenshots, pnpm 11 and Node 24 as in `mobile/`, own lockfile, `packageManager` pinned.
   `pnpm check` runs lint, typecheck and tests. Files kebab-case, components PascalCase,
   functional components only. `src/components/ui` owns every look: callers pick a `variant` or
   `size` and never restyle with `className`; layout classes belong on the parent (T3 Code's
   rule, enforced by a lint check in WP0). Tokens live in `src/theme.css` as semantic variables
   (`--background`, `--sidebar`, `--card`, `--popover`, `--accent`, `--border`, `--primary`,
   `--radius`, `--control-radius`, the glass and shadow tokens) with the `mobile/global.css`
   names where they overlap.

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
   the system font stack, the component set from the mockup with stories rendered by
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

## Deviations (implementation)

- WP0 (2026-09-30): the bridge contract lives in the Swift package as the `StenoBridge` module
  (`Sources/StenoBridge/`, tests in `Tests/StenoBridgeTests/`), not in
  `apps/macos/Steno/Web/BridgeContract.swift` as Decision 5 said. Pure Foundation plus
  `StenoCore` for the JSON convention, so it builds and its fixture test runs on Linux in the
  `steno-swift:6.1` container as well as on macOS CI. The app target imports it; the fixture
  directory `apps/macos/web/fixtures/bridge/` is unchanged. `BridgeSamples` holds one realistic
  value per type and is the sample the web UI's mock transport serves.
- WP1 (2026-09-30): the transparent web view relies on the key-value coded `drawsBackground`
  switch, WebKit's undocumented setter, guarded by `responds(to:)` so a WebKit without it falls
  back to painting the window background. The window background colour follow (the window's
  `backgroundColor` set from the page's canvas token) named in Decision 1 is deferred to WP5 with
  the appearance work; until then the window paints the system window background under a
  transparent page.
- WP2 (2026-09-30): three gaps carried forward. ⌘F "Find Meetings" stays disabled until the
  contract gains a host-to-page event for focusing the search field (Decision 6 names
  `ui.focusSearch`; adding it means a `StenoBridge` change and re-recorded fixtures). The `app`
  snapshot's `phone` is `nil` until the handover service exposes its paired devices
  synchronously. The fixture JSON chunks the mock transport imports are emitted into the
  production bundle as separate files that the app never loads; WP5 gates the mock behind a
  build flag so they leave the bundle.
- WP3 (2026-10-01): the contract grew where the SwiftUI window had more than the WP0 snapshots
  carried. `settings.summaries` gained a `codex` block (confirmation, sign-in, model list) in
  place of the bare `codexStatus` string, plus `defaultContextTokens`; `settings.general` gained
  template descriptions and the `acknowledgements` list that replaces the Acknowledgements
  sheet; `settings.recording` gained `folderUsage` (measuring, measured, unavailable) and the
  `retentionFootnote` sentence. New methods: `settings.general.openLoginItems`,
  `settings.recording.refreshDevices`, `settings.recording.revealFolder`, the five
  `settings.summaries.*Codex*` methods. The deep link is consumed host-side, as the main window
  consumes its meeting request: the `app` publish that carries `requestedSettingsSection`
  clears it (only once the page is ready, since nothing is published before), the page selects
  the section from that snapshot, and the sidebar subtitles refresh after every Settings
  command rather than on selection. (A page-driven `settings.showSection` round trip was tried
  first and removed in review.) Two extra fixtures
  (`settings.summaries.codex`, `settings.iphone.pairing`) feed the `codex` and `pairing`
  scenarios; the pairing fixture's PNG is a 29 by 29 placeholder shaped like a code. The six
  section titles and purpose sentences live in `src/windows/settings/sections.ts`;
  `SettingsSection.swift` keeps only the raw values, the deep-link target. `CodexConsentCard.swift` moved to `Onboarding/` instead of being deleted:
  the SwiftUI onboarding still renders it until WP4; the Settings page has its own copy of the
  consent words. Text fields on the page keep a draft and send one `update` plus a `save` when
  focus leaves, so the host's `@Observable` echo never fights a keystroke. The mock scenarios
  carry no absolute URL (the OpenAI preset's address is empty there) so the production bundle
  passes the offline grep. `pnpm screens` adds `settings-<scheme>-<state>.png` at 760 by 520.
- WP4 (2026-10-01): the onboarding window is fixed at 560 by 620 (the SwiftUI window was 560
  wide and sized to its content; a web page needs a frame, and 620 is what sits above the Dock
  on a 768-point display with the footer reachable, which a 700-tall first cut did not), with
  the page's body scrolling under a pinned footer that holds Later or Done, Back and Finish. The contract's `onboarding` snapshot grew: `permissionsComplete`, `isRequired` per
  permission step, a `summaries` block that is the Settings page's own `SummariesSettingsSnapshot`
  (its `subtitle` empty, there is no sidebar) so the ChatGPT consent card and the endpoint form
  are one React component on both pages (`src/components/codex-consent-card.tsx`; the Swift
  `CodexConsentCard.swift` and its words are gone), and a `vault` block with the chosen folder
  and why a save was refused. New methods beside the WP0 eight: `onboarding.refresh` ("Check
  again" after System Settings), `onboarding.confirmSummariesWithCodex` and `onboarding.chooseVault`;
  the Summaries form's own commands (`settings.summaries.selectPreset`, `update`, `test`,
  `refreshCodexStatus`) are answered by the onboarding bridge on its own model, so the shared
  form sends one set of names, the `NSOpenPanel` whose choice
  is saved as the vault at once (`onboarding.saveVault` stays for the retry after a refused
  folder). The Summaries row offers the same service list as Settings instead of the SwiftUI
  page's two-way provider picker, since the view model method behind that picker left in WP3. Choosing a service through `settings.summaries.selectPreset` commits the preset's
  address and model at once and probes the endpoint, as the deleted provider picker did; only
  the typed fields wait for Save.
  `system.openSystemSettings` is routed onto the onboarding model rather than duplicated as an
  `onboarding.*` method. Two pieces of view glue stay glue, in the bridge and the window: page
  1 moves on by itself once every step is handled (only after a step-changing command, so Back
  works), and the window's close button marks onboarding completed. The exit is page-driven:
  The host dismisses the window itself when the model's `finished` turns true, as the SwiftUI window did; `window.close(onboarding)` stays in the contract for the page's own use. `PermissionRow` and `DraftField` moved from `src/windows/settings/`
  to `src/components/` for both windows. The smoke tests find the window as
  `onboarding-window` and the pages by their visible words (the intro sentence, the row titles,
  the page 2 heading, the button labels); a second fixture, `onboarding.setup`, feeds the page 2
  scenarios. `pnpm screens` adds `onboarding-<scheme>-<state>.png` at 560 by 620.
