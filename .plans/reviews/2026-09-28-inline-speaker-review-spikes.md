# Inline speaker assignment: spikes and feasibility review

Reviewed 2026-09-28 on branch `t3code/rework-inline-speaker-selects` against
`.plans/2026-09-28-inline-speaker-assignment.md`, `CLAUDE.md`,
`Sources/StenoCore` (`Storage/`, `Pipeline/`, `Model/`, `Testing/`),
`Sources/StenoSpeech/Speakers/CosineSpeakerMemory.swift`, `apps/macos/Steno`,
`apps/macos/StenoUITests/LaunchSmokeTests.swift` and
`.github/workflows/swift-ci.yml`. Math was worked by hand and in a script;
API claims were checked against SQLite, GRDB and Apple documentation, cited
per item. Findings are ordered most severe first; each names its spike (what,
how long, pass/fail) and the fallback. Counts: 1 blocker, 5 major, 5 minor.

## Findings

1. **Blocker.** Core change 1 (lines 189 to 196), step 1 test "withdraw is the
   exact inverse of enrol under the cap ... within 1e-5" (lines 255 to 257).
   `e' = normalise(e * w − x̂)` with `w = min(sampleCount, maxSamples)` is not
   the inverse of `CosineSpeakerMemory.enroll` (`CosineSpeakerMemory.swift:42-53`).
   Enrol computes `e₂ = (n e₁ + x̂) / c` with `c = ‖n e₁ + x̂‖`; the plan
   scales by `w = n + 1` instead of `c`, and `c < n + 1` unless `e₁ = x̂`.
   Worked example with `SampleData.embedding(axis:)` (unit axes 0 and 1, the
   very vectors the test would use): enrol `x₁`, enrol `x₂` gives
   `e₂ = (0.7071, 0.7071)`; the plan's withdraw of `x₂` gives
   `normalise((1.4142, 0.4142)) = (0.9597, 0.2811)`, max error 0.28 against
   `x₁`. For two realistic samples with cosine 0.27 the error is 3.4e-2. The
   1e-5 test fails on the first run. The exact inverse recovers `c` from the
   fact that `‖c e₂ − x̂‖ = n`: with `d = e₂ · x̂` and `n = min(sampleCount − 1,
   maxSamples)`, `c = d + sqrt(d² + n² − 1)` and `e₁ = normalise(c e₂ − x̂)`
   (error 2e-16 in the same example). Two further limits of the claim "exact
   while the person is under the cap": the running mean renormalises after
   every step, so it is order dependent and only the most recent enrolment
   can be withdrawn exactly (withdrawing the middle of three samples with the
   exact formula is off by 4e-2 from enrolling the other two); and
   `InMemorySpeakerMemory.enroll` (`InMemorySpeakerMemory.swift:34-45`) mixes
   the raw, unnormalised sample and caps `sampleCount`, so it is not the same
   function as `CosineSpeakerMemory.enroll` and a shared withdraw test cannot
   pass against both. Spike (half a day, before step 1): a Swift Testing
   case in `Tests/StenoSpeechTests` that enrols two random unit vectors,
   withdraws the second with the corrected formula in `Float`, and asserts
   max abs error `< 1e-5` and cosine `> 1 − 1e-6`; run it in the
   `steno-swift:6.1` container. Pass: both bounds hold for 100 seeds. If it
   fails on `Float` rounding, loosen the tolerance to 1e-4 and keep the
   cosine bound. Plan change regardless: replace the formula, reword the
   protocol doc to "exact for the most recent enrolment under the cap,
   approximate otherwise", and align `InMemorySpeakerMemory.enroll` with the
   Cosine one (normalised sample, uncapped count) in the same PR.

2. **Major.** Core changes 3 and 4 (lines 201 to 213), Acceptance 3 (lines 305
   to 306). Withdraw needs the vector that was enrolled, and the plan uses
   `speaker.embedding`. `mergeSpeakers` (`MeetingStore+People.swift:96-103`)
   replaces the kept speaker's embedding with `weightedMean(kept, 1, source, 1)`,
   so after any merge the kept speaker's stored vector was never enrolled into
   anyone; a later reassignment withdraws the wrong vector and the "sample
   count where it was" acceptance is met while the embedding is not restored.
   Second gap in Core change 4: when the kept speaker is `.unknown` it takes
   the source's assignment (`MeetingStore+People.swift:104`), so the kept
   speaker becomes `.confirmed(P)` while the plan says its `enrolledPersonID`
   is "unchanged" (nil). The source's enrolment into P is neither withdrawn
   (same person) nor carried over, so a later reassignment of the kept speaker
   finds no enrolment to withdraw and P keeps a ghost sample. Spike (one hour,
   as tests in step 1): merge an unknown target with a confirmed source, then
   confirm the target to another person, and assert P is at `sampleCount 0`;
   merge two confirmed speakers of different persons, reassign the kept one,
   and compare P's embedding to its pre-enrol value. Plan change if either
   fails (they will as written): in v3 add `enrolledEmbedding BLOB NULL` to
   `speaker` (the vector at enrol time), withdraw that, and in `mergeSpeakers`
   move `enrolledPersonID` and `enrolledEmbedding` from source to target when
   the target took the source's assignment. Alternative with no new column:
   `mergeSpeakers` leaves `kept.embedding` untouched when `kept` is confirmed.

3. **Major.** Design "SpeakerPicker" (lines 139 to 160), "Popover" (lines 123
   to 137), "Transcript tab" (lines 162 to 167). The whole interaction model
   (arrow keys move a highlight while a `TextField` keeps focus, Return
   commits, Tab moves to the next row's field, Escape closes only the list, a
   second `.popover` anchored to a name inside a `LazyVStack` in a
   `ScrollView`) is unverified: `apps/macos/Steno` contains no `.popover`, no
   `onKeyPress`, no `@FocusState` today (grep), so nothing in the codebase
   proves the pattern on macOS 15. Known risks: `onKeyPress` only fires on the
   focused view and a `.handled` ancestor swallows it
   (https://www.avanderlee.com/swiftui/key-press-events-detection/); an
   NSPopover is its own window, so focus can stay in the parent window and
   Escape reaches the popover's `cancelOperation:` before the field, closing
   the whole `SpeakersPopover` instead of the inline list; SwiftUI popovers
   inside a `LazyVStack` lose their anchor when the row is recycled, and a
   per-row `isPresented` binding multiplies that
   (https://serialcoder.dev/text-tutorials/swiftui/presenting-popovers-in-swiftui/).
   Spike (one day, before step 2, in a throwaway view under
   `apps/macos/Steno/Speakers/` behind `-steno-ui-testing`): a popover with
   two rows of `TextField` + list. Pass, all of: Down/Up change the highlight
   without moving the insertion point; Return commits the highlighted row (not
   `onSubmit` of the field alone); Tab lands in the second field; Escape with
   the list open closes the list and leaves the popover; a nested `.popover`
   from row 40 of a 200-row `LazyVStack` opens anchored and survives a scroll
   of 100 pt; all with `NSApp.isFullKeyboardAccessEnabled` off. Fallbacks:
   keyboard through an `NSViewRepresentable` first responder or a local
   `NSEvent` monitor scoped to the popover; a single `presentedSpeakerID`
   state on `TranscriptTab` instead of per-row bindings; if nested popovers
   fail, the transcript trigger opens the header `SpeakersPopover` scrolled to
   that row (the plan's "one select, three places" then becomes two).

4. **Major.** Decision 10 (lines 90 to 92), App changes `Debounce` (lines 236
   to 237), Acceptance 6. `pipeline.redeliver` runs through `exclusively`
   (`ProcessingPipeline.swift:197-217`) and throws "already being processed"
   when the meeting is in flight, which is exactly when a user is likely to be
   naming speakers: `rerunSummary` runs a minute or more, and the user can name
   a speaker while it runs. Today's `finish()` has the same hole but only on
   Done; a 3 s debounce hits it routinely and the change never reaches the
   vault (the rerun's own `deliver` may have read the export before the
   confirm). Also `onDisappear` is synchronous; a `flushRedeliver` started
   with `[weak self]` dies with the view model. Spike (two hours, unit test in
   step 2): `PipelineHarness` with an LLM fake that never returns, start
   `rerunSummary`, `assign`, advance `ManualClock` past the debounce, release
   the LLM, assert one `deliverAll` after the run. Pass: the export happens
   once, no `error`. Plan change: `Debounce` re-arms on a `PipelineFailure`
   whose stage is `.deliver` and meeting is in flight (or `ProcessingPipeline`
   gains `isInFlight(_:)`), and `flushRedeliver` runs in a detached `Task`
   holding `pipeline` and `meetingID`, not `self`.

5. **Major.** Design "Playback" (lines 179 to 185), Decision 6 (lines 74 to
   77), Acceptance 5, step 3 `[manual]`. Three claims need one spike.
   (a) "`mixdownURL` when present, else `url`": `RetentionSweep` deletes the
   files and keeps every URL on the row (`RetentionSweep.swift:3-5, 45-47`),
   so "present" must be a `fileExists` check on each observation tick, not a
   nil check; otherwise Play shows and fails after the sweep. (b) The mixdown
   exists only for non-AAC masters (`Persist.swift:15-20`), so phone
   recordings fall to `url`, which is the `.m4a` itself (fine), and CAF
   masters fall to the two-lane Float32 file when the mixdown failed or an
   older meeting has none: mic on the left, system on the right, both audible.
   (c) `AVAudioPlayer.currentTime` seeking and playback on a 48 kHz Float32
   two-channel CAF and on AAC are Core Audio formats and documented as
   seekable (https://developer.apple.com/documentation/avfaudio/avaudioplayer/currenttime),
   but the app has only ever played 16 kHz WAV clips (`ClipPlayer.swift:4-5`),
   and the stop scheduled on the injected clock is untested against real
   playback. Spike (two hours, before step 3): write a 6 s 48 kHz Float32
   stereo CAF with `AVAudioFile`, play `2...5` through the new
   `play(_:range:)` with `ContinuousClock`, and an `.m4a` the same way. Pass:
   audio starts within 50 ms of second 2 and stops within 100 ms of second 5
   on both files; `play` returns true. Fallback: render the range to a temp
   16 kHz WAV through `dependencies.decoder` (the pipeline's existing clip
   writer) and play that with the unchanged `ClipPlayer.play(_:)`.

6. **Major.** Steps 2 and 3 Done criteria (lines 280 to 286), Header row
   auto-open (lines 118 to 121). The seeded meeting is right for the
   screenshot: `SampleData.speakers()` has Speaker 1 `.confirmed(Nicolai)` and
   Speaker 2 `.suggested(Jérôme)` with segments for both, so "a named and an
   unnamed row" exists. But `PreviewSeed.seed` (`AppEnvironment.swift:351-359`)
   saves rows directly and never posts `speakersNeedReview`, so
   `controller.pendingReviews` is empty in `-steno-ui-testing` and the
   auto-open path is unreachable from `LaunchSmokeTests`; the test must click
   `speakers-row`. The popover is a separate window, so
   `app.buttons["speaker-row-…"]` must be queried through `app.popovers` or
   `app.descendants(matching:)`, and the screenshot must be
   `XCUIScreen.main.screenshot()` (a window screenshot misses the popover).
   No clip file or audio exists under `/tmp/steno/...` on the runner, so Play
   is hidden in the screenshot; Acceptance 5 stays manual. Spike: none
   needed, amend the plan: step 2 "clicks `speakers-row`", step 3 names the
   query and screenshot API, and `AppControllerTests` (not the UI test) covers
   the auto-open trigger. Note also `ui-smoke` is pinned to hosted `macos-15`
   (`swift-ci.yml:309`), not `vars.MACOS_RUNS_ON` as Verification says, because
   Xcode 27 on Forge aborts XCUITest launches (`swift-ci.yml:271-274`).

7. **Minor.** Core change 2 (lines 197 to 200). Verified feasible: SQLite
   allows `ALTER TABLE ADD COLUMN ... REFERENCES` when foreign keys are on
   provided the default is NULL (https://www.sqlite.org/lang_altertable.html,
   "If foreign key constraints are enabled and a column with a REFERENCES
   clause is added, the column must have a default value of NULL"), and GRDB's
   migrator disables foreign keys for the migration and runs a full check
   before commit (GRDB `Migrations.md`, "each migration temporarily disables
   foreign keys, and performs a full check"). The backfill cannot violate the
   check because `personID` already has `ON DELETE SET NULL`. What the plan
   must add: `Migrations.identifiers` gains `"v3"`, `SchemaSnapshotTests`
   needs `Tests/Fixtures/snapshots/schema/v3.sql` (generated with
   `STENO_UPDATE_SNAPSHOTS=1`), and the backfill's `assignment = 'confirmed'`
   must use `SpeakerAssignment.Kind.confirmed.rawValue`
   (`Records.swift:202-225`), not a literal.

8. **Minor.** Decision 3 (lines 63 to 65), Acceptance 2. Verified: the
   observation re-emits. `exportRows` (`MeetingStore+Export.swift:15-70`)
   reads `speaker`, `transcriptSegment` and `person` in the tracked closure,
   so `confirm` (speaker update, person insert) and `mergeSpeakers` (segment
   update, speaker delete) both trigger `observeMeeting(id:)`; the `person`
   filter is on a TEXT key, so the region is the whole table and every open
   detail re-emits on any person save (harmless). Two emissions per confirm
   (the store write, then `memory.enroll`'s `store.save(person)`), and the
   `SpeakersViewModel.update(export:)` must be idempotent under that. No
   spike; add one `SpeakersViewModelTests` case on `MeetingStore.inMemory()`
   asserting `rows` update after `assign` without a manual reload.

9. **Minor.** Core change 3 (lines 204 to 207), orphan rule. `confirm` is
   one store transaction plus up to two `memory` saves (withdraw, enrol) and
   the orphan delete, which depends on the withdraw's `sampleCount`. The
   orphan check must run after the withdraw, in its own write, and its "no
   `speaker` row references them" test must include the new
   `enrolledPersonID` column, or `ON DELETE SET NULL` silently forgets other
   speakers' enrolments. A crash between transactions leaves
   `enrolledPersonID = B` with no enrolment; acceptable for v1, say so in the
   protocol doc. No spike; a step 1 test that the orphan is kept while
   another meeting's speaker still has `enrolledPersonID == A`.

10. **Minor.** Design "Avatars" (lines 169 to 177). `Theme.avatarHues[Int(id.uuid.0) % 8]`
    gives every `SampleData` person (all ids start with byte 0) the same
    hue, so seeded screenshots and `ThemeTokensTests` never see two colours;
    hash two bytes or use `id.hashValue` seeded. The contrast claim is the
    risky part: white initials at 4.5:1 need a background of L* below about
    50, which is not "muted" in light mode. Spike (one hour): compute WCAG
    contrast for the eight proposed light values before writing the tokens.
    Fallback: dark initials on 20 % hue veils in light mode, white on solid
    hues in dark mode, both meeting 4.5:1.

11. **Minor.** Popover (line 126) uses `Motion.functional`; `Motion.swift`
    has `durationFunctional`. `IconButton` and the `popover` fill token exist
    (`Theme.swift:62`). The plan's dependency on the redesign's primitives is
    stated (Non-goals, last bullet); name the token as it exists or say the
    redesign renames it.

## Spike order

Findings 1 and 2 before step 1 (they change the migration and the protocol);
finding 3 before step 2 (it can remove the transcript trigger); findings 4
and 5 during steps 2 and 3; findings 6 to 11 are plan text or tests, no spike.
