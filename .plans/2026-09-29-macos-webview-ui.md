# Steno: system chrome, web content

Status: proposal, 2026-09-29, revised 2026-09-30 after the owner rejected the first draft's
mockups ("fake macOS, like all of those Linux distros"). Triggered by the review of rc.3 ("still
horrible", "still just a gray blob", Settings clipping the window).

Binding context: [`2026-09-24-initial-scope.md`](2026-09-24-initial-scope.md) (scope, audio never
leaves the device, the LLM client sends text only) and
[`2026-09-25-macos-app-and-release.md`](2026-09-25-macos-app-and-release.md) (app target
boundaries, view models, CI and release).

This plan **amends** the scope plan's stack row and **supersedes in part**
[`2026-09-28-macos-visual-redesign.md`](2026-09-28-macos-visual-redesign.md) (the custom
components and tokens in the three windows, layout steps 3 to 13),
[`2026-09-28-settings-redesign.md`](2026-09-28-settings-redesign.md) (the custom sidebar row and
metrics; the sections, copy, view models and update feed stand) and the window rows of the macOS
app plan. All four files carry a note at the top.

## The rule

Nothing the user reads as chrome is drawn by Steno. The window, the sidebar and its glass, the
toolbar and its buttons, the lists, the forms, the switches, pop-ups, menus, popovers and sheets
are the system's own SwiftUI and AppKit components with their default appearance. Steno draws
exactly one surface: the reading pane of a meeting (summary, transcript, tasks, notes), and that
surface is web content in a `WKWebView`, styled as a document, never as chrome.

The first draft of this plan put the whole window in the web view and imitated macOS in CSS. The
mockups looked like a Linux theme of macOS because that is what imitated chrome always looks like.
Web cannot get a real toolbar button, a real sidebar selection or a real segmented control. The
draft's mockups were removed with the revision; the direction they showed is rejected.

## Goal

After this plan the Mac app looks like Notes or Mail with Steno's content in it, on macOS 26 with
Liquid Glass and on macOS 15 with the classic materials, because every visible frame, list,
button and form is the system's. The reading pane, the one surface where typography and rich
layout carry the product, is web content with a document identity: SF Pro through the system
font stack, a 680 pt measure, generous spacing, speaker labels, inline speaker selection through
native pop-up menus, an editable notes tab. That pane builds, renders and screenshots on Linux in
seconds, so its design iterates without a Mac build.

## Findings

- The grey blob is not SwiftUI. It is the design-craft layer painted over it: `Card`, `NavRow`,
  `StenoPrimaryButtonStyle`, `StatusChip`, `SettingsSidebarRow`, 3 percent alpha veils, hairlines
  and one achromatic accent, in `apps/macos/Steno/Design/` and `Settings/SettingsComponents.swift`.
  The system components those replaced are the ones every Mac app the owner likes is made of.
- The rc.3 defects (banner wrapping, nav column at 140 pt, Settings overflow on macOS 26) all
  came from custom layout fighting `NavigationSplitView`. Standard `List(.sidebar)` content and a
  standard toolbar are what the split view is sized for.
- The reading pane is where SwiftUI is weakest for this product: long attributed text with
  speaker turns, per-line controls, hover states, selection across turns, an editable notes area.
  It is also where an LLM designs well and where a Linux screenshot loop has the most value.
- Expo has no macOS platform and `react-native-macos` lags upstream by six minor versions; the
  web target is the way to reuse the React and Tailwind stack. Recorded in the first draft, still
  true, now scoped to one pane.
- `WKWebView` inside a SwiftUI detail column is ordinary hosting: transparent background over the
  window's own background, native scrolling, native text selection, `<select>` opens a real
  `NSMenu`, `-apple-system` is SF Pro, CSS system colours and `prefers-color-scheme` follow the
  window appearance, `prefers-reduced-motion` follows the system.

## Non-goals

- No web chrome. No CSS traffic lights, glass, sidebars, toolbars, tabs, switches or buttons.
- No Electron, Tauri or second process. No network from the web layer, ever.
- No product change: same windows, sections, copy, data and editable surfaces.
- The menu bar item, the floating recording bubble and the detection prompt are untouched.
- No shared component package with `mobile/`; the pane shares Tailwind token names only.
- No support below macOS 15, no Intel, unchanged.

## Decisions

1. **Chrome is system SwiftUI with default appearance.** The main window keeps its three-column
   `NavigationSplitView` in the `.balanced` style with the system title bar and toolbar back (the
   hidden title bar goes; on macOS 26 the toolbar and sidebar glass come for free, on macOS 15 the
   standard sidebar material). The sidebar is a `List(selection:)` with `.listStyle(.sidebar)`,
   `Section("Meetings")` and `Section("Tags")` of `Label`s with badges, Settings reachable from
   the app menu and `⌘,` only, as in every Mac app. The Record control is a toolbar item in the
   sidebar column's `.toolbar` (`.primaryAction`), a `Menu` with "Record call" and "Record in
   person" and a red stop state while recording; the level meter stays in the detail header while
   recording. The meeting list is a `List` in `.inset` style with `Section` headers per day and
   the system row selection; the row shows title, time and one preview line with system text
   styles. The detail column is a `VStack`: a native header (title, meta line, speakers row,
   tags, retention line, the setup banner as a plain `HStack` of system buttons), a native
   `Picker(.segmented)` for Summary, Transcript, Tasks and Notes in the detail toolbar, then the
   web pane. Actions live in a toolbar `Menu`. Confirmations are `.confirmationDialog`, sheets and
   popovers stay native.

2. **Settings and onboarding are plain system forms.** Settings keeps the sidebar window and its
   six sections, view models and copy, but the sidebar rows are `Label`s with SF Symbols and the
   subtitle as the system secondary text, no icon wells, no custom metrics; the detail is
   `Form(.grouped)` with `Toggle`, `Picker`, `TextField`, `LabeledContent` and default buttons.
   The fixed-height workaround from PR #134 stays until measured unnecessary. Onboarding keeps its
   pages and rows on `Form` and system buttons. Nothing in either window imports `Design/`.

3. **One web surface: the reading pane.** `ReadingPaneView` (`NSViewRepresentable`) hosts one
   `WKWebView` per detail column, transparent (`drawsBackground` false, `underPageBackgroundColor`
   clear) so the window background shows through and the pane joins the native header without a
   seam. It renders the selected tab's content: summary sections, the transcript with speaker
   turns and inline speaker `<select>`s, the task list, the editable notes. The native segmented
   control drives which tab the page shows; the page never draws tabs. Scroll position is per
   tab and per meeting in the page.

4. **Document identity, not app identity.** The pane's CSS is a document: `-apple-system` at the
   system sizes (15 pt body for reading, 13 pt secondary, 11 pt captions), CSS system colours
   (`CanvasText`, `-apple-system-secondary-label`, `AccentColor`) so it follows the window's
   appearance and the user's accent, a 680 pt measure, no borders, no cards, no buttons except
   the inline controls the content needs. Empty and error states are text. Motion follows
   `prefers-reduced-motion`. The mockup is
   [`docs/design/webview-mockup/reading-pane.html`](../docs/design/webview-mockup/reading-pane.html),
   rendered on its own because it is the only thing Steno draws.

5. **Web stack.** `apps/macos/web/`: React 19, TypeScript, Vite, Tailwind 4, Biome with the
   `mobile/` rules, Vitest with Testing Library, Playwright for screenshots, pnpm 11, Node 24, own
   lockfile, `pnpm check` runs lint, typecheck and tests. Files kebab-case, components PascalCase,
   functional components only.

6. **Bridge.** Web to Swift through `WKScriptMessageHandlerWithReply` (`bridge.call(method,
   params)` returns a promise); Swift to web through `evaluateJavaScript("steno.emit(...)")`. The
   contract is `Codable` structs in `Steno/Web/BridgeContract.swift` as the source of truth and
   hand-written TypeScript types in `web/src/bridge/contract.ts`. `BridgeContractTests` records
   fixtures to `apps/macos/web/fixtures/bridge/` under `STENO_RECORD_FIXTURES=1` and asserts them
   otherwise; the web tests parse the same files through zod. Topics and commands:

   | Direction | Name | Contents |
   |---|---|---|
   | emit | `pane` | selected tab, appearance hints, the meeting id |
   | emit | `summary` | rendered sections from `MeetingDetailViewModel.summarySections`, `summaryStatus`, template id |
   | emit | `transcript` | turns with speaker id, display name, confirmed flag, options for the inline select from `SpeakersViewModel.options`, playback state |
   | emit | `tasks` | tasks with done state and assignee |
   | emit | `notes` | scratchpad text and save state |
   | call | `speakers.select(speakerID, optionID)` | maps to `SpeakersViewModel.select` |
   | call | `speakers.play(speakerID)`, `speakers.stop()` | playback stays `AVAudioPlayer` |
   | call | `notes.save(text)` | `saveScratchpad`, debounce stays in the view model |
   | call | `notes.flush()` | on tab or meeting change |
   | call | `summary.tryAgain()` | `rerunSummary` |
   | call | `system.openURL(url)` | links in content, `NSWorkspace` |
   | call | `pane.ready()` | first paint gate |

   Snapshots come from `withObservationTracking` on the detail view model, coalesced to the next
   run loop turn. Nothing on the audio thread changes.

7. **Loading and isolation.** Production loads `steno-app://pane/index.html` through a
   `WKURLSchemeHandler` from the bundled `Web/` folder, never `file://`. Content security policy:
   `default-src 'none'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:;
   font-src 'self'; connect-src 'none'`. The navigation delegate cancels every navigation that is
   not the app scheme. In Debug, `STENO_WEB_DEV_URL` loads the Vite dev server with hot reload
   inside the real window and the policy admits that origin. The web view has no file, keychain
   or network access; the privacy rule is enforced by the platform.

8. **Build.** `scripts/build-web.sh` runs `pnpm install --frozen-lockfile` and `pnpm build` in
   `apps/macos/web/` and copies `dist/` into the app resources as `Web/`. `project.yml` runs it as
   a pre-build script with `dist/` as output and a clear failure when `pnpm` is missing. CI
   installs Node 24 and pnpm on the macOS jobs as `mobile-cd.yml` does. Forge needs both.

9. **Tests and review evidence.** `web-ci.yml` on `ubuntu-latest`: `pnpm check`, `pnpm build`,
   Playwright renders the four tabs in light and dark at 440, 680 and 900 pt widths against the
   fixture bridge and uploads `web-screens`. That artifact is the review evidence for the pane.
   The macOS `ui-smoke` job keeps its screenshots for the chrome, which is now the system's and
   is reviewed for content, not pixels. `ReadingPaneTests` loads the bundled page in an
   in-process `WKWebView` with a fake bridge and asserts through JavaScript that the fixture
   meeting's summary and transcript render, that an `https:` navigation is cancelled and that
   `fetch` is blocked. The former `tab-*` and `speaker-*` accessibility identifiers move into the
   page as `data-testid` and are asserted there; XCUITest keeps the native identifiers only.

10. **Rollout.** rc.4 ships from `main` first with the three merged fixes. Then WP1 to WP4, each
    PR deleting what it replaces. The release after WP4 is 0.10.0.

11. **Deviation rule.** Anything that needs a custom-drawn control in a window, a private API, a
    network origin or a second process is recorded here as a deviation before it is written.

## Implementation steps

WP1, system chrome (pure SwiftUI, deletion-heavy, visible at once):
1. Main window: system title bar and toolbar, sidebar as `List(.sidebar)`, Record as a toolbar
   `Menu`, meeting list as `List(.inset)` with day sections, native detail header and segmented
   tab picker, actions in a toolbar `Menu`. Delete `NavigationColumn`, `NavRow`, `MeetingCard`'s
   custom drawing, `Card`, the `Steno*ButtonStyle`s, `StatusChip`, `SectionLabel` and every
   `Design/` symbol no longer referenced outside the menu bar and the panels. The four tab views
   stay SwiftUI for one more PR so the window is never without content.
2. Settings: `Label` rows, `Form(.grouped)` with default controls, `SettingsComponents.swift`
   reduced to the shared rows that remain, `SettingsRedesignTests` updated.
3. Onboarding: system forms and buttons; `OnboardingView.swift` loses its `Design/` imports.

WP2, web scaffold and contract (no visible change):
4. `apps/macos/web/` with the stack in Decision 5 and `web-ci.yml`.
5. `BridgeContract.swift`, `contract.ts`, fixtures, `BridgeContractTests`.
6. `scripts/build-web.sh`, the `project.yml` script phase, Node and pnpm on the macOS CI jobs and
   on Forge.

WP3, the reading pane:
7. `AppSchemeHandler`, `WebBridge`, `ReadingPaneView`, `ReadingPaneBridge`, the Debug dev-server
   switch; the page with the four tabs from the mockup; `Main/Tabs/*.swift` and `SpeakerPicker`'s
   in-transcript use deleted; `ReadingPaneTests`; Playwright screens; Forge check on macOS 26 that
   the transparent pane has no first-paint flash and joins the header without a seam.

WP4, cleanup:
8. `Theme.swift` trimmed to the menu bar and panel needs, `ThemeTokensTests` reduced, README and
   `AGENTS.md` workspace table updated, deviations recorded here, version 0.10.0.

## Verification

- WP1: `ui-smoke` attachments on macOS 15 and a Forge screenshot on macOS 26 show only system
  components; a grep proves no file under `Main/`, `Settings/` or `Onboarding/` imports or
  references `Design/` symbols other than `Theme.Space`.
- WP2 and WP3: `pnpm check`, `web-ci.yml` and `web-screens` green and reviewed; `swift format lint
  --strict`, `app`, `ui-smoke` and `ReadingPaneTests` green on macOS 15 CI and on Forge.
- The web bundle contains no absolute `http` or `https` URL; the CSP and navigation tests pass.
- The owner reviews rc.4 before WP1 merges, a pre-release after WP1, and 0.10.0 after WP3.

## Open questions

- Forge tooling: Node 24 and pnpm 11 before WP3 merges; the flake can pin both.
- Whether the speakers row in the native header and the inline selects in the web transcript
  should share one popover style or keep the system pop-up in the page and the native popover in
  the header. Default: each uses its platform's own control.
- `drawsBackground` on `WKWebView` is key-value coded; it is the established way to a transparent
  web view on macOS and Steno ships outside the App Store. Fallback is an opaque pane painted in
  the window background colour, recorded as a deviation.
