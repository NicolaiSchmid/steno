# Test-strategy review of the processing progress plan

Reviewed 2026-09-28 against `.plans/2026-09-28-processing-progress.md` on
`docs/plan-processing-progress`. Question: can an implementation agent prove
each of the three steps with a test that runs on the `swift-ci.yml` jobs,
`package` and `app` on the macOS runner and `ui-smoke` on the hosted image
only, and can a reviewer trust that proof? Correctness of the estimator and
the fit of the card are for the sibling reviews. The test infrastructure the
proposals reuse: `Tests/StenoCoreTests/Support/PipelineHarness.swift` with
every fake and a fixed `now`, `Sources/StenoCore/Testing/` with
`FakeSpeechEngine`, `FakeDiarizer`, `CallLog`, `ManualClock` and `Snapshot`,
`apps/macos/StenoTests/TestSupport.swift` with `AppEnvironment.preview` over
a `ManualClock`, `GatedSpeechEngine` and `waitUntil`, and
`Tests/stenoTests/CLITests.swift`, which runs the built binary.

Severity: blocker = no machine-checkable proof, or it cannot run in CI;
major = a deterministic test is possible but the plan offers only a manual,
flaky or opt-in check; minor = fixture, flakiness or reviewer hygiene.

## Findings

1. **blocker: the 30 percent acceptance criterion of step 1 cannot pass or
   fail meaningfully in CI.** Step 1, "Acceptance: on the second run of the
   same synthetic asset the remaining-time estimate at `transcribe` start is
   within 30 % of the measured remainder." Every CI run of the pipeline is on
   `FakeSpeechEngine`, `FakeDiarizer`, `PassthroughCleaner` and
   `FakeSummarizer` over a six second fixture, so each stage takes well under
   a millisecond and the measured remainder is GRDB write latency, file copies
   and scheduler jitter under `swift test --parallel`. A ratio of two such
   numbers is noise; the assertion is red or green by luck. The real-engine
   variant lives behind `STENO_MODEL_TESTS` in
   `Tests/StenoEndToEndTests/RealModelsEndToEndTests.swift`, never runs on
   the hosted runner, and on the Forge runner the second run of a nine second
   fixture still varies by more than 30 percent between ANE warm states, so it
   is not a proof either. Replace the criterion with two deterministic
   checks. First, `ProcessingEstimator` is pure: add
   `Tests/StenoCoreTests/ProcessingEstimatorTests.swift` with a 3600 second
   two-lane meeting and the seed `StageRates`; assert that the expected
   seconds per stage equal the table in the plan, that the fraction at each
   stage start is the cumulative share, that `remaining(at:)` at `transcribe`
   equals the sum of the later shares exactly, and that after one recorded
   sample of every stage the second estimate equals the recorded durations
   exactly, since alpha applies to the difference from a seed with
   `samples == 0` in a way the plan must state. Second, the pipeline's timing
   is driven by an injected clock, finding 2, so
   `PipelineIntegrationTests.secondRunEstimatesFromTheFirstRunsMeasuredRates`
   scripts the first run with `ManualClock.advance(by:)` inside the fakes,
   ten seconds per transcribed lane and forty for diarize, then asserts that
   the `estimatedRemaining` posted at the second run's `transcribe` start is
   the exact sum of the scripted stage durations, no tolerance. The real
   measurement on the development Mac and on Forge stays what the plan says
   it is, the instrument for the seed constants, and step 1 marks it
   `[manual]` the way the 2026-09-25 review asked for hardware-only checks.

2. **major: stage durations measured through `PipelineDependencies.now`
   are zero in every existing harness.** D2, "After each stage the pipeline
   records the measured duration"; step 1 tests. The one time source the
   pipeline has is `now: @Sendable () -> Date`, and `PipelineHarness` passes
   `{ Self.now }`, `TestSupport.environment` passes `{ now }` with a fixed
   epoch, and `RecordingIntake` tests pass the same constant. That closure
   exists so row timestamps are stable; a rate recorder built on it measures
   zero for every stage in every test, the EMA converges to zero, "remaining
   decreases" is vacuously true from zero to zero, and the estimator tests
   pass while the recorder is broken. Add
   `clock: any Clock<Duration> = ContinuousClock()` to `PipelineDependencies`
   beside `now`, the pattern `MeetingDetector` and `AppEnvironment.clock`
   already use, and measure with `clock.measure { try await body() }` inside
   `ProcessingPipeline.run` and `attributing`, after `prepare()` returns as
   D2 says. `PipelineHarness` gains a `clock: ManualClock` argument;
   `FakeSpeechEngine` gains `onTranscribe: (@Sendable () async -> Void)?`
   like `FakeDiarizer.onDiarize`, and `PassthroughCleaner` and
   `FakeSummarizer` gain the same hook, so a test advances the clock by a
   known duration per stage. Then
   `PipelineIntegrationTests.stageDurationsAreRecordedFromTheClock` asserts
   the stored `StageRates` after one run equal the scripted advances per
   unit, keyed by `FakeSpeechEngine.id`, and that a stage whose fake threw
   recorded nothing.

3. **major: the step 2 UI test asserts what XCUITest cannot observe and
   needs a launch mode the app does not have.** Step 2, "the bar's
   accessibility value is between the stage's start and end fraction and
   rises between two samples"; D3, "the animation is the clock". A SwiftUI
   `ProgressView` exposes its model value to accessibility, not the
   interpolated presentation, so two samples inside one stage read the same
   number until an event lands, and the assertion is either false or passes
   only because an event happened to arrive between the samples. It is also
   the one job that runs hosted only, so a flake there blocks every PR.
   Second gap: `StenoApp.load` calls `AppEnvironment.preview()` with defaults
   under `-steno-ui-testing`, and `GatedSpeechEngine` lives in `StenoTests`,
   so nothing can hold the fake pipeline inside `transcribe` from an XCUITest.
   Put the motion behind a pure seam and test it hostless. Add
   `apps/macos/Steno/Main/ProcessingPresentation.swift` with
   `static func state(progress: ProcessingProgress, elapsedSinceEvent:
   Duration, reduceMotion: Bool) -> State` returning the displayed fraction,
   the remaining-time text and a `slow` flag, and
   `apps/macos/StenoTests/ProcessingPresentationTests.swift` asserting: at
   zero elapsed the fraction is the event's `fraction`; at the expected time
   to the next event it is `nextFraction - 0.01`; at twice the expected time
   it is still `nextFraction - 0.01` and the text is "a bit longer than
   usual"; the fraction is non-decreasing over a sweep of elapsed values; the
   wording thresholds in D4 map from a table of durations to strings, seed
   wording included. `ProcessingProgressModelTests` in the same folder drive
   the model the way `MenuBarViewModelTests.makeModel` does, post a
   `.progress` with a lower fraction for the same meeting and assert the
   displayed target did not fall. The UI test then asserts only what is
   stable: with launch argument `-steno-ui-testing-hold-transcribe`, which
   `AppEnvironment.preview` honours by building `FakeSpeechEngine` with a new
   `holdTranscribe: Duration?` that sleeps sixty seconds on
   `ContinuousClock`, the seeded meeting is enqueued at launch, the card with
   id `processing-card` appears, `processing-stage` reads "Transcribing…",
   `processing-bar`'s value is within the transcribe share once, and after
   the hold the card is gone and "Executive Summary" is present. Give it its
   own method in `apps/macos/StenoUITests/LaunchSmokeTests.swift` so a failure
   is attributable.

4. **major: the warm-up test of step 3 cannot reach the engine the pipeline
   holds, and the installed-assets guard has no fake path.** D5 first bullet;
   step 3 tests, "`FakeSpeechEngine` counts `prepare()` (once per recording
   start, zero when assets are absent)". Three gaps. `ProcessingPipeline.
   dependencies` is internal to StenoCore, so `AppEnvironment` has no handle
   on the engine and diarizer of the current pipeline; the plan must either
   keep the `PipelineDependencies` value on `AppEnvironment` next to
   `pipeline` and rebuild it in `reloadPipeline`, or add a public
   `ProcessingPipeline.warmUp() async` that calls both `prepare()` methods.
   `ModelStore.isInstalled(_:)` takes a `ModelAsset`, and `FakeSpeechEngine.
   id` is `"fake-engine"`, not a `SpeechEngineID`, so with fakes the guard
   either always skips, and the "once per recording start" assertion fails,
   or always runs, and "zero when assets are absent" cannot be written. Key
   the guard on `SpeechEngineID(settingsValue: settings.speechEngineID)?.
   asset` plus `.offlineDiarizer`; the preview `ModelStore` has a
   `FakeModelDownloader`, so a test flips the guard with
   `for try await _ in await environment.models.ensure(.parakeetV3) {}` and
   the same for `.offlineDiarizer`, which writes the marker files
   `isInstalled` accepts. `FakeDiarizer.prepare()` records nothing today; add
   `preparations = CallLog<Bool>()` to it, and let `AppEnvironment.preview`
   take `makeDiarizer` beside `makeSpeechEngine` so a test holds the instance.
   Then in `apps/macos/StenoTests/RecordingControllerTests.swift`:
   `testStartWarmsBothEnginesWhenTheirModelsAreInstalled` ensures both assets,
   captures `let engine = FakeSpeechEngine()` through `makeSpeechEngine: {
   engine }`, calls `recorder.start(mode: .call)`, waits until
   `engine.preparations.count == 1` and `diarizer.preparations.count == 1`
   before `stop()`, then after `waitUntilIdle` asserts the count is two, the
   pipeline's own call, with the first entry recorded before the first
   `transcriptions` entry; `testStartDoesNotWarmWhenModelsAreAbsent` runs the
   same without `ensure` and asserts zero preparations until `stop()`. The
   no-op property of a second `prepare()` on the real engines belongs in the
   `STENO_MODEL_TESTS` suite as a timing report, not an assertion.

5. **major: progress lines on the CLI's standard output break three
   existing tests, and a byte golden is impossible.** D1, "`steno process`
   prints stage, percent and remaining, one line per event"; step 1. `Tests/
   stenoTests/CLITests.swift` reads `process.stdout` trimmed as the meeting id at
   lines 90, 128 and 217, so any extra stdout line fails them. The remaining
   time in each line comes from wall-clock rates stored in a fresh database,
   so a `Snapshot` golden of the output would differ on every run. Print the
   progress lines to standard error and keep stdout as the meeting id, and
   add `CLITests.processReportsProgressOnStderr`: for the in-person run
   assert exactly ten stderr lines, for the call run eleven, each matching
   `^[a-zA-Z]+ +\d{1,3}% .+$`, percents non-decreasing, first line `decode
   0%`, and the second transcribe line of the call naming lane two of two.
   The `steno` binary is the measurement instrument for the seed constants,
   so a `--progress-json` flag that prints `ProcessingProgress` plus the
   measured duration as one JSON object per line makes the manual
   measurement copyable into `StageRates.seeds` without transcription
   errors; `CLITests` decodes one line as `ProcessingProgress`.

6. **major: monotonic fraction is asserted within one run only; the two
   ways it can go backwards have no test.** D1, "never decreases within a
   run"; D3, "never moves backwards"; step 1 integration test. First,
   `ProcessingPipeline.resumeUnfinished` reprocesses a `.processing` meeting
   from `decode`, so the subscriber keyed by meeting id sees the fraction fall
   from where the interrupted run stopped to zero. A model that "never moves
   backwards" then sticks at the old value for the whole second run and the
   card lies. The plan must say what resets the presenter, a run identity on
   `ProcessingProgress` or the rule that a `.decode` event resets, and test
   it: `PipelineIntegrationTests.resumeUnfinishedStartsAgainAtZero` reuses
   the existing resume test, subscribes before `resumeUnfinished()` and asserts
   the first event of each resumed meeting has `fraction == 0`;
   `ProcessingProgressModelTests.aRestartedRunResetsTheBar` posts a
   `.cleanup` event at 0.6 then a `.decode` event at 0 for the same id and
   asserts the displayed fraction is 0. Second, the estimator's total changes
   when the transcript exists and the token count replaces the estimate from
   duration; if the transcript is ten times longer than estimated the total
   grows and elapsed over total drops at `cleanup` start. Add
   `ProcessingEstimatorTests.fractionNeverDropsWhenTheTokenCountIsRevised`
   with tokens at ten times and one tenth of the estimate, asserting the
   `cleanup` fraction is at least the `merge` fraction in both cases and that
   `nextFraction` stays within `fraction...1`, with the `retention` event's
   `nextFraction == 1`. The integration test in step 1 should also assert
   `nextFraction >= fraction` on every event and that the transcribe lane two
   event's `fraction` equals lane one's `nextFraction`.

7. **major: the learned rates need a two-run test, a concurrency test and
   a stated fallback for unknown engine ids.** D2; step 1 tests, "a
   `SettingsStore` round trip with rates present". Three things the round
   trip does not prove. The EMA across runs: with the scripted clock from
   finding 2, run the fixture twice with transcribe advances of ten then
   twenty seconds per lane and assert the stored rate equals `0.3 * 20/6 +
   0.7 * 10/6` per audio second with `samples == 2`, given the plan says a
   first sample replaces the seed, in `PipelineIntegration
   Tests.ratesAreAveragedAcrossRuns`. The transaction: `SettingsStoreTests.
   updateNeverDropsAConcurrentChange` runs one hundred `update { $0.rates
   .record(...) }` calls from two task groups against `save` calls that flip
   `launchAtLogin`, and asserts every sample and the last flag survive; that
   is the property D2 claims for the pipeline's writer and it is one
   `writer.write` with the read inside. The fallback: seeds are keyed by
   engine id and LLM model, `FakeSpeechEngine.id` is `"fake-engine"`, and
   every fake-driven run therefore hits the "unseen key" branch, so
   `ProcessingEstimatorTests.anUnknownEngineUsesTheParakeetSeedWithZero
   Samples`, or whichever fallback the plan picks, pins the choice; without
   a stated fallback the first event of every CI run has an undefined
   estimate. Also assert in the round trip that a rate row written with an
   unknown stage or engine key is ignored on load, which is how
   `unknownAndMissingRowsAreIgnored` already treats unknown properties.

8. **minor: there is no decoder spy; one is twenty lines and the fallback
   branch it would test is unreachable.** D5 second bullet; step 3 tests, "a
   decoder spy asserts one decode per lane for a call". The only `AudioDecoder`
   implementations are `WAVAudioDecoder` and `AVFoundationAudioCodec`; add
   `RecordingAudioDecoder` to `Sources/StenoCore/Testing/FakeSpeech.swift`,
   wrapping any decoder with `decodes = CallLog<AudioLane>()`, and let
   `PipelineHarness` take `decoder:`. `StageTests.aCallDecodesEachLaneOnce`
   asserts `decodes.entries == [.mic, .system]` after a full call run and
   `[.mixed]` for in-person, against three and two today. Note that with the
   current `diarizedLane` and `orderedLanes` rules the diarized lane is
   always the last transcribed lane, so the "decodes only when it is nil or
   another lane" branch never runs through `process`; test it by calling
   `pipeline.diarize(asset:meeting:buffer:)` directly with a buffer tagged
   for the other lane, or drop the branch and let the type say the buffer is
   always reused. `StageTests.diarizesTheSystemLaneOfACallAndWritesOneClipPer
   Cluster`, `inPersonDiarizesTheMixedLaneAndCapsClipsAtTenSeconds` and
   `twoClustersWithOneLabelFailTheStage` call `diarize(asset:meeting:)` and
   change with the signature.

9. **minor: existing tests the plan changes without naming them.** Step 1
   changes `MeetingEvent.progress`'s payload, adds one transcribe event per
   lane and deletes `PipelineStage.fraction`; step 2 removes `PendingText`'s
   pending branch and `MenuBarViewModel.observeProgress()`. Each PR must
   update these, and a reviewer should expect exactly these diffs:
   `Tests/StenoCoreTests/PipelineIntegrationTests.swift`,
   `macCallRunsEveryStageAndLandsReady` with `collected.count == 12` becoming
   13, `stages == PipelineStage.allCases` becoming the list with two
   `.transcribe`, and the review index 8 becoming 9;
   `rerunSummaryAndRedeliver` and `cleanupFailureKeepsTheMergedTranscript
   AndStopsTheEvents` matching on the new payload;
   `Tests/StenoCoreTests/StageTests.swift`, `progressIsPostedOncePerStage
   AcrossLanes`, whose name and assertion contradict D3 and which becomes
   `transcribePostsOncePerLane`, plus the persist and retention event
   equalities at lines 454 to 523; `Tests/StenoCoreTests/EventBusTests.swift`,
   which constructs `.progress(meetingID:stage:)`;
   `Tests/StenoEndToEndTests/EndToEndTests.swift` lines 263 to 275,
   `collected.count == 13` becoming 14 and the indices 8 and 11 moving by
   one; `apps/macos/StenoTests/MenuBarViewModelTests.swift`,
   `makeModel` running `observeProgress()`, `testQueueOrdersByStartAnd
   FollowsProgress` comparing with `PipelineStage.summarize.fraction`, and
   `testQueueAndRecentPartitionEveryMeetingState` asserting `[0, 0]`;
   `apps/macos/StenoTests/TabTextSnapshotTests.swift`,
   `testEmptyTabsSayWhetherContentIsStillComing`, whose `.processing` branch
   asserts the pending strings D4 deletes, so `TabText.lines` must render the
   card's title and remaining text for a processing export or the test drops
   that branch with a sentence in the PR; and `Tests/stenoTests/CLITests.swift`
   as in finding 5. The goldens under `Tests/Fixtures/snapshots/macos/` are
   rendered from a `.ready` export and do not change; a diff there is a
   rejection.

10. **minor: the wording thresholds and the seed table are not stated as
    numbers, so their tests would be written against the implementation.**
    D4 title row lists "~1 min remaining", "less than a minute", "a few
    seconds", "about a minute", "a bit longer than usual" without the
    boundaries between them; step 2 promises unit tests of the thresholds. Put
    the boundaries in the plan, for example a few seconds below ten, less
    than a minute below sixty, then minutes rounded up with the tilde, and
    the seed label while `samples == 0`, so the test table in finding 3 is
    derivable from the plan and a reviewer can check both against it.
    Likewise "seeded rates reproduce the table" in step 1 compares constants
    in code with a table in a plan file; make `StageRates.seeds` the one
    source, have the plan point at it, and let the test assert structure
    instead: every `PipelineStage` has a seed, every `SpeechEngineID` has a
    transcribe seed, every seed is positive and `samples == 0`.

11. **minor: the presenter's single subscription and the queued pulse have
    no stated proof and one of them cannot have one.** D3, "subscribed to the
    event bus once"; D4, "Indeterminate pulse only while the meeting is queued
    and no event has arrived". `MeetingEventBus` does not replay, so a model
    created after the pipeline started shows the queued state for a meeting
    that is transcribing; the plan should say the model is created in
    `AppController.launch()` before `resumeUnfinished()` runs, and
    `AppControllerTests` should assert that order by enqueuing a held run
    before `launch()` and checking the model's stage after it. The pulse is
    an animation with nothing observable; list it under the manual checklist
    rather than as an acceptance check. Reduce Motion cannot be toggled from
    a hostless test; the `reduceMotion` parameter on the pure seam in
    finding 3 is the testable part, and the view's reading of
    `accessibilityReduceMotion` stays a manual check.

## Coverage as the plan stands

| Step | unit (CI) | integration (CI) | UI (hosted) | opt-in or manual |
|---|---|---|---|---|
| 1 weighted progress and estimator | 3 named, all deterministic once findings 1, 2 and 7 land | 1, vacuous on a constant `now` | 0 | the 30 percent acceptance, the seed measurement |
| 2 card and shared model | 1 named, wording | 0 | 1, flaky as written | Reduce Motion, the queued pulse |
| 3 warm-up and hand-over | 2 named, neither writable against the current seams | 0 | 0 | the real engines' no-op second `prepare()` |

## Resolution

Folded into `.plans/2026-09-28-processing-progress.md` on 2026-09-28.

1. Adopted. Step 1 replaces the 30 percent acceptance with `ProcessingEstimatorTests` and the exact-sum `secondRunEstimatesFromTheFirstRunsMeasuredRates` on a `ManualClock`; the hardware measurement is marked manual.
2. Adopted. D2 adds `clock: any Clock<Duration>` to `PipelineDependencies`; step 1 adds the fakes' duration hooks and `stageDurationsAreRecordedFromTheClock`.
3. Adopted. D3 defines `ProgressPresentation.state(progress:elapsed:reduceMotion:)`; step 2 lists `ProcessingPresentationTests`, `ProcessingProgressModelTests` and the UI test behind `-steno-ui-testing-hold-transcribe` with `FakeSpeechEngine.holdTranscribe`.
4. Adopted. D5 keys the guard on the setting's `SpeechEngineID` asset and `.offlineDiarizer`; step 3 adds `FakeDiarizer.preparations`, `makeDiarizer` on `AppEnvironment.preview` and both `RecordingControllerTests` methods.
5. Adopted in part. D1 and step 1 print progress lines to standard error and add `processReportsProgressOnStderr` with the line shape. The `--progress-json` flag is not added because decision 8 fixes step 1's CLI change to progress lines on stderr.
6. Adopted. D1 makes the clamp per `ProcessingRun`, with a `.decode` event starting a new run; step 1 adds `resumeUnfinishedStartsAgainAtZero` and `fractionNeverDropsWhenTheTokenCountIsRevised`, step 2 the model reset test.
7. Adopted in part. Step 1 adds `ratesAreAveragedAcrossRuns`, `anUnknownEngineUsesTheParakeetSeedWithZeroSamples` and the unknown-row load test; D2 states the first sample replaces the seed. `SettingsStoreTests.updateNeverDropsAConcurrentChange` is declined because decision 1 moves the rates out of `Settings` into `stageRate`, written in one transaction by `MeetingStore+Timings`, so the property it would prove no longer exists.
8. Adopted. Step 3 adds `RecordingAudioDecoder`, `aCallDecodesEachLaneOnce` and `diarizeDecodesWhenHandedAnotherLane`, and names the three `diarize` tests that change.
9. Adopted. Steps 1 and 2 list every named test under the step that changes it.
10. Adopted. D3 states the wording thresholds as a table; D2 makes `StageRates.seeds` the one source and step 1 asserts structure.
11. Adopted. D3 creates the model before `resumeUnfinishedProcessing()` and step 1 adds the `AppControllerTests` assertion; the pulse and Reduce Motion are listed as manual in step 2.
