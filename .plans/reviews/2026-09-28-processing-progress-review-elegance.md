# Steno processing progress plan: architecture and elegance review

Date: 2026-09-28. Reviewer: architecture and elegance pass over
`.plans/2026-09-28-processing-progress.md` on branch
`docs/plan-processing-progress`, read against the code it changes:
`Sources/StenoCore/Pipeline/`, `Sources/StenoCore/Events/MeetingEvent.swift`,
`Sources/StenoCore/Storage/SettingsStore.swift`, `Sources/steno/Commands/Process.swift`,
`apps/macos/Steno/AppController.swift`, `apps/macos/Steno/MenuBar/MenuBarViewModel.swift`,
the detail and list view models, the tabs and `apps/macos/Steno/Design/`. The sibling
`.plans/2026-09-28-macos-visual-redesign.md` was read for the states table this plan
supersedes. Correctness of the rate arithmetic and test coverage are reviewed elsewhere.
Nothing below edits the plan; each finding proposes a change for the plan to adopt first.

**major**: expensive once code compiles against it. **minor**: one PR.

## Findings

1. **major** — D2 "`StageRates` is a new property of `Settings`", F8, step 1
   (`SettingsStore.update(_:)`). `Settings` is documented as "the one settings type",
   every property of it is a choice the owner made, and its one observer today reacts to
   a settings change. Learned rates are measurements the pipeline writes ten times per
   run. Putting them on `Settings` makes the pipeline a settings writer, makes every
   `settings.observe()` subscriber fire ten times per run, needs a new
   read-modify-write API whose only reason to exist is this second writer, leaves the
   app's `updateSettings` load-mutate-save able to drop a sample, rewrites every setting
   row on each sample because `SettingsStore.save` deletes and reinserts the table, and
   puts telemetry into whatever exports or resets `Settings` later. The plan spends a
   paragraph on these consequences instead of avoiding them. Give the rates their own
   rows through the store the pipeline already holds: one append-only migration in
   `Sources/StenoCore/Storage/Migrations.swift` for a `stageRate` table keyed by stage
   and rate key with `seconds_per_unit`, `samples` and `updated_at`, and a
   `Sources/StenoCore/Storage/MeetingStore+Timings.swift` in the pattern of
   `MeetingStore+People.swift` with `stageRates() async throws -> StageRates` and
   `record(_ sample: StageSample) async throws` that applies the EMA inside one write.
   `StageRates`, `ProcessingEstimator` and the CLI-and-app sharing through one
   `steno.sqlite` stay exactly as designed; `SettingsStore.update(_:)`, the observer
   caveat and the dropped-sample caveat go. If the open question about the diagnostics
   bundle matters, the same file can instead log one `processing_run` row per run with
   the per-stage seconds and derive the rates by query, which also answers "why did it
   say one minute" after the fact.

2. **major** — D1 `ProcessingProgress` and the sentence "the fraction is expected elapsed
   time over expected total time". The value is nearly right; three semantics need to be
   fixed before code compiles against it. First, expected elapsed over expected total
   means every step's fraction is fixed when the run starts, so a transcription that runs
   twice as long as planned changes nothing in the number the bar shows next, which is
   not the "honest numbers" the Goal promises. Post `fraction = elapsed / (elapsed +
   expectedRemaining)` with the real elapsed time and the expected seconds of the steps
   not yet started; this is non-decreasing whenever the remaining plan only shrinks, and
   the pace identity the presenter relies on still holds as `1 / (elapsed +
   expectedRemaining)`. Second, the token count is re-estimated when cleanup starts, so
   the remaining plan can grow mid-run and the "never decreases" comment needs an explicit
   clamp against the previous posted fraction in the run state, which the plan's own
   monotonic test then exercises. Third, D2 and D4 want a softer label "while the rate is
   a seed", but nothing on the value says so; add `isEstimateSeeded: Bool`. Keep
   `nextFraction`: a self-contained event is what lets a late subscriber show a correct
   bar, and the app subscribes late whenever it resumes unfinished meetings. Move the
   arithmetic the presenter would otherwise do onto the value as computed members in
   core, `expectedTimeToNextEvent: Duration` and `pace`, so the CLI, the menu bar, the
   list and the card divide nothing themselves and D1's "nothing else may compute a
   fraction" extends to the pace.

3. **major** — D3 "No timers: the animation is the clock" and the "linear SwiftUI
   animation re-targeted on each event", step 2 UI test. The mechanism and its test
   contradict each other, and the copy rules need a clock anyway. A SwiftUI animation
   interpolates the presentation value only; the model value jumps to the target at once,
   so "the bar's accessibility value rises between two samples" cannot pass, and the
   remaining-time text cannot count down nor switch to "a bit longer than usual" after
   1.5x without something observing time. The app already has the pattern: the menu bar's
   elapsed time is a `TimelineView(.periodic(from:by:))` at 1 s in
   `apps/macos/Steno/MenuBar/MenuBarView.swift`. Make the displayed state a pure function
   of the last event and now: `ProgressPresentation.make(entry:, now:, reduceMotion:)`
   returning the displayed fraction, the remaining wording and the slow flag, unit tested
   on fixed dates, which is the "pure function with unit tests" step 2 already asks for
   but only for the wording. The card wraps in a 1 Hz `TimelineView`, sets the bar to the
   presentation fraction and tweens the width over `Motion.countdown`, the linear 1 s
   clock tempo the floating indicator plan adds, so the steps read as continuous motion;
   Reduce Motion drops the tween and keeps the steps. The accessibility value is the
   presentation fraction and the UI test holds. Delete the re-targeting paragraph and the
   "boundary minus one percent" rule, which the presentation function expresses as a
   clamp.

4. **minor** — D1 and step 1, the run-scoped state. The plan names the value, the
   estimator and the rates but not the thing that owns a run's plan, its start time and
   its step timings, which is most of the new code in step 1. Today `run(_:meetingID:_:)`
   in `Sources/StenoCore/Pipeline/ProcessingPipeline.swift` posts a bare stage; the actor
   already keys per-meeting state in `running` and `inFlight`. Name it: a `ProcessingRun`
   value in `runs: [UUID: ProcessingRun]` beside `inFlight`, created in `process` after
   the rates load, updated and posted by `run(_:meetingID:_:)`, removed when
   `exclusively` ends. Two implementers would otherwise either thread it through every
   stage signature or store it on the actor, and only one of those keeps the stage files
   unchanged. Say what `rerunSummary` and `redeliver` post: they go through
   `run(.summarize)` and `post(.deliver)` today, and a whole-run fraction is meaningless
   for them. Simplest is that they post no `progress` at all; the detail view already
   shows `isBusy` for both. Also state the first event's timing: post the first
   `progress` after both `prepare()` calls return rather than before `speechEngine.prepare()`
   as `DecodeTranscribe.swift:36` does now, so the decode timer and the bar's first event
   start together and the queued pulse covers a cold compile.

5. **minor** — D3 "One app-wide `ProcessingProgressModel`", D4, step 2. The plan makes a
   second long-lived bus subscriber when `AppController.launch()` already holds the one
   subscription with `case .progress: break`, and it does not say where the model lives or
   how a detail view for meeting X reaches its slice. Fit the existing shape: `let
   progress: ProcessingProgressModel` on `AppController` beside `recorder`, `menuBar` and
   `detection`, fed from the existing switch, evicting on `.retentionApplied` and
   `.deleted` so a finished run does not linger; `MenuBarViewModel.observeProgress()`,
   its `stages` map and `QueueItem.stage` and `.fraction` go, and the queue row reads
   `controller.progress.entry(for:)`. `MeetingDetailView` already receives `controller`;
   `MeetingListView` receives only its view model, so `MainWindow` passes the model
   through, or `MeetingRow` takes the entry. `TabText.lines` is pure over the export and
   pins the tabs' words in the snapshot test; give it a `progress:` parameter so the
   card's title and remaining text are pinned with the rest. Finally, `launch()` appends
   the observers after `resumeUnfinishedProcessing()`, so the first events of resumed
   runs are missed today; subscribe first, or note that self-contained events repair it
   on the next post.

6. **minor** — D4 Bar row "Indeterminate pulse (1 s ease-in-out, opacity 1 to 0.5)",
   Container row, and D3 "linear SwiftUI animation". The floating indicator plan adds
   `Motion.pulse` at 1 s ease-in-out, opacity 1 to 0.4, and `Motion.countdown`, the
   linear 1 s tempo; the redesign restyles `Card` to radius 12 on a `raised` surface.
   The card spec restates a near-duplicate pulse with a different endpoint, an untokened
   linear animation and today's `Card` tokens. Reference `Motion.pulse` and
   `Motion.countdown` by name and say "`Card` as the redesign plan restyles it". The
   redesign's Processing row also draws a 240 pt `ProgressView` under the header meta
   line; with the card in the reading column that makes two bars in one pane. Say the
   header bar goes when the redesign's Processing row points here, so the chip is the only
   header signal.

7. **minor** — Binding plans paragraph "one row, Processing", D4 first paragraph and the
   Title row's Queued cell, Sub-line. D4 shows the card for `.queued` as well, with its
   own Queued title and trailing text, so the plan supersedes two rows of the states
   table, each in the preview and tab-body columns, and the redesign's Queued copy
   "Waiting to process" and "Processing starts when the current meeting finishes."
   competes with this plan's "Waiting for the current meeting". Pick one Queued wording
   and name both rows. "2 lanes" in the sub-line is an internal word; `Labels.swift`
   translates lanes to "Mic", "System" and "Room" and never shows the noun. Use
   `MeetingSource.label` or "Mic and system audio". The title takes `PipelineStage.label`
   plus an ellipsis while the chip, the list preview and the menu bar row show the same
   label without one; either put the ellipsis in one place on the model's entry or drop
   it, so "the same words" holds. `PendingText` is restructured by both plans: the
   redesign makes it the states-table picker, this plan removes its pending branch. Say
   that the tabs branch on the model's entry before `PendingText` is consulted, so
   `PendingText` stays the redesign's and this plan touches only the two rows.

8. **minor** — Steps 2 and 3, D5 first bullet. Step 3 joins a `macos` change and a
   `core` stage refactor that share nothing; the buffer hand-over touches the same two
   stage files step 1 rewrites and belongs there or in its own core PR. Step 2 depends on
   the redesign's step 7a for `EmptyState`, the entry preview line and the header, and
   should say so, and it is large enough to split into the model plus menu bar migration
   and the card plus tabs plus list. The warm-up is under-specified where it matters:
   `ProcessingPipeline.dependencies` is internal, the engines are built per
   `makeDependencies` call, and F3's "keeps the loaded manager" only helps when the
   pipeline's own instances are warmed. Add a public `ProcessingPipeline.warmUp() async`
   that calls both `prepare()`s; the app guards with `models.isInstalled` for the
   engine's `ModelAsset` and the diarizer's before calling
   `environment.pipeline.warmUp()`, wired from `AppController`'s existing
   `recorder.recordingDidChange` hook rather than a new call from `RecordingController`
   into `AppEnvironment`. Note that `reloadPipeline()` during a recording yields a cold
   replacement, and that `FakeSpeechEngine.preparations` already counts `prepare()`.

9. **minor** — D1 "`steno process` prints stage, percent and remaining, one line per
   event". `Wiring.dependencies` creates `events: MeetingEventBus()` inline and
   `Process.run` never sees it, so the command has nothing to subscribe to. Add an
   `events:` parameter to `Wiring.dependencies`, create the bus in `Process`, subscribe
   before `enqueue` and print until `waitUntilIdle` returns; the same seam serves the
   step 1 integration test.

## Keep

- The fraction and the estimate computed in core and carried on every event. One
  arithmetic for the CLI, the menu bar, the list and the card, and a late subscriber
  gets a correct bar from the next event.
- The estimator as a pure type with seeded rates keyed by engine and model, and the
  measured numbers in the plan replacing guesses.
- The card replacing the spinner in every content tab while the Scratchpad stays
  editable, and failure staying the redesign's row.
- Measuring cleanup before touching its concurrency, and the deferred list with reasons,
  in particular naming live transcription as the only route to Jamie's 45 s and leaving
  it out of v1.
- The buffer hand-over to diarize for a call, which removes a decode without a second
  buffer.

## Wording, not findings

House style asks for full sentences, no em-dashes, no parentheticals for asides and
repo-relative paths. No em-dashes were found. Parenthetical asides: the Binding plans
paragraph has three; the "Where a 60 min call spends its time" table uses "(estimate)"
in three cells; D2 last sentence; D3 "(target is boundary minus one percent)" and "(it
conveys progress)"; D4 Sub-line "(copy from the redesign plan)"; D5 "(F3)", "(F4)";
Deferred first and third bullets; step 1 and step 3 test descriptions; Open questions
second bullet. Short file names without their repo path appear in F2 (`TranscriptTab`,
`MenuBarView.swift:85-108`), F4 (`DecodeTranscribe.swift:39-49`, `Diarize.swift:33-37`)
and D5. The Binding plans paragraph says the first-run index "gains this plan as a ninth
row"; the index already has nine rows, so this is the tenth.

## Resolution

Folded into `.plans/2026-09-28-processing-progress.md` on 2026-09-28.

1. Adopted. D2 gives the rates a `stageRate` table through one append-only migration and `Sources/StenoCore/Storage/MeetingStore+Timings.swift`; `SettingsStore.update(_:)` and both caveats are gone. The `processing_run` log alternative is not taken because the diagnostics question it served is dropped.
2. Adopted. D1 posts `elapsed / (elapsed + expectedRemaining)` with a per-run clamp, adds `isEstimateSeeded` and the computed `expectedTimeToNextEvent`, and keeps `nextFraction`.
3. Adopted. D3 makes the displayed state a pure function sampled from a 1 Hz `TimelineView`, tweened with `Motion.countdown`; the re-targeting paragraph is gone.
4. Adopted in part. D1 names `ProcessingRun` in `runs: [UUID: ProcessingRun]` and D2 posts the first event after both `prepare()` calls return. `rerunSummary` and `redeliver` keep posting `progress`, per decision 3, and the presenter ignores them because it tracks only `.queued` and `.processing` meetings.
5. Adopted. D3 puts the model on `AppController` as `progress`, fed from the existing switch, created before `resumeUnfinishedProcessing()`, with `MainWindow` passing it through and `TabText.lines` taking `progress:`.
6. Adopted. D4 references `Card` as the redesign restyles it, `Motion.pulse` and `Motion.countdown`, and removes the redesign's 240 pt header bar.
7. Adopted. The header paragraph names both rows and columns; D4 takes the redesign's "Waiting to process", uses `MeetingSource.label`, puts the ellipsis on the model's `title` for every surface, and leaves `PendingText` as the redesign's picker.
8. Adopted in part. D5 adds `ProcessingPipeline.warmUp()` wired from `recorder.recordingDidChange` and notes the cold replacement after `reloadPipeline()`; the Steps intro states the dependency on the redesign's step 7a. Step 3 keeps warm-up and hand-over together and step 2 is not split, per decision 8.
9. Adopted. D1 last paragraph: `Wiring.dependencies` takes `events:` and `Process` subscribes before `enqueue`.

Wording: every parenthetical aside, short path and the "ninth row" slip listed above is fixed.
