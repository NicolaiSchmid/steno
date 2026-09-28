# Steno: processing progress and time to summary

Status: proposal, 2026-09-28. Triggered by first-run feedback, third round: "Jamie has this
cool animation when transcribing, and it only takes 45 s even for multi-hour meetings."

Binding plans: [`.plans/2026-09-24-initial-scope.md`](2026-09-24-initial-scope.md) sets the
scope, post-meeting processing on the Mac and no live transcript in v1;
[`.plans/2026-09-25-speech-and-speakers.md`](2026-09-25-speech-and-speakers.md) holds the
engines and their measured throughput;
[`.plans/2026-09-25-macos-app-and-release.md`](2026-09-25-macos-app-and-release.md) holds the
app architecture. The sibling
[`.plans/2026-09-28-macos-visual-redesign.md`](2026-09-28-macos-visual-redesign.md) owns the
look of every screen. This plan overrides three things in the sibling plans, and whichever
plan lands second updates them. In the redesign plan's "Recording and processing states"
table, the Queued and Processing rows' tab-body cells and the Processing row's 240 pt header
progress bar are replaced by the card in D4, so the status chip is the only header signal;
the redesign's step 9 acceptance "`MenuBarViewModelTests` is unchanged" no longer holds once
step 1 here replaces `MenuBarViewModel.observeProgress()`. In
[`.plans/2026-09-28-onboarding-vault-and-llm.md`](2026-09-28-onboarding-vault-and-llm.md),
the Summary tab section's sentence "Queued and processing keep today's pending texts" is
replaced by the card. The index
[`.plans/2026-09-28-first-run-feedback.md`](2026-09-28-first-run-feedback.md) gains this plan
as a tenth row and orders it after the floating recording indicator.

## Goal

After a recording stops, the owner watches the meeting and sees a spinner and "Summary appears
after processing". Jamie shows a card at the top of the summary: the current step,
"Transcribing...", a bar that moves continuously, and "~1 min remaining". Steno gets the
same card, driven by honest numbers: a fraction weighted by how long each stage really takes
on this Mac, an estimate learned from previous runs, and continuous motion between events. The
plan also takes the cheap seconds out of the run and states plainly which part of Jamie's
45 s is out of reach for an on-device recorder and why.

## Findings

| # | Where | What | Consequence |
|---|---|---|---|
| F1 | `Sources/StenoCore/Pipeline/PipelineStage.swift:15-20`, `Sources/StenoCore/Events/MeetingEvent.swift:8` | `progress` is posted once as each of ten stages starts; `PipelineStage.fraction` is the stage's index over ten | The bar sits at 10 % for the whole transcription, jumps to 20 %, sits again. The sub-second stages weigh as much as transcribe |
| F2 | `apps/macos/Steno/Main/Tabs/SummaryTab.swift:15-22`, `apps/macos/Steno/Main/Tabs/TranscriptTab.swift`, `apps/macos/Steno/Main/Tabs/TasksTab.swift` via `PendingText` | The detail view ignores progress events; it shows a small indeterminate spinner and "Summary appears after processing" | The only place with a bar is the menu bar queue in `apps/macos/Steno/MenuBar/MenuBarView.swift:85-108`, which the owner does not have open while waiting |
| F3 | `Sources/StenoCore/Pipeline/Stages/DecodeTranscribe.swift:36`, `Sources/StenoCore/Pipeline/Stages/Diarize.swift:35`, `Sources/StenoSpeech/Engines/ParakeetEngine.swift:29` | `speechEngine.prepare()` and `diarizer.prepare()` run inside the pipeline, after the meeting has ended. All three engines keep the loaded manager, so a later `prepare()` returns at once | CoreML compile and model load land in the wait the owner watches. Measured: diarizer first call 2.7 s for 8.9 s of audio, then 0.1 to 0.2 s. Parakeet's first-run compile is not measured yet; step 1 measures it |
| F4 | `Sources/StenoCore/Pipeline/Stages/DecodeTranscribe.swift:39-49`, `Sources/StenoCore/Pipeline/Stages/Diarize.swift:33-37` | Lanes are transcribed one after the other, one `AudioBuffer16k` alive by decision; the diarize stage decodes its lane a second time | The diarized lane is always the last transcribed lane: `diarizedLane` prefers `.system` for a call and `.mixed` otherwise, and `orderedLanes` ends on the same lane. A call decodes three times today, mic, system and system again; the third decode is avoidable without a second buffer |
| F5 | `Sources/StenoCore/Protocols/PipelineBoundaries.swift:20-22`, `Sources/StenoLLM/Cleanup/LLMTranscriptCleaner.swift:28`, `Sources/StenoLLM/LLMEndpoint.swift:45` | Cleanup sends the transcript to the LLM in chunks, `maxConcurrentRequests` at a time, and reports nothing until all are back. `maxConcurrentRequests` is an `LLMEndpoint` constructor default of 2, not a setting | The longest stage by far against a local model, and the one with the least feedback |
| F6 | `.plans/2026-09-25-speech-and-speakers.md:232,509` | Parakeet v3 measured at RTFx 63 to 119 after warm-up on the fixtures, against a published 190x on M4 Pro; WhisperKit large-v3 turbo RTFx 3 to 10 | Whisper is 10x slower, so the estimate must be per engine |
| F7 | `.plans/2026-09-24-initial-scope.md:24` | "Live transcript during the meeting" is not in v1 | Jamie's 45 s for a two-hour meeting is cloud GPU transcription of audio that left the device; the only on-device way to match it is transcribing while recording. Named under Deferred |
| F8 | `Sources/steno/Wiring.swift:52,70`, `apps/macos/Steno/AppEnvironment.swift:193,209` | The CLI and the app open the same `steno.sqlite`. The CLI decodes 16 kHz WAV through `WAVAudioDecoder`, the app decodes 48 kHz CAF through `AVFoundationAudioCodec` | Rates the CLI measures on the development Mac are the app's rates for every stage except decode, whose two decoders must not share a learned rate |
| F9 | `Sources/StenoLLM/Summary/LLMMeetingSummarizer.swift:37-46,70-100` | Summarize is one request only while the transcript fits the token budget; past it, the summarizer map-reduces, one request per chunk at `maxConcurrentRequests` and one reduce call | With `llmContextTokens` at its 32 000 default a two-hour meeting crosses that line, so summarize scales with tokens like cleanup |

### Where a 60 min call spends its time today

Two lanes, mic and system, a local LLM at LM Studio. The basis column says where each number
comes from; step 1 replaces them with numbers the CLI prints.

| Stage | Cost driver | Expected | Basis | Feedback today |
|---|---|---|---|---|
| decode | file length, three times for a call today, plus the first-run compile from F3 | seconds per lane | estimated | "Decoding" |
| transcribe | audio seconds per lane, RTFx 63 to 119 | 30 to 60 s per lane, so 1 to 2 min | extrapolated from sub-10 s fixtures, where per-call overhead dominates | "Transcribing" for the whole span, bar at 10 % |
| diarize | audio seconds of one lane, RTFx 45 to 90 | 40 to 80 s | derived from 8.9 s in 0.1 to 0.2 s, extrapolated from sub-10 s fixtures; clustering grows faster than linearly with the embedding count | "Finding speakers" |
| matchSpeakers, merge | cluster count | under a second | measured | one flash each |
| cleanup | transcript tokens, chunks, endpoint | minutes on a local model, tens of seconds on a hosted one | estimated | "Cleaning up", nothing else |
| summarize | transcript tokens; one request while the transcript fits the budget, map-reduce beyond it | 10 to 60 s single-shot, minutes map-reduced on a local model | estimated | "Summarising" |
| persist, deliver, retention | rows and files | under a second | measured | three bar jumps of 10 % each |

Transcription itself is roughly a minute per hour of audio per lane, which is what the owner
compares against Jamie's card, and the bar should say so. The whole run is longer than
Jamie's because Jamie's card covers only cloud ASR and the summary follows later, while
Steno's single bar covers everything up to delivery. The card therefore names the stage and
gives a time, so a slow cleanup reads as "Cleaning up, ~3 min" rather than as a stuck bar.

## Decisions

### D1. Progress is one fraction plus an estimate, computed in core

`MeetingEvent.progress` carries a value instead of a bare stage:

```swift
public struct ProcessingProgress: Sendable, Equatable, Hashable {
  public var stage: PipelineStage
  /// elapsed / (elapsed + expectedRemaining) at the moment of posting,
  /// clamped so it never decreases within one run.
  public var fraction: Double
  /// Where the next progress event is expected to land: the end of the
  /// stage's share, or the lane boundary inside transcribe. At most 1,
  /// never below `fraction`.
  public var nextFraction: Double
  /// Wall clock the run is expected to still take.
  public var estimatedRemaining: Duration
  /// True while any rate behind the estimate is still a seed.
  public var isEstimateSeeded: Bool
  /// The lane this event starts inside transcribe, zero based; 0 elsewhere.
  public var lane: Int
  /// How many lanes transcribe runs over; 1 elsewhere.
  public var laneCount: Int
  /// (nextFraction - fraction) / (1 - fraction) of estimatedRemaining.
  public var expectedTimeToNextEvent: Duration { get }
}
case progress(meetingID: UUID, progress: ProcessingProgress)
```

`fraction` is the real elapsed time of the run over that elapsed time plus the expected
seconds of the work not yet done, so a transcription that runs twice as long as planned moves
the next number the bar shows. It is clamped per run: the pipeline keeps the last posted
fraction in the run's state and posts `max(previous, computed)`, so a re-estimate never moves
the bar backwards; `nextFraction` is rescaled with the clamp so the next step keeps its honest
share of the remaining time and a stage that finished early does not shorten the next window.
The one re-estimate inside a run is at cleanup start, when the transcript's
real token count replaces the guess from audio duration; it may shrink or grow
`estimatedRemaining` but never lowers `fraction`. `nextFraction` stays on the value so a late
subscriber shows a correct bar from the next event, and `expectedTimeToNextEvent` is computed
on the value, so no presenter divides. `PipelineStage.fraction` is deleted; nothing else may
compute a fraction or a pace.

The run's state has a name: a `ProcessingRun` value in `runs: [UUID: ProcessingRun]` on the
actor beside `inFlight`, holding the plan from the estimator, the run's start instant, the
last posted fraction and the stage timings. `process` creates it after the rates load, `run`
and `post` update and post from it, and `exclusively` removes it. A meeting processed again by
`resumeUnfinished` starts a new `ProcessingRun` and its first event is `.decode` at fraction 0;
the monotonic clamp belongs to the run, not the meeting.

`rerunSummary` and `redeliver` keep posting `progress` through `run(.summarize)` and
`post(.deliver)` over a `ProcessingRun` built from their own stage list. The presenter in D3
tracks only meetings in `.queued` or `.processing`, so these events drive nothing in the card,
the list entry or the menu bar row; the detail view's existing `isBusy` spinner covers both
re-runs, as it does today.

The CLI's `steno process` prints one line per event to standard error, `stage percent
remaining`, with `lane N of M` from the event's `lane` and `laneCount` when a stage runs over
more than one lane, and keeps the meeting id alone on standard output. `Wiring.dependencies` gains an
`events:` parameter, `Process` creates the bus, subscribes before `enqueue` and prints until
`waitUntilIdle` returns. Those lines are also how the rates below get measured on the
development Mac and on Forge.

### D2. Weights come from learned per-stage rates, seeded with the measured numbers

A new pure type `ProcessingEstimator` in `Sources/StenoCore/Pipeline/` takes the meeting's
duration, its lanes, the transcript's token count and a `StageRates` value; it returns the
expected seconds per stage, the expected remaining time from any point and the next event's
fraction. Until the transcript exists the token count is estimated from the audio duration, so
an estimate exists from the first event. Rates are seconds of work per unit of driver: per
audio second for transcribe and diarize, and for persist, which encodes the whole recording to
AAC; per thousand transcript tokens for cleanup and summarize, because `LLMMeetingSummarizer`
map-reduces long transcripts, per F9; a flat cost for deliver, retention, matchSpeakers and
merge. A rate is keyed by stage and by what it
depends on: the speech engine id for transcribe, the LLM model for cleanup and summarize,
nothing else for the others. Decode is not learned: it uses a flat seed per audio second,
because the CLI and the app decode through different decoders, per F8, and a shared rate would
mix them.

`StageRates.seeds` in code is the one source of the seed constants, and the table above points
at it rather than the other way round. A key without a sample uses its seed with
`samples == 0`; an engine id without a seed, such as `FakeSpeechEngine`'s `"fake-engine"`,
uses the `parakeet-v3` seed with `samples == 0`. The event's `isEstimateSeeded` is true while
any rate in the plan has `samples == 0`, and the card then uses the softer wording in D3.

Rates live in their own table, not in `Settings`, because they are measurements the pipeline
writes ten times per run and `Settings` holds choices the owner made. One append-only
migration in `Sources/StenoCore/Storage/Migrations.swift` adds `stageRate` with columns
`stage`, `key`, `samples`, `secondsPerUnit` and `updatedAt`, primary key `(stage, key)`.
`Sources/StenoCore/Storage/MeetingStore+Timings.swift`, in the pattern of
`MeetingStore+People.swift`, adds `stageRates() async throws -> StageRates` and
`record(_ sample: StageSample) async throws`, which applies the update inside one write. A
row whose stage or key is unknown is ignored on load. The update is an exponential moving
average with alpha 0.3; the first sample replaces the seed outright, so the second run already
runs on this Mac's numbers and the third smooths them. The CLI and the app share `steno.sqlite`,
per F8, so rates the CLI measures on the development Mac are the app's rates.

Stage durations come from a new `clock: any Clock<Duration>` on `PipelineDependencies`, default
`ContinuousClock()`, beside `now`, which stays the source of row timestamps; the pipeline
measures with `clock.measure` inside `run` and `attributing`. A stage's duration is recorded
only when the meeting was the only one in flight for the whole stage, so two overlapping runs
sharing the Neural Engine and the LLM endpoint do not pollute the rates. A stage whose body
threw records nothing. Timers start after `prepare()` returns, so a cold compile never enters
a rate; `process` runs both `prepare()` calls before posting its first event, and the card's
waiting state in D4 covers that span. D5 moves the compile out of the run for recordings made
in the app. Tests compare `stage` and `fraction` where the remaining time is not the point.

### D3. Within a stage, real sub-steps where they exist, motion in the presenter

Real sub-steps: transcribe posts once per lane, lane 2 of 2 starting at the lane boundary of
the stage's share. Every other stage posts once; per-chunk cleanup progress is deferred
below.

Motion between events is the presenter's job and stays out of core. One app-wide
`ProcessingProgressModel` lives on `AppController` as `progress`, beside `recorder`,
`menuBar` and `detection`. It is fed from the controller's existing event switch, where
`case .progress: break` is today, and is created and subscribed before
`resumeUnfinishedProcessing()` runs so the first events of resumed runs are not missed. It
keeps one entry per meeting in `.queued` or `.processing`, keyed by meeting id, starts a new
entry on a `.decode` event and evicts on `.deleted` and when the meeting leaves those states.
It replaces `MenuBarViewModel.observeProgress()`, its `stages` map and `QueueItem.stage` and
`.fraction`; the queue row, the list entry and the detail view read
`controller.progress.entry(for:)`. `MeetingListView` receives only its view model, so
`MainWindow` passes the model through.

The displayed state is a pure function of the last event and the time since it:

```swift
enum ProcessingPresentation {
  struct State: Equatable { var fraction: Double; var remainingText: String; var isSlow: Bool }
  static func state(progress: ProcessingProgress, elapsed: Duration, reduceMotion: Bool) -> State
}
```

in `apps/macos/Steno/Main/ProcessingPresentation.swift`. The fraction moves linearly from
`progress.fraction` at zero elapsed to `progress.nextFraction - 0.01` at
`expectedTimeToNextEvent` and rests there, so the bar never reaches a boundary before its
event and never moves backwards; when an event lands earlier than expected the next sample
jumps forward. The remaining text counts down from `estimatedRemaining` less elapsed, floored
at zero, with these thresholds:

| Condition, checked in this order | Text |
|---|---|
| elapsed is more than 1.5x `expectedTimeToNextEvent` | "a bit longer than usual" |
| `isEstimateSeeded` and remaining is under 90 s | "about a minute" |
| `isEstimateSeeded` | "about N min", N the minutes rounded up |
| remaining is under 10 s | "a few seconds" |
| remaining is under 60 s | "less than a minute" |
| otherwise | "~N min remaining", N the minutes rounded up |

The card samples this function from a 1 Hz `TimelineView(.periodic(from:by:))`, exactly as
the menu bar's elapsed time does in `apps/macos/Steno/MenuBar/MenuBarView.swift:54`, sets the
bar to the sampled fraction and tweens the width between samples with `Motion.countdown`, the
linear 1 s tempo the floating indicator plan adds, so the steps read as continuous motion. The
bar's accessibility value is the sampled fraction. Reduce Motion drops the tween and keeps
the 1 Hz steps, which convey progress; `reduceMotion` is a parameter of the pure function so
the choice is tested.

### D4. The card

While a meeting is `.queued` or `.processing`, every content tab, Summary, Transcript and
Tasks, shows one `ProcessingCard` at the top of its reading column in place of the
`PendingText` copy and the spinner. Scratchpad keeps its editor and shows the card above it.
The tabs branch on `controller.progress.entry(for:)` before `PendingText` is consulted, so
`PendingText` stays the redesign plan's states-table picker and this plan removes only the
Queued and Processing tab-body cells from it.

| Element | Spec |
|---|---|
| Container | `Card` as the redesign plan restyles it, radius 12, `raised` surface, hairline `border`, full reading-column width, padding `md` |
| Title row | the model's `title`, which is `PipelineStage.label` plus an ellipsis, "Transcribing…", 14 medium `strong`; the chip, the list preview and the menu bar row read the same `title`. Trailing, the remaining text from D3, 13 `muted` mono digits. Before the first event of the run the title is "Waiting to process", the redesign plan's Queued copy, with no trailing text |
| Bar | 4 pt linear, radius 2, track `secondary`, fill `strong`. Indeterminate `Motion.pulse`, still under Reduce Motion, only before the first event of the run |
| Sub-line | 12 `faint`: "Audio stays on this Mac.", the redesign plan's copy, followed by `MeetingSource.label` and the meeting duration `clockText` |
| Failure | Not this card. The failed state is the redesign plan's row |
| Ids | `processing-card`, `processing-stage`, `processing-remaining`, `processing-bar` for UI tests |

The detail header keeps the redesign plan's status chip, which reads the same `title`, and
drops the redesign's 240 pt header progress bar so one pane has one bar. The list entry
preview and the menu bar queue row read the same fraction and remaining text, so the three
surfaces never disagree. `TabText.lines` gains a `progress:` parameter and renders the card's
title and remaining text as the pending lines, so `TabTextSnapshotTests` pins them with the
rest; the goldens under `Tests/Fixtures/snapshots/macos/` are rendered from a `.ready` export
and do not change.

### D5. Take the avoidable seconds out of the run

- Warm during the recording. Core gains `public func warmUp() async throws` on
  `ProcessingPipeline`, which runs `speechEngine.prepare()` and `diarizer.prepare()` once,
  through one in-flight `Task` held on the actor that `process` awaits before its first event,
  so a warm-up racing a run on the same pipeline cannot double-load: the engines' `loaded()`
  methods check a cached manager and then `await` the load, and actors are re-entrant across
  that `await`, so the pipeline is the serialization point. `AppController` calls
  `environment.pipeline.warmUp()` from its existing `recorder.recordingDidChange` hook when a
  recording starts, guarded by `models.isInstalled` for
  `SpeechEngineID(settingsValue: settings.speechEngineID)?.asset` and for `.offlineDiarizer`,
  because a `prepare()` may download and nothing downloads during a call. The guard reads the
  setting, not the engine's `id`, so the preview environment's `FakeSpeechEngine` with id
  `"fake-engine"` is guarded by the `parakeet-v3` marker files. Failures are logged and
  swallowed; the pipeline's own `prepare()` reports them as today. `reloadPipeline()` during
  a recording yields a cold replacement, which is accepted. Memory: the models sit resident
  during the recording, which every later meeting on the same pipeline already does.
- Hand the last transcribed buffer to the diarize stage, per F4. `decodeAndTranscribe` returns
  the last lane's buffer and its lane as a second value beside `Transcription`; `process`
  passes it to `diarize(asset:meeting:buffer:)` as its own argument and it goes out of scope
  before `matchSpeakers`, so it never outlives `diarize` through `merge`. `diarize` decodes
  only when the buffer's lane is not the diarized lane, a branch `process` never takes with
  today's lane rules and a test reaches directly. The one-buffer invariant holds: the buffer
  that would be alive anyway is reused instead of decoded again.
- Measure before touching cleanup concurrency. `maxConcurrentRequests` is an `LLMEndpoint`
  constructor default of 2, per F5; raising it for hosted endpoints needs a
  `Settings.llmMaxConcurrentRequests` property and a line in `LLMEndpoint(settings:)`, which is
  out of this plan. The CLI timings from D1 tell whether that is worth a plan.

### Deferred, with the reason

- Transcribing while recording, the only path to Jamie's 45 s on-device: the scope excludes a
  live transcript in v1, and an incremental Parakeet run during a call competes with the
  capture for the Neural Engine and battery. A separate plan, if at all, after v1 ships.
- Per-chunk cleanup progress: `TranscriptCleaner.clean` would need a progress parameter and
  the LLM cleaner a call per chunk, a protocol change that touches StenoLLM, the fakes and
  the bake-off, to correct drift inside one stage that the learned per-token rate and the
  presenter's motion already cover. If the D1 timings show cleanup on a local model routinely
  crossing the 1.5x threshold, add it then.
- Transcribing both lanes concurrently: two hour-long buffers of about 230 MB each alive at
  once against the one-buffer decision; the gain is bounded by the shared ANE anyway.
- Running diarize concurrently with the second lane's transcription: same ANE contention;
  measure with the D1 timings first.

## Steps

Each step is one PR with tests and builds green on its own; Conventional Commits scopes in
brackets. This file is committed with step 1. When the redesign plan lands, its states table,
its step 9 acceptance and the first-run index are updated to point here, one line each, in
that PR. Step 2 depends on the redesign's step 7a for `EmptyState`, the entry preview line and
the header, and says so in its PR.

1. **Weighted progress, the estimator and learned rates** [`core`, `macos`].
   `ProcessingProgress`, `ProcessingRun`, `ProcessingEstimator`, `StageRates` with `seeds`,
   the `stageRate` migration and `MeetingStore+Timings.swift`, `clock` on
   `PipelineDependencies`, the pipeline running both `prepare()` calls before its first
   event, timing stages after them and posting clamped fractions, per lane in transcribe,
   `PipelineStage.fraction` removed, `Wiring.dependencies` taking `events:` and `steno process`
   printing progress lines to standard error. The macOS consumers that compile against the
   event: `ProcessingProgressModel` on `AppController`, created before
   `resumeUnfinishedProcessing()`, replacing `MenuBarViewModel.observeProgress()`, its `stages`
   map and `QueueItem.stage` and `.fraction`; the menu bar row reads the model's fraction and
   `estimatedRemaining`. Fakes: `PipelineHarness` gains `clock: ManualClock`;
   `FakeSpeechEngine` gains `onTranscribe`, and `PassthroughCleaner` and `FakeSummarizer` the
   same hook, beside `FakeDiarizer.onDiarize`, so a test advances the clock by a known
   duration per stage.
   New tests: `Tests/StenoCoreTests/ProcessingEstimatorTests.swift`, pure, on a 3600 s two-lane
   meeting and the seeds: every `PipelineStage` has a seed, every `SpeechEngineID` a transcribe
   seed, every seed positive with `samples == 0`; remaining at transcribe start equals the sum
   of the later shares exactly; after one sample of every stage the second estimate equals the
   recorded durations exactly; `fractionNeverDropsWhenTheTokenCountIsRevised` with tokens at
   ten times and one tenth of the guess asserts the cleanup fraction is at least the merge
   fraction and `nextFraction` stays within `fraction...1`, with retention's
   `nextFraction == 1`; `anUnknownEngineUsesTheParakeetSeedWithZeroSamples`.
   `PipelineIntegrationTests`: `stageDurationsAreRecordedFromTheClock` asserts the stored
   rates after one scripted run equal the advances per unit keyed by `FakeSpeechEngine.id` and
   that a stage whose fake threw recorded nothing;
   `secondRunEstimatesFromTheFirstRunsMeasuredRates` scripts ten seconds per transcribed lane
   and forty for diarize and asserts the `estimatedRemaining` posted at the second run's
   transcribe start is the exact sum, no tolerance; `ratesAreAveragedAcrossRuns` runs the
   fixture twice with transcribe advances of ten then twenty seconds per lane and asserts
   `0.3 * 20/6 + 0.7 * 10/6` per audio second with `samples == 2`;
   `concurrentRunsRecordNoRates` holds two meetings inside transcribe and asserts no sample;
   `resumeUnfinishedStartsAgainAtZero` subscribes before `resumeUnfinished()` and asserts the
   first event of each resumed meeting has `fraction == 0`; every posted event has
   `nextFraction >= fraction`, fractions non-decreasing within a run, and the transcribe lane
   two event's `fraction` equal to lane one's `nextFraction`. `MeetingStoreTests`: a
   `stageRate` round trip and a row with an unknown stage or key ignored on load.
   `Tests/stenoTests/CLITests.swift`: `processReportsProgressOnStderr` asserts stdout is the
   meeting id alone, ten stderr lines for the in-person run and eleven for the call, each
   matching `^[a-zA-Z]+ +\d{1,3}% .+$`, percents non-decreasing, the first line `decode 0%`,
   and the call's second transcribe line naming lane two of two.
   Existing tests that change: `Tests/StenoCoreTests/PipelineIntegrationTests.swift`
   `macCallRunsEveryStageAndLandsReady` with `collected.count` 12 becoming 13, `stages` gaining
   a second `.transcribe` and the review index 8 becoming 9, `rerunSummaryAndRedeliver` and
   `cleanupFailureKeepsTheMergedTranscriptAndStopsTheEvents` matching the new payload;
   `Tests/StenoCoreTests/StageTests.swift` `progressIsPostedOncePerStageAcrossLanes` becoming
   `transcribePostsOncePerLane` and the persist and retention event equalities at lines 454 to
   523; `Tests/StenoCoreTests/EventBusTests.swift`;
   `Tests/StenoEndToEndTests/EndToEndTests.swift:263-275` with `collected.count` 13 becoming
   14 and the indices 8 and 11 moving by one; `apps/macos/StenoTests/MenuBarViewModelTests.swift`
   `makeModel`, `testQueueOrdersByStartAndFollowsProgress` and
   `testQueueAndRecentPartitionEveryMeetingState`; `apps/macos/StenoTests/AppControllerTests.swift`
   gains the assertion that a held run enqueued before `launch()` shows its stage on the model
   after it. Manual, marked so in the PR: a 60 min synthetic asset through
   `steno process --engine parakeet-v3` on the development Mac before the seed constants are
   committed, and the Parakeet first-run compile from F3 read off the same lines.
2. **The card, the tabs and the presenter** [`macos`]. `ProcessingCard`, the three tabs and
   the Scratchpad branching on the model's entry before `PendingText`, the list entry preview
   and the detail chip reading the model's `title`, the redesign's header bar removed,
   `ProcessingPresentation` with the thresholds from D3, `TabText.lines` taking `progress:`,
   and `AppEnvironment.preview` honouring `-steno-ui-testing-hold-transcribe` by sleeping on
   `ContinuousClock` in the fake engine's `onTranscribe` hook.
   New tests: `apps/macos/StenoTests/ProcessingPresentationTests.swift` asserts the fraction at
   zero elapsed is the event's `fraction`, at `expectedTimeToNextEvent` it is
   `nextFraction - 0.01`, at twice that it is unchanged and the text is "a bit longer than
   usual", the fraction is non-decreasing over a sweep of elapsed values, and the wording
   table maps from durations to strings with `isEstimateSeeded` on and off and `reduceMotion`
   on and off. `ProcessingProgressModelTests`, driven like `MenuBarViewModelTests.makeModel`:
   a `.progress` with a lower fraction for the same meeting does not lower the entry; a
   `.cleanup` event at 0.6 followed by a `.decode` event at 0 for the same id resets the entry
   to 0. UI test in `apps/macos/StenoUITests/LaunchSmokeTests.swift`, its own method: with the
   hold argument the seeded meeting is enqueued at launch, `processing-card` appears,
   `processing-stage` reads "Transcribing…", `processing-bar`'s value is within the transcribe
   share once, and after the hold the card is gone and "Executive Summary" is present. Manual:
   the pulse before the first event and the view's reading of `accessibilityReduceMotion`.
   Existing tests that change: `apps/macos/StenoTests/TabTextSnapshotTests.swift`
   `testEmptyTabsSayWhetherContentIsStillComing`, whose `.processing` branch asserts the
   card's title and remaining text; the snapshot fixture under `Tests/Fixtures/snapshots/macos/`
   stays unchanged and a diff there is a rejection.
3. **Warm-up and buffer hand-over** [`core`, `macos`]. `ProcessingPipeline.warmUp()` and the
   shared in-flight prepare, the controller's guarded call on recording start, the diarize
   stage taking the buffer. Fakes: `FakeDiarizer` gains `preparations = CallLog<Bool>()`;
   `AppEnvironment.preview` takes `makeDiarizer` beside `makeSpeechEngine`;
   `RecordingAudioDecoder` in `Sources/StenoCore/Testing/FakeSpeech.swift` wraps any decoder
   with `decodes = CallLog<AudioLane>()` and `PipelineHarness` takes `decoder:`.
   New tests: `apps/macos/StenoTests/RecordingControllerTests.swift`
   `testStartWarmsBothEnginesWhenTheirModelsAreInstalled` installs `.parakeetV3` and
   `.offlineDiarizer` through `models.ensure` over the `FakeModelDownloader`, starts a call
   recording, waits until both `preparations` counts are one, stops, and after `waitUntilIdle`
   asserts each count is two with the first entry before the first `transcriptions` entry and
   `droppedFrames` zero for every lane; `testStartDoesNotWarmWhenModelsAreAbsent` asserts zero
   preparations until `stop()`. `PipelineIntegrationTests.warmUpRacingARunLoadsOnce` calls
   `warmUp()` and `enqueue` together and asserts one `prepare()` per engine. `StageTests`
   `aCallDecodesEachLaneOnce` asserts `decodes.entries == [.mic, .system]` for a call and
   `[.mixed]` for in-person, and `diarizeDecodesWhenHandedAnotherLane` calls
   `diarize(asset:meeting:buffer:)` directly with a buffer tagged for the other lane. Manual,
   in the `STENO_MODEL_TESTS` suite as a timing report: the real engines' second `prepare()`
   is a no-op.
   Existing tests that change: `StageTests` `diarizesTheSystemLaneOfACallAndWritesOneClipPerCluster`,
   `inPersonDiarizesTheMixedLaneAndCapsClipsAtTenSeconds` and
   `twoClustersWithOneLabelFailTheStage` with the new `diarize` signature.

## Open questions

- Brand hue for the fill: decided together with the redesign and the icon plan, not here.
- Whether the stage and remaining time also appear in the sidebar entry while the window is in
  the background; the menu bar row already covers the "am I done yet" glance. Default: yes,
  preview line only.
