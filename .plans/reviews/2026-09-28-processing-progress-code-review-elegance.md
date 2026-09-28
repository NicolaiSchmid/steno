# Steno processing progress code: architecture and elegance review

Date: 2026-09-28. Reviewer: architecture and elegance pass over the code of the three
stacked PRs for `.plans/2026-09-28-processing-progress.md` on branch
`feat/processing-warmup`, commits `87a1b7e` step 1, `0a14a9a` step 2 and `5e34b96` step 3,
plus the uncommitted simplification pass in the working tree, which was reviewed as it
stands. The diff was read against the code it touches: `Sources/StenoCore/Pipeline/`,
`Sources/StenoCore/Storage/MeetingStore+People.swift` and `Records.swift`,
`Sources/StenoCore/Testing/`, `Sources/steno/Commands/`, `apps/macos/Steno/AppController.swift`,
`AppEnvironment.swift`, `MenuBar/`, `Main/` and `Design/`. Correctness and test coverage are
reviewed elsewhere; nothing below is about formatting the tools enforce. Line numbers are
the working tree's.

**major**: expensive to change once other code compiles against it or a database has
applied it. **minor**: one PR.

## Findings

1. **major**: `Sources/StenoCore/Storage/Migrations.swift:211-212`,
   `Sources/StenoCore/Storage/Records.swift:612-613`,
   `Tests/Fixtures/snapshots/schema/v3.sql`, `Tests/StenoCoreTests/MeetingStoreTests.swift:513`.
   The `stageRate` table spells its columns `seconds_per_unit` and `updated_at`. Every other
   column in the schema is camelCase: `updatedAt`, `meetingID`, `sampleClipURL`,
   `clusterConfidence`. Migrations are append-only, so once v3 ships the only way back is a
   v4 that rebuilds the table, and `StageRateRow` is the one record in `Records.swift` that
   needs a `CodingKeys` mapping. The plan review that proposed the table wrote the names in
   snake_case and the plan copied them; that is where the spelling came from, not from a
   decision about the schema. Rename the columns to `secondsPerUnit` and `updatedAt` in
   `v3` before it merges, drop the `CodingKeys` enum from `StageRateRow`, regenerate
   `v3.sql`, fix the raw SQL in the store test, and change the two words in the plan's D2.

2. **major**: `Sources/StenoCore/Pipeline/ProcessingProgress.swift:7-33`,
   `Sources/steno/Commands/Process.swift:131-140`. The event does not say which lane a
   `transcribe` event starts, so the CLI counts transcribe events per meeting to print
   `lane 2 of 2`, and a subscriber that joined late would count wrong. The pipeline knows
   the lane at the moment it posts, in `ProcessingRun.progress(_:lane:elapsed:)` at
   `Sources/StenoCore/Pipeline/ProcessingRun.swift:43`, and throws it away. The public
   memberwise `init` is what makes this expensive: today it has six call sites in test
   files and none in the app; after the card ships every consumer that builds a
   `ProcessingProgress` for a test or a preview compiles against it. Decide now. Either add
   `lane: Int` and `laneCount: Int` to the value, zero and one for every stage but
   transcribe, and let the CLI print from them, or drop the lane suffix from the CLI line
   and the `CLITests` assertion and leave the value as it is. The first is the honest one,
   since the card's next design round will want "lane 2 of 2" too.

3. **minor**: `apps/macos/Steno/Main/ProcessingProgressModel.swift:66-70`. `apply` clamps
   the fraction and lifts `nextFraction` a second time. Core already does exactly this on
   the run at `Sources/StenoCore/Pipeline/ProcessingRun.swift:47-48`, and the plan's D1
   says nothing else may compute a fraction. Within one run core's clamp is a guarantee;
   across runs a `.decode` event resets the entry, which the model handles on the branch
   above. The app clamp therefore protects against events the pipeline never sends, and it
   is arithmetic living on the wrong side of the boundary. Delete the three lines so the
   `else if` branch stores the event as received, and move
   `testALowerFractionForTheSameMeetingDoesNotLowerTheEntry` to assert on core, where
   `ProcessingEstimatorTests.fractionNeverDropsWhenTheTokenCountIsRevised` already covers
   the same property; the model test keeps the decode reset.

4. **minor**: `apps/macos/Steno/AppController.swift:89-109` and `118-127`,
   `apps/macos/StenoTests/MenuBarViewModelTests.swift:24-45`,
   `apps/macos/StenoTests/ProcessingProgressModelTests.swift:22-40`. The model is passive:
   whoever holds it must subscribe to the bus, subscribe to `observeMeetings()` and call
   `apply` and `meetingsChanged` from two tasks. The controller does it once and two test
   files do it again, each with its own `XCTFail` on the meeting stream. The existing
   pattern beside it is `MenuBarViewModel.observe()`, which owns its subscription and is
   started with one `Task` from `launch()`. Give `ProcessingProgressModel` the same shape in
   two steps so the ordering the plan wants holds: `func subscribe(_ environment:
   AppEnvironment) async` awaits `events.subscribe()` and stores the stream, then `func
   observe() async` drives both loops until cancelled. `launch()` calls `subscribe` before
   `resumeUnfinishedProcessing()` and starts `observe()` as it starts the menu bar's;
   the controller's `case .progress` disappears and `case .deleted` keeps only
   `pendingReviews`. Both `makeModel` helpers become two lines.

5. **minor**: `apps/macos/Steno/MenuBar/MenuBarView.swift:101-124`,
   `apps/macos/Steno/Design/Components.swift:186-190`. The plan's D4 says the card, the
   list entry and the menu bar row read the same fraction and remaining text so the three
   surfaces never disagree. The row prints `remaining.clockText`, a static `mm:ss` from the
   last event, while the card three windows away says "~3 min remaining" and counts down.
   `Duration.clockText` was added for this one call. The row also manufactures a model
   value inside a view, `Entry(meetingID:since: controller.environment.now())`, to get a
   title and a zero fraction. Wrap the row in the `TimelineView` the recording clock at
   line 54 already uses, sample `ProgressPresentation.state` from the entry as the card
   does, and print `remainingText`; read `entry?.title ?? Entry.waitingTitle` and
   `entry?.fraction ?? 0` instead of building an entry. Delete `Duration.clockText`.
   `Entry.waitingTitle` is the "Waiting to process" literal at
   `ProcessingProgressModel.swift:31`, which then lives next to `PipelineStage.label` in
   `apps/macos/Steno/Design/Labels.swift:17-33`, the one file for user-facing words.

6. **minor**: `Sources/StenoCore/Pipeline/ProcessingEstimator.swift:114-124`, `208-237`.
   Per-stage policy is spread over five switches on `PipelineStage` in two types:
   `StageRates.isKeyed`, `StageRates.seedKey`, `ProcessingEstimator.learns`,
   `ProcessingEstimator.key` and `ProcessingEstimator.units`. Adding a stage or changing
   what one is keyed by means finding all five, and `isKeyed` and `key` can drift apart
   with nothing to catch it. One internal table in `Sources/StenoCore/Pipeline/` says it
   once: `extension PipelineStage { var costDriver: CostDriver; var rateKeying:
   RateKeying; var isLearned: Bool }` with `enum CostDriver { case audioSecondsAllLanes,
   audioSecondsOneLane, thousandTokens, flat }` and `enum RateKeying { case none,
   speechEngine, llmModel }`. `units`, `key`, `isKeyed`, `seedKey` and `learns` become
   one-line reads of the table, and `everyStageAndEveryEngineHasAPositiveSeed` can assert
   the seeds cover every `rateKeying == .none` stage under `unkeyed`.

7. **minor**: `Sources/StenoCore/Pipeline/ProcessingEstimator.swift:23-29`, `69`, `140-145`,
   `Sources/StenoCore/Storage/MeetingStore+Timings.swift:24-34`. `StageRates.record` has no
   caller outside tests; the store implements the same fold itself, fetch the row or the
   seed, absorb, save. Two implementations of one rule, and the public one is the unused
   one. Either the store reads `stageRates()`, calls `record`, and saves the one changed
   row, or `StageRates.record` goes and the tests compute expectations with `absorbing`.
   While there: `StageRate.absorbing` reaches into `StageRates.alpha`, so the value type
   depends on its collection for a constant that is about the value; move `alpha` onto
   `StageRate`. `StageRates.fakeModel = "fake"` is a production key named after test
   fakes; what it means is "no LLM configured, the passthrough passes ran", so call it
   `noModel` or `passthrough`. `StageRates.Key.key` reads as `key.key` at every use;
   `Key.dependency` or `Key.variant` says what the string is.

8. **minor**: `Sources/StenoCore/Pipeline/ProcessingPipeline.swift:329-344`. `post` builds a
   throwaway `ProcessingRun` on the seeds when no run exists, and the comment says why:
   tests reach stages directly. A production branch that exists for tests is the smell the
   `Testing/` module is there to avoid. Add an internal `beginRun(meeting:lanes:stages:)`
   that stores the run, have `process`, `rerunSummary` and `redeliver` call it, and let
   `PipelineHarness` expose it so `StageTests` start a run before calling a stage; `post`
   then does `guard var run = runs[meetingID] else { return }` and the three-line fallback
   goes.

9. **minor**: `Sources/StenoCore/Pipeline/ProcessingPipeline.swift:145-177`,
   `Sources/StenoCore/Protocols/SpeechEngine.swift:9`. The protocol says `prepare()` is
   "Called once before the first `transcribe`". After this branch it is called on every
   warm-up and again on every run, and the pipeline's doc explains that it must hold a
   task because the engines' `loaded()` methods check a cached manager and then await the
   load. That sentence describes another module's private implementation from inside
   core, and the contract it works around is not written anywhere the engines can see it.
   The smaller fix is on the protocol and the engines: document `prepare()` as idempotent
   and safe to call concurrently, and have each engine's `loaded()` keep the in-flight
   `Task` so a second caller joins it. Then `warmUp()` is two `prepare()` calls, the
   `preparing` property and the actor re-entrancy paragraph go, and `warmUpRacingARunLoadsOnce`
   moves to `StenoSpeechTests` where the race lives. If that stays out of this plan, at
   least move the contract sentence to the protocol now and shorten the pipeline doc to
   "serialised here until the engines serialise themselves".

10. **minor**: `apps/macos/Steno/AppController.swift:33`, `62-76`. The guard that decides
    whether warming is safe reads settings, decodes an engine id and checks two model
    assets. That is environment policy, and the environment already hosts the pipeline's
    other lifecycle operations in this style, `resumeUnfinishedProcessing()` at
    `AppEnvironment.swift:155-162` and `runRetentionSweep()`, each reporting through
    `startupWarnings`. The first `os.Logger` in the app arrives here with a hard-coded
    subsystem string for one error the run itself will report anyway. Move the body to
    `AppEnvironment.warmUpPipelineIfModelsInstalled() async`, drop the logger, and let the
    controller's hook be `Task { await environment.warmUpPipelineIfModelsInstalled() }`. If
    the app wants logging later, one `Log` enum with the subsystem read from
    `Bundle.main.bundleIdentifier` is the place, not a static on a controller.

11. **minor**: `apps/macos/Steno/Recording/RecordingController.swift:52-55`, `140`,
    `apps/macos/StenoTests/RecordingControllerTests.swift:166-168`. `lastStatistics` is
    production state whose doc comment names the test that reads it. The assertion it
    serves, zero dropped frames while a fake engine warms over a synthetic backend, cannot
    show the thing the plan worries about, Neural Engine contention with the capture,
    because neither the fake nor the backend touches it. Remove the property and the
    assertion. If dropped frames matter to the owner they deserve a line in a plan and a
    `lastWarning`, not a test-only accessor.

12. **minor**: `Sources/StenoCore/Testing/FakeSpeech.swift:39-43`, `54`, `63`, `76`,
    `apps/macos/Steno/AppEnvironment.swift:312`, `Tests/StenoCoreTests/FakesTests.swift:26-36`.
    `holdTranscribe` is a second way to delay `transcribe` beside `onTranscribe`, added
    one commit later. The preview can set `engine.onTranscribe = { try? await
    ContinuousClock().sleep(for: hold) }` and get the same effect. Make the three hooks
    `@Sendable () async throws -> Void` so cancellation propagates as the doc promises,
    delete `holdTranscribe` with its `init` parameter, and delete the 200 ms wall-clock
    test in `FakesTests`, which is the only sleep on real time in the core suite.
    `RecordingAudioDecoder` at line 181 is a decoder in a file named for speech; a
    `Testing/FakeAudio.swift` beside it keeps the file names honest.

13. **minor**: `Tests/StenoCoreTests/PipelineIntegrationTests.swift:479-510`,
    `Tests/StenoSpeechTests/ModelStoreTests.swift:316`, `apps/macos/StenoTests/TestSupport.swift:133-171`,
    `apps/macos/StenoTests/AppControllerTests.swift:211`. `Gate` now exists three times and
    this branch grows the pipeline copy with `waitUntilBlocked(_:)` and `releaseOne()`.
    Move that richest copy to `Sources/StenoCore/Testing/Gate.swift` beside `CallLog` and
    `ManualClock` and delete the other two. `GatedSpeechEngine` is redundant since
    `onTranscribe` exists: `var engine = FakeSpeechEngine(); engine.onTranscribe = { await
    gate.wait() }` in `AppControllerTests` replaces it, and `TestSupport` loses twenty lines.

14. **minor**: `apps/macos/Steno/Main/Tabs/SummaryTab.swift:13-23`,
    `TranscriptTab.swift:11-20`, `TasksTab.swift:10-19`, `ScratchpadTab.swift:11-17`,
    `apps/macos/Steno/Main/MeetingListView.swift:133`, `158-159`. Four tabs carry the same
    two branches, card when an entry exists and `PendingText` only when it does not, and
    each unwraps `model.meeting` again to hand the card a meeting it only reads `source`
    and `duration` from. A `TabColumn(progress:meeting:) { content }` in `Tabs/` that
    renders the card above its content leaves each tab with one `progress == nil` guard on
    its `PendingText`, and the scratchpad with none. `MeetingRow` takes an `Entry` to show a
    string; `var statusLine: String?` says what the row needs and keeps the model type out
    of the list.

15. **minor**: names and comments. `apps/macos/Steno/Main/ProcessingPresentation.swift`
    holds `enum ProgressPresentation`; every neighbour is `Processing`-prefixed, so rename
    the type or the file. `Entry.stage` at `ProcessingProgressModel.swift:25` has no
    production reader; if it stays for tests, say so. `Motion.swift:17-19` "Named by the
    floating-indicator plan; added here first for the processing card" narrates history in
    a token comment; state what the token is for. `MeetingEventBus.swift:28-29` names the
    CLI as its caller; core does not know who calls it. `ProcessingPipeline.swift:148-152`
    describes the engines' `loaded()` internals, per finding 9. `StageRates.seeds` at
    `ProcessingEstimator.swift:81-94` is a fourteen-line provenance paragraph; the plan
    holds the basis, and one line pointing at it is enough. `Motion.pulseOpacity` is an
    opacity, not a tempo; `Theme` is where the other opacities live. The remaining
    vocabulary is consistent once finding 5 lands: `label` is the stage word, `title` the
    label with its ellipsis, `estimatedRemaining` the `Duration`, `remainingText` its
    wording, and `clockText` stays the recording clock's.

16. **minor**: commit granularity. The three cuts are right and step 1's size is forced:
    the event's payload type changes, so the menu bar consumers cannot compile in a
    separate commit. Two things to fold before the PRs open. The working tree's
    simplification touches step 1 files, `ProcessingEstimator.swift`, `ProcessingRun.swift`,
    `ProcessingPipeline.swift`, `Process.swift`, step 2 files, `ProcessingPresentation.swift`,
    `ProcessingProgressModel.swift`, and step 3 files, `DecodeTranscribe.swift`,
    `Diarize.swift`, the `warmUp()` collapse; commit it as three fixups onto `87a1b7e`,
    `0a14a9a` and `5e34b96` rather than as a fourth commit, so each PR reviews as one shape.
    `Entry` moved from nested in step 1 to top-level `ProcessingEntry` in step 2 and back
    to nested in the working tree; with the fixup only the final shape appears. `since` and
    the `now` injection were added to step 1's model in step 2 because step 2 was their
    first reader, which is fine; `Duration.clockText` was added in step 1 for the menu bar
    and goes with finding 5. Nothing in a later commit belongs in an earlier one.

## Keep

- `ProcessingProgress` as five facts and one derived duration; `expectedTimeToNextEvent`
  on the value so no presenter divides. `nextFraction` and `isEstimateSeeded` are facts
  about the estimate, not view state, and the CLI's ignoring them is fine.
- `ProcessingRun` as the owner of the clamp and the timings. It does two jobs over one
  estimator, emit clamped events and accumulate spans, in fifty lines; `measure` returning
  the sample or nil is the right seam and the pipeline stays a caller.
- `ProcessingEstimator` as a configured value with pure methods and `tokens` as its one
  revisable input; a free function would carry the same six arguments on every call.
- `Stopwatch` for an existential clock whose `Instant` cannot be named; both uses, the run
  and the stage, want the same thing.
- `DecodedLane`, the labelled tuple from `decodeAndTranscribe`, and `transcribeAndDiarize`
  as the buffer's scope. The tuple says the buffer is not part of the transcription, and
  the helper makes the one-buffer invariant a matter of scope rather than of the optimiser.
- `MeetingStore+Timings.swift` in the shape of `MeetingStore+People.swift`, and
  `StageRateRow.stage` as a string so a renamed stage is skipped rather than failing the
  fetch.
- `MeetingEventBus.finish()`: small, honest, and the right answer to a printer that must
  drain rather than be cancelled.
- `ProgressPresentation.state` as a pure function with `reduceMotion` as a parameter, and
  `ProcessingCard` in the same file; `SummaryTab.swift` and `TranscriptTab.swift` already
  pair a pure enum with its view.
- `Motion.countdown` and `Motion.pulse`, with `durationCountdown` shared between the
  `TimelineView` period and the tween so the two cannot drift.
- The CLI's `progressLine` and `remainingText` as `static func`s on `Process`: the wording
  is the CLI's and stays there.
- `PipelineHarness` with `decoder:` and `clock:`; nine parameters with defaults is still a
  flat call site, and a configuration struct would be ceremony today.
- `expectMonotonic`, `expectLaneBoundary`, `callStages` and `clone` as statics on the
  integration suite, and `MeetingEvent.progress` and `.stage` in `Support/`.
- `PreviewSeed.queueForProcessing` writing a real two-channel WAV rather than faking the
  decoder, so the UI test crosses the real `AVFoundationAudioCodec`.

## Resolution

Applied 2026-09-28 in one commit at the top of the stack; decision numbers refer to the
folding brief.

1. adopted: `v3` creates `secondsPerUnit` and `updatedAt`, `StageRateRow` has no
   `CodingKeys`, `Tests/Fixtures/snapshots/schema/v3.sql` regenerated, the store test's SQL
   and the plan's D2 updated.
2. adopted: `ProcessingProgress.lane` and `laneCount` (defaults 0 and 1 in the memberwise
   init), filled by `ProcessingEstimator.progress`; the CLI prints `lane N of M` from them.
3. declined (decision 13): a plan-named test pins the app-side clamp.
4. adopted as `ProcessingProgressModel.observe(events:meetings:)`; `AppController.launch()`,
   `MenuBarViewModelTests.makeModel` share it, `ProcessingProgressModelTests` drive the model
   directly.
5. adopted: the menu bar row samples `ProcessingPresentation.state` in a `TimelineView`,
   reads `entry?.title ?? Entry.waitingTitle` (in `Design/Labels.swift`) and
   `state?.fraction ?? 0`; `Duration.clockText` deleted.
6. adopted: `PipelineStage.costDriver`, `rateKeying` and `isLearned` in
   `ProcessingEstimator.swift`; `units`, `key`, `isKeyed`, `seedKey`, `learns` and `isSeeded`
   read them; the seed test asserts every unkeyed stage is seeded under the empty key.
7. adopted: `StageRates.record` removed (the store keeps its one-row fold), `alpha` on
   `StageRate`, `fakeModel` is `noModel`, `Key.key` is `Key.dependency`.
8. declined (decision 13): stage tests need `post()`'s standalone run.
9. declined in its large form (decision 13); the contract sentence moved to
   `SpeechEngine.prepare()` and `Diarizer.prepare()`, the pipeline's doc shortened.
10. adopted: `AppEnvironment.warmUpPipelineIfModelsInstalled()`, no logger, the controller's
    hook is one `Task`.
11. kept (decision 13): `droppedFrames` has no other reader in the app, so `lastStatistics`
    stays with a neutral doc comment.
12. adopted: `holdTranscribe` gone, the preview sleeps in `onTranscribe`, the three hooks
    throw, the 200 ms test deleted, `RecordingAudioDecoder` in `Testing/FakeAudio.swift`.
13. adopted: one `Gate` in `Sources/StenoCore/Testing/Gate.swift`; the copies in
    `PipelineIntegrationTests`, `ModelStoreTests` and `TestSupport` and `GatedSpeechEngine`
    deleted.
14. adopted: `ProcessingCardSlot` in `ProcessingPresentation.swift` is the one card branch;
    `MeetingRow` takes `statusLine: String?`.
15. adopted: the type is `ProcessingPresentation` (plan D3 updated), `Entry.stage` and
    `estimatedRemaining` say they are read by tests, the `Motion.durationCountdown`,
    `MeetingEventBus.finish` and `StageRates.seeds` comments state purpose rather than
    history. `Motion.pulseOpacity` stays beside `pressOpacity`, the other opacity token.
16. declined (decision 13): the fixes land as one commit at the top of the stack.
