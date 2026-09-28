# Steno: floating recording indicator and detection prompt

Status: proposal, 2026-09-28, revised after the 2026-09-28 reviews. Triggered by first-run feedback (second round).

Binding plans: [`.plans/2026-09-24-initial-scope.md`](2026-09-24-initial-scope.md) (scope),
[`.plans/2026-09-25-macos-app-and-release.md`](2026-09-25-macos-app-and-release.md) (app
structure, the detection prompt as a floating `NSPanel`, tests),
[`.plans/2026-09-25-audio-capture.md`](2026-09-25-audio-capture.md) (`MeetingDetector`,
`LaneLevels`). Siblings:
[`.plans/2026-09-28-start-recording-from-main-window.md`](2026-09-28-start-recording-from-main-window.md)
owns the recorder seams this plan reads (`activeMeetingID`, `LevelBars` in
`Recording/RecordingViews.swift`) and lands first;
[`.plans/2026-09-28-device-change-during-recording.md`](2026-09-28-device-change-during-recording.md)
owns the auto-stop countdown whose bubble row this plan's states table references;
[`.plans/2026-09-28-macos-visual-redesign.md`](2026-09-28-macos-visual-redesign.md) owns the
window's tokens and components; its system PR (tokens, `Theme.Radius`, components) lands before
this plan, so the radius comes from `Theme.Radius.xl`, and this plan adds the two motion tokens it
needs (`Motion.countdown`, `Motion.pulse`), the shared `Countdown`, `CountdownHairline` and
`AutoStopPresentation`, which the redesign's layout PRs and the device-change plan reuse;
[`.plans/2026-09-28-app-icon.md`](2026-09-28-app-icon.md) decides the glyph. Index:
[`.plans/2026-09-28-first-run-feedback.md`](2026-09-28-first-run-feedback.md), step 7.

## Goal

The owner recorded a call and could not tell, from anywhere but the main window, that Steno was
recording; the menu bar item "did not seem to work". Jamie answers the same need with a small
white pill under the menu bar (app glyph, five live bars, an orange stop square) and asks
"Are you in a meeting?" from a pill of the same family. Steno gets both in one panel: a
borderless, non-activating, always-on-top `FloatingPanel` that shows the detection prompt when
another app opens the microphone and morphs, in place, into the recording bubble when Record is
clicked or when any other surface starts a recording. The bubble is the primary live indicator;
the menu bar item stays and becomes unmistakable while recording. Findings diagnoses the menu
bar report without guessing one cause.

## Findings

Line numbers are as of commit `9cd7cf5` (source identical to `bcf5eef` on `main`).

### How the prompt and the menu bar item are built today

- `apps/macos/Steno/Detection/DetectionPanel.swift:14-45`: `DetectionPanelPresenter.present`
  builds a new `NSPanel` per prompt with style mask `[.nonactivatingPanel, .titled,
  .fullSizeContentView, .utilityWindow]`, a hidden transparent title bar, level `.floating`,
  `collectionBehavior [.canJoinAllSpaces, .fullScreenAuxiliary]`, `hidesOnDeactivate false`,
  `isMovableByWindowBackground true`, `backgroundColor = Theme.popover`, a fixed 360 x 132
  frame placed at the top-right corner of `NSScreen.main.visibleFrame` inset by
  `Theme.Space.lg` (lines 36-41), then `orderFrontRegardless()`. The position is not
  remembered; the panel is discarded on dismiss (`orderOut`, line 48). The system title bar is
  why the screenshot shows a window with system corners rather than a pill.
- `DetectionPanel.swift:54-86`: `DetectionPromptView` is a card: a `liveBright` dot, "<App>
  opened the microphone" at 14 semibold, "51s" countdown text at 12 in `faint`, a body line,
  and two buttons "Not now" (secondary) and "Record" (primary). `.keyboardShortcut` on both
  buttons is inert because a non-activating panel is never key.
- `apps/macos/Steno/Detection/DetectionPromptViewModel.swift:36-54`: the model owns the
  countdown on the injected clock (`begin`, one tick per second, `close(.timedOut)`), `start`
  and `dismiss`; `remainingSeconds` is the only derived value. Nothing here changes.
- `apps/macos/Steno/Detection/DetectionController.swift:16-20, 74-88`: one prompt at a
  time, suppressed while recording, dismissed on `microphoneReleased`; the presenter is
  attached through the `promptDidChange` closure, installed in
  `apps/macos/Steno/StenoApp.swift:81`. `DetectionTests` and `AppControllerTests` read
  `controller.prompt`, never the closure.
- `apps/macos/Steno/Recording/RecordingController.swift:43-53, 131-159, 189-195`: the one
  recorder; `recording` (`.idle`, `.starting`, `.recording(since:)`, `.stopping`), `levels`
  (`LaneLevels`, 10 Hz, `mic` plus optional `system`, each `rms` and `peak` in dBFS), `stop()`
  drains the session, completes the intake and returns to `.idle`. `AppController.shutdown()`
  (`apps/macos/Steno/AppController.swift:127-133`) stops a live recording on Quit after
  `awaitSettled()`, and `AppDelegate.applicationShouldTerminate`
  (`StenoApp.swift:204-211`) returns `.terminateLater` until it finishes.
- `apps/macos/Steno/MenuBar/MenuBarView.swift:177-215`: `LevelBars` maps dBFS -60...0 onto
  0...1 (`fraction`, line 212) and animates with `Motion.functional`. The start-recording plan
  moves it to `Recording/RecordingViews.swift` and makes its fill achromatic; the bubble reuses
  `LevelBars.fraction`.
- `apps/macos/Steno/Main/MainWindow.swift:48-52`: `controller.requestedMeetingID` becomes the
  list selection; the menu bar uses it (`MenuBarView.swift:169-172`) together with
  `openWindow(id: "main")` and `NSApp.activate()`.
- `apps/macos/Steno/Design/Theme.swift:62, 67, 80-88, 116-125`: `popover` is the only opaque
  surface in both appearances (`#ffffff` / `#101010`); `border` is the hairline; `live`,
  `liveBright`, `destructive`, `destructiveForeground` are the status hues; radii stop at 8.
  `apps/macos/Steno/Design/Motion.swift`: `functional`, `exit`, `entrance`, `spatial`; no
  linear clock tempo and no ambient pulse.
- `apps/macos/project.yml:79-92` and `apps/macos/StenoTests/InfoPlistTests.swift:77`: no
  `LSUIElement`; Steno is a regular app with a Dock icon and a full application menu, which
  matters for menu bar crowding (cause B below).
- `.github/workflows/swift-ci.yml:307-362` (`ui-smoke` job): the UI smoke test runs on the hosted `macos-15`
  image with `-steno-ui-testing`, so a panel shown in the preview environment is testable
  there; `app.buttons[...]` queries reach `NSPanel` content because panels are windows of the
  same process in the accessibility tree.

### Menu bar diagnosis

`apps/macos/Steno/StenoApp.swift:36-43` declares `MenuBarExtra { RootView { MenuBarView } }
label: { MenuBarLabel }` with `.menuBarExtraStyle(.window)`. There is no `isInserted`
binding, so code never removes the item. `MenuBarLabel` (`StenoApp.swift:114-123`) reads
`bootstrap.controller?.recorder.isRecording` and shows `waveform` when idle and
`record.circle.fill` with `.symbolRenderingMode(.multicolor)` while recording. Both
`AppBootstrap` and `RecordingController` are `@Observable`, so the label is re-evaluated when
`recording` changes. The item exists in Debug ad-hoc builds exactly as in Release; signing does
not affect `NSStatusItem`. The owner's report fits several causes; each has a check that takes
under a minute.

| # | Cause | Evidence in the code or platform | Check for the owner |
|---|---|---|---|
| A | The item is there but its recording state is not noticeable. SwiftUI renders `MenuBarExtra` label images as template images, so `.multicolor` is ignored and the "recording" symbol is a black filled circle instead of the red one the code intends; at 16 pt `waveform` versus `record.circle.fill` is a shape change only, with no text | `StenoApp.swift:119-120`; template rendering of status item images is AppKit behaviour | Press ⌘⇧R with the window open and look at the right side of the menu bar for the shape change; take a screenshot of the menu bar while `Recording` shows in the window |
| B | The item is dropped by menu bar crowding. macOS hides status items that do not fit, silently and without an overflow chevron; a notched 14 inch display has about 1512 pt, Steno as a regular app contributes eight application menus when frontmost (Steno, File, Edit, View, Record, Debug in Debug builds, Window, Help), and every other status item competes | `project.yml:79-92` (regular app, no `LSUIElement`); `AppCommands` adds `Record` and `Debug` menus (`StenoApp.swift:166-193`) | `system_profiler SPDisplaysDataType \| grep -E "Resolution\|UI Looks"`; click the Desktop so only Finder's menus show and look again; connect an external display; quit two or three other menu bar apps |
| C | The item was removed by a ⌘-drag off the menu bar. macOS persists this per app as `NSStatusItem Visible <autosave> = 0` and never shows the item again until the key is deleted | Platform behaviour for every `NSStatusItem` that has an autosave name, which SwiftUI assigns | `defaults read uno.schmid.steno.mac \| grep -i NSStatusItem`; if a `Visible` key reads 0, `defaults delete uno.schmid.steno.mac "<that key>"` and relaunch |
| D | A menu bar manager (Bartender 5, Ice, Hidden Bar, Vanilla, Dozer) hides newly registered items by default | Third-party, common on developer Macs | `ps -axo comm \| grep -iE "bartender\|ice\|hidden bar\|vanilla\|dozer"`; open the manager's list and unhide Steno |
| E | Two Steno processes (an Xcode Debug build and an installed or release-rehearsal copy) share the bundle id but not the database; the item clicked belonged to the copy that was not recording, so its popover said "Not recording" | Both roots come from `StenoPaths.default()`; two builds from different paths run side by side | `pgrep -lf Steno.app`; quit every copy, launch one |
| F | The popover opened but was empty or stale: `RootView` shows a spinner until `AppBootstrap.load()` finishes and an error row with Quit if `AppEnvironment.live` threw; a `.window` style extra also closes on the first click outside it, which can read as "nothing happened" | `StenoApp.swift:91-112`; `AppBootstrap.load` at 69-87 | Click the item and wait two seconds; if it shows "Steno could not start", copy the text; `log show --last 10m --predicate 'process == "Steno"' \| tail -50` |
| G | The label did not refresh. Observation through `bootstrap.controller?.recorder` is tracked, but `MenuBarExtra` labels have a history of stale renders on macOS 14 and 15 when the label's only dependency is an `@Observable` reached through an optional chain | `StenoApp.swift:118`; platform reports | Same check as A; if the window says Recording and the item still shows the waveform after five seconds, this is the cause and the elapsed-time label in step 6 below is the fix, since a `TimelineView` re-renders on its own schedule |

What this plan changes in the menu bar regardless of which cause holds: while recording the
label becomes the symbol plus the elapsed time (`record.circle.fill` and `12:34`, monospaced
digits), which is legible at a glance, immune to template rendering (A), and self-refreshing
(G). Causes B to E are environment, not code; the checks above settle them and the bubble makes
the app usable even when the menu bar cannot show the item.

### Jamie's reference, measured from the screenshots

- Prompt: one white pill about 56 pt tall with radius about 16, no title bar, top centre of the
  screen just under the menu bar. Left: "Are you in a meeting?" at about 15 pt regular
  `strong`, then "Start Jamie to take notes" at about 13 pt `muted`. Right: one bordered button
  with the app icon and "Start Jamie". A small X sits at the far right. No countdown is shown.
- Bubble: about 44 pt tall, radius about 16, white, hairline, soft shadow. App glyph, five
  black vertical bars of varying height, an orange rounded square (about 28 pt) with a white
  square stop glyph. No text.

## Non-goals

- No change to the detector, the prompt's countdown, suppression or auto-dismiss rules, or the
  recorder's state machine. Presentation only.
- No "Record in person" in the prompt; the detector only reports a call.
- No per-app ignore list, no "remind me later", no pause, no transcript or captions in the
  bubble, no meeting title in the bubble.
- No `LSUIElement`; the menu bar item is not removed or replaced.
- No setting to hide the bubble (open question).
- No brand hue. Jamie's orange is not adopted; see decision 6.
- No second bubble per display; one panel, on the screen chosen below.
- No hover expansion with extra controls; hover changes appearance only.

## Decisions

1. **One `FloatingPanel` host for both surfaces.** `DetectionPanelPresenter` is replaced by
   `FloatingPanelPresenter`, which owns one borderless non-activating `NSPanel` for the app's
   lifetime and swaps its SwiftUI content between the prompt and the bubble. Reason: the morph
   from prompt to bubble is a content swap inside one window at one anchor, the panel
   configuration is written once, and the position is remembered once.
2. **The presenter observes, it is not called.** It follows `controller.detection.prompt` and
   `controller.recorder.recording` with `withObservationTracking` and resolves content with a
   pure function `FloatingContent.resolve(prompt:recording:)`: any state other than `.idle`
   wins, else a prompt if one exists, else hidden. The `promptDidChange` closure on
   `DetectionController` goes away. `withObservationTracking` is one-shot and its `onChange`
   closure is `@Sendable`, running on whichever thread performed the mutation, so the presenter
   hops back (`Task { @MainActor in self.apply(...); self.observe() }`) and re-registers on every
   change; no `nonisolated(unsafe)` shortcut. Reason: the surfaces drive the one recorder and
   never each other, and the resolve function is testable without AppKit.
3. **Shared anchor, top centre under the menu bar.** Both contents are laid out from one
   anchor, the top-centre point of the panel, stored in `UserDefaults` under
   `steno.floatingPanel.anchor` together with the frame of the screen it was saved on.
   Default: `visibleFrame.midX`, `visibleFrame.maxY - Theme.Space.sm` on `NSScreen.main`,
   falling back to the first screen. A saved anchor that lies on no current screen's
   `visibleFrame` is discarded. Reason: this is where Jamie puts both, it is the one spot
   no app draws its own controls, and one anchor means the prompt morphs into the bubble
   without moving.
4. **Panel configuration.** `styleMask [.nonactivatingPanel, .borderless]`, `level .floating`,
   `collectionBehavior [.canJoinAllSpaces, .fullScreenAuxiliary, .stationary]`,
   `hidesOnDeactivate false`, `isMovableByWindowBackground true`, `isReleasedWhenClosed
   false`, `becomesKeyOnlyIfNeeded true`, `isOpaque false`, `backgroundColor .clear`,
   `hasShadow true`, `animationBehavior .none`. Content through `NSHostingController` with
   `sizingOptions = .preferredContentSize`, so the window tracks the SwiftUI size; after every
   resize the presenter re-applies the anchor. Reason: `.floating` sits above normal windows
   and, with `.fullScreenAuxiliary`, over a full-screen Zoom; `.statusBar` would also cover
   system alerts and the menu bar's own panels. `.stationary` keeps it out of Exposé. The
   system window shadow is the one shadow the redesign's "hairlines, not shadows" rule does not
   cover, because a floating panel over arbitrary content needs separation from it.
5. **Copy: the title names the trigger.** Title "<App> opened the microphone", subtitle
   "Record with Steno? Audio stays on this Mac.", one primary button "Record" with the glyph,
   an X. Reason: Jamie asks "Are you in a meeting?" because it cannot attribute the
   microphone; Steno can, and the app name is the one fact that lets the user decide in a
   second. Unknown bundle ids keep `DetectionController.liveAppName`'s "Another app".
6. **Stop is a `raised` square with a `destructive` glyph; bars are achromatic.** The stop
   button is a 28 pt square, radius 8, `raised` fill with a hairline `border`, a `destructive`
   `stop.fill` glyph, hover `card`. The level bars are `strong` over a `border` track, the same
   meter the start-recording plan ships in the sidebar, header and menu bar. Reason: the house
   has no brand hue, so orange is out; red stays a glyph or dot everywhere and never a fill, so
   the bubble's Stop reads as the same family as the window's Stop 200 pt away (raised surface,
   red dot, red word) instead of a second construction for one action. Recording is one hue on
   every surface: the red dot or glyph; `live` green is reserved for success. Jamie keeps its
   bars black next to its orange for the same reason.
7. **The prompt's countdown is a hairline without a number.** A 2 pt track along the bottom
   inside the radius, `border` as the track, `strong` at 40 percent as the fill, draining from
   full to empty over `timeout`, one linear step per tick. Rule, shared with the device-change
   plan: the prompt shows no number because nothing is at stake when it closes; a countdown
   that ends a recording (the auto-stop) shows both the hairline and the number, because the
   user must decide. Both are the one `CountdownHairline(countdown:)` view over the shared
   `Countdown`. The auto-dismiss behaviour is unchanged.
8. **Click on the body opens the live meeting.** Sets `controller.requestedMeetingID =
   recorder.activeMeetingID`, then calls `bootstrap.openMain`, a closure captured inside a
   SwiftUI scene: `RootView` (inside the `MenuBarExtra` scene) reads `@Environment(\.openWindow)`
   and installs `.task { bootstrap.openMain = { openWindow(id: "main"); NSApp.activate() } }`,
   which is how `MenuBarView.swift:169-172` already opens the window. A view hosted by
   `NSHostingController` inside the presenter's `NSPanel` is outside every scene, and the
   `openWindow` action read there silently does nothing before macOS 26, so the closure is
   handed to `FloatingPanelPresenter.follow(_:openMain:)`. No `NSWorkspace` fallback. Reason:
   the bubble is also the shortest path back to the meeting.
9. **Elapsed time in the menu bar label while recording.** Decided above in the diagnosis.
10. **Bubble during `.starting` and `.stopping`.** Shown, with "Starting…" or "Finishing…" in
    place of the bars and the stop button disabled. Reason: `stop()` can take seconds
    (intake, enqueue), and on Quit `shutdown()` runs the same `stop()`; the bubble showing
    "Finishing…" until the process exits is the honest signal that the recording is being
    saved, not lost.
11. **The main window keeps its own Stop.** The redesign plan's header Stop
    (`header-stop`) and the sidebar control stay; the bubble is a third surface
    over the same `RecordingController.stop()`. Reason: the pane the user is reading should
    never lack the one action that matters.

## Design spec

### Panel behaviour

- One panel, created lazily on the first non-hidden content and kept. Hidden content orders
  it out (`orderOut`) after `Motion.exit`; shown content orders it front with
  `orderFrontRegardless()` and fades in over `Motion.entrance`. Content swaps prompt to
  bubble crossfade over `Motion.functional` while the frame animates to the new size with
  `Motion.spatial`, anchor held.
- The user can drag it anywhere by its background; `NSWindow.didMoveNotification` saves the
  anchor. `NSApplication.didChangeScreenParametersNotification` re-validates the anchor and
  re-anchors (a display unplugged mid-call brings the bubble to the remaining screen).
- Screen choice on show: the screen holding the saved anchor, else `NSScreen.main`, else
  the first screen. Spaces: `.canJoinAllSpaces` means it follows the user; full-screen apps
  see it because of `.fullScreenAuxiliary`.
- The panel never becomes key on its own and never activates Steno; buttons work through
  `becomesKeyOnlyIfNeeded`. Keyboard shortcuts on the prompt are removed (they were inert).
- Quit while recording: `applicationShouldTerminate` runs `shutdown()`, the recorder enters
  `.stopping`, the bubble shows "Finishing…", the process exits when `stop()` returns. The
  panel holds nothing that delays termination.
- Reduce Motion: bars change height without animation, the countdown steps without
  animation, entrance and exit are opacity only (already so).
- Accessibility: the root of each content is one `accessibilityElement(children: .contain)`
  with identifiers `detection-prompt` and `recording-bubble`. Buttons: `prompt-record`
  (label "Record with Steno"), `prompt-dismiss` (label "Not now"), `bubble-stop` (label
  "Stop recording"), `bubble-open` (the body; label "Recording", value the elapsed time,
  hint "Opens the meeting in Steno"). Bars and the countdown hairline are
  `accessibilityHidden`. The prompt has hint "Closes on its own after a minute."

### Geometry and tokens

| Part | Value |
|---|---|
| Bar fill and edge | `popover` fill (opaque in both appearances), hairline `border`, radius `Theme.Radius.xl` 16 (from the redesign's system PR; at 40 pt tall this is a rounded bar, not a capsule, and the spec calls it that), system window shadow |
| Bubble box | height 40 (64 while the auto-stop is armed, see the device-change plan), padding 6 leading 6 trailing 6, gap 10; contents glyph 20, bars 5 x 3 pt with 2 pt gaps and heights 4 to 16, elapsed `xxs` mono `mutedForeground`, stop 28 x 28 radius 8 |
| Prompt box | height 56, padding 16 leading 8 trailing, gap 16; min width 360, max 480, title one line tail-truncated |
| Prompt text | title `sm` 14 semibold `strong`, subtitle `xxs` 12 regular `mutedForeground`, 2 pt between |
| Prompt Record button | `StenoPrimaryButtonStyle` (the redesign plan restyles it later), label: glyph 16 then "Record" |
| Prompt X | 24 x 24 plain button, `xmark` 11 semibold `faint`, hover `strong`, hit area 28 |
| Countdown | `CountdownHairline`: 2 pt, inset 12 from each side, 4 from the bottom, `border` track, `strong` at 40 percent fill, width times `countdown.fractionRemaining` |
| Glyph | SF Symbol `waveform` 16 semibold `strong` in a 20 pt well (`card` veil, radius 6) now; `Image(nsImage: NSApp.applicationIconImage)` at 20 pt once the icon plan ships, behind one `BubbleGlyph` view so it is a one-line switch |
| Bars | `strong` fill, `border` track; heights from a five-sample history of `max(mic, system).rms` through `LevelBars.fraction`, newest bar trailing; `Motion.functional` between samples |
| Hover | body veils `card`; stop veils `card` and its glyph brightens to `destructiveStrong`; X to `strong` |
| Motion tokens added | `Motion.countdown` (linear, 1 s, the clock tempo for the hairline); `Motion.pulse` (1 s ease-in-out, opacity 1 to 0.4), which the redesign plan's list-entry recording dot reuses (the one ambient animation per viewport; nothing in the bubble pulses) |

### States

Bubble (`BubblePresentation.make(state:autoStop:)`, `autoStop: AutoStopPresentation?`):

| Recorder state | Content | Stop button | Body click |
|---|---|---|---|
| `.idle` | hidden | | |
| `.starting` | glyph, "Starting…" `xxs` `mutedForeground`, spinner 12 | hidden | opens Steno |
| `.recording(since:)` | glyph, bars, elapsed `mm:ss` (`h:mm:ss` past an hour) from `TimelineView(.periodic(from: since, by: 1))` | enabled, calls `recorder.stop()` | opens the live meeting |
| `.recording(since:)`, auto-stop armed | as above plus the countdown line and "Keep recording" per [`2026-09-28-device-change-during-recording.md`](2026-09-28-device-change-during-recording.md), "Surfaces while the auto-stop is armed" | enabled | opens the live meeting |
| `.stopping` | glyph, "Finishing…", spinner | disabled, dimmed 0.5 | opens Steno |

A failed start returns the recorder to `.idle` (`RecordingController.swift:116-123`), so the
bubble hides and the error appears where it does today (menu bar item, window).

Prompt (`DetectionPromptViewModel` holds a `Countdown`; its `remainingSeconds` becomes
`countdown.remaining`, its behaviour is unchanged):

| Event | Panel |
|---|---|
| `DetectionController.prompt` set | shows the prompt at the anchor, hairline full, fades in |
| each tick | hairline width steps to `countdown.fractionRemaining` |
| Record clicked | `prompt.start()`; the controller starts a `.call` recording; the recorder enters `.starting`, so the resolve function switches the same panel to the bubble; crossfade and resize |
| X clicked, microphone released, timed out, detection disabled | prompt nil, panel fades out |
| recording started elsewhere while the prompt is up | `DetectionController.recordingDidChange` dismisses the prompt, the bubble takes the panel |
| start failed | prompt nil, recorder back to `.idle`, panel fades out |

### Copy

- Prompt title: "<App> opened the microphone". Subtitle: "Record with Steno? Audio stays on
  this Mac." Button: "Record". X accessibility label: "Not now".
- Bubble: "Starting…", "Finishing…" (the recorder's `RecordingState.label` strings, reused),
  elapsed time; no other text.
- Menu bar label while recording: elapsed time next to `record.circle.fill`; accessibility
  label "Steno, recording, <elapsed>".

## Implementation steps

One PR, commits in this order, after the start-recording plan has merged (it supplies
`activeMeetingID` and moves `LevelBars`).

1. **Motion tokens and the shared countdown** (`apps/macos/Steno/Design/Motion.swift`, new
   `apps/macos/Steno/Recording/Countdown.swift`,
   `apps/macos/Steno/Detection/DetectionPromptViewModel.swift`): `Motion.countdown` and
   `Motion.pulse`. `@MainActor @Observable final class Countdown` with
   `init(duration: Duration, clock: any Clock<Duration>, onElapsed: @MainActor () -> Void)`,
   `remaining`, `fractionRemaining` (clamped 0...1), `remainingText` ("1:29"), `begin()` ticking
   once a second on the injected clock, `cancel()`, and `var presentation: CountdownPresentation`
   (a `Sendable` value with `remainingText` and `fractionRemaining`). `DetectionPromptViewModel`
   holds one instead of its own tick loop; `remainingSeconds` reads through. Radius comes from
   `Theme.Radius.xl`, already added by the redesign's system PR;
   `apps/macos/StenoTests/ThemeTokensTests.swift` is untouched.
2. **Pure pieces** (new `apps/macos/Steno/Panels/FloatingContent.swift`):
   `enum FloatingContent { case prompt(DetectionPromptViewModel), bubble }` with
   `nonisolated static func resolve(prompt:recording:) -> FloatingContent?`;
   `struct AutoStopPresentation: Equatable, Sendable { appName: String?; remainingText: String;
   fractionRemaining: Double; var line: String }` (`line` is "<App> closed the microphone.
   Stopping in 1:29." or "The call app closed the microphone. Stopping in 1:29." when the name
   is unknown; defined here so the device-change plan fills it without changing this plan's
   signatures or tests, and the string has one owner); `struct BubblePresentation: Equatable`
   with `nonisolated static make(state:autoStop: AutoStopPresentation?)` (text, showsBars,
   showsStop, stopEnabled, isBusy, autoStop; nil until the device-change plan lands; the
   argument is a value, never the `@MainActor` countdown class, so `make` stays `nonisolated`);
   `struct PanelAnchor: Codable, Equatable` with
   `static func defaultAnchor(in visibleFrame: CGRect) -> CGPoint`,
   `func frame(for size: CGSize) -> CGRect`, and `static func validated(_ saved: PanelAnchor?,
   screens: [CGRect], fallback: CGRect) -> PanelAnchor`; `LiveBarsHistory` (a five-sample
   ring of fractions with `push(levels:)` using `LevelBars.fraction`);
   `MenuBarLabelPresentation.make(state:)` (symbol name and optional elapsed text) so step 6 has
   a test.
3. **Panel host** (new `apps/macos/Steno/Panels/FloatingPanel.swift` and
   `apps/macos/Steno/Panels/FloatingPanelModel.swift`): `final class FloatingPanel: NSPanel`
   with the configuration from decision 4 and `canBecomeKey` returning `false`, `canBecomeMain`
   returning `false`. `protocol PanelHost` (`show(frame:)`, `hide()`, `setContent(_:)`,
   `currentScreens`) with `FloatingPanel` as the product conformance. `FloatingPanelModel`
   (`@MainActor`, pure over the host): content resolution, anchor load, save and re-validation,
   the observation-to-apply sequence (`apply(_ content: FloatingContent?)`), so anchor
   persistence and screen changes are unit tests against a fake host. `FloatingPanelPresenter`
   (`@MainActor`) owns the model, the `withObservationTracking` loop with the main-actor hop
   from decision 2, the anchor store over an injected `UserDefaults`, the move and screen
   notifications, and `follow(_ controller: AppController, openMain: @MainActor () -> Void)`.
   Delete `apps/macos/Steno/Detection/DetectionPanel.swift`; remove `promptDidChange` from
   `DetectionController.swift`.
4. **Views** (new `apps/macos/Steno/Panels/DetectionPromptView.swift` and
   `apps/macos/Steno/Panels/RecordingBubbleView.swift`; the shared bar background, hairline,
   hover veil, `CountdownHairline(countdown:)` and the five live bars live in the bubble file):
   the geometry and states above.
   `RecordingBubbleView` takes `controller` and `openMain`; `DetectionPromptView` takes the
   model. `#Preview` blocks in light and dark for every state (starting, recording with
   sample levels, stopping; prompt at full, half and near-empty countdown).
5. **Wiring** (`apps/macos/Steno/StenoApp.swift`, new
   `apps/macos/Steno/Services/UITestScenario.swift`): `AppBootstrap` holds
   `FloatingPanelPresenter` instead of `DetectionPanelPresenter` and `var openMain: @MainActor
   () -> Void`; `RootView` installs the closure from its `@Environment(\.openWindow)` as in
   decision 8; `panels.follow(controller, openMain:)` runs after `launch()`. Launch arguments are
   parsed once: `struct UITestScenario: Equatable { init(arguments: [String]) }` with
   `showPrompt` for `-steno-show-prompt` (the redesign plan adds its flags to the same struct),
   and a unit test that a misspelt flag is reported, so a typo is not a silent no-op in the
   smoke suite. Under `showPrompt`, after launch, call
   `controller.detection.handle(.microphoneOpened(bundleID: "us.zoom.xos", pid: 1))` with
   `appName` overridden to return "Zoom".
6. **Menu bar label** (`StenoApp.swift` `MenuBarLabel`): while `.recording(since:)`, an
   `HStack` of `record.circle.fill` and the elapsed time in a `TimelineView(.periodic)` with
   monospaced digits; `.starting` and `.stopping` show the symbol alone; idle stays
   `waveform`. Drop `.symbolRenderingMode(.multicolor)` (it has no effect there).
7. **Unit tests** (new `apps/macos/StenoTests/FloatingPanelTests.swift` and
   `CountdownTests.swift`, existing fakes plus a `FakePanelHost`: `TestSupport.environment`,
   `ManualClock`, `SyntheticCaptureBackend` through the preview root):
   - `resolve`: every `RecordingState` times prompt nil or set; the bubble wins whenever the
     state is not idle; the prompt shows only when idle; nil otherwise.
   - `BubblePresentation.make`: the four states, including `stopEnabled` false in `.stopping`
     and `showsBars` only in `.recording`; `autoStop` passes through unchanged.
   - `AutoStopPresentation.line` with and without an app name.
   - `MenuBarLabelPresentation.make`: `waveform` and no text when idle, `record.circle.fill`
     alone in `.starting` and `.stopping`, symbol plus elapsed text in `.recording`.
   - `PanelAnchor`: default anchor sits at the top centre of the visible frame; `frame(for:)`
     keeps the top-centre point for two sizes; a saved anchor off every screen falls back;
     one on a second screen is kept.
   - `FloatingPanelModel` over `FakePanelHost`: nil content hides; prompt then bubble keeps the
     anchor and resizes; a move saves the anchor to the injected defaults; a screen-parameter
     change with the saved screen gone re-anchors to the fallback (the model half of manual
     steps 4 and 6).
   - `LiveBarsHistory`: five samples, newest last, silence maps to 0, `-60` dBFS to 0, `0`
     to 1, `system` nil uses `mic` alone.
   - `CountdownTests`: `fractionRemaining` goes 1, then 2/3, 1/3, 0 on a 3 s duration with
     `ManualClock`, `remainingText` formats `m:ss`, `onElapsed` fires once, `cancel()` stops
     the ticks. `DetectionTests.testPromptCountsDownOnTheClockAndTimesOut` is unchanged and
     proves the prompt still closes through the shared type.
   - `AppControllerTests.testDetectionPromptStartsACallRecordingWhileTheDetectorRunsOn`: add
     two `resolve` assertions, prompt before `prompt.start()` and bubble after.
     `testShutdownStopsTheRecordingAndTheDetector`: assert
     `FloatingContent.resolve(prompt: nil, recording: .stopping) == .bubble` so the "Finishing…"
     bubble during Quit has a hostless proof.
   - `UITestScenarioTests`: known flags parse, an unknown `-steno-*` flag is reported.
   - The presenter's `NSPanel` and the `NSWindow` move notification are covered by the UI
     smoke test and manual steps, not by the hostless bundle.
8. **UI smoke** (`apps/macos/StenoUITests/LaunchSmokeTests.swift`):
   `testPromptMorphsIntoTheBubbleAndStopHidesIt`: launch with `-steno-ui-testing
   -steno-show-prompt`, wait for `app.buttons["prompt-record"]` (timeout 10), assert
   `app.staticTexts["Zoom opened the microphone"]` exists, click Record, wait for
   `app.buttons["bubble-stop"]`, wait on an `XCTNSPredicateExpectation(exists == false)` for
   `prompt-record`, click stop, wait for `bubble-stop` to disappear (timeout 10). A second test
   `testBubbleFollowsARecordingStartedFromTheWindow`: launch with `-steno-ui-testing`,
   click `sidebar-record`, wait for `bubble-stop`, click `bubble-open`, then wait until
   `app.descendants(matching: .any).matching(NSPredicate(format: "identifier BEGINSWITH 'meeting-' AND selected == true")).count == 1`
   (the live entry carries the `isSelected` trait; no title text and no "frontmost" assertion,
   which is flaky on the hosted runner), then stop. Attach a screenshot of each panel with
   `lifetime = .keepAlways` and a name (`prompt.png`, `bubble.png`) so the
   `macos-ui-smoke-output` artefact carries them.
9. **Plan bookkeeping.** One line under "Deviations (implementation)" in
   `.plans/2026-09-25-macos-app-and-release.md`: the detection prompt and the recording
   bubble share `FloatingPanel`, pointing here.

Files touched: `apps/macos/Steno/Design/Motion.swift`,
`apps/macos/Steno/Recording/Countdown.swift` (new),
`apps/macos/Steno/Services/UITestScenario.swift` (new),
`apps/macos/Steno/Panels/FloatingContent.swift` (new),
`apps/macos/Steno/Panels/FloatingPanel.swift` (new),
`apps/macos/Steno/Panels/FloatingPanelModel.swift` (new),
`apps/macos/Steno/Panels/DetectionPromptView.swift` (new, replaces the view in
`DetectionPanel.swift`), `apps/macos/Steno/Panels/RecordingBubbleView.swift` (new),
`apps/macos/Steno/Detection/DetectionPanel.swift` (deleted),
`apps/macos/Steno/Detection/DetectionController.swift`,
`apps/macos/Steno/Detection/DetectionPromptViewModel.swift`, `apps/macos/Steno/StenoApp.swift`,
`apps/macos/StenoTests/FloatingPanelTests.swift` (new),
`apps/macos/StenoTests/CountdownTests.swift` (new),
`apps/macos/StenoTests/UITestScenarioTests.swift` (new),
`apps/macos/StenoTests/DetectionTests.swift`, `apps/macos/StenoTests/AppControllerTests.swift`,
`apps/macos/StenoUITests/LaunchSmokeTests.swift`,
`.plans/2026-09-25-macos-app-and-release.md`. xcodegen picks the new folder up from the
`Steno` source path; `project.yml` does not change.

Commit as `feat(macos): floating recording bubble and pill detection prompt` with this plan in
the same PR.

## Verification

Automated, as CI runs them:

- `xcodegen generate --spec apps/macos/project.yml`, then `xcodebuild test -scheme StenoTests`
  with the `XCODEBUILD_FLAGS` from `.github/workflows/swift-ci.yml`: `FloatingPanelTests`,
  `CountdownTests`, `UITestScenarioTests`, `DetectionTests`, `AppControllerTests`,
  `InfoPlistTests` (still no `LSUIElement`) pass.
- `xcodebuild test -scheme Steno -only-testing:StenoUITests`: the two new smoke tests pass
  on the hosted `macos-15` image and the artefact contains both screenshots.

Manual, on the owner's Mac, in this order (each maps to a finding or decision):

1. Run the menu bar checks A to G from the table and record which held.
2. Start a FaceTime call. The prompt appears top centre under the menu bar with the
   FaceTime name, the hairline drains; wait it out once (auto-dismiss at 60 s).
3. Call again, click Record: the prompt becomes the bubble in place, "Starting…" then bars
   and the clock within two seconds; the menu bar shows the symbol and the elapsed time.
4. Drag the bubble to the bottom right, stop, start again from ⌘⇧R: it reappears where it was
   dragged; relaunch the app and start again: same place.
5. Make Zoom full screen on the same display: the bubble stays visible over it.
6. Connect a second display, drag the bubble there, unplug it: the bubble returns to the
   remaining screen at the default anchor.
7. Click the bubble body: the main window comes forward with the live meeting selected.
8. System Settings > Accessibility > Reduce Motion on: bars step without animation, the
   countdown steps, nothing pulses.
9. VoiceOver on: the prompt reads as one element with the two buttons; the bubble reads
   "Recording, 01:23, button" and the stop button "Stop recording".
10. Quit Steno while recording: the bubble shows "Finishing…", the app exits, the meeting is
    Queued on relaunch.

Risks and checks (settled by the steps, not by the owner):

- `openWindow` is captured inside the `MenuBarExtra` scene (decision 8); manual step 7 confirms
  the window comes forward after the user closed it, which is the case the bubble exists for.
- If the menu bar check finds cause B on the owner's display, the `Debug` menu could move under
  `Help` in Debug builds to free 60 pt; not done here, noted for the PR that runs the checks.
- SwiftUI buttons inside a `.nonactivatingPanel` with `canBecomeKey == false` receiving clicks is
  what the shipped `DetectionPanel` already relies on; manual step 3 is the first confirmation.

## Open questions

- A setting to hide the bubble (screen-sharing users). Not added; the owner asked for an
  always-on indicator. A `Settings` `Bool` needs no migration if it comes.
- Should the bubble hide while Steno itself is the frontmost app with the main window
  visible? Left shown; Jamie shows it always.
- Should the prompt offer "Not for <App> again"? Deferred in the app plan's follow-ups; the
  bar has room for a menu on the X if it comes.
- Brand hue: if the redesign or icon plan ever adopts one, the stop glyph is the first
  candidate to carry it; until then it stays `destructive`.
