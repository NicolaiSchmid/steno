# Steno: start and stop recording from the main window

Status: proposal, 2026-09-28. Triggered by first-run feedback.

Binding plans: [`.plans/2026-09-24-initial-scope.md`](2026-09-24-initial-scope.md)
(scope), [`.plans/2026-09-25-macos-app-and-release.md`](2026-09-25-macos-app-and-release.md)
(app architecture, recording flow, tests). A sibling plan,
[`.plans/2026-09-28-macos-visual-redesign.md`](2026-09-28-macos-visual-redesign.md), owns the
overall look of the app and the visual structure of the sidebar. This plan owns the wiring,
state and behaviour of recording from the main window, and describes the control's placement
only as far as building it needs. Where the two disagree on appearance, the redesign plan
wins; where they disagree on behaviour, this one does.

## Goal

The owner opened the app for the first time, saw an empty sidebar reading "Start a recording
from the menu bar item", and could not test anything from the window in front of him. Jamie
puts a large "Start Jamie" button at the top of its sidebar. Steno gets the same affordance:
a primary control at the top of the sidebar that starts a recording, turns into a Stop control
with the elapsed time and levels while recording, is disabled with a reason while a required
permission is denied, and drives exactly the state machine the menu bar item already drives.
The empty state points at it, and the Record menu's shortcut works from the window.

## Findings

How recording is started today, and why the window cannot:

- `apps/macos/Steno/Recording/RecordingController.swift:33-64` is the one recorder in the
  app. `start(mode:)` (lines 86-124) writes the `.recording` meeting row through core's
  `LocalRecordingIntake.begin` before the capture session starts (line 101), so a row for
  the live meeting exists in the store from the first second. `stop()` (lines 131-159) hands
  the result to `complete`, which sets retention, writes the duration and enqueues. The state
  is `RecordingState` (lines 7-21): `.idle`, `.starting`, `.recording(since:)`, `.stopping`.
  There is no processing state on the recorder; after `stop()` it is `.idle` again and the
  meeting's own `MeetingState` (`.queued`, `.processing`) carries the pipeline progress.
- The controller keeps the active meeting id private inside `Active` (line 36-41, stored at
  line 56). Nothing outside can learn which row belongs to the live recording.
- Nothing gates `start()` on permissions. `OnboardingOpener` in
  `apps/macos/Steno/StenoApp.swift:127-145` checks the two required kinds
  (`PermissionKind.isRequired`, `apps/macos/Steno/Services/AppProtocols.swift:47-54`) once at
  launch and opens the onboarding window; after that a start with a denied microphone fails
  inside the capture backend and surfaces as `lastError`. The system audio state is whatever
  the last probe found (`apps/macos/Steno/Services/PermissionsService.swift:40-41`), `.unknown`
  until the onboarding probe ran, and a recording with the tap unauthorised ends with the
  "system audio lane stayed silent" warning at stop (`RecordingController.swift:139-141`).
- The only manual start UI is the menu bar item: `apps/macos/Steno/MenuBar/MenuBarView.swift:139-178`.
  It renders the status dot and label, a `TimelineView` elapsed timer (lines 149-155),
  `LevelBars` (lines 157-158, defined at 272-311) and the buttons per state (lines 160-176):
  "Record call" and "Record in person" when idle, "Stop" when recording, a spinner while
  starting or stopping. Accessibility identifiers `record-call`, `record-in-person`,
  `stop-recording`.
- A `Record` menu already exists: `StenoApp.swift:166-179`. "Record Call" / "Stop Recording"
  on ⌘⇧R calls `recorder.toggleRecording()`; "Record In Person" has no shortcut and is
  disabled unless idle. The label switches on `isRecording`, which is also true during
  `.starting` and `.stopping` (`RecordingController.swift:66-71`) when `toggleRecording()`
  does nothing (lines 161-167), so the menu can read "Stop Recording" while a click is a
  no-op. Menu commands work whenever the app is active, so the shortcut already reaches the
  main window; the window itself just never shows it.
- `apps/macos/Steno/Main/MainWindow.swift:19-21` builds the sidebar column from
  `MeetingListView(model: list)` alone. `MainWindow` holds the `AppController` (line 7) and
  therefore `controller.recorder`, but `MeetingListView`
  (`apps/macos/Steno/Main/MeetingListView.swift:66-67`) takes only a
  `MeetingListViewModel`, which is built from the store and the clock (`MainWindow.swift:13-15`)
  and knows nothing about the recorder. That is the whole gap: the window has the recorder in
  hand and never renders it.
- The empty state copy is `MeetingListView.swift:175`: "Start a recording from the menu bar
  item." The detail pane's placeholder is "Select a meeting" (`MainWindow.swift:31`).
- A live recording already shows in the list: `MeetingRow` renders `StatusChip(meeting.state)`
  (`MeetingListView.swift:184-213`, chip text "Recording" from
  `apps/macos/Steno/Design/Components.swift:116-127`), the "In progress" filter includes
  `.recording` (`apps/macos/Steno/Main/MeetingListViewModel.swift:244-246`), and delete is
  refused for it (`MeetingListView.swift:135-140`). The detail header shows the same chip
  (`apps/macos/Steno/Main/MeetingDetailView.swift:46-54`) and the tabs show their pending text.
- The menu bar reads the recorder through `AppController.recorder` (`apps/macos/Steno/AppController.swift:14`);
  the detection prompt starts a `.call` recording through the closure at line 35; `shutdown()`
  stops an in-flight recording (lines 127-134). The menu bar icon follows `recorder.isRecording`
  (`StenoApp.swift:114-123`). Everything already observes one object; the window can too.
- Design tokens: `Color.stenoPrimary` and `stenoPrimaryForeground` back
  `StenoPrimaryButtonStyle` (`Components.swift:8-22`); `Theme.destructive`,
  `destructiveStrong` and `destructiveForeground` exist as tokens
  (`apps/macos/Steno/Design/Theme.swift:84-88`) but only `stenoDestructive` has a `Color`
  shortcut (lines 128-150). There is no destructive button style yet.
- Test seams: `AppEnvironment.preview()` uses `FakePermissions.allGranted()`
  (`apps/macos/Steno/AppEnvironment.swift:326`); `FakePermissions.states` is mutable
  (`apps/macos/Steno/Services/Fakes.swift:175-206`) and tests already downcast environment
  fakes (`environment.calendar as? FakeCalendar`, `RecordingControllerTests.swift:199`). The
  synthetic capture backend and fake pipeline let a recording start, stop and reach `.ready`
  in tests (`RecordingControllerTests.swift:152-182`). The UI smoke test launches the preview
  environment with `-steno-ui-testing` (`apps/macos/StenoUITests/LaunchSmokeTests.swift:117-150`)
  and runs on the hosted `macos-15` job only.
- Scope: `.plans/2026-09-24-initial-scope.md` "Capture (Mac)" names "manual start/stop from
  the menu bar" and "App UI (macOS)" lists start/stop under the menu bar item, with the main
  window as "meeting list plus four tabs ... Display only". A start control in the window adds
  a second entry point to the same recorder; it does not widen capture, processing or data.
  The "display only" note refers to the tabs' content, which stays untouched.

## Non-goals

- No change to what a recording is: modes, lanes, the intake transaction, retention, the
  pipeline and the detection prompt stay as they are.
- No new recording state. The recorder keeps `.idle`, `.starting`, `.recording`, `.stopping`.
  Processing is the meeting's state, shown by the row and the header, not by the control.
- No permission requests from the control. It reports a denied required permission and hands
  off to the onboarding window or System Settings; the onboarding flow owns requesting.
- No redesign of the sidebar, the list rows, the toolbar or the detail pane. The redesign plan
  owns that; this plan places one control and changes two lines of copy.
- No pause, no scheduled start, no calendar-triggered start (deferred in the app plan).
- No change to the menu bar item's behaviour beyond sharing the extracted views.

## Decisions

- **One recorder, observed from both surfaces.** The window reads `controller.recorder`
  exactly as `MenuBarView` does. No second view model mirrors the recorder's state, no
  bindings copy it. `RecordingController` is `@Observable`, so SwiftUI tracks it from any view
  that reads it. Reason: the app plan's "the menu bar, the Record menu, the detection prompt and
  `shutdown()` all drive this, none of them each other" is the rule; a sidebar copy would be
  the first drift.
- **The control lives in `MainWindow`'s sidebar column, above `MeetingListView`.**
  `MeetingListView` stays store-only (its tests need no controller). `MainWindow` composes
  `VStack(spacing: 0) { RecordingControl(controller:) ; MeetingListView(model:) }`. The
  `.searchable(placement: .sidebar)` and `.toolbar` modifiers on the list still resolve to the
  window toolbar from inside the stack.
- **Presentation is a pure function.** `RecordingControlPresentation.make(state:denied:)` maps
  `RecordingState` plus the denied required permissions to label, colour role, enabled flag
  and disabled reason. The sidebar control, the Record menu and the menu bar item all render
  from it, so the state table below is asserted once in a unit test, the way
  `RecordingState.label` and `PendingText.text` are today. This also fixes the Record menu
  reading "Stop Recording" during `.starting`.
- **Default mode is a call; in person is one click away.** The CTA's primary action is
  `start(mode: .call)`, matching ⌘⇧R and the detection prompt. In-person start is a secondary
  item on the same control (`Menu` with `primaryAction`, which macOS renders as a split
  button), so the sidebar carries one control, not two. If the split button cannot take the
  app's primary button style within the implementation step, the fallback is the menu bar's
  layout: a full-width primary "Record call" with a small secondary "Record in person"
  beneath. Identifiers and behaviour are the same either way; only the geometry differs.
- **Stop carries the destructive role, not the achromatic primary.** The menu bar's Stop uses
  the primary style today; the sidebar's Stop is the one red signal in the window, because it
  ends something and sits in the user's eye line for the whole meeting. `StenoDestructiveButtonStyle`
  is added and implements the treatment the redesign plan specifies: a `raised` surface with
  hairline border, a `Theme.destructive` dot and "Stop" in `Theme.destructive` text, not a
  red fill. The menu bar Stop adopts it too so the two surfaces agree. The elapsed time sits
  inside the button, per the redesign plan's decision 13.
- **Permission gating is a report, not a guard.** `RecordingController.refreshPermissions()`
  reads `permissions.state(of:)` for the required kinds and stores the ones that are
  `.denied`. Only `.denied` disables the control: `.unknown` means macOS has not asked yet
  (the microphone prompt and the tap's TCC prompt appear on the first recording), and
  disabling on `.unknown` would block a user who clicked "Later" in onboarding while the menu
  bar still lets them record. `start(mode:)` itself stays unguarded, so the detection prompt
  and Quit paths do not change; a denied microphone fails there as it does today and shows
  `lastError`. The report refreshes when the control appears and whenever the app becomes
  active (the user comes back from System Settings).
- **The live row is selected when the recording started from the window.** After `start`
  returns in `.recording`, the control sets `controller.requestedMeetingID` to the new
  `recorder.activeMeetingID`, which `MainWindow` already turns into a selection
  (`MainWindow.swift:48-52`). Starts from the menu bar or the detection prompt do not steal
  the selection: the user may be reading another meeting. `activeMeetingID` becomes a
  read-only property of the recorder; the row transaction stays core's.
- **Shortcut stays ⌘⇧R.** It is wired, tested (`testToggleRecordingStartsThenStops`) and free
  of conflicts. ⌘R is the conventional refresh key and ⌘. is Cancel in every dialog; neither
  is worth a second binding. The control's help text and the empty state name the shortcut.
  "Record In Person" in the Record menu keeps no shortcut.
- **Errors surface where the click happened.** The control shows `recorder.lastError` and
  `lastWarning` beneath itself, the way the menu bar item does, so a failed start from the
  window is not silent. Messages clear on the next start (`start` already resets them).
- **Placement, as far as building needs it.** Top of the sidebar column, full width inside
  `Theme.Space.md` horizontal padding, above the filters row, separated by the same hairline
  divider the list uses. Height, corner radius, icon and how the status line sits relative to
  the button are the redesign plan's to set; this plan only requires that the label, colour
  role, elapsed time, level bars and disabled reason are present in the states below.

## Behaviour spec

The control's states, from `RecordingControlPresentation.make(state:denied:)`. "Colour role"
names the token family, not a pixel value.

| State | Condition | Label | Colour role | Action | Enabled | Disabled reason shown |
|---|---|---|---|---|---|---|
| Idle | `.idle`, `denied` empty | "Record call"; secondary item "Record in person" | primary (`stenoPrimary` / `stenoPrimaryForeground`) | `start(mode: .call)`; secondary `start(mode: .inPerson)`, then select the new row | yes | none |
| Starting | `.starting` | "Starting…" | primary, spinner in place of the label | none | no | none (transient) |
| Recording | `.recording(since:)` | "Stop" with elapsed `mm:ss` (`h:mm:ss` past an hour) on the status line, ticking once a second; level bars for mic and, in call mode, system | destructive (`stenoDestructive` / `stenoDestructiveForeground`); status dot `stenoDestructive` | `stop()` | yes | none |
| Stopping | `.stopping` | "Finishing…" | destructive, spinner | none | no | none (transient) |
| Permission denied | `.idle`, `denied` non-empty | "Record call" | primary at disabled opacity | none on the button; a "Fix permissions…" text button opens the onboarding window | no | "Microphone access is denied." / "System audio access is denied." / both, plus the fix link |

Rules that hold across states:

- The status line above or beside the button reads `recording.label` ("Not recording",
  "Starting…", "Recording", "Finishing…") with the dot coloured `stenoDestructive` while
  `isRecording`, `stenoGhost` otherwise, the same as the menu bar's line today.
- The elapsed time comes from `TimelineView(.periodic(from: since, by: 1))` over
  `TimeInterval.clockText`; no timer state on any model.
- Level bars render only while `.recording` and `levels != nil`.
- The Record menu shows the same label per state and is disabled in `.starting`,
  `.stopping` and while denied; "Record In Person" is enabled only in idle with nothing denied.
- `lastError` renders as `MessageRow(kind: .error)` and `lastWarning` as `.warning` under the
  control; they are the recorder's, so the menu bar shows the same text.
- After a stop the control is idle again immediately; the finished meeting's row shows
  "Queued" then "Processing" then "Ready" through its chip, and a second recording may start
  while the first processes (the pipeline queues).
- Empty state (no meetings at all): title "No meetings yet", body "Press Record call above,
  or ⌘⇧R. The menu bar item works too." When meetings exist but the filter hides them, the
  body stays "No meetings match". The detail placeholder reads "Your first recording will
  appear here." while the store is empty and "Select a meeting" otherwise.
- Nothing in the window starts a recording on its own: not window open, not selection, not
  a calendar event.

## Implementation steps

1. **Recorder seams** (`apps/macos/Steno/Recording/RecordingController.swift`). Add
   `var activeMeetingID: UUID? { active?.meetingID }`. Add
   `private(set) var deniedPermissions: [PermissionKind] = []` and
   `func refreshPermissions() async` that collects the required kinds whose
   `environment.permissions.state(of:)` is `.denied`, in `PermissionKind.allCases` order. No
   change to `start`, `stop`, `toggleRecording` or `awaitSettled`.
2. **Presentation** (new `apps/macos/Steno/Recording/RecordingControlPresentation.swift`).
   `struct RecordingControlPresentation: Equatable, Sendable` with `enum Role { case primary,
   destructive }`, `label: String`, `role: Role`, `isEnabled: Bool`, `isBusy: Bool` (spinner),
   `offersInPerson: Bool`, `disabledReason: String?`, and
   `nonisolated static func make(state: RecordingState, denied: [PermissionKind]) ->
   RecordingControlPresentation` implementing the table above. Also
   `PermissionKind.deniedMessage` ("Microphone access is denied.", "System audio access is
   denied."), joined with a space when both are denied.
3. **Shared recording views** (new `apps/macos/Steno/Recording/RecordingViews.swift`). Move
   `LevelBars` out of `MenuBarView.swift` unchanged. Extract the status dot, label and elapsed
   `TimelineView` from `MenuBarView.recordingSection` into `RecordingStatusLine(recorder:)`.
   Add `RecordingMessages(recorder:)` for the error and warning rows. `MenuBarView` renders
   these three instead of its inline copies; its buttons switch on the presentation's role
   and the Stop adopts the destructive style. Existing identifiers `record-call`,
   `record-in-person`, `stop-recording` stay.
4. **Destructive button style and colour shortcut**
   (`apps/macos/Steno/Design/Components.swift`, `apps/macos/Steno/Design/Theme.swift`). Add
   `Color.stenoDestructiveForeground`. Add `StenoDestructiveButtonStyle` mirroring
   `StenoPrimaryButtonStyle` with `Color.stenoDestructive` fill and
   `Color.stenoDestructiveForeground` text. `ThemeTokensTests` is unaffected (no new token,
   only a shortcut).
5. **Sidebar control** (new `apps/macos/Steno/Recording/RecordingControl.swift`).
   `struct RecordingControl: View { let controller: AppController }`. Reads
   `controller.recorder`, computes the presentation, renders: `RecordingStatusLine`, the
   button (a `Menu` with `primaryAction` for the idle state, accessibility identifiers
   `sidebar-record`, `sidebar-record-in-person`; a plain `Button` for `sidebar-stop`; a
   `ProgressView` while busy), `LevelBars` while recording, the disabled reason with a
   "Fix permissions…" button that calls `openWindow(id: "onboarding")`, and
   `RecordingMessages`. `.help("Record a call (⌘⇧R)")` on the primary. `.task { await
   recorder.refreshPermissions() }` and
   `.onReceive(NotificationCenter.default.publisher(for: NSApplication.didBecomeActiveNotification))`
   calling it again. The start action: `await recorder.start(mode:)`, then if
   `case .recording = recorder.recording`, `controller.requestedMeetingID =
   recorder.activeMeetingID`. Full width via `.frame(maxWidth: .infinity)`, padded
   `Theme.Space.md`, followed by `Divider().overlay(Color.stenoBorder)`.
6. **Compose in the window** (`apps/macos/Steno/Main/MainWindow.swift`). Sidebar column
   becomes `VStack(spacing: 0) { RecordingControl(controller: controller);
   MeetingListView(model: list) }` inside the existing `navigationSplitViewColumnWidth`. Detail
   placeholder text switches on `list.all.isEmpty` per the spec.
7. **Empty state copy** (`apps/macos/Steno/Main/MeetingListView.swift:175`). Replace the menu
   bar sentence with the spec's body. No new control in the empty state; the CTA sits directly
   above it.
8. **Record menu** (`apps/macos/Steno/StenoApp.swift:166-179`). Build the label and enabled
   flag from `RecordingControlPresentation.make(state:denied:)`: idle "Record Call",
   recording "Stop Recording", transient states show the presentation label and are disabled,
   denied states are disabled. "Record In Person" is enabled only when
   `presentation.offersInPerson`. Shortcut stays ⌘⇧R. The `#if DEBUG` probe item is untouched.
9. **Unit tests** (`apps/macos/StenoTests`).
   - New `RecordingControlPresentationTests.swift`: every `RecordingState` times `denied` in
     `[]`, `[.microphone]`, `[.systemAudio]`, `[.microphone, .systemAudio]`. Asserts label,
     role, `isEnabled`, `isBusy`, `offersInPerson` and the exact `disabledReason` strings; an
     optional kind in `denied` never disables (the recorder never puts one there, but the
     function is total).
   - `RecordingControllerTests.swift`: `testActiveMeetingIDFollowsTheRecording` (nil, then the
     store's one row id after `start`, nil after `stop`);
     `testRefreshPermissionsReportsDeniedRequiredKindsOnly` using
     `environment.permissions as? FakePermissions` with `.states[.microphone] = .denied`,
     `.states[.systemAudio] = .unknown`, `.states[.calendar] = .denied`, expecting
     `[.microphone]`; and that `start(mode:)` still runs with a denied kind reported (the
     guard is in the UI, not the recorder), so `testAFailingSessionFactoryLeavesNoMeetingRow`
     remains the failure path.
   - `MenuBarViewModelTests.testLabelsAreWordsNotRawValues`: add the two `deniedMessage`
     strings.
   Existing fakes suffice: `FakePermissions`, `FakeCalendar`, the synthetic capture backend
   through `TestSupport.environment`. No new seam on `AppEnvironment.preview`.
10. **UI smoke** (`apps/macos/StenoUITests/LaunchSmokeTests.swift`). Add
    `testSidebarStartsAndStopsARecording`: launch with `-steno-ui-testing`, wait for the
    window, click `app.buttons["sidebar-record"]`, wait for `app.buttons["sidebar-stop"]`
    (timeout 10), assert `app.staticTexts["Recording"].firstMatch.exists` (row chip or status
    line), click stop, wait for `sidebar-record` to return, then assert a row whose label
    begins with "Meeting " exists (core's default title in the preview environment has no
    calendar event). Keep the whole test under 30 seconds; the synthetic backend delivers
    audio immediately. The existing `testMainWindowOpens` is unchanged.
11. **Plan bookkeeping.** Append one line to the "Deviations (implementation)" list of
    `.plans/2026-09-25-macos-app-and-release.md` recording that the main window gained a
    start/stop control and pointing here, and one line to this file's status when merged.

Files touched: `apps/macos/Steno/Recording/RecordingController.swift`,
`apps/macos/Steno/Recording/RecordingControlPresentation.swift` (new),
`apps/macos/Steno/Recording/RecordingViews.swift` (new),
`apps/macos/Steno/Recording/RecordingControl.swift` (new),
`apps/macos/Steno/MenuBar/MenuBarView.swift`, `apps/macos/Steno/Main/MainWindow.swift`,
`apps/macos/Steno/Main/MeetingListView.swift`, `apps/macos/Steno/StenoApp.swift`,
`apps/macos/Steno/Design/Components.swift`, `apps/macos/Steno/Design/Theme.swift`,
`apps/macos/StenoTests/RecordingControlPresentationTests.swift` (new),
`apps/macos/StenoTests/RecordingControllerTests.swift`,
`apps/macos/StenoTests/MenuBarViewModelTests.swift`,
`apps/macos/StenoUITests/LaunchSmokeTests.swift`,
`.plans/2026-09-25-macos-app-and-release.md`. xcodegen picks the new files up from the
`Steno` source folder; `project.yml` does not change.

Commit as `feat(macos): start and stop recording from the main window` with the plan file in
the same PR.

## Verification

Automated, the way CI runs it (`apps/macos/README.md`):

- `cd apps/macos && xcodegen generate`
- `xcodebuild build -scheme Steno ...` succeeds with no new warnings under strict concurrency
  (`RecordingControlPresentation.make` is `nonisolated` and `Sendable`).
- `xcodebuild test -scheme StenoTests ...`: the new presentation table, the two recorder
  tests and the label test pass; every existing `RecordingControllerTests`,
  `AppControllerTests` and `MenuBarViewModelTests` case still passes unchanged, which proves
  the state machine did not move.
- `xcodebuild test -scheme Steno -only-testing:StenoUITests ...` on the hosted `macos-15` job:
  both smoke tests pass; the new one under 30 seconds.
- `swift format` over the touched files, default configuration.

Manual, on a Mac with a Debug build signed per `apps/macos/README.md` so TCC remembers it:

1. Fresh user defaults, launch: onboarding opens; click "Later". The sidebar shows the
   control enabled ("Record call") and the empty state names it and ⌘⇧R.
2. Click Record call: the microphone prompt appears, then the tap prompt; the row appears at
   once with the "Recording" chip and is selected; the timer ticks; level bars move when you
   speak and when audio plays. The menu bar icon turns red and its window shows the same
   timer.
3. Press ⌘⇧R: the recording stops from the window's shortcut; the row goes Queued, Processing,
   Ready; the control is idle again.
4. Open the split button's menu, choose Record in person: one "Room" level bar; stop from
   the menu bar item; the sidebar control follows.
5. System Settings, Privacy, Microphone: switch Steno off. Return to Steno: the control
   disables with "Microphone access is denied." and the fix link opens onboarding. Switch it
   back on, return: the control re-enables without a relaunch.
6. Start a recording, then Quit: the recording is stopped and enqueued (unchanged behaviour,
   checked because the window is now a second driver).

## Open questions

- Should "Record In Person" get a shortcut (⌘⌥⇧R) now that the window advertises ⌘⇧R? Left
  out; add if the owner uses in-person mode often.
- Should the control hint at `.unknown` system audio ("macOS will ask for system audio on the
  first recording") rather than say nothing? The onboarding step already explains it; a
  one-line hint under the button is cheap if first-run confusion persists.
- Resolved by the redesign plan (decision 13): the elapsed time sits inside the button and
  the separate status line is dropped in the nav column. The presentation struct supports
  either, so this plan's first implementation may still render the line above the button
  until the redesign lands.
- Whether the sidebar control should also list the processing queue the menu bar shows, or
  whether the row chips are enough. This plan says the chips are enough; revisit if users lose
  track of a meeting after stopping.
