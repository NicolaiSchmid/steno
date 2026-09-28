# Steno processing progress plan: correctness and feasibility review

Reviewed 2026-09-28 on branch `docs/plan-processing-progress`:
`.plans/2026-09-28-processing-progress.md` against `AGENTS.md`,
`.plans/2026-09-24-initial-scope.md`, `.plans/2026-09-25-speech-and-speakers.md`,
`.plans/2026-09-25-macos-app-and-release.md`, and the code it cites:
`Sources/StenoCore/Pipeline/` including `Stages/`, `Sources/StenoCore/Events/`,
`Sources/StenoCore/Protocols/`, `Sources/StenoCore/Storage/SettingsStore.swift`,
`Sources/StenoCore/Model/Settings.swift`, `Sources/StenoSpeech/Engines/`,
`Sources/StenoSpeech/Diarization/FluidDiarizer.swift`,
`Sources/StenoSpeech/Models/ModelStore.swift`, `Sources/StenoLLM/`, `Sources/steno/`,
`apps/macos/Steno/` and the app tests. The sibling plans were read from the
uncommitted worktree and are cited by repo-relative name:
`.plans/2026-09-28-macos-visual-redesign.md`, `.plans/2026-09-28-first-run-feedback.md`,
`.plans/2026-09-28-onboarding-vault-and-llm.md`. Findings are ordered most severe
first. Counts: 1 blocker, 6 major, 10 minor.

## Findings

1. **Blocker.** Steps, step 1 `[core]`.
   Step 1 changes the `MeetingEvent.progress` payload and deletes
   `PipelineStage.fraction`, and is scoped to core alone. Both are consumed by the
   app: `apps/macos/Steno/MenuBar/MenuBarViewModel.swift:16` reads `stage?.fraction`
   and `:49` pattern-matches `.progress(let meetingID, let stage)`;
   `apps/macos/StenoTests/MenuBarViewModelTests.swift:43,47,113` post the old shape
   and compare against `PipelineStage.summarize.fraction`. `.github/workflows/swift-ci.yml:19-33`
   runs on `Sources/**` changes and `:187-290` builds and tests the macOS app in the
   same run, so the step 1 PR cannot go green. Fix: state in step 1 that it carries
   the minimal app adaptation (`QueueItem` stores a `ProcessingProgress?` and the
   `observeProgress()` match reads it; the two tests post the new shape), or keep
   `PipelineStage.fraction` and the old case alive until step 2 and delete them there.

2. **Major.** D5, first bullet, and step 3.
   "`AppEnvironment` launches a `Task` that calls `speechEngine.prepare()` and
   `diarizer.prepare()`" has no object to call. The engines live only inside
   `ProcessingPipeline.dependencies`, which is `let dependencies: PipelineDependencies`
   without `public` (`Sources/StenoCore/Pipeline/ProcessingPipeline.swift:55`), and
   `AppEnvironment` builds `PipelineDependencies` inside the `makeDependencies` closure
   without retaining it (`apps/macos/Steno/AppEnvironment.swift:205-220`). Each
   `reloadPipeline()` also builds a cold engine (`:114-125`), so warming a cached
   engine reference would warm the retired pipeline after a Speech or LLM save. Fix:
   add `public func warmUp() async throws` to `ProcessingPipeline` in core, which calls
   `prepare()` on both dependencies, and have `AppEnvironment` call
   `pipeline.warmUp()` on the current pipeline; name it in step 3 as the core half.

3. **Major.** D1, `ProcessingProgress.fraction` and its doc comment.
   "The fraction is expected elapsed time over expected total time" and "never
   decreases within a run" contradict each other as soon as the total is
   re-estimated during the run, which D2 requires: the token count is estimated from
   the audio duration until the transcript exists, and each stage's measured duration
   feeds the EMA. When the transcript after `merge` has more tokens than the
   duration-based guess, the expected total grows and the cleanup-start fraction lands
   below the `nextFraction` the diarize event promised, so the bar would move
   backwards or the presenter would silently clamp. Fix: define the fraction as the
   cumulative share of the estimate frozen at the run's first event, and let only
   `estimatedRemaining` and `nextFraction` absorb re-estimation; or state that core
   clamps `fraction = max(previous.nextFraction, computed)` and that the pipeline
   test asserts it.

4. **Major.** D1 and step 1, the paths that post `progress` outside `process`.
   `rerunSummary` and `redeliver` post `progress` through `run` and `post`
   (`Sources/StenoCore/Pipeline/ProcessingPipeline.swift:190-192,202`,
   `Sources/StenoCore/Pipeline/Stages/Deliver.swift:7`), and
   `Tests/StenoCoreTests/PipelineIntegrationTests.swift:186-190` asserts that
   sequence. The plan defines the fraction "over the whole run" and the estimator's
   inputs as duration, lanes and token count, and says nothing about what these two
   operations post. Whatever they post reaches the shared `ProcessingProgressModel`
   for a `.ready` meeting. Fix: state that `rerunSummary` and `redeliver` build a
   `ProcessingProgress` over their own stage list only (summarize then deliver; deliver
   alone), that `fraction` restarts at 0 for them, and that every presenter shows the
   card and the row only while the meeting is `.queued` or `.processing`, as
   `MenuBarViewModel.rebuildQueue` already filters (`MenuBarViewModel.swift:59`).

5. **Major.** "Where a 60 min call spends its time today", rows transcribe and diarize.
   The section says "Rates from the speech plan's measurements" and "Estimates are
   marked", but the two largest rates are extrapolations. RTFx 63 to 119 was measured
   on fixtures shorter than 10 s (`.plans/2026-09-25-speech-and-speakers.md:156-159,509`),
   where per-call overhead dominates; the diarizer figure is "8.9 s in 0.1 to 0.2 s"
   (`:512`), and "RTFx 45 to 90" appears nowhere in the speech plan. Long-form
   behaviour differs in both directions: Parakeet's throughput on an hour is not the
   throughput on six seconds, and the offline diarizer's clustering grows faster than
   linearly with the number of embeddings. D2 then seeds `StageRates` from this table.
   Fix: mark both rows "(extrapolated from sub-10 s fixtures)", and make step 1's
   acceptance measure a 60 min synthetic asset with `steno process --engine parakeet-v3`
   before the seed constants are committed.

6. **Major.** D5, third bullet.
   "`maxConcurrentRequests` is a setting" is false. It is a stored property of
   `LLMEndpoint` with a constructor default of 2
   (`Sources/StenoLLM/LLMEndpoint.swift:35,45`); `LLMEndpoint(settings:)` does not set
   it (`:60-65`) and `Settings` has no such property
   (`Sources/StenoCore/Model/Settings.swift:5-23`). The owner cannot tune it, and the
   "no change in this plan" conclusion rests on a knob that does not exist. Fix:
   "is an `LLMEndpoint` constructor default of 2; raising it for hosted endpoints
   needs a `Settings.llmMaxConcurrentRequests` property and a line in
   `LLMEndpoint(settings:)`, which is out of this plan".

7. **Major.** "Where a 60 min call spends its time today", row summarize, and D2's
   "a flat cost for summarize".
   `LLMMeetingSummarizer.summarize` is one request only when the transcript fits the
   token budget; otherwise it runs map-reduce, one request per chunk at
   `maxConcurrentRequests` plus a reduce call
   (`Sources/StenoLLM/Summary/LLMMeetingSummarizer.swift:37-46,70-100`). With
   `llmContextTokens` at its 32 000 default a two-hour meeting crosses that line, so a
   flat summarize rate mis-estimates exactly the meetings the estimate exists for.
   Fix: rate summarize per thousand transcript tokens like cleanup, keyed by the LLM
   model; the single-shot case becomes a small intercept.

8. **Minor.** D4, Queued row, "Waiting for the current meeting".
   The pipeline does not queue meetings behind each other: `start(assetID:)` launches
   one `Task` per asset (`Sources/StenoCore/Pipeline/ProcessingPipeline.swift:111-116`)
   and `process` writes `.processing` at once (`:144`), so `.queued` is transient and
   two meetings run their stages concurrently. The same copy problem is in the
   redesign plan's Queued row. Two consequences for this plan: the title text promises
   a wait that never happens, and two overlapping runs share the Neural Engine and the
   LLM endpoint, so their stage timings pollute the EMA. Fix: title "Starting…" with
   no trailing text; in D2, skip the rate sample when `inFlight.count > 1`.

9. **Minor.** D5, first bullet, the engines under concurrent `prepare()`.
   `ParakeetEngine.loaded()`, `FluidDiarizer.loaded()` and `WhisperKitEngine.loaded()`
   check the cached manager and then `await` the load
   (`Sources/StenoSpeech/Engines/ParakeetEngine.swift:36-46`,
   `Sources/StenoSpeech/Diarization/FluidDiarizer.swift:95-108`). Actors are
   re-entrant across `await`, so a warm-up `prepare()` racing a `transcribe` from a
   meeting still processing when the recording starts loads two managers, about
   0.5 GB each for Parakeet, and compiles twice. The FluidDiarizer comment at `:37-40`
   names this as the follow-up it never needed until now. Fix: cache the in-flight
   load as a `Task` in each engine; step 3's fake-engine test includes a concurrent
   `prepare()` and `transcribe()` and asserts one load.

10. **Minor.** D5, first bullet, memory sentence, and step 3 acceptance.
    "Which the second and every later meeting of a session already do" holds only
    while no `reloadPipeline()` intervened; each rebuild creates cold engines
    (`apps/macos/Steno/AppEnvironment.swift:205-220`), and the retired pipeline's
    resident models stay until it drains (`:118-124`). Loading CoreML models while the
    capture's real-time path runs is plausible at `.utility` priority but unmeasured.
    Fix: reword to "which every later meeting on the same pipeline already does", and
    add to step 3's acceptance "`droppedFrames` is zero for every lane on a synthetic
    recording during which the warm-up runs".

11. **Minor.** D5, second bullet, the one-buffer invariant.
    If `decodeAndTranscribe` returns the buffer inside `Transcription`, the value is
    reachable until `merge` reads `transcription.lanes`
    (`Sources/StenoCore/Pipeline/ProcessingPipeline.swift:155-156`), so a 230 MB
    buffer outlives `diarize` through `matchSpeakers` and `merge`. It is still one
    buffer, but not "the buffer that would be alive anyway". Fix: return the buffer as
    a second value, pass it to `diarize` as its own argument, and let it go out of
    scope before `matchSpeakers`.

12. **Minor.** D3, "No timers: the animation is the clock".
    Two text behaviours need a clock: "~1 min remaining" counts down between events,
    and "a bit longer than usual" flips at 1.5x the expected time to the next event.
    SwiftUI animations interpolate values and fire no callbacks. Fix: state that a
    `TimelineView(.periodic(by: 1))` drives the text, the same device the redesign
    plan uses for elapsed time (`.plans/2026-09-28-macos-visual-redesign.md`,
    "Recording and processing states" preamble), and that the animation drives the bar.

13. **Minor.** F8 and D2, "rates the CLI measures on the development Mac are the
    app's rates too".
    The CLI decodes 16 kHz WAV through `WAVAudioDecoder` (`Sources/steno/Wiring.swift:70`)
    while the app decodes 48 kHz CAF and resamples through `AVFoundationAudioCodec`
    (`apps/macos/Steno/AppEnvironment.swift:209`), so a decode rate keyed by "nothing
    else" mixes two decoders. Transcribe, diarize and the LLM stages are unaffected.
    Fix: key decode by `AudioFormat`, or exclude decode from the CLI's samples.

14. **Minor.** Header paragraph, "one row, Processing, that this plan supersedes".
    D4 shows the card while the meeting is `.queued` as well, so it also supersedes
    the Queued row's tab-body cell and the Processing row's 240 pt header progress bar
    in `.plans/2026-09-28-macos-visual-redesign.md`, "Recording and processing
    states". Two further overlaps: the redesign's step 9 accepts only when
    "`MenuBarViewModelTests` is unchanged", which step 2 here rewrites by replacing
    `observeProgress()`; and `.plans/2026-09-28-onboarding-vault-and-llm.md`, Summary
    tab section, says "Queued and processing keep today's pending texts", which D4
    removes. Fix: list the three touch points in the header paragraph so whichever
    plan lands second knows what to update.

15. **Minor.** Step 2, `PendingText` reduced to the ready branch.
    `TabText.lines` builds the pending copy from `PendingText.text`
    (`apps/macos/Steno/Main/Tabs/TabText.swift:28-30,53-56,69-71`) and
    `TabTextSnapshotTests` pins those lines. Step 2 lists neither. Fix: add `TabText`
    and the snapshot fixture to step 2, with the card's title and sub-line as the
    pending lines.

16. **Minor.** D1 and step 1 tests, event equality.
    `ProcessingProgress` is `Equatable` and carries `estimatedRemaining`, which depends
    on measured stage durations. `Tests/StenoCoreTests/StageTests.swift:70-77` and
    `PipelineIntegrationTests.swift:186-190` compare whole events with `==`. They stay
    deterministic only if the stage timers read the injected `dependencies.now`
    (`Sources/StenoCore/Pipeline/ProcessingPipeline.swift:18`) rather than a
    `ContinuousClock`. Fix: say so in D2, and let the tests compare `stage` and
    `fraction` where the remaining time is not the point.

17. **Minor.** Open questions, "included in the diagnostics bundle".
    No diagnostics bundle exists in any plan or in `apps/macos/Steno`. Fix: drop the
    question, or name the plan that introduces the bundle.

## Verified claims

Everything not listed above was checked and holds.

- F1: `Sources/StenoCore/Pipeline/PipelineStage.swift:15-20` and
  `Sources/StenoCore/Events/MeetingEvent.swift:6-8` read as quoted; the plan's 16-19 is
  the body of a property that spans 15-20; `run` posts once per stage (`ProcessingPipeline.swift:240-245`).
- F2: `apps/macos/Steno/Main/Tabs/SummaryTab.swift:15-23` and
  `MenuBarView.swift:84-109` read as quoted.
- F3: `Stages/DecodeTranscribe.swift:36`, `Stages/Diarize.swift:35`,
  `ParakeetEngine.swift:29-31`; all three engines cache the loaded manager
  (`ParakeetEngine.swift:17,37`, `WhisperKitEngine.swift:52,67`,
  `FluidDiarizer.swift:81,96`). The diarizer numbers match
  `.plans/2026-09-25-speech-and-speakers.md:512`; Parakeet's compile is indeed
  unmeasured because the bake-off runs one untimed pass first (`:473-474`).
- F4: `orderedLanes` is `.mic`, `.system`, `.mixed` (`DecodeTranscribe.swift:54-56`);
  `diarizedLane` prefers `.system` for `.macCall` and `.mixed` otherwise, falling back
  to the last ordered lane (`Diarize.swift:13-17`). Assets carry `[.mic, .system]` or
  `[.mixed]` (`Sources/steno/Commands/Process.swift:98-106`, `.plans/2026-09-25-audio-capture.md:51`),
  so the diarized lane is the last transcribed lane for every source, including a
  single-lane phone recording. The plan's "twice for a call" for decode is three
  today: mic, system, and system again in `diarize`.
- F5: `PipelineBoundaries.swift:20-22`, `LLMTranscriptCleaner.swift:28`, default 2 at
  `LLMEndpoint.swift:45`.
- F6: `.plans/2026-09-25-speech-and-speakers.md:232` for the published 190x and `:509-510`
  for RTFx 63 to 119 and WhisperKit 3 to 10, as quoted.
- F7: `.plans/2026-09-24-initial-scope.md:24`; Jamie's processing is cloud per
  `docs/research/2026-09-24-jamie-and-oss-landscape.md:11`.
- F8: `SettingsStore.swift:4-7`, where a missing property loads as its default without a migration;
  `Wiring.swift:51-52` and `AppEnvironment.swift:192-193` open the same
  `StenoPaths.default().databaseURL`.
- `SettingsStore.update(_:)` as a read-modify-write in one `writer.write` transaction
  is implementable with the existing `rows(for:)` and `settings(from:)` helpers
  (`SettingsStore.swift:34-50`); `save` is `deleteAll` plus inserts (`:20-26`), so the
  app's `updateSettings` (`AppEnvironment.swift:95-100`) can indeed drop the samples
  written between its load and save, as the plan says. `DetectionController` is the
  only `observe()` subscriber (`apps/macos/Steno/Detection/DetectionController.swift:40`)
  and ignores an unchanged flag (`:56`). No `MeetingStore` observation reads the
  `setting` table, so rate writes cause no list or detail churn.
- `ModelStore.isInstalled(_:)` is `nonisolated` and takes a `ModelAsset`
  (`ModelStore.swift:52-54`); `ensureInstalled` downloads when absent (`:111-113`), so
  the guard is needed and sufficient. `AppEnvironment` holds `models` (`:27`) and can
  derive the asset from `SpeechEngineID(settingsValue:)` as `makeDependencies` does.
- `MeetingEventBus` does not replay (`MeetingEventBus.swift:3-4`), so an app-wide
  model subscribed once at launch is the right shape for a detail view opened mid-run.
- `resumeUnfinished` runs the full `process` from `decode` (`ProcessingPipeline.swift:92-107`),
  so the estimator's inputs exist for resumed meetings.
- Design tokens exist as named: `Card` with `Theme.Space.radius` 8, `card` veil and
  hairline `border` (`Components.swift:60-73`, `Theme.swift:122`), `Motion.functional`
  (`Motion.swift:26-28`), `clockText` (`Components.swift:173`), `PipelineStage.label`
  with "Summarising" (`Labels.swift:17-33`), and "Audio stays on this Mac." in the
  redesign plan's Processing row. The plan's "13 `muted`" follows the redesign plan's
  shorthand for `muted-foreground`.
- 230 MB per hour of `AudioBuffer16k` is right: `[Float]` at 16 kHz
  (`Sources/StenoCore/Model/Transcript.swift:52-55`).

## Scope

Nothing widens or narrows `.plans/2026-09-24-initial-scope.md`. The warm-up loads
models during a meeting but transcribes nothing, so the "no live transcript" non-goal
holds; learned rates are settings, not part of the canonical data model adapters
receive; the card is display only.

## Resolution

Folded into `.plans/2026-09-28-processing-progress.md` on 2026-09-28.

1. Adopted. Steps, step 1 is scoped `[core, macos]` and carries `ProcessingProgressModel`, the menu bar view model and `MenuBarViewModelTests`.
2. Adopted. D5 first bullet and step 3: public `ProcessingPipeline.warmUp()`, called by `AppController` on the current pipeline.
3. Adopted. D1 defines `fraction` as `elapsed / (elapsed + expectedRemaining)` with an explicit per-run clamp held in `ProcessingRun`; step 1 tests assert it.
4. Adopted. D1 last paragraph: both operations post over their own stage list and the presenter tracks only `.queued` and `.processing` meetings.
5. Adopted. The time table gains a basis column marking both rows as extrapolated and deriving the diarizer's 45 to 90; step 1 names the manual 60 min measurement before the seeds are committed.
6. Adopted. F5 and D5 third bullet.
7. Adopted. F9, D2 and the table's summarize row rate summarize per thousand tokens.
8. Adopted in part. D2 skips the rate sample when the meeting was not alone in flight for the whole stage. The Queued title is "Waiting to process" rather than "Starting…", per decision 7, which takes the redesign plan's copy so the two plans agree.
9. Adopted with a different mechanism. D5 serializes both `prepare()` calls through one in-flight task on the pipeline actor, per decision 5, instead of a cached load task in each engine; step 3's `warmUpRacingARunLoadsOnce` asserts one load.
10. Adopted. D5 memory sentence reworded; step 3's warm-up test asserts `droppedFrames` is zero for every lane.
11. Adopted. D5 second bullet returns the buffer as a second value that goes out of scope before `matchSpeakers`.
12. Adopted. D3 drives the presenter from a 1 Hz `TimelineView`.
13. Adopted. F8 and D2: decode is not learned and uses a flat seed.
14. Adopted. The header paragraph names the three touch points.
15. Adopted. D4 last paragraph and step 2 list `TabText.lines` and `TabTextSnapshotTests`.
16. Adopted. D2 injects `clock` beside `now` and says tests compare `stage` and `fraction` where remaining time is not the point.
17. Adopted. The diagnostics question is dropped from Open questions.
