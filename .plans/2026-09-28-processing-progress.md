# Steno: processing progress and time to summary

Status: proposal, 2026-09-28. Triggered by first-run feedback, third round: "Jamie has this
cool animation when transcribing, and it only takes 45 s even for multi-hour meetings."

Binding plans: [`.plans/2026-09-24-initial-scope.md`](2026-09-24-initial-scope.md) (scope:
post-meeting processing on the Mac, no live transcript in v1),
[`.plans/2026-09-25-speech-and-speakers.md`](2026-09-25-speech-and-speakers.md) (engines,
measured throughput), [`.plans/2026-09-25-macos-app-and-release.md`](2026-09-25-macos-app-and-release.md)
(app architecture). The sibling
[`.plans/2026-09-28-macos-visual-redesign.md`](2026-09-28-macos-visual-redesign.md) owns the
look of every screen; its "Recording and processing states" table has one row, Processing,
that this plan supersedes (detail header bar, tab-body empty state with a spinner). When the
redesign plan lands, its Processing row points here. The index
[`.plans/2026-09-28-first-run-feedback.md`](2026-09-28-first-run-feedback.md) gains this plan
as a ninth row and orders it after the floating recording indicator.

## Goal

After a recording stops, the owner watches the meeting and sees a spinner and "Summary appears
after processing". Jamie shows a card at the top of the summary: the current step
("Transcribing..."), a bar that moves continuously, and "~1 min remaining". Steno gets the
same card, driven by honest numbers: a fraction weighted by how long each stage really takes
on this Mac, an estimate learned from previous runs, and continuous motion between events. The
plan also takes the cheap seconds out of the run (model compile moved into the recording,
one decode fewer) and states plainly which part of Jamie's 45 s is out of reach for an
on-device recorder and why.

## Findings

| # | Where | What | Consequence |
|---|---|---|---|
| F1 | `Sources/StenoCore/Pipeline/PipelineStage.swift:16-19`, `Sources/StenoCore/Events/MeetingEvent.swift:8` | `progress` is posted once as each of ten stages starts; `PipelineStage.fraction` is the stage's index over ten | The bar sits at 10 % for the whole transcription, jumps to 20 %, sits again. Persist, deliver and retention (milliseconds) weigh as much as transcribe (a minute per hour of audio) |
| F2 | `apps/macos/Steno/Main/Tabs/SummaryTab.swift:15-22`, `TranscriptTab`, `TasksTab` via `PendingText` | The detail view ignores progress events; it shows a small indeterminate spinner and "Summary appears after processing" | The only place with a bar is the menu bar queue (`MenuBarView.swift:85-108`), which the owner does not have open while waiting |
| F3 | `Sources/StenoCore/Pipeline/Stages/DecodeTranscribe.swift:36`, `Stages/Diarize.swift:35` | `speechEngine.prepare()` and `diarizer.prepare()` run inside the pipeline, after the meeting has ended | CoreML compile and model load land in the wait the owner watches. Measured: diarizer first call 2.7 s for 8.9 s of audio, then 0.1 to 0.2 s. Parakeet's first-run compile is not measured yet; step 1 measures it |
| F4 | `DecodeTranscribe.swift:39-49`, `Diarize.swift:33-37` | Lanes are transcribed one after the other (one `AudioBuffer16k` alive by decision); the diarize stage decodes its lane a second time | For a call, the last transcribed lane (`.system`, `orderedLanes`) is the diarized lane (`diarizedLane` prefers `.system`), so the second decode of the same file is avoidable without a second buffer |
| F5 | `Sources/StenoCore/Protocols/PipelineBoundaries.swift:20-22`, `Sources/StenoLLM` cleaner, `LLMEndpoint.maxConcurrentRequests` (default 2) | Cleanup sends the transcript to the LLM in chunks with a concurrency of two and reports nothing until all are back | For a 60 min call against a local model this is the longest stage by far, and the one with the least feedback |
| F6 | `.plans/2026-09-25-speech-and-speakers.md:232,509` | Parakeet v3 measured at RTFx 63 to 119 after warm-up on the fixtures (published 190x on M4 Pro); WhisperKit large-v3 turbo RTFx 3 to 10 | Steno's transcription is in Jamie's league per lane: about 30 to 60 s per hour of audio with Parakeet. Whisper is 10x slower; the estimate must be per engine |
| F7 | `.plans/2026-09-24-initial-scope.md:24` | "Live transcript during the meeting" is not in v1 | Jamie's 45 s for a two-hour meeting is cloud GPU transcription of audio that left the device; the only on-device way to match it is transcribing while recording, so that only the tail remains at stop. Out of scope here; named under Deferred |

### Where a 60 min call spends its time today

Rates from the speech plan's measurements on the development Mac, two lanes (mic and
system), a local LLM at LM Studio. Estimates are marked; step 1 replaces them with numbers
the CLI prints.

| Stage | Cost driver | Expected | Feedback today |
|---|---|---|---|
| decode | file length, twice for a call | seconds per lane (estimate) | "Decoding" |
| prepare (inside decode) | first run after launch | 2.7 s diarizer measured; Parakeet not measured | none |
| transcribe | audio seconds per lane, RTFx 63 to 119 | 30 to 60 s per lane, so 1 to 2 min | "Transcribing" for the whole span, bar at 10 % |
| diarize | audio seconds of one lane, RTFx 45 to 90 | 40 to 80 s | "Finding speakers" |
| matchSpeakers, merge | cluster count | under a second | one flash each |
| cleanup | transcript tokens, chunks, endpoint | minutes on a local model, tens of seconds on a hosted one (estimate) | "Cleaning up", nothing else |
| summarize | one request | 10 to 60 s (estimate) | "Summarising" |
| persist, deliver, retention | rows and files | under a second | three bar jumps of 10 % each |

Two conclusions. Transcription itself is roughly a minute per hour of audio per lane, which
is what the owner compares against Jamie's card, and the bar should say so. The whole run is
longer than Jamie's because Jamie's card covers only cloud ASR and the summary follows later;
Steno's single bar covers everything up to delivery. The card therefore names the stage and
gives a time, so a slow cleanup reads as "Cleaning up, ~3 min" rather than as a stuck bar.

## Decisions

### D1. Progress is one weighted fraction plus an estimate, computed in core

`MeetingEvent.progress` carries a value instead of a bare stage:

```swift
public struct ProcessingProgress: Sendable, Equatable, Hashable, Codable {
  public var stage: PipelineStage
  /// 0...1 over the whole run; never decreases within a run.
  public var fraction: Double
  /// Wall clock the run is expected to still take; nil when no rate is known yet.
  public var estimatedRemaining: Duration?
  /// Wall clock the current stage is expected to take from its start, so a
  /// presenter can move the bar towards the next stage boundary at the right pace.
  public var expectedStageDuration: Duration?
}
case progress(meetingID: UUID, progress: ProcessingProgress)
```

`PipelineStage.fraction` is deleted; nothing else may compute a fraction. The CLI's
`steno process` prints the same value (stage, percent, remaining) one line per event, which
is also how the rates below get measured on the development Mac and on Forge.

### D2. Weights come from learned per-stage rates, seeded with the measured numbers

A new pure type `ProcessingEstimator` (in `Sources/StenoCore/Pipeline/`) takes the
meeting's duration, its lanes, the engine id, the transcript's approximate token count once
known, and a `StageRates` value; it returns the expected seconds per stage, the cumulative
fraction at each stage start and the remaining time from any point. Rates are "seconds of
work per unit of driver": per audio second for decode, transcribe and diarize; per thousand
transcript tokens for cleanup; a flat cost for summarize, persist, deliver, retention,
matchSpeakers and merge.

`StageRates` persist in GRDB in a new table `stage_timing` (`stage`, `engine_id`, `samples`,
`seconds_per_unit`), appended to `Sources/StenoCore/Storage/Migrations.swift`. After each
stage the pipeline records the measured duration and updates the row as an exponential
moving average (alpha 0.3, so the third run already reflects this Mac). Seeds for an
unseen `(stage, engine)` are the constants from the table above, marked as seeds
(`samples = 0`) so the estimate is shown with a softer label on the very first run ("about
a minute" rather than "~1 min"). The first run after launch adds the prepare cost, which is
recorded separately (`stage = "prepare"`, same table) and added when the engine reports it
is not warm.

### D3. Within a stage, real sub-steps where they exist, motion in the presenter

Real sub-steps: transcribe posts once per lane (lane 2 of 2 starts at the lane boundary of
the stage's share); cleanup gains a progress callback so chunk k of m posts a fraction
inside the stage. `TranscriptCleaner.clean(_:)` gets a second parameter
`progress: @Sendable (Int, Int) async -> Void` with a default no-op via a protocol
extension, so `FakeTranscriptCleaner` and the bake-off keep compiling. Diarize, summarize
and the sub-second stages post nothing inside.

Motion between events is the presenter's job and stays out of core: the app's
`ProcessingProgressModel` (one instance per meeting, shared by the detail view, the list
entry and the menu bar row) animates the displayed fraction from the last posted value
towards the next stage boundary over `expectedStageDuration`, using a linear SwiftUI
animation re-targeted on each event. The bar never reaches the boundary before the event
arrives (target is boundary minus one percent) and never moves backwards; when an event
lands earlier than expected the fraction jumps forward with `Motion.functional`. When a
stage runs longer than expected the bar rests at its target and the remaining-time text
switches to "a bit longer than usual" after 1.5x the expected duration, so the owner learns
that something is slow rather than that the app is stuck. No timers: the animation is the
clock. Reduce Motion keeps the interpolation (it conveys progress) and drops only the
`Motion.functional` jump easing.

### D4. The card

While a meeting is `.queued` or `.processing`, every content tab (Summary, Transcript, Tasks)
shows one `ProcessingCard` at the top of its reading column instead of `PendingText` and the
spinner. Scratchpad keeps its editor and shows the card above it.

| Element | Spec |
|---|---|
| Container | `Card` (radius `Theme.Space.radius`, `card` veil, hairline `border`), full reading-column width, padding `md` |
| Title row | stage label from `PipelineStage.label` with an ellipsis ("Transcribing…"), 14 medium `strong`; trailing, the remaining time 13 `muted` mono digits: "~1 min remaining", "less than a minute", "a few seconds", or "about a minute" while the rate is a seed, or "a bit longer than usual" per D3. Queued: title "Queued", trailing "Waiting for the current meeting" |
| Bar | 4 pt linear, radius 2, track `secondary`, fill `strong` (the accent stays achromatic; the redesign plan's brand-hue question applies here too). Indeterminate pulse (1 s ease-in-out, opacity 1 to 0.5, still under Reduce Motion) only while `estimatedRemaining` is nil |
| Sub-line | 12 `faint`: "Audio stays on this Mac." (copy from the redesign plan), followed for a call by "2 lanes" and the meeting duration `clockText` |
| Failure | Not this card. The failed state is the redesign plan's row |
| Ids | `processing-card`, `processing-stage`, `processing-remaining`, `processing-bar` for UI tests |

The detail header keeps the redesign plan's status chip, but the chip reads the stage label
from the same model. The list entry preview and the menu bar queue row read the same
fraction and remaining text, so the three surfaces never disagree. `PendingText`'s pending
branch goes away; its `.ready` branch ("No summary" and friends) stays as the redesign plan
specifies.

### D5. Take the avoidable seconds out of the run

- Warm during the recording. When `RecordingController` starts a recording, `AppEnvironment`
  launches a `Task(priority: .utility)` that calls `speechEngine.prepare()` and
  `diarizer.prepare()`, guarded by `ModelStore` reporting the engine's assets installed (a
  `prepare()` may download, and nothing downloads during a call). Both engines already keep
  their managers loaded after the first call, so the pipeline's own `prepare()` becomes a
  no-op. Failures are logged and swallowed; the pipeline's `prepare()` reports them as today.
  Memory: the models sit resident during the recording, which the second and every later
  meeting of a session already do.
- Hand the last transcribed buffer to the diarize stage when its lane is the diarized lane
  (F4). `decodeAndTranscribe` returns the last lane's buffer alongside the segments; `diarize`
  takes an optional buffer and decodes only when it is nil or another lane. The one-buffer
  invariant holds: the buffer that would be alive anyway is reused instead of decoded again.
- Measure before touching cleanup concurrency. `maxConcurrentRequests` is a setting; the CLI
  timings from D1 tell whether hosted endpoints should default higher. No change in this plan.

### Deferred, with the reason

- Transcribing while recording (the only path to Jamie's 45 s on-device): the scope excludes a
  live transcript in v1, and an incremental Parakeet run during a call competes with the
  capture for the Neural Engine and battery. A separate plan, if at all, after v1 ships.
- Transcribing both lanes concurrently: two hour-long buffers (about 230 MB each) alive at
  once against the one-buffer decision; the gain is bounded by the shared ANE anyway.
- Running diarize concurrently with the second lane's transcription: same ANE contention;
  measure with the D1 timings first.

## Steps

Each step is one PR with tests; Conventional Commits scopes in brackets.

1. **Weighted progress and the estimator** [`core`]. `ProcessingProgress`, `ProcessingEstimator`,
   `StageRates` with the `stage_timing` migration and store methods, the pipeline recording
   stage durations and posting weighted fractions (per lane in transcribe), `PipelineStage.fraction`
   removed, `steno process` printing progress lines. Tests: estimator is pure (seeded rates
   reproduce the table; EMA converges; fraction monotonic; remaining decreases); pipeline
   integration test on the fake engines asserts the posted sequence and that every fraction is
   within `[0, 1]` and non-decreasing; migration test appends without touching earlier rows.
   Acceptance: on the second run of the same synthetic asset the remaining-time estimate at
   `transcribe` start is within 30 % of the measured remainder.
2. **Cleanup chunk progress** [`core`, `llm`]. The `TranscriptCleaner` callback, the LLM cleaner
   calling it per chunk, the stage posting inside-stage fractions. Tests: fake cleaner drives
   `k of m` and the posted fractions land inside the cleanup share.
3. **The card and the shared model** [`macos`]. `ProcessingProgressModel`, `ProcessingCard`,
   the three tabs and the Scratchpad, the list entry preview and the menu bar row reading it,
   `PendingText` reduced to the ready branch, remaining-time wording as a pure function with
   unit tests (thresholds, seed wording, "a bit longer than usual"). UI test: with the fake
   pipeline paused inside `transcribe`, the card shows "Transcribing…", the bar's
   accessibility value is between the stage's start and end fraction and rises between two
   samples; on `.ready` the card is gone and the summary is present.
4. **Warm-up and buffer hand-over** [`macos`, `core`]. Prepare on recording start behind the
   installed-assets guard; the diarize stage reusing the buffer. Tests: `FakeSpeechEngine`
   counts `prepare()` (once per recording start, zero when assets are absent, the pipeline's
   call a no-op afterwards); a decoder spy asserts one decode per lane for a call.
5. **Plans and index** [`plans`]. This file committed with step 1; when the redesign plan lands,
   its Processing row and the first-run index are updated to point here (a one-line change in
   each, in that PR).

## Open questions

- Brand hue for the fill: decided together with the redesign and the icon plan, not here.
- Whether the stage and remaining time also appear in the sidebar entry while the window is in the
  background (the menu bar row already covers the "am I done yet" glance). Default: yes,
  preview line only.
- Whether `stage_timing` rows should be exported in the diagnostics bundle. Default: yes, they
  contain no content.
