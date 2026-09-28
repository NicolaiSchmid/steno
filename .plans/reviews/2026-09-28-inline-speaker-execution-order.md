# Inline speaker assignment: execution order

2026-09-28. Decisions, spikes and PRs for `.plans/2026-09-28-inline-speaker-assignment.md`. Reviews cited as Simp,
Spk, Conf, Ref, Test, Eleg plus finding number.

## 1. Decisions to take before coding

1. **Voice model: recompute, no `withdraw`, no `enrolledPersonID`.** A person's voice is `normalise(mean(embedding of
   their confirmed speakers, newest 50 by meeting.startedAt))`, `sampleCount = count`, written by one private
   `MeetingStore.refreshVoice(personID, db)` inside the transaction of `confirm`, `mergeSpeakers` (both persons) and
   `mergePersons`. `enroll` leaves `SpeakerMemory`; `confirm` drops `memory:`. Why: the previous person is
   `assignment` (Simp 1, Eleg 5), the inverse formula is wrong (Spk 1) and merges make any withdraw inexact (Spk 2,
   Test 6); recompute keeps Acceptance 3 exact at any count and removes migration v3 (Conf 8, Test 2, 13, Ref 1-3,
   13). `.suggested` and embedding-less ("Me") speakers never count, closing the Test 5 trap. Cost, stated in the
   speech plan: a 50-sample window replaces the capped running mean; samples of deleted meetings drop out at the next
   refresh.
2. **No orphan-person deletion.** `recentPersons()` returns persons owning a confirmed speaker (inner join, latest
   `startedAt` desc, then `displayName COLLATE NOCASE`); a typed query filters all `persons()`, so `name(_:_:)` reuses
   a ghost. Settles Simp 3, Eleg 7, Spk 9, Test 7; avoids Conf 3's permanent stale vault line.
3. **Clips: the scope wins.** The sweep removes clips of `.confirmed` speakers only, with the audio; unconfirmed
   speakers keep theirs. `confirm` deletes the clip only when the meeting's master file is already gone. Settles Conf
   1 (blocker), Conf 2; `sampleClipURL` stays in `CodingKeys`, Decision 6 notes the path in vault JSON (Conf 12, Ref
   13).
4. **No range playback.** Play iff the clip file exists; `ClipPlayer` unchanged (Simp 7; drops Spk 5, Ref 10, Test 16).
5. **Dirty flag, not debounce.** `MeetingDetailViewModel` (Eleg 8) sets `speakersDirty` on every speaker write and
   redelivers once when the header popover or a transcript picker closes and on `onDisappear`, only when dirty. An
   in-flight failure keeps the flag and retries at the next `.ready` tick; the `onDisappear` flush is a `Task`
   capturing `pipeline`, `store`, `meetingID` (not `self`) with the same one retry. No `Debounce` type; scratchpad
   untouched (Simp 4, Spk 4, Test 12, Ref 7). Acceptance 6 becomes "once per popover session".
6. **Flat ranked list, pure, in core.**
   `SpeakerOptions.build(speaker:speakers:persons:participants:suggestion:recent:query:) -> [Option]`, `Option =
   .person(Person, tag:) | .create(String)`: voice match, LLM guess, attendees, recent, Create last; tags "Sounds
   like", "Mentioned", "Attendee", "In this meeting". One verb `select(_:for:)`; fold `.caseInsensitive,
   .diacriticInsensitive`. Settles Eleg 2, Ref 6, Test 8, Simp 9 (no `candidates`).
7. **No "Same voice as".** Merge only by choosing a person who owns another speaker here, and that rule lives inside
   `confirm`'s write (shared `mergeSpeakerRows` body), not in the view model. Settles Simp 6, Eleg 1, Ref 5.
8. **One trigger, no nested popovers, no Tab.** Every trigger is a button (name, or placeholder "Name this speaker…").
   In the header popover the list expands inline under its row; in the transcript it opens its own `.popover` driven
   by one `presentedSpeakerID` on the tab. Keyboard state lives in `SpeakerPickerState` (`move`, `commit() ->
   Option?`). Simp 5, Spk 3, Test 9.
9. **Pre-fill instead of hidden default.** A `.suggested` speaker's field opens with the suggested name selected;
   Return confirms what is shown. Otherwise nothing is highlighted until typing or Down; Return on an empty field is a
   no-op. The Deferred pre-fill item moves into scope. Settles Eleg 4, Conf 10.
10. **No auto-open.** `pendingReviews` drives the list badge only; `AppController` drops ids whose speakers are all
    confirmed on each observation tick; `reviewCompleted` and the view `onChange` go. Settles Eleg 3, Test 3, Conf 9,
    Spk 6, Test 14 (auto-open was unreachable in smoke tests).
11. **Achromatic avatars.** Initials on `secondary`, glyph for unnamed, no `Theme.avatarHues`: a plan cannot override
    the shared achromatic decision (Conf 7). Also removes Spk 10, Test 11, Ref 11; hues deferred.
12. **Header and popover shape.** Row slot: after the meta line and the retention plan's "Recording" line, before
    Tags, only when the meeting is `.ready` and has a speaker (Conf 5). Names by `displayName(forSpeaker:)`, then "n
    to confirm" in `faint`, no `warning` chip (Eleg 9). Popover rows in stable `clusterLabel` order, no regroup motion
    (Simp 10, Eleg 12); excerpt (≤160 chars, leading "…") only on unconfirmed rows (Eleg 10, Test 15).
13. **No redesign dependency.** Today's primitives (`.buttonStyle(.plain)` + accessibility label, own 88 pt label);
    the redesign restyles the speaker surfaces later and deletes its sheet step (Conf 4, Ref 9, Simp 11).
14. **Vault integrity is in scope.** Redeliver removes this meeting's `%%steno:<uuid>%%` line from person pages in the
    previous receipt that this delivery did not render (Conf 3).

## 2. Spikes first

| Spike | Before | Box | Pass | On failure |
|---|---|---|---|---|
| S1 picker keyboard in a popover (Spk 3, reduced by D8): throwaway view behind `-steno-ui-testing` | WP7 | 1 day | Down/Up move the highlight without moving the caret; Return commits the highlighted option; Escape with the list expanded collapses it and leaves `SpeakersPopover` open; a transcript `.popover` from row 40 of a 200-row `LazyVStack` via `presentedSpeakerID` opens anchored and survives a 100 pt scroll; full keyboard access off | Keys through a local `NSEvent` monitor scoped to the popover; if the transcript popover fails, the trigger opens the header popover with that row expanded (WP8 shrinks to that) |
| S2 redeliver while in flight (Spk 4) as a test in WP7 | WP7 merge | 2 h | `PipelineHarness`, LLM fake held, `rerunSummary`, `assign`, close picker, release: exactly one `deliverAll` after the run, no `error` | Add `ProcessingPipeline.isInFlight(_:)` and defer instead of catching the failure |

Dropped by D1, D4, D11: Spk 1, 2, 5, 10. If the owner rejects D1, run Spk 1, 2 before WP2 and add `enrolledEmbedding`.

## 3. Work packages (one PR each)

**WP0 `docs(plans): settle inline speaker assignment after review`**, this branch (PR #102), with the six reviews and
this file. Section 5 edits; on `main` plans (Conf 6): scope App UI bullet; macOS step 5, surface row, acceptance 5, 8;
core-foundation 157, 169, 325-327, Deferred 393-394 ("`forget` stays deferred; recompute replaces enrolment"); speech
Enrolment row, 340-341; LLM 84, 342. Unlanded siblings: Section 4. Deps: none.

**WP1 `refactor(core): share speaker merge and person resolution`**, `refactor/core-speaker-merge`. Extract `static
mergeSpeakerRows(_:into:_ db:)` from `mergeSpeakers`; move name/attendee resolution from `SpeakerReviewViewModel` to
`MeetingStore.resolvePerson(named:email:now:)` with the case- and diacritic-insensitive match; view model calls it.
Tests: existing `SpeakerReviewViewModelTests` stay green; add `resolvePersonReusesJeromeForJérôme`,
`resolvePersonCreatesWithEmail`, `resolvePersonIgnoresBlank`. Deps: none. Parallel with WP4, WP6.

**WP2 `feat(core): reassignable confirmations with a recomputed voice`**, `feat/core-reassign`. D1, D2, D3 (confirm
half), D7: `refreshVoice`; `confirm(speakerID:person:)` same-person no-op, merge when the person owns another speaker,
clip per D3; `mergeSpeakers`/`mergePersons` refresh; `recentPersons()`; `enroll` and `enrolments` removed; stale doc
comments fixed (`People.swift:235`, `Audio.swift:107`, `MeetingStore+People.swift:123`, `SpeakerMemory.swift:3`,
`Diarize.swift:25`). Re-pin: `confirmSetsConfirmedEnrolsOnceAndRemovesTheClip` ->
`...RefreshesTheVoiceAndKeepsTheClip`; `confirmKeepsAnExistingPersonAndSkipsEnrolWithoutAnEmbedding` ->
`...LeavesTheVoiceWithoutAnEmbedding`; enrol tests (`SpeakerMemoryTests` 45-67, `CosineSpeakerMemoryTests` 58-127,
182) deleted or reseeded by saving `Person(embedding:sampleCount:)`, likewise `ModelIntegrationTests` 204-205,
`SpeakerNameSuggestionTests` 55. Add: `confirmTwiceWithTheSamePersonIsANoOp`, `confirmAThenBRestoresAExactly` (1e-6),
`confirmingAwayFromASuggestionLeavesTheSuggestedPersonUntouched`, `confirmIntoAPersonOwningAnotherSpeakerMerges`,
`confirmDeletesTheClipWhenTheAudioIsGone`, `mergeIntoAnUnknownTargetKeepsTheVoice`,
`mergePersonsRefreshesTheKeptVoice`, `refreshVoiceSkipsAMismatchedDimensionAndCapsAtFifty`,
`recentPersonsOrdersByLatestConfirmedMeeting` (Test 7 arrangement, `["Jérôme", "Nicolai"]`),
`PipelineIntegrationTests.confirmThenReassignKeepsTheClipAndMovesTheVoice`. `git diff --exit-code Tests/Fixtures`
clean. Done: Linux `StenoCoreTests`, `package` job. Deps: WP1. Parallel with WP3, WP5.

**WP3 `feat(core): retention sweep removes named speakers' clips`**, `feat/core-sweep-clips`. Sweep gathers clips of
confirmed speakers per expired asset, counts them in `clean`, then `MeetingStore.clearSampleClips(meetingID:)` nulls
only those. Re-pin `removesMasterSidecarsAndMixdownButNeverClipsOrStrangers` ->
`removesMasterSidecarsMixdownAndConfirmedClipsButNeverUnconfirmedClipsOrStrangers`; add
`anUndeletableClipKeepsExpiresAtAndTheClipColumn`. `retentionZeroSweep...KeepsTheSampleClips` stays green (fresh clips
are unconfirmed). Header doc of `RetentionSweep` rewritten. Deps: retention plan step 2 (built on its `expiredAssets`
join). Must merge before any release that contains WP2.

**WP4 `fix(adapters): drop a meeting's line from pages of removed persons`**, `fix/adapters-stale-person-lines`. D14
via `ManagedBlock.remove`; receipt keeps the path. Test `redeliverAfterReassignmentRemovesTheOldPersonLine` (bytes
outside the block intact). Deps: none; before WP7.

**WP5 `feat(core): ranked speaker options and excerpts`**, `feat/core-speaker-options`. D6 and D12:
`SpeakerOptions.build`, `SpeakerExcerpts.text(for:in:)` in StenoCore. `SpeakerOptionsTests`: order, suggested person
not repeated under recent, a person owning another speaker here listed once as "In this meeting", the speaker's own
person excluded, "jerome" finds Jérôme, Create hidden for a matching name and for whitespace, LLM guess as person when
it exists, nothing for nil/empty guess, query searches all persons. `SpeakerExcerptsTests`: two-segment join, nil
range -> longest, no segments -> "", partial overlap -> "…", 160-char cap. Deps: WP1. Parallel with WP2.

**WP6 `refactor(macos): clear pending reviews from the store`**, `refactor/macos-pending-reviews`. D10 controller
half; the sheet keeps working. Add `testSpeakersNeedReviewClearsWhenTheLastSpeakerIsConfirmed`, keep the ghost half of
the existing test. Deps: none.

**WP7 `feat(macos): inline speakers in the header`**, `feat/macos-inline-speakers`. Delete `SpeakerReviewSheet.swift`,
`.sheet`, `showsSpeakerReview`, `review-speakers`, auto-open, `reviewCompleted`; `SpeakersViewModel` fed by
`update(export:)` plus `nameSuggestions` and `recentPersons` in one cancellable load (Ref 8); `SpeakersRow`,
`SpeakersPopover`, `SpeakerPicker`, `SpeakerPickerState`, `Design/Avatar.swift`; dirty-flag redeliver (D5); ids from
the plan plus `speaker-field-<uuid>`, minus `speaker-option-speaker-<uuid>`. Tests: `SpeakersViewModelTests` replaces
`SpeakerReviewViewModelTests`, re-homing `testAssignAttendeeCreatesPersonWithEmail`,
`testBlankNamesAndUnknownSpeakersAreIgnored`, `testNamingCreatesAPersonAndConfirms`, `testLLMNameSuggestion...` (as an
option), `testPlayingAMissingClipReportsAnError` -> `canPlay == false`; drop `testSkip...`,
`testMergeNeedsAChosenTarget`; `testMergeWithItselfIsANoOp` becomes assign-own-person no-op; add
`testRowsUpdateFromTheObservationAfterAssign`, `testBuildingOptionsWritesNothing`. `SpeakerPickerStateTests`
(pre-filled Return -> `.person(Jérôme)`, empty Return -> nil, Down past end stays). `MeetingDetailViewModelTests`:
drop `showsSpeakerReview`; add `testClosingThePickerRedeliversOnceAfterChanges` (obsidian settings arranged),
`testClosingWithoutChangesDoesNotRedeliver`, `testDisappearFlushesAPendingRedeliver`, S2; scratchpad test unchanged.
`AvatarTests` initials. `LaunchSmokeTests` `[ci hosted]`: `speakers-row` reads "1 to confirm", click,
`app.popovers.firstMatch` exists, row `…14` has "Nicolai", clicking row `…15` shows `speaker-field-…15` with value
"Jérôme". Done: `grep -rn 'SpeakerReviewSheet\|showsSpeakerReview\|review-speakers\|clusterConfidence'
apps/macos/Steno` empty; `ThemeTokensTests` unchanged. Deps: WP2, WP4, WP5, WP6, S1; onboarding step 6 if open.

**WP8 `feat(macos): speaker picker on transcript names`**, `feat/macos-transcript-speaker-picker`. Trigger renders
exactly `displayName(forSpeaker:)`; hover state in the trigger; one `presentedSpeakerID`. Tests:
`TabTextSnapshotTests` and `git diff --exit-code Tests/Fixtures/snapshots/macos` unchanged; smoke clicks
`speaker-picker-…14`, asserts the popover, attaches `XCUIScreen.main.screenshot()` `.keepAlways`. Deps: WP7.

Parallel lanes: {WP1, WP4, WP6} at once; then {WP2, WP5} and WP3 once retention step 2 is in; WP7 joins; WP8 last.

## 4. Merge order with sibling plans

1. WP0 (PR #102). When the first-run index lands, it gains this plan as a tenth row after processing-progress (#101)
   and the Ownership row "Speakers row, popover, picker, avatars".
2. Retention plan: its PR drops Non-goal 149, rewrites rule 5 and the Audio footnote per Conf 2; its step 2 merges
   before WP3, which must be on `main` before the next release tag that contains WP2.
3. Onboarding step 6 (`MeetingDetailViewModel` init) before WP7 if it is open; do not wait for unstarted work.
4. Processing-progress (#101) and start-recording impose no order; the row renders only in `.ready`, so it never sits
   beside the progress card.
5. Visual redesign after WP8, amended: drop "Review speakers (n)", the sheet paragraph, step 10, F28 and the sheet
   half of F19; its transcript turn header uses the WP8 trigger.

## 5. Amendments to `.plans/2026-09-28-inline-speaker-assignment.md`

- Status/header: reviewed; points to this file; drops "the app cannot ship step 2 before step 1".
- Non-goals: `mergePersons` "stays a store operation without a UI or CLI caller" (Conf 11); primitives line per D13.
- Decisions 1 (no auto-open, controller clears), 4 (D1), 5 (drop second sentence), 6 (D3, D4, exported path), 7 (D6
  flat list, query over all persons), 9 (D11), 10 (D5); add D8, D9, D14.
- Design: Header (slot, `.ready` gate, "n to confirm", no chip, no auto-open); Popover (stable order, excerpt on
  unconfirmed rows only, plain play button, no motion); SpeakerPicker (one trigger, inline list vs transcript popover,
  flat option table without "Same voice as", pre-fill, no Tab, `speaker-field-<uuid>`); Avatars achromatic, hue
  paragraph and `ThemeTokensTests` claim removed; Playback: range playback deleted, Play iff the clip file exists.
- Core changes: 1 becomes `refreshVoice` and `enroll` removal; 2 deleted; 3 per D1-D3, D7; 4 refresh both persons; 5
  inner join; 6 confirmed clips only, `clearSampleClips`; 7 lists every Conf 6 edit; new 8 adapters stale lines; new 9
  `resolvePerson`, `SpeakerOptions`, `SpeakerExcerpts` in core.
- App changes: no `Debounce.swift`, no `flushRedeliver` on the child, no `candidates`, no `merge(_:into:)`;
  `select(_:for:)`; `SpeakerPickerState`.
- Implementation steps: replace with WP1-WP8 and their tests; Verification: `ui-smoke` runs on hosted `macos-15` (Spk
  6).
- Acceptance 1 ("to confirm", click opens, suggested name pre-filled), 3 (voice exactly restored), 5 (unnamed speaker
  of a retention-0 meeting still plays; named speaker plays while audio exists; `[manual]`), 6 (once per popover
  session), tag every item `[ci]`/`[ci hosted]`/`[manual]`.
- Reviewer trap: replace the withdraw bullet with "voice adjusted instead of recomputed", add "merge decided in the
  view model", "`pendingReviews` cleared from a view".
- Deferred: add avatar hues, range playback, merge-before-naming, Tab between rows, voice refresh on meeting delete;
  remove the pre-fill item.
