# Steno processing progress code: correctness review

Date: 2026-09-28. Reviewer: correctness pass over the code of the three stacked PRs for
`.plans/2026-09-28-processing-progress.md` on branch `feat/processing-warmup`, commits
`87a1b7e` step 1, `0a14a9a` step 2 and `5e34b96` step 3, plus the uncommitted simplification
pass in the working tree, reviewed as it stands. Read: `git diff origin/main...HEAD -- Sources
Tests apps` and `git diff HEAD`, against the surrounding code in `Sources/StenoCore/Pipeline/`,
`Sources/StenoCore/Storage/`, `Sources/steno/Commands/`, `apps/macos/Steno/` and the three test
targets. Verified by running the package tests in the Linux scratch copy: 112 tests pass, among
them `ProcessingEstimatorTests`, `PipelineIntegrationTests`, `StageTests`, `MeetingStoreTests`,
`SchemaSnapshotTests` and `CLITests.processReportsProgressOnStderr`, which exercises
`String(format:)` and `padding(toLength:)` on Linux Foundation. The clamp arithmetic in finding
1 was replayed numerically from `ProcessingEstimator.progress` and `ProcessingRun.progress`
with the committed seeds. The nested `ProcessingProgressModel.Entry` was checked against the
Swift 6.1 compiler in strict concurrency mode. Line numbers are the working tree's.

Severity: blocker means data loss, a crash, a privacy leak, or a test that will fail on macOS;
major means wrong behaviour the owner will see; minor means an edge the code should close.
Counts: 0 blockers, 1 major, 5 minor.

## Findings

1. **major**: `Sources/StenoCore/Pipeline/ProcessingRun.swift:46-48`, read with
   `Sources/StenoCore/Pipeline/ProcessingProgress.swift:109-113` and
   `apps/macos/Steno/Main/ProcessingPresentation.swift:128-129,155-161`. The run clamps the
   posted `fraction` up to the previous event's `nextFraction`, then sets `nextFraction` to
   `max(fraction, honestNext)`. Whenever a stage finishes faster than its rate predicted, the
   honest `nextFraction` of the following event sits below the lifted `fraction`, so the two
   become equal and `expectedTimeToNextEvent` is zero. The presenter then returns `target`
   at once, so the bar does not move for the whole stage, and `isSlow` is `elapsed > .zero`,
   so from the second 1 Hz sample the card reads "a bit longer than usual" while the run is
   ahead of schedule. Replayed with the seeds for a one-hour two-lane call: with the
   WhisperKit seed and an engine that runs at RTFx 18 instead of the seeded 6, the second
   lane ends at elapsed 405 s with `nextFraction` 0.744, diarize then posts `fraction` 0.744,
   `nextFraction` 0.744 and the bar is frozen at 74 percent under "a bit longer than usual"
   for the sixty seconds diarize takes. The milder form is the common one: with the Parakeet
   seed, decode finishing in 5 s instead of the seeded 36 s lifts transcribe lane one from
   0.014 to 0.091 but leaves its `nextFraction` at the honest 0.124, so the window that
   should be 40 s is 12.8 s; the bar reaches 11 percent after 13 s and the card says "a bit
   longer than usual" for the last twenty seconds of a lane that ran at exactly the seeded
   rate. The spec's D1 wants the clamp to keep the bar from moving backwards, not to shorten
   the next step. Smallest fix: keep the honest step's share of the remaining time across
   the clamp, so `expectedTimeToNextEvent` survives it:

   ```swift
   let honest = estimator.progress(stage, lane: lane, elapsed: elapsed / .seconds(1), in: stages)
   var event = honest
   event.fraction = max(honest.fraction, last?.nextFraction ?? 0)
   let share = honest.fraction < 1
     ? (honest.nextFraction - honest.fraction) / (1 - honest.fraction) : 0
   event.nextFraction = min(1, event.fraction + share * (1 - event.fraction))
   ```

   With this the transcribe window above is 39.9 s and diarize keeps its 60 s. Add a test
   beside `fractionNeverDropsWhenTheTokenCountIsRevised` that runs a stage in a tenth of its
   expected time and asserts the next event's `expectedTimeToNextEvent` equals
   `expectedStep` of that stage within a tolerance, and that `nextFraction > fraction` for
   every stage whose `expectedSeconds` is positive. `expectLaneBoundary` in
   `PipelineIntegrationTests` still holds because the lifted `fraction` is unchanged.

2. **minor**: `Sources/StenoCore/Pipeline/ProcessingEstimator.swift:235` with
   `Sources/StenoCore/Pipeline/Stages/Persist.swift:15-19`. `persist` is modelled as a flat
   unit, but for every asset that is not already AAC, which is every Mac recording and every
   CLI run, the stage encodes the whole recording to AAC through `decoder.mixdown`, so its
   duration grows with the meeting. The learned flat rate is whatever the last meeting took:
   a five-minute meeting teaches two seconds, and the next two-hour meeting then spends
   half a minute in persist with the bar at 99 percent and the text passing from "a few
   seconds" to "a bit longer than usual". Smallest fix: make `units(.persist)` the meeting's
   duration so the rate is per audio second, and update the seed to roughly 0.005 like
   decode; the two flat stages that follow are sub-second and can stay flat.

3. **minor**: `Sources/StenoCore/Storage/MeetingStore+Timings.swift:11-16` and
   `Sources/StenoCore/Pipeline/ProcessingEstimator.swift:282`. `stageRates()` loads any
   `seconds_per_unit` value and `progress` turns `remaining` into `Duration.seconds(_:)`,
   which traps on a non-finite argument and on values beyond the Int128 attosecond range.
   Nothing in this diff can write such a row, so the only path is a hand-edited or corrupted
   database, but that row would then crash the app at the first event of every run and
   nothing but deleting the row recovers it. Smallest fix: in the load loop skip rows whose
   `secondsPerUnit` is not finite or is negative, and in `ProcessingRun.measure` return nil
   when `seconds` is not finite.

4. **minor**: `Sources/StenoCore/Pipeline/ProcessingPipeline.swift:368-375` with
   `apps/macos/Steno/AppEnvironment.swift:114-124` and F8 in the plan. "Alone in flight" is
   judged per `ProcessingPipeline` instance. Two pipelines over one `steno.sqlite` both
   record: the retired pipeline that `reloadPipeline()` keeps draining while the replacement
   starts a new meeting, and the CLI processing a file while the app processes a recording.
   Both runs then write samples measured under shared Neural Engine and LLM load, which is
   exactly what D2 wanted to keep out of the rates. Rare, and the EMA smooths it out, so
   minor. Smallest fix, if wanted: have `run` also ask the store for
   `meetings(inStates: [.processing]).count` once at stage start and treat more than one as
   not alone; otherwise note the limit in the doc comment.

5. **minor**: `apps/macos/Steno/Main/ProcessingProgressModel.swift:88-90`.
   `meetingsChanged` evicts every entry whose id is absent from the list. A `.decode` event
   can create an entry a moment before the first `observeMeetings()` snapshot that predates
   the meeting's insert is delivered, because the event travels pipeline actor to bus actor
   to the controller's task while the snapshot travels the GRDB queue to the main queue and
   neither orders itself against the other. The snapshot then evicts the live entry and the
   next snapshot recreates it as "Waiting to process" until the transcribe event arrives.
   Only the first snapshot after `launch()` can predate an insert, so the window is the
   warm-up span of a resumed run, and the effect is a cosmetic flicker. Smallest fix: evict
   only ids the list does contain in a state other than `.queued` or `.processing`, and
   leave deletion to the `.deleted` event, which already evicts.

6. **minor**: `apps/macos/StenoUITests/LaunchSmokeTests.swift:64-71` with
   `apps/macos/Steno/AppEnvironment.swift:263`. The held run starts inside `launch()` as
   soon as the app is up and sits in transcribe for two lanes of ten seconds. The test
   reaches the card only after `launchAndSelectTheFixtureMeeting`, which allows up to ten
   seconds for the window and another ten for the fixture meeting. On a slow hosted runner
   the card can be gone before `waitForExistence` on `processing-card` begins, and the test
   then fails on a timing it does not control. Smallest fix: hold each lane for 30 s and
   raise `waitForNonExistence` to 80 s, or hold the fake engine's `prepare()` instead so the
   run cannot leave "Waiting to process" until the test has found the card.

## Areas checked and found clean

- `ProcessingPipeline` run lifecycle: `runs[meetingID]` is created after the rates load and
  removed by `exclusively`'s `defer`, and `deliver` and `retention` post inside the body, so
  no stage posts over a missing run; a stage reached directly posts over a throwaway run and
  records nothing. `post` and `run` read and write the run without an `await` between them.
- `warmUp()`: one in-flight task, every caller awaits the same task, `preparing` is cleared
  by the caller that set it and cannot be replaced while set because every later caller
  sees it and joins; a thrown `prepare()` propagates to every awaiter, clears the task and is
  retried on the next call. `Task.value` does not propagate the awaiting task's
  cancellation, so a cancelled caller cannot leave `preparing` set.
- "Alone in flight": `inFlight.count == 1` at stage start plus an unchanged `admissions`
  counter at stage end covers a meeting admitted and finished within the stage, a meeting
  admitted before and finished during, and `rerunSummary` or `redeliver` on another meeting.
  `concurrentRunsRecordNoRates` pins it.
- Buffer hand-over: `decodeAndTranscribe` releases `last` before each decode, returns the
  final lane, `transcribeAndDiarize` holds it as a local through `diarize` and drops it on
  return, so one `AudioBuffer16k` is alive at any time on the path `process` takes; `diarize`
  still writes one clip per cluster and returns an empty `Diarization` for an asset with no
  diarizable lane. `aCallDecodesEachLaneOnce` proves the decode count.
- `ProcessingEstimator` and `StageRates`: zero `duration`, zero `units` and zero `total` are
  guarded, `measure` returns nil for zero units, the fallback chain `entries` then seed then
  `parakeet-v3` seed covers a fake engine, `isKeyed` and `key(_:)` agree, `tokenCount([])`
  is 0, `absorbing` replaces the seed on the first sample and averages after, and the
  read-modify-write in `record` is inside one `writer.write` transaction, so two pipelines on
  one database serialize on SQLite.
- Migration v3 and `StageRateRow`: composite primary key, GRDB `save` updates by that key
  or inserts, `updated_at` is a `.datetime` column like every other timestamp, the golden
  `v3.sql` matches and unknown stages and stray keys are skipped on load.
- CLI `Process`: the bus is private to the command, `waitUntilIdle` returns after the last
  post, `finish()` flushes the unbounded buffers before ending the stream, a failed run
  exits non-zero with its stage in the reason, the lane counter counts only `.transcribe`
  events of this meeting and `rerunSummary` never runs under `steno process`.
- macOS app: `launch()` subscribes before `resumeUnfinishedProcessing()`, `apply` runs on
  the main actor from a task that inherits `launch()`'s isolation, `warmUpPipeline()` reads
  the setting and both markers on every recording start and only logs the error text with
  no meeting id, the hold argument is read only inside `preview()`, which only
  `-steno-ui-testing` selects, `since` is stamped per event and `TimelineView` samples from
  it with `elapsed` floored at zero, and `reduceMotion` changes the tween alone.
- Swift 6: the nested `ProcessingProgressModel.Entry` does not inherit the class's main-actor
  isolation, so `TabText` and the nonisolated `ProcessingPresentationTests` build it freely;
  the closures stored on `FakeSpeechEngine`, `FakeDiarizer`, `PassthroughCleaner` and
  `FakeSummarizer` are `@Sendable`; `recordingDidChange` is formed in the controller's
  main-actor context and calls `warmUpPipeline()` synchronously on the same actor.
- Privacy and scope: no new code path sends bytes anywhere; `warmUp()` only reaches
  `ensureInstalled` through `prepare()`, which the app guards with the marker files; the
  stderr lines carry stage, percent, remaining time and lane, never transcript text.

## Resolution

Applied 2026-09-28 in one commit at the top of the stack; decision numbers refer to the
folding brief.

1. adopted: `Sources/StenoCore/Pipeline/ProcessingRun.swift` `progress(_:lane:elapsed:)`
   clamps to the previous `nextFraction` and rescales `nextFraction` so the honest step's share
   of the remaining time survives; an unclamped event is returned as computed. Test
   `ProcessingEstimatorTests.anEarlyStageLiftsTheBarWithoutShorteningTheNextWindow`.
2. adopted: `PipelineStage.costDriver` makes `persist` `.audioSecondsAllLanes`, seed 0.005;
   the estimator test's persist duration and units, and the plan's D2, updated.
3. adopted: `MeetingStore+Timings.swift` `stageRates()` skips a row whose `secondsPerUnit` is
   not finite or is negative; `ProcessingRun.measure` returns nil for a non-finite total.
   Test `MeetingStoreTests.stageRatesSkipRowsWhoseRateIsNotFiniteOrIsNegative`.
4. adopted as a doc sentence on `ProcessingPipeline.run` (decision 4, no code change).
5. adopted: `ProcessingProgressModel.meetingsChanged` evicts only a meeting the list shows in a
   state other than queued or processing; an absent meeting keeps its entry until `.deleted`.
   Tests `ProcessingProgressModelTests.testAMeetingAbsentFromTheListKeepsItsEntryUntilListedElsewhereOrDeleted`
   and `testAMeetingProcessedAgainReentersWithAFreshEntry`.
6. adopted: `AppEnvironment.uiTestingTranscribeHold` is 60 s per lane, the hold sleeps in the
   fake engine's `onTranscribe`, and `LaunchSmokeTests.testProcessingCardFollowsAHeldRun`
   sizes its waits from its own readiness, asserts the bar is above zero by the second sample
   and waits 150 s for the card to go.
