# Test review of the processing progress code

Reviewed 2026-09-28 on branch `feat/processing-warmup` at commit `5e34b96` plus the
uncommitted simplification pass in the working tree, which touches
`Sources/StenoCore/Pipeline/PipelineStage.swift`, `ProcessingEstimator.swift`,
`ProcessingPipeline.swift`, `ProcessingRun.swift`, `Stages/DecodeTranscribe.swift`,
`Stages/Diarize.swift`, `Sources/steno/Commands/Process.swift`,
`apps/macos/Steno/Main/ProcessingPresentation.swift` and
`apps/macos/Steno/Main/ProcessingProgressModel.swift`. The code under review is
`git diff origin/main...HEAD -- Sources Tests apps` as the working tree now has it; the
spec is `.plans/2026-09-28-processing-progress.md`, steps 1 to 3. The package suites were
run in the Linux scratch copy through `/tmp/pp-linux.sh`: the filtered core suites pass,
88 tests, and `StenoCoreTests` plus `stenoTests` together pass, 209 tests, including
`CLITests.processReportsProgressOnStderr`. Five deliberate mutations were each run and then
reverted, with the working tree checked byte-identical afterwards: dropping the monotonic
clamp in `ProcessingRun.progress` fails five tests, ignoring the `alone` check in
`ProcessingPipeline.run` fails `concurrentRunsRecordNoRates`, removing the shared-task
early return in `warmUp()` fails `warmUpRacingARunLoadsOnce`, loading unknown-key rows in
`stageRates()` fails `stageRatesRoundTripAndUnknownRowsAreIgnored`, and recording
transcribe per lane instead of on its last lane fails three rate tests. The macOS app
tests cannot be compiled here; a struct nested in a `@MainActor` class was confirmed
usable from nonisolated code under Swift 6.1, so the simplification's move of `Entry`
into `ProcessingProgressModel` is not a compile risk. Counts: 0 blockers, 4 major, 11 minor.

Severity: blocker means a promised proof is missing or cannot run in CI; major means a
test exists but does not prove the claim; minor means hygiene.

Every test the plan names in steps 1, 2 and 3 exists under its name and asserts what the
plan says, with the qualifications in findings 1, 3, 9 and 11:
`secondRunEstimatesFromTheFirstRunsMeasuredRates` asserts `estimatedRemaining ==
.seconds(60)` with no tolerance, `ratesAreAveragedAcrossRuns` asserts `0.3 * 20 / 6 + 0.7
* 10 / 6` with `samples == 2`, `concurrentRunsRecordNoRates` asserts the seeds are
untouched, `resumeUnfinishedStartsAgainAtZero` subscribes before `resumeUnfinished()` and
asserts `.decode` at fraction 0 for both resumed meetings, the CLI test matches
`^[a-zA-Z]+ +\d{1,3}% .+$` on ten and eleven lines with "lane 2 of 2", the presenter
tests cover the wording table times seeded times `reduceMotion`, the UI test asserts the
card, the stage text, a percentage and the summary afterwards, and the warm-up tests
assert the ordering and `droppedFrames`. Every pipeline test measures on the harness's
`ManualClock`; no pipeline test touches `ContinuousClock`.

## Findings

1. **Major.** `apps/macos/StenoUITests/LaunchSmokeTests.swift:63-91` with
   `apps/macos/Steno/AppEnvironment.swift:262`. The transcribe hold is ten seconds per
   lane and starts when `AppController.launch()` resumes the queued meeting, which
   `apps/macos/Steno/StenoApp.swift:83` runs at app start, before the window exists. The
   test then spends up to twenty seconds in `launchAndSelectTheFixtureMeeting` waiting for
   the window and the list, and only afterwards looks for the card and the "Transcribing…"
   label. On the hosted `macos-15` runner the first launch of a freshly built app is the
   slow one, so the twenty-second transcribe window and the test's own readiness overlap
   by luck, not by construction; when the launch takes longer than the hold the card is
   gone before line 69 runs and the test fails with "the processing card did not appear",
   which reads as a product bug. The bar assertion is also weaker than promised: the plan
   asks for a value "within the transcribe share" and line 81 accepts `0..<100`, which the
   comment justifies by the presenter never crossing `nextFraction`; that is true, but the
   share itself is never computed, so the assertion cannot tell transcribe from any other
   stage. Change: anchor the hold to the test rather than to app start, for example a hold
   that ends when the test writes a marker file under the temporary root, or raise
   `uiTestingTranscribeHold` to at least 45 s per lane with `waitForNonExistence` at
   120 s, and compute the transcribe share from the seeds in the test so the percent can
   be bounded. The job's budget is 60 minutes, so a longer hold costs nothing that matters.

2. **Major.** Step 3 promises, under "Manual, in the `STENO_MODEL_TESTS` suite as a timing
   report", that the real engines' second `prepare()` is a no-op. Nothing was added to
   `Tests/StenoSpeechTests/` for it; the only change there is `StageRateSeedTests.swift`,
   and `ModelIntegrationTests.swift:240-245` times one `prepare()` and never a second. The
   claim that carries D5 is that a warm `prepare()` is free, and no opt-in test says so.
   Change: in `ModelIntegrationTests`, after the existing timed `prepare()`, time a second
   `prepare()` on the same engine and on `FluidDiarizer`, print both as `[model-tests]`
   lines and assert the second is under one second.

3. **Major.** `Sources/StenoCore/Pipeline/ProcessingPipeline.swift:156-177`. The doc says a
   failed load is retried rather than cached and that errors carry `.decode` for the
   engine and `.diarize` for the diarizer, and `apps/macos/Steno/AppController.swift:62-76`
   says a warm-up failure is logged and swallowed. No test reaches any of it:
   `FakeSpeechEngine.prepare()` and `FakeDiarizer.prepare()` in
   `Sources/StenoCore/Testing/FakeSpeech.swift:66-69,134-136` cannot throw, so the
   `defer { preparing = nil }` path after a throw, the stage attribution and the
   controller's swallow are unproven. A `preparing` that stayed set after a failure would
   make every later `process` on that pipeline rethrow the old error. Change: give both
   fakes a `prepareFailure: (any Error & Sendable)?`, add
   `PipelineIntegrationTests.warmUpFailureIsRetriedAndAttributed` asserting the first
   `warmUp()` throws a `PipelineFailure` with the expected stage and the second, after
   clearing the failure, succeeds and prepares again, and add a
   `RecordingControllerTests` case in which the engine's prepare throws during the
   recording and the stop still lands the meeting `.ready` with `startupWarnings` empty.

4. **Major.** `Sources/StenoCore/Events/MeetingEventBus.swift:30-35`. `finish()` is
   proven only through `Tests/stenoTests/CLITests.swift:331`, where a `finish()` that did
   not end the streams would leave the `steno` process waiting on its printer task and the
   test blocked in `process.waitUntilExit()` at line 49 with no timeout, so a regression
   shows up as a hung CI job rather than a failed test. `Tests/StenoCoreTests/EventBusTests.swift`
   has no case for it. Change: add `finishEndsEverySubscriptionAndLaterPostsReachNobody`
   asserting two subscribed streams deliver what was posted before `finish()` and then
   return nil, that a `post` after `finish()` reaches neither, and that a `subscribe()`
   after `finish()` receives later posts; and give `CLITests.run` a deadline that
   terminates the process and fails the test.

5. **Minor.** `apps/macos/Steno/Main/ProcessingPresentation.swift:35-44,63-70`. When
   `expectedTimeToNextEvent` is zero, which `ProcessingProgress` yields for `fraction ==
   nextFraction` and for rates learned at zero as `rerunSummaryAndRedeliver` shows, the
   fraction jumps to `target` at once and `isSlow` is true for any positive elapsed, so
   the first 1 Hz sample says "a bit longer than usual". No presenter test passes a zero
   expected time; `ProcessingPresentationTests.swift:49-56` covers a narrow gap, not an
   empty one. Change: add a case with `fraction == nextFraction` and one with
   `estimatedRemaining == .zero` pinning the fraction and the text, so the correctness
   reviewer's decision on that wording has a test to land in.

6. **Minor.** `apps/macos/Steno/Main/ProcessingProgressModel.swift:72-73,82-91`. Eviction
   on `.deleted` and re-entry are untested: no app test posts `.deleted` for a tracked
   meeting, and none takes a meeting from `.processing` to `.ready`, which
   `MenuBarViewModelTests.swift:88` covers, and back to `.queued`, as
   `secondRunEstimatesFromTheFirstRunsMeasuredRates` does in core by enqueuing the same
   meeting again. Change: extend `ProcessingProgressModelTests` with a `.deleted` event
   asserting `entry(for:)` is nil, and a state cycle asserting the re-entered entry has no
   progress and a fresh `since`.

7. **Minor.** `Sources/steno/Commands/Process.swift:158-160,164-178`. The CLI test covers
   two successful runs only. A failed run's stderr lines, the "processing failed" error,
   the non-zero exit and the empty stdout are unproven, and `progressLine` and
   `remainingText` have no unit test: the regex's `.+` accepts any remaining text, so the
   rounding up, `1m 0s` for 59.2 s and `0s` for a negative duration are asserted nowhere.
   Change: a `stenoTests` case for `Process.remainingText` and `Process.progressLine` on
   fixed values, and a CLI run with `STENO_LLM_FAIL` or a missing system lane file that
   asserts the failure path.

8. **Minor.** `apps/macos/StenoTests/ProcessingPresentationTests.swift:68,113-121`.
   `ProgressPresentation.state` never reads `reduceMotion`, so the loop over
   `[false, true]` at line 68 runs the same assertions twice and cannot fail on that
   axis, and line 113-121 pins `tween(reduceMotion:)`, a one-line function, rather than
   anything the view does. The plan's promise is met literally. Change: either drop the
   parameter from `state` and keep the tween test, or keep it and say in the test that
   the sample is independent of it by design, so a reader does not take the loop as a
   proof of Reduce Motion behaviour.

9. **Minor.** `Tests/StenoCoreTests/ProcessingEstimatorTests.swift:38-42`. `later` is
   computed with the same `drop { $0 != .transcribe }.reduce` the implementation uses at
   `ProcessingEstimator.swift:256`, so the "exact sum" assertion compares the code with
   itself; the independent proof is the seeds arithmetic on lines 51-55. Change: spell the
   sum out as `expectedSeconds(.transcribe) + expectedSeconds(.diarize) + ...` over the
   eight later stages, or as the literal seconds from the seed constants.

10. **Minor.** `Tests/StenoCoreTests/PipelineIntegrationTests.swift:739-740`. The plan's
    expression `0.3 * 20 / 6 + 0.7 * 10 / 6` is compared with `==` to a value the code
    computes as `0.3 * (40 / 12) + 0.7 * (20 / 12)`. The two agree in IEEE arithmetic
    today, the run confirms it, but a rewrite of `absorbing` that regroups the terms
    would fail on an ulp. Change: compare with `abs(difference) < 1e-12` as
    `MeetingStoreTests.swift:495` and `ProcessingEstimatorTests.swift:93` already do.

11. **Minor.** `Tests/StenoCoreTests/PipelineIntegrationTests.swift:780-808`. The race is
    synchronised on the meeting row turning `.processing` through `observeMeeting`, not
    on the actor: `process` writes that row and then calls `warmUp()`, so a `gate.open()`
    that lands between the two lets the warm task settle and `preparing` clear before the
    run joins it, and the run prepares again. GRDB delivers the row change after the
    commit, so `process` almost always wins, but the test is a low-probability flake by
    construction. Change: hold the gate until the engine's `preparations` log shows the
    warm-up and the diarizer's shows nothing, then have `onPrepare` record a second
    waiter count through `Gate.waitUntilBlocked(1)` only, and open once
    `harness.pipeline` has admitted the run, observable as `inFlight` through a
    `@testable` accessor.

12. **Minor.** `apps/macos/StenoTests/RecordingControllerTests.swift:189`. The "zero
    preparations while recording" assertion follows `TestSupport.settle()`, twenty yields,
    so a warm-up scheduled a little later would pass it; the assertion that carries the
    test is `== 1` after the run. Change: drop the mid-recording assertion or state that
    the final count is the proof.

13. **Minor.** `Tests/StenoCoreTests/FakesTests.swift:26-35`. The fake's hold is proven
    with a real 200 ms sleep on `ContinuousClock` in the package suite, the only wall-clock
    sleep in `StenoCoreTests`. The fake is specified to sleep on `ContinuousClock`, so the
    test is honest, but it adds latency and a lower bound is all it can assert. Change:
    keep it at 200 ms and say so, or give `FakeSpeechEngine` a `holdClock` defaulting to
    `ContinuousClock()` and test with `ManualClock`.

14. **Minor.** `apps/macos/StenoTests/ProcessingProgressModelTests.swift:22-49`. The two
    tests drive a pure `apply(_:)` through the bus, `observeMeetings()` and `waitUntil`
    polling at 10 ms with a 10 s ceiling. That mirrors `MenuBarViewModelTests.makeModel` as
    the plan asked, but nothing here needs the bus: `model.meetingsChanged([meeting])` and
    `model.apply(event)` prove the clamp and the reset without a timeout. Change: call the
    model directly in these two tests and keep the bus-driven path in
    `AppControllerTests.testLaunchShowsTheStageOfAHeldResumedRunOnTheProgressModel`.

15. **Minor.** `Sources/StenoCore/Pipeline/Stages/Diarize.swift:40-42` and
    `apps/macos/Steno/AppController.swift:63-67`. Two guards have no test through their
    caller: `diarize(asset:meeting:buffer:)` with an asset whose `lanes` is empty and a
    handed buffer, where the empty `Diarization` must win over the buffer, is covered only
    by `StageTests.swift:139` on `diarizedLane` alone; and `warmUpPipeline` with a
    `speechEngineID` setting that `SpeechEngineID(settingsValue:)` rejects must skip the
    warm-up. Change: a `DiarizeStageTests` case with `lanes: []` and a buffer asserting no
    decode, no diarization and an empty result; a `RecordingControllerTests` case saving
    `speechEngineID = "unknown"` with both models installed and asserting zero
    preparations until `stop()`.

## CI fit

`swift-ci.yml` runs the package suites, `StenoCoreTests`, `stenoTests` including
`CLITests`, `StenoSpeechTests` including `StageRateSeedTests` and `StenoEndToEndTests`,
in the `package` job on the macOS runner; the app unit tests, including
`ProcessingPresentationTests`, `ProcessingProgressModelTests`, `RecordingControllerTests`
and `AppControllerTests`, in the `app` job; and both `LaunchSmokeTests` methods in the
hosted `ui-smoke` job. No new test needs a real model or the network: the CLI runs
without `--engine` and gets `FakeSpeechEngine` from `Wiring.dependencies`, the warm-up
tests install the models through `FakeModelDownloader`, and the UI test's hold is on the
preview's fake engine over a synthetic WAV. The UI test fits the smoke job's 60-minute
budget with about half a minute of run time; finding 1 is about where its window sits,
not its length.

## Resolution

Applied 2026-09-28 in one commit at the top of the stack; decision numbers refer to the
folding brief.

1. adopted (decision 6): 60 s hold per lane, waits sized from the test's readiness, the bar
   asserted above zero by the second sample; the `0..<100` assertion kept.
2. adopted: `ModelIntegrationTests.secondPrepareOnALoadedEngineAndDiarizerIsANoOp`, gated on
   `STENO_MODEL_TESTS`, prints both `prepare()` durations for Parakeet v3 and `FluidDiarizer`
   and asserts the second is under a second.
3. adopted: `FakeSpeechEngine.onPrepare` and the new `FakeDiarizer.onPrepare` throw;
   `PipelineIntegrationTests.warmUpFailureIsRetriedAndAttributed` covers the retry, the
   `.decode` and `.diarize` attribution and a run failed by its warm-up;
   `RecordingControllerTests.testAFailingWarmUpIsSwallowedAndTheRunPreparesAgain` covers the
   app's swallow with the existing fakes.
4. adopted: `EventBusTests.finishEndsEverySubscriptionAndLaterPostsReachNobody`. The CLI
   runner's deadline was not added: `finish()` is now proven directly, and the brief did not
   list it.
5. adopted: `ProcessingPresentationTests.testAZeroExpectedTimeToNextEventPutsTheBarAtItsTargetAndReadsSlowAtOnce`
   pins the current wording; the presenter is unchanged.
6. adopted: see the two model tests under correctness finding 5.
7. adopted in part: the CLI regex now pins the remaining text's shape (`(\d+m )?\d{1,2}s`) and
   the lane suffix, and a failed run (a system lane that is not a WAV) asserts the lines up to
   the failing stage, the error naming `decode`, a non-zero exit and an empty stdout. A unit
   test of `Process.remainingText` would need `stenoTests` to depend on the executable target;
   declined as out of the brief.
8. adopted: the `reduceMotion` loop is gone from the wording test, which says the sample is
   independent of it by design; the tween test is the axis's proof.
9. adopted: the later stages' sum is spelled out.
10. adopted: `abs(difference) < 1e-12`.
11. adopted: `process` now calls `warmUp()` before `setState(.processing)` inside
    `exclusively`, so admission to `inFlight` and joining the shared task happen in one
    stretch on the actor; `inFlight` is readable and the test opens the gate once the meeting
    is in flight.
12. adopted: a comment says the final count is the proof.
13. adopted: the wall-clock test went with `holdTranscribe` (decision 6).
14. adopted: `ProcessingProgressModelTests` drives `apply` and `meetingsChanged` directly.
15. adopted: `DiarizeStageTests.anAssetWithoutLanesDiarizesNothingEvenWhenHandedABuffer` and
    `RecordingControllerTests.testStartDoesNotWarmWhenTheEngineSettingIsUnknown`.
