# Steno first-run feedback plans: test strategy review

Reviewed 2026-09-28 against commit `9cd7cf5` (`docs(plans): simplify the first-run
feedback plans`): the index `2026-09-28-first-run-feedback.md` and its seven plans
(start-recording-from-main-window, onboarding-vault-and-llm, audio-retention-keep-forever,
floating-recording-indicator, device-change-during-recording, app-icon,
macos-visual-redesign). Question, as in the 2026-09-25 review: can an implementing agent
prove each decision and each state-table row with a test that runs on CI as it exists
(`swift-ci.yml`: `package` and `app` jobs on `vars.MACOS_RUNS_ON` or `macos-15`,
`ui-smoke` on hosted `macos-15` only; no Linux job, the container is a local step), and
can a reviewer trust that proof without a Mac?

Read for this review: the workflows, `Tests/README.md`, `apps/macos/StenoTests/*`,
`apps/macos/StenoUITests/LaunchSmokeTests.swift`, `Services/Fakes.swift`, `TestSupport.swift`,
`Sources/*/Testing/*`, `CaptureSession.swift`, `LaneFrameSink.swift`,
`SyntheticCaptureBackend.swift`, `RecordingController.swift`, `DetectionController.swift`,
`ProcessingPipeline.swift`, `MeetingStore.expiredAssets`, `Migrations.swift`,
`SchemaSnapshotTests.swift`, `TabTextSnapshotTests.swift`, `AppEnvironment.preview`.

Severity: blocker = the named test cannot pass, cannot run on CI, or cannot fail when the
behaviour is wrong; major = a deterministic test is possible but the plan offers none, or
an existing test breaks and the plan does not name it; minor = seam, flakiness or reviewer
hygiene.

## Findings

### Blocker

1. **Device-change, decision 6 and step 4: the one test of "same files, contiguous" cannot
   fail when the gap fill is wrong, and its expected value is wall-clock.** Step 4 measures
   the gap "from the report time" and writes it with `sink.writeGap` after `backend.stop()`
   and `processing.stop()` and before the new `ProcessingThread` starts. At that moment no
   consumer drains `LaneRings`; `beginCallback` refuses once a ring is full, so any gap
   longer than the ring drops silence into `droppedSamples` and the master is shorter than
   wall time, which is the misalignment decision 6 exists to prevent. The named test
   `aDeviceChangeKeepsRecordingOnTheSameFiles` asserts "duration is the two halves plus the
   gap" with a gap taken from `ContinuousClock` while the test runs, so the expected value
   is unknown and any tolerance wide enough to pass hides the drop. Change: measure the gap
   on the injected `clock` (the same one the backoff sleeps on), write it after the new
   processing thread runs (or straight into `FrameRelay`), and assert exactly.
   `CaptureSessionTests.aDeviceChangeKeepsRecordingOnTheSameFiles`: `ManualClock`,
   `changeDeviceAfter: 1`, immediate restart, expect `gapSeconds == 0`,
   `droppedSamples == [:]`, master frames `== framesDelivered` of both starts.
   `CaptureSessionTests.aGapLongerThanTheRingIsWrittenInFull`: `restartsThatFail: 3`,
   advance the clock 0.25, 0.5, 1.0 s between attempts, expect `gapSeconds == 1.75`,
   `droppedSamples == [:]`, master frames `== framesDelivered + 1.75 * 48_000` per lane,
   `deviceChanges == 1`. Add `#expect(sink.droppedSamples == [:])` to
   `LaneFrameSinkTests` for `writeGap(frames:)` with a frame count above the ring capacity,
   which is what forces the ordering fix.

2. **Redesign step 7a: the seeded recording meeting is reconciled to `.failed` before the
   UI test can see it.** The preview environment runs `AppController.launch()`, which calls
   `reconcileInterruptedRecordings()`; `AppEnvironmentTests.testReconcileMarksInterruptedRecordingsFailed`
   pins that a `.recording` row with no live recorder becomes `.failed`. A UI test that
   selects "the recording meeting" and expects `stop-recording-header` therefore fails, and
   a seed that bypasses reconcile would prove a state the product never has (a `.recording`
   row with the recorder `.idle`, where `recorder.stop()` from the header is a no-op).
   Change: no seeded recording row. Honour `-steno-start-recording` in `AppBootstrap.load`
   for the UI-testing environment: after `launch()`, `await controller.recorder.start(mode:
   .call)` and set `requestedMeetingID = recorder.activeMeetingID`. The row is then real,
   `LocalRecordingIntake.begin` wrote it, the synthetic backend feeds it, and the header
   Stop stops it. `LaunchSmokeTests.testHeaderStopEndsTheRecording`: launch with the flag,
   wait for `stop-recording-header`, assert the "Recording" empty-state title in the tab
   body, click Stop, wait for the button to disappear and a "Queued" or "Ready" chip. The
   failed and processing entries can stay seeded (they are terminal or resumed by
   `resumeUnfinished`; for processing, assert the state chip exists rather than the stage
   label, which changes as the fake pipeline runs).

### Major

3. **Redesign step 4 breaks eight existing assertions and names none.** Seeding "four more
   synthetic meetings over three days" in `AppEnvironment.preview()` changes the set every
   app unit test sees through `TestSupport.environment(seed: true)`.
   `MeetingListViewModelTests` asserts `meetings.count == 2` and `== 1` after adding one
   meeting to the seed (lines 18, 31, 55, 64, 87, 91, 104), and
   `MenuBarViewModelTests.testQueueAndRecentPartitionEveryMeetingState` waits for
   `recent.count == 2`. Change: keep `preview()`'s seed as it is and add the richer set
   behind the launch argument only (`PreviewSeed.seedForScreenshots` called from
   `AppBootstrap` when `-steno-rich-seed` is present, or a `seed: PreviewSeed.Set`
   parameter with `.sample` as the default). Add
   `AppEnvironmentTests.testRichSeedSpansThreeDaysAndEveryState` (counts per `StateFilter`,
   three `dayGroups`, the fixture meeting newest) so the UI test's precondition is pinned
   hostless.

4. **Two UI tests assert a row label that begins with "Meeting ", which no meeting ever has.**
   Start-recording step 8 (`testSidebarStartsAndStopsARecording`) and floating step 8
   (`testBubbleFollowsARecordingStartedFromTheWindow`). `LocalRecordingIntake.defaultTitle`
   renders `"Call yyyy-MM-dd HH:mm"` (`RecordingControllerTests.testDefaultTitleComesFromCore`),
   and after the redesign the display title is `"Call, Monday 10:06"`. Both tests fail by
   construction on their first run. Change: assert on the row's stable identifier instead:
   read `recorder.activeMeetingID` indirectly by waiting for
   `app.buttons.matching(NSPredicate(format: "identifier BEGINSWITH 'meeting-'")).count == 2`
   (fixture plus the new row), or on the `StatusChip` text "Recording" while recording and
   "Queued" or "Ready" after. Do not assert on the title text anywhere in the smoke suite.

5. **Device-change: `SyntheticCaptureBackend.changeDeviceAfter` as specified re-fires on
   every `start`, so the keep-recording test loops.** Today `loseDeviceAfter` is evaluated
   per `start` (`CaptureSessionTests` line 136 comments that a restarted backend "loses its
   device again"). Step 3 keeps that shape and adds `seconds` per start. In
   `aDeviceChangeKeepsRecordingOnTheSameFiles` the rebuilt backend then reports `.synthetic`
   again one second in, the session rebuilds again, and so on until the test's `stop()`;
   `deviceChanges` is whatever the timing produced and the assertion `deviceChanges == 1`
   is a race. Change: `changeDeviceAfter` fires once per backend instance (a
   `changesRemaining: Int = 1` counter), documented in the type's comment. Add
   `CaptureSessionTests.twoDeviceChangesRebuildTwice` with `changesRemaining: 2` asserting
   notices `[.deviceChanged, .deviceResumed, .deviceChanged, .deviceResumed]` and
   `deviceChanges == 2`, which is the shape the manual Bluetooth check produces.

6. **Device-change: three rows of the session table and four of the recorder table have no
   test.** Session table: "`DeviceChange` reported while not `.recording`: ignored" and
   "writer fails during a rebuild: `.failed(.writerFailed)`" name no test. Recorder table:
   "user stops from any surface while armed: `.manual`, `autoStop = nil`", "device change
   notice: countdown unaffected", "any `stop()` clears `sawForeignMicrophone` and
   `callAppName`", and the prompt-started path (`start(mode:callApp:)` sets
   `sawForeignMicrophone` so a release with no later `.microphoneOpened` still arms). Add to
   `CaptureSessionTests`: `aDeviceChangeWhileIdleOrStoppingIsIgnored` (report before
   `start` and during `stop`, no notice, state unchanged) and
   `aWriterFailureDuringARebuildEndsWriterFailed` (the existing failing `makeWriter` seam,
   `restartsThatFail: 1` so the rebuild is in flight). Add to `RecordingControllerTests`:
   `testManualStopWhileArmedStoresManualAndClearsTheCountdown`,
   `testADeviceChangeNoticeLeavesTheCountdownRunning` (`changeDeviceAfter` plus
   `microphoneActivity(.released)`, `remaining` unchanged after the resumed warning),
   `testStopResetsTheForeignMicrophoneMemory` (second recording, `.released` alone does
   not arm), `testAPromptStartedRecordingArmsOnReleaseWithoutAnOpenedEvent`
   (`start(mode: .call, callApp: "Zen")`, then `.released`, expect `autoStop?.appName ==
   "Zen"`).

7. **Retention and onboarding both rewrite `OnboardingViewModel.init` and both promise the
   81 x 4 matrix "still holds"; neither names the other.** Retention step 6 adds a
   `SettingsStore` for `retentionSentence`; onboarding step 8 adds the LLM and Obsidian view
   models from the environment and a `defaults` parameter. Retention lands first (index
   step 3), onboarding's app part fourth. As written the matrix test
   (`testCurrentIsDerivedForEveryPermissionCombination`, `OnboardingViewModel(permissions:)`)
   is rewritten twice and the second PR has to undo the first's constructor. Change: the
   retention plan introduces `OnboardingViewModel(environment:)` (permissions, settings) and
   keeps `init(permissions:)` as a convenience for the matrix; the onboarding plan extends
   the environment initialiser only. State this in both plans' step lists.

8. **Onboarding step 5: "the request is set and cleared" cannot be a unit test as designed.**
   `requestedSettingsTab` is cleared in `SettingsView.onChange`, a view; the hostless bundle
   never runs it. The same holds for the start-recording plan's "select the live row"
   (decision 8), whose logic sits in `RecordingControl`'s action closure. Change: move both
   into `AppController` methods (`openSettings(_ tab:)` sets the request and returns it;
   `startRecordingFromWindow(mode:)` starts and sets `requestedMeetingID` to
   `recorder.activeMeetingID`), test them in `AppControllerTests`
   (`testStartFromTheWindowSelectsTheLiveRowAndTheMenuBarDoesNot`), and say plainly that the
   clearing in the view is covered by the UI smoke test that opens Settings on the right
   tab (`app.tabs["LLM"].isSelected` or the tab's identifier).

9. **Redesign decision 13 and the `DisplayTitleTests` case "a default title in another time
   zone is still recognised" contradict each other.** The decision compares the stored title
   with `defaultTitle(source:startedAt:)` "for the current time zone"; a title recorded in
   Berlin and viewed in UTC then does not match and renders as the ISO string, which is the
   case the test says must be recognised. Change: recognise by pattern
   (`^(Call|In person|Phone) \d{4}-\d{2}-\d{2} \d{2}:\d{2}$`, the three `MeetingSource`
   labels `defaultTitle` uses) and render from `startedAt` in the viewer's zone. Keep the
   five named cases and add "a user title that happens to match the pattern is still
   replaced" so the rule is stated honestly.

10. **Redesign step 12: one screenshot per window in the runner's appearance is not enough
    to review "both appearances ship" without Forge.** The hosted runner is light, default
    window size, no hover, no focus. Dark mode, the 960 x 600 minimum ("no text truncates"),
    the empty states and the recording header exist only on the owner's Mac. Change: the
    UI-testing environment honours `-steno-appearance light|dark` (sets `NSApp.appearance`)
    and `-steno-window 960x600`; `LaunchSmokeTests` runs the screenshot test twice through a
    parameterised helper and attaches light and dark for the main window (empty, fixture
    selected, recording via finding 2), onboarding page 1 and 2, and Settings > Audio. Name
    the attachments (`main-dark-selected.png`) so `xcrun xcresulttool export attachments`
    yields a reviewable set. The numeric checks (five tiers, hue only on status) stay a
    reviewer reading of the screenshots; say so.

### Minor

11. **Onboarding step 2 names no test for the CLI change.** `CLITests.migrateGenerateProcessAndExport`
    line 113 asserts `decoded.meeting.title == "Summary of Sweep"`, the fake summariser's
    title; it fails once the CLI stops wiring the fake. Change: assert `title == "Sweep"`,
    `summary == nil`, `llmUsage == nil`, and that stderr contains "summary skipped: no LLM
    endpoint configured". `PipelineHarness` (`Tests/StenoCoreTests/Support`) types
    `cleaner` and `summarizer` as non-optional; step 1 must make them optional for the
    nil-passes test to be constructible.

12. **Onboarding: the invariant test and the banner test lack identifiers and one case.**
    Decision 2's invariant is only the contrapositive today
    (`summarizeFailureMarksFailedAndKeepsTheTranscript`); add
    `PipelineIntegrationTests.aConfiguredSummarizerNeverLeavesReadyWithoutASummary` with a
    `FakeSummarizer` that returns an empty document, expecting a non-nil `summary`. Give the
    banner buttons ids (`setup-summaries`, `choose-vault`, `banner-not-now`) so the UI test
    does not match on long copy, and add `SetupStateTests.testDeriveCoversTheFourCombinations`
    on the pure `derive`. Route the new Summary and Tasks copy through `TabText` (a
    `setup` argument) so `TabTextSnapshotTests` still describes what the tabs show; the plan
    leaves `TabText` alone and the two proofs drift.

13. **Retention step 2: "a later `redeliver` with the failure cleared" needs a seam that does
    not exist.** `FakeDestination` is a value type held by `FakeDeliveryDispatcher`, built
    once in `PipelineHarness`; nothing clears `deliverFailure` afterwards. Add
    `FakeDestination(failUntil: Int)` (a class-backed counter) or a `FailureSwitch`
    reference, and name the test `aFailedDeliveryDefersExpiryUntilRedeliverSucceeds`. Add
    `rerunSummaryAlsoStampsADeferredExpiry` (rule 2 names both paths, the tests name one).
    Check `MeetingStoreTests.assetsRoundTripAndExpire` after `expiredAssets` joins
    `meeting`: an asset saved without a meeting row disappears from the join.

14. **Retention step 4: `.totalFileAllocatedSizeKey` makes the "known sizes" assertion
    machine-dependent.** APFS rounds to blocks and Linux (the container the plan's
    verification names) may not report it. Use `.fileSizeKey` for the test's assertion, or
    assert `>= logical` and `< logical + 4096 * files`. Say `AudioFolderUsageTests` runs on
    both.

15. **Device-change step 1: `SchemaSnapshotTests` stops guarding `v2.sql` once `v3` is
    latest.** The suite pins `v1` and `identifiers.last` only. Change it to iterate every
    prefix: for `n` in `identifiers.indices`, migrate `identifiers[...n]` and compare with
    `v<n+1>.sql`. Also name `callEnded(appName: nil)` in `ModelCodableTests`: `CaseCoding`
    encodes payload cases as a one-key object and has no nil payload today, so this is a
    new shape, and `payloadEnumsEncodeReadably` should show it.

16. **Device-change step 7: say which clock the four-failure app test runs on.**
    `TestSupport.deviceLosingCaptureSession` builds `CaptureSession` directly; with the new
    `clock` defaulting to `ContinuousClock` the test sleeps 1.75 s twice (about 4 s), with
    `environment.clock` (a `ManualClock` nobody advances) it hangs until `waitUntil` fails.
    Pass `ContinuousClock()` explicitly, or advance the `ManualClock` and assert the
    `.deviceLost` warning appears only after the fourth attempt.

17. **Start-recording step 8: `app.buttons["sidebar-record"]` on a `Menu(primaryAction:)`.**
    SwiftUI renders it as a split `NSPopUpButton`; XCUITest often exposes it as
    `popUpButton` or `menuButton`, not `button`. Query `app.descendants(matching: .any)`
    by identifier and assert `elementType` once in the test so the failure is readable.
    The "Recording" chip assertion also breaks when the redesign removes the chip from a
    recording entry; the redesign plan should name that line.

18. **Floating step 7 and 8: the presenter is untested and one UI assertion is flaky.**
    Split `FloatingPanelPresenter` into a pure `FloatingPanelModel` (content, anchor,
    screen validation, the observation-to-apply sequence) over a `PanelHost` protocol with
    a fake in `FloatingPanelTests`, so anchor persistence and re-validation are unit
    tests, not manual step 4 and 6. In `testBubbleFollowsARecordingStartedFromTheWindow`
    replace "the main window is frontmost" with the entry's `isSelected` trait; use an
    `XCTNSPredicateExpectation(exists == false)` for "prompt-record no longer exists". Add
    a pure `MenuBarLabelPresentation.make(state:)` so the elapsed label has a test, and in
    `AppControllerTests.testShutdownStopsTheRecordingAndTheDetector` assert
    `FloatingContent.resolve(prompt: nil, recording: .stopping) == .bubble`.

19. **Floating and device-change: `BubblePresentation.make(state:autoStop:)` takes a type
    the floating plan does not own.** Define `struct AutoStopPresentation: Equatable {
    appName: String?, remainingText: String, fractionRemaining: Double }` in the floating
    plan (nil until the device-change plan lands) so the second plan fills it without
    changing the signature or the first plan's tests.

20. **Redesign steps 7 and 8: two named tests change more than the plan says.**
    `TabTextSnapshotTests.testEmptyTabsSayWhetherContentIsStillComing` asserts the literal
    pending strings, not only the golden; list it. `-steno-show-onboarding` "with all
    permissions unknown" needs a preview seam (`preview()` hard-codes
    `FakePermissions.allGranted()`) and must bypass the opener's preview guard the
    onboarding plan keeps; add `permissions:` to `preview()` and open the window from
    `AppBootstrap` under the flag. The arrow-key test should click an entry first and
    press once; keep next and previous in `MeetingListViewModelTests`.

21. **App icon step 4: two thresholds are guesses and the SVG-to-PNG drift is unguarded.**
    `AppIcon.icns > 100 KB` depends on PNG compression of a flat design; assert the icns
    header (`icns` magic) and that `NSImage(contentsOf:)` yields a 1024 representation
    instead. Assert `CFBundleIconFile` or `CFBundleIconName`, whichever Xcode 16.4 writes.
    Record `sha256(AppIcon.svg)` in `AppIcon.appiconset/SOURCE.sha256` and assert it in
    `testIconSourceIsCommittedBesideTheSet`, so an edited SVG without regenerated PNGs
    fails (the `RendererVersionTests` pattern). Add `testMobileIconIsOpaque1024`
    (`NSBitmapImageRep.hasAlpha == false`, 1024 x 1024) since the script's check never runs
    on CI.

22. **Every plan's verification names `swift test` "on the Linux container"; CI has no
    Linux job.** The container is a local step on atlas. Either the PR body states the run
    with its test count, or the plans drop the claim. Also: five plans add `-steno-*`
    launch arguments in three files; parse them once (`UITestScenario(arguments:)` with a
    unit test) so a typo is not a silent no-op in the smoke suite.

## Coverage matrix

Verdict: covered = a named test fails when the behaviour is wrong; partial = named but
weaker than the behaviour; gap = no test; manual = only a human can check, and the plan
says so; breaks = an existing test the plan must change.

### Start recording from the main window

| Behaviour | Planned test | Verdict |
|---|---|---|
| Presentation table, five states x four denied sets | `RecordingControlPresentationTests` | covered |
| Optional kinds never disable | same file | covered |
| `activeMeetingID` nil, id, nil | `testActiveMeetingIDFollowsTheRecording` | covered |
| `refreshPermissions` reports denied required kinds only, `.unknown` allowed | `testRefreshPermissionsReportsDeniedRequiredKindsOnly` | covered |
| `start` unguarded with a denied kind | same, plus existing failure-path test | covered |
| Live row selected from the window, not from menu bar or prompt | none (view closure) | gap, finding 8 |
| Record menu label and enabled state per state | presentation test only; menu wiring untested | partial, manual step 3 |
| Elapsed time, level bars, messages under the control | none (views) | manual |
| Empty-state copy | none | gap (low value) |
| `deniedMessage` strings are words | `testLabelsAreWordsNotRawValues` | covered |
| Sidebar start and stop end to end | `testSidebarStartsAndStopsARecording` | partial, findings 4 and 17 |
| State machine unchanged | existing `RecordingControllerTests`, `AppControllerTests` | covered |
| Permission toggling in System Settings re-enables without relaunch | manual 5 | manual |

### Onboarding: vault and LLM

| Behaviour | Planned test | Verdict |
|---|---|---|
| Nil passes: `.ready`, no summary, no usage, raw text kept, events posted, delivered | `PipelineIntegrationTests` nil-passes case | covered |
| Skipped summarize clears rows a fake run wrote | `StageTests` | covered |
| `rerunSummary` with nil summarizer throws `.summarize` | named | covered |
| Invariant: configured summarizer never leaves `.ready` without a summary | contrapositive only | partial, finding 12 |
| CLI without endpoint: stdout unchanged, stderr notice, no fake title | "update any expectation" | breaks `CLITests` line 113, finding 11 |
| App `live()` passes nil, `preview()` keeps fakes | `AppEnvironmentTests.testPreviewRootBuildsAndIsSeeded` | covered (preview), live untested |
| `SetupState` flips on Save, banner dismiss per launch | `AppControllerTests` additions | covered |
| `SetupState.derive` four combinations | implied | partial, finding 12 |
| Deep link set and cleared | `AppControllerTests` | partial, finding 8 |
| `summaryStatus`, `exportStatus`, `canRerunSummary`, `canReexport` | `MeetingDetailViewModelTests` | covered |
| Banner copy per missing set, buttons, Not now | UI smoke (preview: both missing) | partial (one of three variants; ids missing) |
| Two pages, step order, Done and Later advance | `OnboardingViewModelTests` | covered |
| `isFinished` waits for setup rows | named | covered |
| Save summaries rebuilds pipeline; save vault validates; unwritable path message | named | covered |
| `shouldOpen` with flag unset or set | named, `UserDefaults` injected | covered |
| Existing install opens on page 2 | `shouldOpen` case | covered |
| Settings copy | none | manual |
| Test connection against a live server | manual | manual |

### Audio retention: keep forever

| Behaviour | Planned test | Verdict |
|---|---|---|
| Default `.keepForever` for new installs | `SettingsStoreTests`, `AppEnvironmentTests:96`, `RecordingControllerTests:47,165`, `SettingsViewModelTests:42` | breaks, all named |
| Picker order, titles, footnote per mode | `SettingsViewModelTests` | covered |
| Failed delivery leaves `expiresAt` nil, no `.retentionApplied` | `PipelineIntegrationTests` | covered |
| Later `redeliver` stamps once; second `redeliver` does not move it | named | partial, finding 13 (seam) |
| `rerunSummary` stamps a deferred expiry | none | gap, finding 13 |
| `.keepForever` unaffected either way | named | covered |
| Processing failure leaves `expiresAt` nil | named | covered |
| Sweep skips recording, queued, processing; sweeps once ready | `RetentionSweepTests` | covered; check `MeetingStoreTests.assetsRoundTripAndExpire` |
| `audio.m4a` lands after a deferred delivery | `StenoEndToEndTests` case | covered |
| Switching to Forever clears stamps with files, not without | `SettingsViewModelTests` | covered |
| Switching to days changes nothing | named | covered |
| `AudioFolderUsage` sizes, hidden skipped, missing throws | `AudioFolderUsageTests` | partial, finding 14 |
| Detail "Recording" line, six statuses; toggle hidden when default forever | `MeetingDetailViewModelTests` | covered |
| Confirmation when default is delete-after-processing | none (view) | manual |
| Onboarding retention sentence, three variants | `OnboardingViewModelTests` | covered; finding 7 |
| Launch sweep after a deferred stamp | existing `AppControllerTests` | covered |

### Floating recording indicator

| Behaviour | Planned test | Verdict |
|---|---|---|
| `resolve`: recorder state x prompt | `FloatingPanelTests` | covered |
| `BubblePresentation` four states | named | covered |
| `PanelAnchor` default, `frame(for:)`, off-screen fallback, second screen kept | named | covered |
| `LiveBarsHistory` mapping | named | covered |
| `fractionRemaining` on `ManualClock` | `DetectionTests` | covered |
| Prompt to bubble on `prompt.start()` | `AppControllerTests` resolve assertions | covered |
| Presenter: observe, create panel, apply, order out, save anchor on move, re-validate on screen change | UI smoke plus manual 4 and 6 | partial, finding 18 |
| Prompt appears, morphs, stop hides | `testPromptMorphsIntoTheBubbleAndStopHidesIt` | covered (fix non-existence wait) |
| Body click opens the live meeting | second UI test | partial, findings 4 and 18 |
| Menu bar label with elapsed time | none | gap, finding 18 |
| `.stopping` bubble during Quit | none | gap, finding 18 |
| Full-screen overlay, Reduce Motion, VoiceOver, drag persistence across relaunch | manual 4 to 9 | manual |
| Menu bar causes A to G | manual 1 | manual |
| `radiusXL`, `Motion.countdown`, `Motion.pulse` | none; `testMotionTokensMirrorMobile` pins values not the set, so it does not break | gap (low value) |

### Device changes during a recording

| Behaviour | Planned test | Verdict |
|---|---|---|
| Identical devices: ignored | `DeviceSnapshot.difference` unit | partial; coalescing and HAL manual |
| Change: stays `.recording`, notices, same files, contiguous | `aDeviceChangeKeepsRecordingOnTheSameFiles` | finding 1, finding 5 |
| Restart throws, attempt < 4: backoff on clock | `aRestartThatKeepsFailingEndsInDeviceLost` (three then success) | covered once clock-driven |
| Attempt 4: `.failed(.deviceLost)`, `endedOnDeviceLoss` | same | covered |
| Change while not `.recording`: ignored | none | gap, finding 6 |
| `stop()` during a rebuild | `stopDuringARebuildFinalisesOnce` | covered |
| Writer fails during a rebuild | none | gap, finding 6 |
| `session.stream` reflects the restart | named | covered |
| Sink rearm, `writeGap` per lane | `LaneFrameSinkTests` | partial, finding 1 |
| Real-time path unchanged | `RealTimeAllocationTests` | covered |
| Shared `LevelSlot` | `ProcessingThreadTests` | covered |
| `endReason` model, `v3`, codable, intake writes it | `MigrationsTests`, `SchemaSnapshotTests`, `ModelCodableTests`, `LocalRecordingIntakeTests` | covered; finding 15; `RecordingResult` call sites (5 in intake tests, 1 in the app) |
| Recorder passes `.deviceLost`, `.quit`, `.manual` | `RecordingControllerTests`, `AppControllerTests` | covered; finding 16 |
| Device change warning lines | `testADeviceChangeKeepsTheRecordingAndWarns` | covered |
| Arm on release after a call, stop after 90 s | named | covered |
| Reopen cancels; keep recording; in person never; no foreign mic never; stop now | named | covered |
| Manual stop while armed; notice leaves countdown; stop clears memory; prompt-started arms | none | gap, finding 6 |
| Events reach the recorder while recording | `DetectionTests.testEventsWhileRecordingReachTheRecorder` | covered |
| Bubble and sidebar rows | one case each in sibling presentation tests | covered |
| End-reason sentences | `MeetingDetailViewModelTests` via `RecordingEndReason.sentence` | covered |
| CLI prints notices | "only if a test pins" | gap (acceptable) |
| Bluetooth, USB, sample-rate, four attempts on hardware, transcript alignment | manual 1 to 8 | manual |

### App icon

| Behaviour | Planned test | Verdict |
|---|---|---|
| Ten entries, every file present, pixel size = size x scale | `testAppIconSetListsEveryFileAndEveryFileExists` | covered |
| Built app carries the icon | `testBuiltAppCarriesTheIcon` | partial, finding 21 |
| SVG committed with `glyph` and `live` ids | `testIconSourceIsCommittedBesideTheSet` | covered |
| Script fails friendly without `magick` | `ReleaseScriptsTests` case | covered |
| Script parses under bash 3.2 | `testEveryScriptParsesUnderBash`, repository CI `bash -n` | covered |
| PNGs match the SVG | `--check`, never on CI | gap, finding 21 |
| iOS icon opaque 1024 | script only | gap, finding 21 |
| Mobile config carries the icon; fingerprint moves | `pnpm check`, CD comment | partial (no test reads `app.config.ts`) |
| Dock, Finder, Launchpad, light and dark Dock, 16 px read | manual | manual |

### macOS visual redesign

| Behaviour | Planned test | Verdict |
|---|---|---|
| `macTokens` resolve, no CSS collision; spacing on 4; radii descend | `ThemeTokensTests` additions | covered |
| CSS mirror untouched | existing test | covered |
| `displayTitle` five cases | `DisplayTitleTests` | finding 9 |
| `previewLine`, `dayGroups`, counts, next and previous | `MeetingListViewModelTests` | covered |
| Rich preview seed | `AppEnvironmentTests` builds | finding 3 |
| Nav rows filter, search field exists, trash gone, CTA above rows | UI smoke step 5 | covered |
| No `List`, `isSelected` trait, weekday header, arrow key | UI smoke step 6 | partial, finding 20 |
| Pending copy per state | `TabTextSnapshotTests` regenerated | breaks, partly named, finding 20 |
| Raw `destinationID` and raw default title never in a `Text` | none (done criterion) | gap (reviewer grep) |
| Header Stop while recording, level bars, progress row | UI test on a seeded recording | finding 2 |
| Onboarding subtitle no longer truncates, four titles | UI test with `-steno-show-onboarding` | partial, finding 20 |
| Menu bar, speaker sheet, Settings restyle | "tests unchanged" | manual (views only) |
| Light and dark both pass, minimum size, hover, focus | screenshots (light only) plus local | finding 10 |
| Jamie side by side, numeric tier count | manual | manual |

## Manual-only

What no automated test in these plans can prove, and what the PR checklist must state
was run, on which Mac, with which devices.

- Menu bar item: causes A to G (crowding, ⌘-drag removal, Bartender, two processes, stale
  label); the elapsed label rendering as a template image; the popover opening at all.
- Floating panel: drag and anchor persistence across relaunch (finding 18 makes the model
  part testable, the `NSWindow` move notification stays manual); second display unplugged
  mid-call; overlay above a full-screen Zoom; Reduce Motion; VoiceOver reading order;
  "Finishing…" shown until the process exits on Quit.
- HAL: the coalescing window, `DeviceSnapshot` against real device UIDs, the aggregate's
  nominal sample rate listener, Bluetooth HFP profile switches, USB unplug and replug, the
  four-attempt path with a device that stays gone, and transcript timestamps agreeing with
  the bubble clock after a switch (only real devices produce a real gap).
- TCC: the microphone and tap prompts on the first recording; switching Steno off and on in
  System Settings > Privacy and the control re-enabling without a relaunch; `.unknown`
  system audio ending with the silent-lane warning.
- Retention in the app: the "Recording kept until the export succeeds" line against a
  read-only vault and its change to "deleted on today" after Re-export (the core and
  end-to-end tests prove the stamp; the line's timing on a live store is manual); the
  owner's stored `{"keepDays":30}` row and the one-time flip.
- Onboarding: Test connection against LM Studio; the NSOpenPanel vault chooser; the
  existing-install path landing on page 2 with the window's real size.
- Icon: Dock, Finder Get Info, Launchpad, ⌘-Tab at default size, light and dark Dock, the
  16 px read; the iPhone home screen after the first TestFlight build.
- Redesign: dark appearance (until finding 10), hover veils, focus rings, the 960 x 600
  minimum with no truncation, `NavigationSplitView` toolbar background on three columns
  (open question 6), the Jamie side by side, and the numeric tier and hue count.
- Record menu label during `.starting` and `.stopping`; ⌘F focusing the search field.
