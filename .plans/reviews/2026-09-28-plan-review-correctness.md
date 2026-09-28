# Steno first-run feedback plans: correctness and feasibility review

Reviewed 2026-09-28 at commit `9cd7cf5b8f82cd2b7849ab556dfc5851d634d512` (branch
`t3code/plan-ui-onboarding-recording-icon`, source tree identical to `bcf5eef` on `main` for
`Sources/`, `Tests/` and `apps/macos/Steno/`). Under review: the index
`2026-09-28-first-run-feedback.md` and its seven plans (start recording from the main window,
onboarding vault and LLM, audio retention, floating recording indicator, device change during
recording, app icon, macOS visual redesign). Binding context: the initial scope, the macOS app
plan, the audio capture plan, the core foundation plan, the LLM plan and the adapters plan.
Every `file:line` claim in the Findings sections was opened; every API the plans build on was
looked up in `Sources/`, `apps/macos/Steno/`, `apps/macos/StenoTests/` and `Tests/`; platform
claims were checked against Apple documentation, vendor documentation or the tools installed
on this host, with the source named per item. The icon SVG was rendered with the exact
`magick` invocation the icon plan specifies. Findings are ordered most severe first.
Counts: 1 blocker, 5 major, 14 minor.

## Findings

1. **Blocker.** `2026-09-28-audio-retention-keep-forever.md`, Decision 4, Spec "Deletion guard
   rules" rule 2, Spec "Settings > Audio" item 4, step 3.
   Switching the default to "Forever" calls `MeetingStore.clearExpiry(assetIDs:)`, which
   clears `expiresAt` and leaves `asset.retention` at `.keepDays(N)` or
   `.deleteAfterProcessing`. Guard rule 2 then says `redeliver` and `rerunSummary` stamp
   `expiresAt` whenever "the asset's retention is not `.keepForever` and `expiresAt` is nil".
   Every asset the user just rescued matches that condition, so the next "Re-export", "Export
   now" (onboarding plan footer) or template change re-arms deletion, and the launch sweep
   removes the audio the user explicitly asked to keep. This is the owner's request ("no
   deletion") violated by the plan's own guard. The detail line has the same hole: a cleared
   asset (`.keepDays`, `expiresAt` nil, `.ready`, every delivery `.delivered`) falls through
   the status table to "Recording kept while processing". Evidence: `AudioRetention` and
   `expiresAt` are separate fields (`Sources/StenoCore/Model/Audio.swift:22-30, 83`);
   `rerunSummary` and `redeliver` both call `deliver` and are the paths rule 2 hooks
   (`Sources/StenoCore/Pipeline/ProcessingPipeline.swift:179-203`).
   Fix: replace `clearExpiry(assetIDs:)` with `keepForever(assetIDs:)` that sets
   `retention = .keepForever` and `expiresAt = nil` in one write. Rule 2 then excludes these
   assets by its existing `.keepForever` test, and the header reads "Recording kept forever"
   without a new table row. Add to the step 3 test: after switching to Forever, `redeliver`
   leaves `expiresAt` nil.

2. **Major.** `2026-09-28-device-change-during-recording.md`, Decision 6, step 2 (`writeGap`),
   step 4 (rebuild order).
   The gap is written "through the sink's producer API, at a moment when no producer runs",
   after `processing.stop()` and before the new `ProcessingThread` starts, capped at 10 s.
   The rings behind `LaneFrameSink` hold 2 s (`ringSeconds: 2`,
   `Sources/StenoAudio/RealTime/LaneFrameSink.swift:22-28`), `beginCallback` returns `false`
   when `rings.reserve` fails (line 35-39), and with the consumer stopped nothing drains. Any
   gap over about 2 s (the retry ladder alone is about 4 s, a USB replug longer) is silently
   truncated, the master is no longer wall-time aligned, `gapSeconds` reports silence that was
   never written, and transcript timestamps after the switch lag the elapsed timer by the
   dropped amount, which is the opposite of the decision's stated reason. Step 7 of the
   manual verification would fail on step 4 of the same list (four attempts, about 4 s).
   Fix: write the gap through `FrameRelay` instead of the rings. `WriterThread` keeps draining
   the relay during the rebuild (the plan already keeps it running), the relay's
   `beginFrame` / `write(channel:from:)` / `endFrame` API exists
   (`Sources/StenoAudio/RealTime/FrameRelay.swift:26-36`), the relay is single-producer and
   the processing thread that normally produces into it is stopped. Write in `frameSize`
   chunks and, when `beginFrame()` returns `false`, `try await clock.sleep(for:
   .milliseconds(5))` and retry; this is on the actor, off the real-time path. Drop the
   `writeGap(frames:)` addition to `LaneFrameSink`; `LaneFrameSinkTests` then keeps only the
   rearm case.

3. **Major.** `2026-09-28-floating-recording-indicator.md`, Decision 8, step 5, step 8
   (`testBubbleFollowsARecordingStartedFromTheWindow`), Open questions item 1.
   The bubble's body click opens the main window with `openWindow(id: "main")` "from a
   `@Environment` captured in the hosted view". A view hosted by `NSHostingController` inside
   an `NSPanel` the presenter creates is outside every SwiftUI scene, and the `openWindow`
   environment action silently does nothing there; Apple's own answer is
   `NSHostingSceneRepresentation`, which is macOS 26 only and is reported not to open
   `Window(id:)` scenes (FB20432458). Sources:
   https://developer.apple.com/forums/thread/651592,
   https://developer.apple.com/forums/thread/802435. The plan's first fallback,
   `NSWorkspace.shared.openApplication(at: Bundle.main.bundleURL, ...)`, relies on reopen
   handling for a running app and is unverified; the second fallback (`makeKeyAndOrderFront`
   on a found `NSWindow`) cannot create the window after the user closed it, which is exactly
   when the bubble is the only surface left. Step 8's second smoke test asserts the main
   window comes forward and would fail on the hosted `macos-15` job.
   Fix: capture the action inside a scene and hand it to the presenter: in `RootView` (or
   `MenuBarLabel`, both inside the `MenuBarExtra` scene) add `@Environment(\.openWindow)`
   and `.task { bootstrap.openMain = { openWindow(id: "main"); NSApp.activate() } }`;
   `FloatingPanelPresenter.follow` takes that closure. `OpenWindowAction` is a value and works
   when called later from a closure created in a scene view, which is how
   `MenuBarView.swift:170-171` already opens the window. Delete the `NSWorkspace` fallback
   and the open question.

4. **Major.** `2026-09-28-app-icon.md`, step 2 "Mac renders".
   The density is `$((96 * px / 1024))`. Bash arithmetic is integer, so for `px=16` the
   density is `1` (1536 / 1024 truncates) and librsvg renders the 1024 pt canvas at 1 dpi
   over 96: a 10 x 10 PNG. Verified on this host with the committed SVG and the exact
   command: `px=16 density=1 -> 10x10`, `px=32 density=3 -> 32x32`, `density=1.5 -> 16x16`
   (ImageMagick 7.1.2-24, RSVG 2.62.1). The script would commit a wrong `icon_16x16.png`,
   actool warns and drops the mismatched image, the Finder sidebar and Spotlight size falls
   back, and `testAppIconSetListsEveryFileAndEveryFileExists` fails on `pixelsWide == 16`
   (the test is right; the script is wrong). Every other size divides evenly and is fine.
   Fix: compute a fractional density, `density=$(awk -v px="$px" 'BEGIN { printf "%.6f",
   96 * px / 1024 }')`, and pass `-density "$density"`; ImageMagick accepts fractional
   densities (verified above). Add "the 16 px render is 16 x 16" to the Verification list so
   the trap stays visible.

5. **Major.** `2026-09-28-start-recording-from-main-window.md`,
   `2026-09-28-floating-recording-indicator.md`, `2026-09-28-onboarding-vault-and-llm.md`,
   `2026-09-28-macos-visual-redesign.md`, Findings sections.
   A large share of the `file:line` citations in these four plans do not match the tree at
   `9cd7cf5` (nor at `bcf5eef`, nor at any commit since the last refactor of these files).
   The wrong numbers are consistently 1.4 to 1.9 times the real ones inside the same file
   while other citations in the same plan are exact, so the plans were partly written from a
   different rendering of the files. The retention and device-change plans cite correctly
   throughout (the retention plan states its commit, which is the house practice). The
   house review format requires exact references; a reader following these lands on
   unrelated code. Wrong citation, then the actual location at `9cd7cf5`:
   - start plan: `MenuBarView.swift:139-178` (recording section) is 45-80; `:149-155`
     (`TimelineView`) is 54-58; `:157-158` is 60-62; `:160-176` (buttons) is 63-79;
     `:272-311` (`LevelBars`) is 177-215. `MeetingListView.swift:66-67` (init) is 6-7;
     `:175` (empty copy) is 115; `:184-213` (`MeetingRow`) is 124-153; `:135-140` (delete
     refused) is 19-20 and 44, with `canDelete` at 75-80. `MeetingListViewModel.swift:244-246`
     is 30-32 (the file has 139 lines). `Fakes.swift:175-206` (`FakePermissions`) is 25-56
     (77 lines). `RecordingControllerTests.swift:199` (`as? FakeCalendar`) is 66; `:152-182`
     is 19-50 and 140-173. `LaunchSmokeTests.swift:117-150` is 11-44 (45 lines).
   - floating plan: `AppController.swift:345-352` (`shutdown`) is 127-133 (135 lines).
     `MenuBarView.swift:388-427` is 177-215, `:424` (`fraction`) is 212, `:381-385`
     (`requestedMeetingID`) is 169-172. `DetectionPromptViewModel.swift:100-157` is 36-54
     (72 lines). `DetectionController.swift:174-178` is 16-20 and `:232-247` (`handle`) is
     74-88 (128 lines).
   - onboarding plan: `Services/LLMWiring.swift:211-218` is 18-19 (49 lines);
     `Sources/StenoLLM/LLMEndpoint.swift:167-172` is 60-62 (79 lines);
     `Stages/Summarize.swift:276-278` is 30-31 and `:280` is 34 (42 lines);
     `MeetingDetailView.swift:290` (chip) is 54, `:297-299` (tokens) is 61-62, `:322-325`
     (actions) is 86-88, `:415-418` (footer) is about 176-190; `OnboardingView.swift:138-140`
     (auto-close) is 42-44 (116 lines). Step 1 names `Pipeline/PipelineStage.swift` for
     `PipelineDependencies`; the struct is `Pipeline/ProcessingPipeline.swift:7-40`.
   - redesign plan: `MeetingListView.swift:97-108` (toolbar) is 37-46; `:142-164` (filters)
     is 82-104; `:72` (`Divider`) is 12; `:95` (`searchable`) is 35; `:73-85` and `:73-84`
     (`List`) is 13-24; `:96` (background) is 36; `:166-181` (empty state) is 106-121;
     `:184-214` is 124-153; `:190` (title) is 130; `:208-209` (meta) is 142-148.
     `MeetingDetailView.swift:260-323` (header) is 46-100; `:263` is 49; `:270-280` is
     56-63; `:328-329` (`roundedBorder`) is 115; `:340-346` ("Add tags") is 124-127;
     `:351-374` (tabs) is about 140-175; `:391-406` and `:409-441` (footer, badge) are about
     176-190 and 195-218. `MenuBarView.swift:131-157` is 45-80; `:170-174` is 54-58;
     `:293-331` is 177-215; `:180-196, 228-248` (queue and recent rows) are within 81-176.
     `SpeakerReviewSheet.swift:387` (frame) is 56, `:401` (`Card`) is 70, `:464-479`
     (candidate scroller) is 133 (193 lines).
   The behaviour each citation describes was confirmed at the actual location in every case
   above; only the numbers are wrong. Fix: re-cite from `9cd7cf5` and, as the retention plan
   does, state the commit at the top of each Findings section.

6. **Major.** `2026-09-28-macos-visual-redesign.md`, step 7a.
   "The preview environment seeds one meeting in each of recording (with a fake session so
   the recorder reports `.recording(since:)`), queued, processing and failed." A `.recording`
   row seeded when the preview root is built is reconciled to `.failed` by
   `AppEnvironment.reconcileInterruptedRecordings()` during `AppController.launch()`
   (`apps/macos/Steno/AppEnvironment.swift:142-146`;
   `AppEnvironmentTests.testReconcileMarksInterruptedRecordingsFailed`,
   `AppControllerTests.testLaunchMarksInterruptedRecordingsFailedAndSweepsExpiredAudio`), so
   the UI test in the same step cannot find `stop-recording-header` or the "Recording"
   empty-state title, and the seeded recording meeting also disappears from the second
   screenshot checklist in Verification. The recorder cannot be told to "report
   `.recording(since:)`" for a row it did not begin: `RecordingController.recording` is
   `private(set)` and only `start(mode:)` sets it
   (`apps/macos/Steno/Recording/RecordingController.swift:43-50, 86-124`).
   Fix: do not seed a `.recording` row. Honour a `-steno-recording` launch argument that,
   after `launch()`, calls `controller.recorder.start(mode: .call)` in the preview
   environment; the synthetic backend (`seconds: 2`) leaves the session in `.recording`
   after the tone ends, and the row is written by `LocalRecordingIntake.begin` after
   reconciliation, as the start plan's `testSidebarStartsAndStopsARecording` already relies
   on.

7. **Minor.** `2026-09-28-audio-retention-keep-forever.md`, Spec "Detail header" toggle,
   Decision 2.
   `setKeepAudio(false)` writes "default rule, `expiresAt` from now" directly, bypassing the
   delivery guard that Decision 2 introduces for the pipeline. With a `.keepDays(N)` default
   and a failed export, switching the toggle off on that meeting stamps a deadline the guard
   would have refused, and the `audio.m4a` re-export becomes impossible after N days. With
   `.deleteAfterProcessing` the confirmation dialog makes it explicit, so only the days case
   is silent. Fix: in `setKeepAudio(false)` stamp `expiresAt` only when
   `store.deliveries(meetingID:)` are all `.delivered` (or empty); otherwise leave it nil so
   the header reads "Recording kept until the export succeeds". One extra case in
   `testKeepAudioTogglesRetention`.

8. **Minor.** `2026-09-28-device-change-during-recording.md`, step 1.
   `SchemaSnapshotTests.fullMigratorMatchesTheLatestGolden` picks the golden from
   `Migrations.identifiers.last` (`Tests/StenoCoreTests/SchemaSnapshotTests.swift:26-32`),
   and `identifiers` is a hand-written list `["v1", "v2"]`
   (`Sources/StenoCore/Storage/Migrations.swift:22`). Step 1 adds `registerMigration("v3")`
   and `v3.sql` but not the identifier, so the test keeps comparing against `v2.sql` and
   fails on the new column while the intended `v3.sql` is never read. Fix: add `"v3"` to
   `Migrations.identifiers` in the same step. `FixtureManifestTests` is indeed unaffected:
   `Tests/Fixtures/MANIFEST.sha256` lists no `snapshots/schema` file.

9. **Minor.** `2026-09-28-start-recording-from-main-window.md`, Decisions "Default mode is a
   call", step 4, step 8; `2026-09-28-macos-visual-redesign.md`, Components table
   (`RecordingControl` split geometry).
   The idle control is "a `Menu` with `primaryAction`" styled with `StenoPrimaryButtonStyle`.
   A SwiftUI `Menu` is styled by `menuStyle`, not `buttonStyle`; only `.menuStyle(.button)`
   lets a `buttonStyle` reach it, and even then the redesign's custom split geometry (main
   segment, 28 pt chevron segment, hairline between) is not something the system control
   exposes. Separately, XCUITest exposes a macOS `Menu` as a `popUpButton` or `menuButton`,
   not a `button`, so step 8's `app.buttons["sidebar-record"]` and the redesign's step 5
   frame comparison may not resolve. The macOS rendering of `Menu` with `primaryAction`
   itself is documented only by third parties (pull-down button with a primary click,
   segmented control in toolbars): https://www.hackingwithswift.com/quick-start/swiftui/how-to-show-a-menu-when-a-button-is-pressed.
   Fix: compose the split control from two views in one `HStack`: a `Button("Record call")`
   with `StenoPrimaryButtonStyle` and id `sidebar-record`, and a `Menu { Button("Record in
   person") }` with `.menuIndicator(.hidden)`, a chevron label and id
   `sidebar-record-in-person`. Behaviour, ids and the presentation table are unchanged; the
   redesign then owns the geometry it already specifies.

10. **Minor.** `2026-09-28-macos-visual-redesign.md`, Decision 14, "Selection styling".
    The claim "macOS 15 paints the system accent behind a selected `List` row in every list
    style and above `.listRowBackground`" is inverted. The highlight is AppKit's
    `NSTableRowView` selection, and an opaque `.listRowBackground` draws over it; it is
    `Color.clear` (or no background) that lets the accent show. Source:
    https://pd95.github.io/SwiftUI-List/ ("our row background is now drawing over the
    selection box drawn by macOS"; "Color.clear ... triggers the system color for
    selections"). The conclusion (own the selection in a `ScrollView`) still holds and
    avoids the timing quirk of mouse-down highlight versus mouse-up selection, but the
    reason should be stated as "the highlight follows the system accent and cannot be
    recoloured; covering it per row needs an opaque background and leaves the mouse-down
    flash". Fix: reword Decision 14 and the first paragraph of Selection styling.

11. **Minor.** `2026-09-28-floating-recording-indicator.md`, step 2 (`BubblePresentation.make`)
    and step 7; `2026-09-28-device-change-during-recording.md`, step 8.
    `BubblePresentation.make(state:autoStop:)` is a static on an `Equatable` value type with
    `autoStop` typed as the device plan's `AutoStopCountdown`, a `@MainActor @Observable`
    class. Reading `remainingText` or `fractionRemaining` from a `nonisolated` static is a
    strict-concurrency error, and making `make` main-actor isolated defeats the hostless
    test the plan wants. The same shape works for `RecordingControlPresentation.autoStopLine`.
    Fix: give `make` a small `Sendable` value (`appName: String?`, `remainingText: String`,
    `fractionRemaining: Double`) that `AutoStopCountdown` exposes as one property, and pass
    that. `FloatingContent.resolve(prompt:recording:)` is fine as written: it only stores the
    prompt model and is called on the main actor.

12. **Minor.** `2026-09-28-floating-recording-indicator.md`, Decision 2, step 3.
    `withObservationTracking`'s `onChange` closure is `@Sendable` and runs on whichever
    thread performed the mutation, not on the main actor; the presenter must re-enter the
    actor (`Task { @MainActor in self.apply(...) ; self.observe() }`) and re-register on every
    change because the tracking is one-shot. Compiles under Swift 6 only with that hop.
    Fix: name the hop in step 3 so the implementer does not reach for a `nonisolated(unsafe)`
    shortcut.

13. **Minor.** `2026-09-28-start-recording-from-main-window.md`, Behaviour spec row
    "Permission denied", step 2 (`PermissionKind.deniedMessage`), step 7.
    `PermissionsService.state(of: .systemAudio)` returns `.granted` or `.unknown`, never
    `.denied` (`apps/macos/Steno/Services/PermissionsService.swift:40-41`; the macOS plan's
    deviations say there is no status API for the tap). "System audio access is denied." and
    the both-denied string are therefore unreachable in the product; only `FakePermissions`
    produces them, and manual verification step 5 covers the microphone only. Not wrong,
    but the plan should say so, otherwise a reader expects a state the app cannot enter.
    Fix: add one sentence under "Permission gating is a report, not a guard": "In the live
    app only the microphone can be `.denied`; the system audio rows exist for the fake and
    for a future status API."

14. **Minor.** `2026-09-28-first-run-feedback.md`, preamble ("Nothing here widens the
    scope"); `2026-09-28-device-change-during-recording.md`, Decision 8.
    The auto-stop 90 s after the call app releases the microphone is a new behaviour that
    ends recordings on its own. The scope names "popup prompt when another app opens the
    microphone, plus manual start/stop from the menu bar"
    (`.plans/2026-09-24-initial-scope.md:59-60`) and nothing about stopping. The device plan
    is honest about lifting the audio plan's v1.1 deferral (rebuild) but the auto-stop is a
    second addition, and the index's blanket claim is inaccurate. Fix: in the index say
    "the device-change plan adds one behaviour, an intentional auto-stop after the call
    ends; see its Decision 8", and have the implementing PR add one line to the scope's
    Capture (Mac) list.

15. **Minor.** `2026-09-28-app-icon.md`, step 1 and step 3.
    `apps/macos/Steno/Resources/AppIcon.svg` is already committed, by the plans commit
    `0d6dd14 docs(plans): first-run feedback plans for the macOS app`, and is byte-identical
    to the plan's block (checked with `diff`). Step 1 ("Commit the source") is done, a
    `docs(plans)` commit carried a code asset into the app target's resources, and step 3's
    "in one commit" is no longer possible. Fix: mark step 1 as landed with the hash and note
    that the SVG is shipped as a plain resource since `0d6dd14`.

16. **Minor.** `2026-09-28-macos-visual-redesign.md`, "Recording and processing states"
    table, Recording row; `2026-09-28-onboarding-vault-and-llm.md`, step 7.
    The redesign cites `LevelBars` at `MenuBarView.swift:293-331`, but by its own ordering
    the start plan has moved `LevelBars` to `Recording/RecordingViews.swift` before the
    redesign lands (index Ownership table). Same file, different gap: the onboarding plan
    mounts `SetupBanner` "above the detail column content", and the redesign, which owns
    every window token and the detail pane layout, never mentions the banner, so the last
    plan to touch the pane has no spec for a surface the fourth plan added. Fix: cite
    `Recording/RecordingViews.swift` and add one row to the redesign's detail pane spec for
    the banner (a `Card` with `MessageRow(kind: .info)`, 32 pt insets like the header).

17. **Minor.** `2026-09-28-onboarding-vault-and-llm.md`, UX spec page 1;
    `2026-09-28-audio-retention-keep-forever.md`, step 6.
    The two plans quote the shared sentence differently: "Recordings are kept forever in
    Steno." (onboarding) versus "Recordings are kept forever in <folder name>." (retention,
    the owner). Fix: the onboarding plan quotes the retention plan's wording or says "the
    retention plan's sentence" without an example.

18. **Minor.** `2026-09-28-audio-retention-keep-forever.md`, Findings "The setting exists".
    "`{"keepDays":30}` (`Tests/StenoCoreTests/SettingsStoreTests.swift:36`)": the line
    asserts `{"keepDays":7}`. The encoding claim is right; the example value is not what the
    test pins. Fix: quote `7` or drop the number.

19. **Minor.** `2026-09-28-device-change-during-recording.md`, Findings "Which listener
    fired", "The detector's 2 s debounce and 1 s poll".
    Correct against the audio plan's deviations (`2026-09-25-audio-capture.md:460`, poll
    every second) but the same plan's design table still says "Poll every 2 s" (line 50).
    Not this plan's defect; noted so the implementing PR's pointer line in the audio plan
    also fixes the table.

20. **Minor.** `2026-09-28-floating-recording-indicator.md`, Findings "How the prompt and the
    menu bar item are built today", `swift-ci.yml:307-362`.
    Correct (`ui-smoke` job at line 307, hosted `macos-15`, `-only-testing:StenoUITests`).
    Listed here only because the same paragraph's other citations are wrong (finding 5) and
    a reader should know which ones to keep.

## Verified claims

| Claim | Plan | Status | Source |
|---|---|---|---|
| `RecordingController` is `@MainActor @Observable`; `RecordingState` has `idle`, `starting`, `recording(since:)`, `stopping`; `label` strings "Not recording", "Starting…", "Recording", "Finishing…"; `toggleRecording`, `awaitSettled`, `lastError`, `lastWarning`, `levels`, private `Active.meetingID` | start, floating, device | verified | `apps/macos/Steno/Recording/RecordingController.swift:7-21, 29-58, 66-71, 86-124, 131-159, 161-175` |
| `AppController.recorder`, `detection`, `requestedMeetingID`, `loginItemRegisteredKey`, `startRecording` closure, `shutdown()` stops a live recording after `awaitSettled` | start, floating, onboarding, device | verified | `apps/macos/Steno/AppController.swift:14-35, 127-133` |
| `MeetingListViewModel.stateFilter`, `tagFilter`, `all`, `selection`, `StateFilter` cases `all/processing/ready/failed`, "In progress" includes `.recording` | start, redesign | verified | `apps/macos/Steno/Main/MeetingListViewModel.swift:10-52` |
| `SettingsStore.load/save/observe`; `save` writes every property as a row | retention, onboarding | verified | `Sources/StenoCore/Storage/SettingsStore.swift:15-33` |
| `LocalRecordingIntake.defaultTitle(source:startedAt:timeZone:)` is `public static`; `complete` sets retention and clears `expiresAt` | retention, redesign, device | verified | `Sources/StenoCore/Storage/LocalRecordingIntake.swift:128-178` |
| `PipelineStage.label` at `Labels.swift:17-33`; `TimeInterval.clockText` | redesign, start | verified | `apps/macos/Steno/Design/Labels.swift:17-33`, `Design/Components.swift:173` |
| `LevelBars.fraction(_:)` is `static`, maps -60...0 dBFS to 0...1, animates with `Motion.functional` | floating | verified (location wrong, finding 5) | `apps/macos/Steno/MenuBar/MenuBarView.swift:177-215` |
| `ManualClock` (StenoCore/Testing), `FakeProcessAudioActivity` (StenoAudio/Testing), `FakePermissions.states` mutable with `allGranted()`, `FakeCalendar`, `FakeDestination(deliverFailure:)`, `FakeDeliveryDispatcher` | all | verified | `Sources/StenoCore/Testing/ManualClock.swift:7`, `Sources/StenoAudio/Testing/FakeProcessAudioActivity.swift:7`, `apps/macos/Steno/Services/Fakes.swift:25-56`, `Sources/StenoCore/Testing/FakeDelivery.swift:5-67` |
| `SyntheticCaptureBackend(lanes:tone:seconds:loseDeviceAfter:realTime:)` and `(signals:seconds:callbackFrames:realTime:loseDeviceAfter:)`; `start` throws `invalidState` unless `stop()` cleared the thread; two external `loseDeviceAfter` call sites | device | verified | `Sources/StenoAudio/Testing/SyntheticCaptureBackend.swift:43-76, 89-96, 147-150`; `apps/macos/StenoTests/TestSupport.swift:29-41`, `Tests/StenoAudioTests/CaptureSessionTests.swift` |
| `MessageRow.Kind` has `error`, `warning`, `info`; `StatusChip(meeting.state)` text "Recording" in `liveBright`; `PendingText`; `Card`; two button styles | start, onboarding, redesign | verified | `apps/macos/Steno/Design/Components.swift:8-30, 44-73, 87-127, 131-147, 162-166` |
| `Theme.tokens` list; `card/muted/secondary/accent` are black alpha veils in light; radii `8` and `5` only; `Space` stops at `xl 24`; `popover` opaque both appearances; `TextSize` carries `lineHeight`; status hues as quoted (dark values) | redesign, floating, icon | verified | `apps/macos/Steno/Design/Theme.swift:43-124` |
| `Motion` has `functional`, `exit`, `entrance`, `spatial`, no `pulse` or `countdown` | floating, redesign | verified | `apps/macos/Steno/Design/Motion.swift` |
| `DetectionPanelPresenter` style mask, level, collection behaviour, 360 x 132 frame at top-right, `orderOut` on dismiss; prompt shows "51s"-style countdown and two buttons with inert shortcuts | floating | verified | `apps/macos/Steno/Detection/DetectionPanel.swift:14-86` |
| `DetectionController.prompt` with `promptDidChange`, `startRecording: (() async -> Void)?`, `appName: @MainActor (String?) -> String`, `liveAppName` returns "Another app" for nil; `handle` drops `.microphoneOpened` while recording and dismisses on `.microphoneReleased`; `MeetingDetector.Event.microphoneOpened(bundleID:pid:)` | floating, device | verified | `apps/macos/Steno/Detection/DetectionController.swift:16-29, 74-88, 119-127`; `Sources/StenoAudio/Detection/MeetingDetector.swift:19-21` |
| Onboarding is permissions only; window auto-closes on `isFinished`; subtitle has no `fixedSize` inside the 520 pt frame; opener runs once per launch for required kinds | onboarding, redesign, start | verified | `apps/macos/Steno/Onboarding/OnboardingViewModel.swift:17, 44-46`; `OnboardingView.swift:16-18, 39, 42-44`; `StenoApp.swift:127-145` |
| Fakes run in the product without an endpoint: `LLMWiring.passes` nil until URL and model set; `PassthroughCleaner()` and `FakeSummarizer()` fallbacks in app and CLI; fake output strings and usage 200/100 and 100/50; summarize replaces a non-calendar title | onboarding | verified | `apps/macos/Steno/AppEnvironment.swift:207-214`, `Services/LLMWiring.swift:18-19`, `Sources/StenoLLM/LLMEndpoint.swift:60-62`, `Sources/steno/Wiring.swift:74-75`, `Sources/StenoCore/Testing/FakeLLM.swift:31-41, 57-97`, `Pipeline/Stages/Summarize.swift:30-34` |
| Delivery: `destinations(for:)` returns `[]` without `settings.obsidian`; `deliverAll` returns without rows; `deliver` never throws; `retention` runs after `deliver` regardless; `rerunSummary` sets `.ready` and delivers; `redeliver` delivers only | onboarding, retention | verified | `Sources/StenoAdapters/Runtime/DeliveryCoordinator.swift:32-56`, `Sources/StenoCore/Pipeline/Stages/Deliver.swift:6-9`, `Pipeline/ProcessingPipeline.swift:159-203`, `Stages/Retention.swift` |
| `Settings.defaultRetention = .keepDays(30)`; `AudioRetention` three cases, `expiry(from:)`; `expiredAssets(now:)` is a pure `expiresAt <= now` query; `RetentionSweep` keeps URLs; `MeetingStore.deliveries(meetingID:)` exists; `Settings.swift:6` says the folder is picked in onboarding | retention, onboarding | verified | `Sources/StenoCore/Model/Settings.swift:6-8, 27`, `Model/Audio.swift:22-30`, `Storage/MeetingStore.swift:357-365, 374`, `Storage/RetentionSweep.swift:1-12` |
| Migrations append-only in one file, `v1` and `v2` registered, `audioAsset.retention/retentionDays/expiresAt` columns; schema goldens `v1.sql`, `v2.sql`; the snapshot test reads `Migrations.identifiers.last` | device, retention | verified (identifier omission, finding 8) | `Sources/StenoCore/Storage/Migrations.swift:4-22, 127-132`; `Tests/Fixtures/snapshots/schema/`; `Tests/StenoCoreTests/SchemaSnapshotTests.swift:26-32` |
| GRDB `db.alter(table:) { t.add(column:) }` for `ALTER TABLE ADD COLUMN`; synthesised `Codable` omits a nil optional on encode and tolerates a missing key on decode | device | verified | GRDB README "Modify tables"; Swift stdlib `Codable` synthesis (`encodeIfPresent` / `decodeIfPresent`) |
| Five HAL listeners share one `reportDeviceLost` closure on `listenerQueue`; nominal rate read once at start; latencies read once; `LaneFrameSink` latches with `Atomic<Bool>`; `CaptureSession` is an actor, `deviceLost()` via `Task`, `finish()` closes files; `ProcessingThread` builds the delay line at init and owns a `LevelSlot`; `IOProcRunner.deliver` is the real-time path; tap is `stereoGlobalTapButExcludeProcesses` | device | verified | `Sources/StenoAudio/Capture/LiveCaptureBackend.swift:16-27, 105-116, 142-154, 161-171`; `RealTime/LaneFrameSink.swift:14-29, 71-74`; `Capture/CaptureSession.swift:16, 34-45, 122, 140-149, 152-202, 241-281`; `RealTime/ProcessingThread.swift:49-83`; `RealTime/IOProcRunner.swift:57`; `Capture/ProcessTap.swift:17` |
| `AudioDevices.defaultInput()`, `defaultSystemOutput()`, `device(uid:)` exist; `CaptureError.inputDeviceUnavailable`, `.deviceLost`, `.writerFailed`; `CaptureStatistics.endedOnDeviceLoss` | device | verified | `Sources/StenoAudio/Capture/AudioDevices.swift:48-58`; `Capture/CaptureConfiguration.swift:61-94, 172-191` |
| Test names the plans extend exist: `testToggleRecordingStartsThenStops`, `testAFailingSessionFactoryLeavesNoMeetingRow`, `testDeviceLossStopsAndEnqueuesThePartialRecording`, `testPromptCountsDownOnTheClockAndTimesOut`, `testDetectionPromptStartsACallRecordingWhileTheDetectorRunsOn`, `testShutdownStopsTheRecordingAndTheDetector`, `testKeepAudioTogglesRetention`, `testLabelsAreWordsNotRawValues`, `testMainWindowOpens`, `testCurrentIsDerivedForEveryPermissionCombination` (the 81-case matrix) | all | verified | `apps/macos/StenoTests/*.swift`, `apps/macos/StenoUITests/LaunchSmokeTests.swift:11` |
| `project.yml` sets `ASSETCATALOG_COMPILER_APPICON_NAME: AppIcon`, empty accent, no `LSUIElement`; `InfoPlistTests:77` pins no `LSUIElement`; `AppIcon.appiconset/Contents.json` lists ten mac entries with no `filename` and the folder has no images; `mobile/app.config.ts` has no icon and `mobile/assets/` does not exist; `mobile/README.md:98, 127-128` as quoted | icon, floating | verified | the files named |
| ImageMagick 7.1.2 with the librsvg delegate on this host; `magick -list format` shows `RSVG`; fractional `-density` accepted; `-define png:exclude-chunks=date,time`, `-strip`, `%[opaque]` are valid | icon | verified (integer-density defect, finding 4) | `magick -version` (7.1.2-24, RSVG 2.62.1); renders run for this review |
| macOS icon grid: 1024 canvas, 824 x 824 tile at (100,100), radius about 185.4, transparent margin, no system mask on macOS | icon | verified | https://developer.apple.com/forums/thread/670578; https://github.com/block/buzz/issues/3272 |
| actool with `--app-icon AppIcon` emits `AppIcon.icns` plus `Assets.car` and a partial Info.plist with `CFBundleIconName` and `CFBundleIconFile` | icon | verified (community reports, consistent across three projects) | https://github.com/justin13888/Sift/pull/133; https://mjtsai.com/blog/2025/08/08/separate-icons-for-macos-tahoe-vs-earlier/ |
| Expo `ios.icon` accepts an object with `light`, `dark`, `tinted`; 1024 x 1024 recommended | icon | verified | https://docs.expo.dev/versions/latest/config/app/ |
| App Store Connect rejects a 1024 marketing icon with an alpha channel | icon | verified | Apple HIG "App icons" (no transparency for the App Store icon); long-standing ITMS-90717 |
| `MenuBarExtra` label images are rendered as template images; `.symbolRenderingMode(.multicolor)` and `foregroundStyle` are ignored there (cause A) | floating | verified | https://developer.apple.com/forums/thread/738716; https://developer.apple.com/forums/thread/726565 |
| `NSStatusItem` visibility is persisted per `autosaveName` as `NSStatusItem Visible <name>` in user defaults after a ⌘-drag removal (cause C) | floating | verified | AppKit `NSStatusItem.isVisible` and `autosaveName` documentation |
| `NSHostingSizingOptions.preferredContentSize` exists (macOS 13+) | floating | verified | https://developer.apple.com/documentation/swiftui/nshostingsizingoptions |
| `openWindow` environment action from a view hosted outside a SwiftUI scene does nothing before macOS 26 | floating | verified, plan wrong (finding 3) | https://developer.apple.com/forums/thread/651592; https://developer.apple.com/forums/thread/802435 |
| `List` selection on macOS is AppKit-drawn in the system accent and cannot be recoloured from SwiftUI; an opaque `listRowBackground` covers it, `Color.clear` reveals it | redesign | verified, plan's mechanism inverted (finding 10) | https://pd95.github.io/SwiftUI-List/; https://developer.apple.com/forums/thread/719507 |
| `.toolbar(removing: .title)` and `.toolbarBackground(.hidden, for: .windowToolbar)` on macOS 15 | redesign | unverified (Apple page body not retrievable; `.title` kind believed macOS 15+, which matches the deployment target) | https://developer.apple.com/documentation/swiftui/view/toolbar(removing:) |
| `NavigationSplitView(sidebar:content:detail:)` three columns with `.hiddenTitleBar` paint their own opaque backgrounds reliably | redesign | unverified; the plan's own open question 6 covers it | |
| `Menu` with `primaryAction` renders as a split or pull-down button on macOS and accepts `buttonStyle` | start, redesign | unverified; third-party only (finding 9) | https://www.hackingwithswift.com/quick-start/swiftui/how-to-show-a-menu-when-a-button-is-pressed |
| `TimelineView(.periodic)` inside a `MenuBarExtra` label refreshes on its own schedule | floating | unverified; no bug report found, label refresh on state change confirmed by a forum thread | https://developer.apple.com/forums/thread/720625 |
| SwiftUI buttons inside a `.nonactivatingPanel` with `canBecomeKey == false` receive clicks | floating | unverified; the shipped `DetectionPanel` relies on the same property and no field report confirms the click worked | `apps/macos/Steno/Detection/DetectionPanel.swift:24-33` |
| Core Audio: a private aggregate keeps running when a tapped process stops (silence); default-device and `DeviceIsAlive` notifications fire on a Bluetooth HFP transition; `NominalSampleRate` on an aggregate | device | unverified; the plan and the audio plan both mark these as needing a Mac (`2026-09-25-audio-capture.md:395-397`) | |
| Menu bar crowding hides status items without an overflow chevron (cause B) | floating | unverified; widely reported, no Apple source | |
| GitHub `macos-15` image has Homebrew and lacks ImageMagick | icon | Homebrew verified (runner image README); ImageMagick absence unverified and not load-bearing (the script never runs in CI) | https://github.com/actions/runner-images/blob/main/images/macos/macos-15-Readme.md |

## Not checked

- Jamie's pixel measurements, colours and copy in the floating and redesign plans; they are
  design references, not correctness claims.
- The owner's device configuration during the 56 minute call and which HAL selector fired;
  the device plan lists it as its first open question.
- Bluetooth HFP transition behaviour on macOS 15 (`coreaudiod` log lines, device object
  recreation). No Mac was available to this review.
- Whether `NSWorkspace.shared.openApplication(at: Bundle.main.bundleURL, ...)` reopens a
  closed `Window(id: "main")` scene in a running SwiftUI-lifecycle app; finding 3 removes the
  need.
- Rendering equivalence of librsvg and CoreSVG for the icon's gradient and pills (icon plan
  reason 4); only the Linux render was produced.
- The redesign's type scale, spacing and contrast values against the design-craft system;
  the elegance review owns that.
- `xcodegen generate` picking up `AppIcon.svg` and the new `Panels/` folder; no macOS
  toolchain on this host.
- Byte-level determinism of PNG output across ImageMagick and librsvg versions
  (`make-app-icon.sh --check`); the icon plan's open question 4 already scopes it to the
  regenerating developer.
