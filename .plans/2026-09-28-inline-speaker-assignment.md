# Inline speaker assignment: the review sheet becomes live selects

Status: reviewed, 2026-09-28. Third round of the owner's first-run feedback ("the speaker selection
UI needs major rework. See how Jamie does it: all inline and live-updating selects"). Six reviews
(simplification, spikes, conflicts, refactoring, tests, elegance) and the execution order that settles
them live in [`reviews/2026-09-28-inline-speaker-execution-order.md`](reviews/2026-09-28-inline-speaker-execution-order.md);
decisions below are the settled ones. Binding context:
[`2026-09-24-initial-scope.md`](2026-09-24-initial-scope.md) (App UI: "Speaker review sheet after
processing for unknown speakers") and [`2026-09-25-macos-app-and-release.md`](2026-09-25-macos-app-and-release.md)
(step 5, the `SpeakerReviewViewModel` surface row, acceptance 5 and 8). This plan replaces the sheet
with an inline surface and amends both files (Supersedes, below). It also replaces the "Speaker
review sheet" paragraph, findings F28, the sheet half of F19, and the "Review speakers (n)" button of
[`2026-09-28-macos-visual-redesign.md`](2026-09-28-macos-visual-redesign.md); when that plan lands
it points here for speakers.

## Goal

Speakers are named where they are read. The meeting header shows who spoke (avatars, names, "n to
confirm"); clicking it opens a popover with one row per speaker, each an inline search-or-create
select with a transcript excerpt and a play button. The same select sits on every speaker name in the
Transcript tab. A choice applies on click: the transcript, the summary and the header update from the
store observation, the voice is remembered, and the vault re-exports when the picker closes. Nothing
is modal, nothing needs Done, and every speaker (named or not) can be changed at any time. Picking the
same person for two clusters merges them, which is how Jamie handles a split voice ("just add the same
name twice").

## Non-goals

- A People screen, contact photos, email editing, or importing Contacts.app. `Person` stays what it
  is: display name, optional email, voice.
- Cross-meeting person merge UI. `MeetingStore.mergePersons` stays a store operation without a UI or
  CLI caller.
- Changing diarization, the sample clip picker, or the match threshold. The 3 s clips in the owner's
  screenshot (`00:23:29 – 00:23:32`) are `SampleClipPicker` capping the clip to the longest contiguous
  turn; the excerpt text in this plan compensates, a longer clip is a StenoSpeech follow-up.
- Live transcription or naming during a recording.
- Playing a range of the recording when the clip file is gone (Deferred).
- Per-person avatar hues (Deferred): the first-run index's shared decision keeps the accent
  achromatic, and a plan does not override it.
- Restyling anything outside the speaker surfaces. This plan uses today's primitives
  (`.buttonStyle(.plain)` with accessibility labels, its own 88 pt row label); the visual redesign
  restyles the speaker surfaces later and deletes its sheet step.

## Findings

From the owner's screenshot of the current sheet and the code behind it.

| # | Where | What is wrong |
|---|---|---|
| S1 | `apps/macos/Steno/Speakers/SpeakerReviewSheet.swift` (whole file), `Main/MeetingDetailView.swift` (`.sheet`, "Review speakers (n)") | A 560 x 520 modal with one card per unresolved speaker. Naming is two steps (type, click Name); merging is a third control ("Same voice as" picker plus Merge); Skip hides a card for the session; nothing applies to the vault until Done. Jamie does all of this in one field per speaker, in the header, with no confirmation step. |
| S2 | `SpeakerReviewViewModel.swift` (`cards` filter `!isConfirmed`, `confirm` guard), `Sources/StenoCore/Storage/MeetingStore+People.swift` `confirm` | Only unresolved speakers are shown, and a confirmed speaker can never be revisited: `confirm` enrols the embedding into the person's running mean every time, so a second confirm would enrol twice and a wrong name cannot be taken back. Jamie: "you can change the name at any point". |
| S3 | `MeetingStore+People.swift` `confirm` (deletes `sampleClipURL`) | Confirming deletes the clip, so there is nothing to listen to when a name looks wrong later. Jamie keeps the play button on every speaker. |
| S4 | `SpeakerReviewSheet.swift` (card subtitle) | "confidence 50%" is `clusterConfidence`, the diarizer's cluster quality. It reads as "we are 50 % sure this is Philipp" and helps nobody name a voice. |
| S5 | `SpeakerReviewSheet.swift` (candidates as a horizontal scroller of buttons, attendees as more buttons, two suggestion rows) | Three suggestion mechanisms with three looks (voice match with Accept, LLM guess with Use, chips for candidates and attendees). Jamie has one list under the field, best guess first. |
| S6 | `Main/Tabs/TranscriptTab.swift` (turn header `Text`) | Speaker names in the transcript are static text. The owner wants every name to be the select. |
| S7 | `Main/MeetingDetailView.swift` header | Nothing in the header says who was in the meeting; the only speaker affordance is a primary button that opens the modal. Jamie's header has a Speakers row (avatar stack, first names, "+4") that is also the entry point. |
| S8 | `MeetingStore.persons()` orders by name | There is no notion of recency, so a long people list buries the ones you met this week. Jamie's list is "Recent Contacts". |

## Decisions

1. **Inline, not modal.** The sheet, Later, Done, Skip and "Review speakers (n)" go away. The entry
   points are the header Speakers row and every speaker name in the Transcript tab. The popover never
   opens itself. `AppController.pendingReviews` stays for the list badge and clears itself: on every
   `observeMeetings` tick the controller drops ids whose speakers are all confirmed (read from the
   store), so `reviewCompleted` and the view `onChange` go.
2. **One select, three places.** `SpeakerPicker` is one SwiftUI component with one trigger style: a
   button showing the name, or the placeholder "Name this speaker…" for an unnamed speaker. In the
   header popover the list expands inline under its row; in the transcript it opens its own
   `.popover` driven by one `presentedSpeakerID` on the tab. Both share `SpeakerPickerState` and the
   same keyboard model.
3. **Selection applies immediately** through `MeetingStore.confirm`; the UI re-renders from
   `observeMeeting(id:)`, which the detail view model already follows. No local optimistic state
   beyond the field's draft text.
4. **Any speaker can be reassigned, and the voice is recomputed, not adjusted.** A person's voice is
   `normalise(mean(embedding of their confirmed speakers, newest 50 by meeting.startedAt))` with
   `sampleCount = count`, written by one private `MeetingStore.refreshVoice(personID:db:)` inside the
   transaction of `confirm`, `mergeSpeakers` (both persons) and `mergePersons`. `SpeakerMemory.enroll`
   goes; `confirm` drops its `memory:` parameter; the previous person is whoever `assignment` named.
   Same person again is a no-op. `.suggested` and embedding-less ("Me") speakers never count. Cost: a
   50-sample window replaces the capped running mean, and samples of deleted meetings drop out at the
   person's next refresh.
5. **Same person twice merges.** Choosing a person who already owns another speaker of this meeting
   merges the chosen speaker into that one. The rule lives inside `confirm`'s write (shared
   `mergeSpeakerRows` body), not in the view model. There is no separate merge control and no "Same
   voice as" section.
6. **Clips outlive confirmation, within the scope's retention rules.** `confirm` deletes the clip only
   when the meeting's master file is already gone. The retention sweep removes the clips of
   `.confirmed` speakers together with the audio and nulls their `sampleClipURL`; unconfirmed speakers
   keep theirs, so a retention-0 meeting still plays its unnamed speakers when the popover first opens.
   Play is shown iff the clip file exists; `ClipPlayer` is unchanged. `sampleClipURL` stays in
   `CodingKeys`, so the exported `meeting.json` keeps the path while the file exists.
7. **One flat ranked list, built in core.** `SpeakerOptions.build(speaker:speakers:persons:participants:suggestion:recent:query:)
   -> [Option]`, `Option = .person(Person, tag:) | .create(String)`: the voice match ("Sounds like"),
   the LLM's name guess ("Mentioned", as the person of that name when one exists, else a create row),
   calendar attendees not yet assigned ("Attendee"), a person owning another speaker here ("In this
   meeting"), recent people, then Create last. No section headers, no Accept or Use buttons. Typing
   filters with `.caseInsensitive, .diacriticInsensitive` over all persons, so "jerome" finds Jérôme
   and a retyped name reuses a ghost. One verb in the view model: `select(_:for:)`.
8. **Pre-fill instead of a hidden default.** A `.suggested` speaker's field opens with the suggested
   name filled in and selected; Return confirms what is shown. Otherwise nothing is highlighted until
   the user types or presses Down; Return on an empty field is a no-op. Escape with the list open
   collapses the list; Escape again closes the popover. No Tab between rows.
9. **Excerpt instead of confidence.** Each unconfirmed popover row shows what the speaker said in the
   clip range (the segments overlapping `sampleClipRange`, else the speaker's longest segment), two
   lines, 13 pt `faint`, at most 160 characters with a leading "…" when cut. Confirmed rows show none.
   `clusterConfidence` leaves the UI.
10. **Achromatic avatars.** Initials on a `secondary` veil, `strong` text; unnamed speakers get a
    `person` glyph in `faint`. No hue tokens.
11. **Re-export on close, not on a timer.** `MeetingDetailViewModel` sets `speakersDirty` on every
    speaker write and calls `redeliver` once when the header popover or a transcript picker closes and
    on `onDisappear`, only when dirty. A failure because the pipeline holds the meeting keeps the flag
    and retries at the next `.ready` tick; the `onDisappear` flush is a `Task` capturing `pipeline`,
    `store` and the meeting id, not `self`, with the same one retry. The scratchpad debounce is
    untouched.
12. **No orphan-person deletion.** `recentPersons()` returns persons who own a confirmed speaker
    (inner join, latest `startedAt` descending, then `displayName COLLATE NOCASE`); a person left with
    no speakers simply stops appearing in Recent and is found again by typing.
13. **Vault integrity.** A redeliver removes this meeting's `%%steno:<uuid>%%` line from every person
    page named in the previous receipt that this delivery did not render, so a reassignment leaves no
    stale line on the wrong person's page.

## Jamie reference

From the owner's screenshot and https://docs.meetjamie.ai/getting-started/identify-speaker: speakers
live in the meeting header ("Hover over the Speakers list in the header"); each has a play button for
a short clip; the field says "Search or create contact…" and offers "Recent Contacts", with pre-filled
names when Jamie recognised the voice; "you can change the name at any point", "simply click on the
speaker again and rename it"; a split voice is fixed by adding "the same name twice"; names flow into
"both your meeting summary and your transcript"; a named voice is remembered for future meetings.

## Design

### Header: Speakers row

`MeetingDetailView.header` gains a labelled row after the meta line (and after the retention plan's
"Recording" line when that lands), before Tags, shown only when the meeting is `.ready` and the export
has at least one speaker:

- Label "Speakers" 12 pt `faint`, 88 pt wide.
- Control: a `Button` (id `speakers-row`, `.buttonStyle(.plain)`) with a hairline pill (`secondary`
  fill, radius 8, height 28): an avatar stack (first three speakers in `clusterLabel` order, 20 pt,
  6 pt overlap, unnamed ones as the glyph avatar), a "·", `displayName(forSpeaker:)` of the named
  speakers in order, "+n" 13 pt `faint` for the rest, then, when any speaker is unconfirmed, "n to
  confirm" 12 pt `faint`, then a 10 pt `chevron.down`.
- Click opens `SpeakersPopover` as a `.popover(arrowEdge: .bottom)`. Nothing opens it automatically.

### Popover: `SpeakersPopover`

360 pt wide, content height up to 440 then scrolls, `popover` fill, padding 16. Title "Speakers" 16
semibold. One row per speaker in stable `clusterLabel` order; rows never regroup or animate when a
speaker is named. A row (`speaker-row-<uuid>`):

- `Avatar` 28 pt.
- `SpeakerPicker` trigger (`speaker-picker-<uuid>`): the name 14 medium `strong` with the person's
  email 12 `faint` after a "·" when known, or the placeholder "Name this speaker…" 14 `muted`; a
  `chevron.down` on hover. Clicking expands the field (`speaker-field-<uuid>`) and the option list
  inline under the row.
- Excerpt 13 `faint`, two lines, under the trigger, on unconfirmed rows only.
- Trailing plain button 28 pt: `play.fill` / `stop.fill`, id `speaker-play-<uuid>`, shown iff the
  clip file exists. One clip plays at a time (`ClipPlayer`).

No footer. Escape or clicking outside closes; nothing is lost because nothing is pending.

### `SpeakerPicker` and `SpeakerPickerState`

`SpeakerPickerState` (pure, tested hostless): `query`, `highlighted: Int?`, `options: [Option]`,
`move(.up/.down)`, `commit() -> Option?`, `reset(prefill:)`. Opening a picker for a `.suggested`
speaker pre-fills the suggested name selected and highlights that option; otherwise `highlighted` is
nil until typing or Down. Return commits the highlighted option, or nothing. Committing calls
`select(option, for: speakerID)`: `.person` confirms (and merges when that person owns another speaker
here, inside `confirm`), `.create(text)` resolves the name through `MeetingStore.resolvePerson` and
confirms.

Option rows are 24 pt: avatar 18 pt, name 13, a trailing tag 12 `faint` when the option has one:

| Option | Source | Tag |
|---|---|---|
| Voice match | `.suggested(personID, similarity)` on the speaker | "Sounds like" |
| LLM guess | `SpeakerNameSuggestion` for the speaker; the person of that name when one exists, else a create row | "Mentioned" |
| Attendees | participants with `role == .them` whose person is not assigned to another speaker | "Attendee" |
| In this meeting | persons owning another speaker of this meeting | "In this meeting" |
| Recent | `MeetingStore.recentPersons()` minus anything above | none |
| Create | when the trimmed query is non-empty and matches no person's name (`.caseInsensitive, .diacriticInsensitive`) | `Create "Anna"` |

The speaker's own person is never listed. Typing filters every option by the same fold and, once the
query is non-empty, searches all `persons()`, not only Recent.

### Transcript tab

The turn header's name `Text` becomes the compact `SpeakerPicker` trigger (name 13 semibold `strong`,
`chevron.down` 9 pt `ghost` on hover, id `speaker-picker-<uuid>`) that opens a `.popover` holding the
field and the option list, one presented at a time via `presentedSpeakerID`; turns without a speaker
keep the static "Unknown". The trigger renders exactly `displayName(forSpeaker:)`, so
`TabText.transcript` and `TabTextSnapshotTests` do not move.

### Avatars

`Design/Avatar.swift`: `Avatar(name: String?, size:)`. Initials are the first letters of the first two
words of the name (one letter for a single word), uppercased, semibold, size `0.42 × size`, `strong`
on a `secondary` circle; nil name draws a `person` glyph in `faint`.

## Core changes (`Sources/StenoCore`, `Sources/StenoSpeech`, `Sources/StenoAdapters`)

1. **Shared merge body and person resolution.** `MeetingStore.mergeSpeakers` extracts
   `static mergeSpeakerRows(_:into:_ db:)` so `confirm` can call it in its own transaction.
   `MeetingStore.resolvePerson(named:email:now:) -> Person` moves the "existing person of that name,
   else a new one" rule out of the view model, matching `.caseInsensitive, .diacriticInsensitive`;
   blank names throw.
2. **Voice recompute.** Private `refreshVoice(personID:db:)` per Decision 4; called by `confirm` (for
   the previous and the new person), `mergeSpeakers` (both persons when they differ) and
   `mergePersons` (the kept person). `SpeakerMemory` keeps `candidates(for:limit:)` and the provided
   `match`; `enroll` and `InMemorySpeakerMemory.enrolments` go. Mismatched-dimension embeddings are
   skipped; the window is the newest 50 by `meeting.startedAt`.
3. **`confirm(speakerID:person:)`** (no `memory:`): same person as the current `.confirmed` is a
   no-op beyond deleting the name suggestion; otherwise it saves the person when new, sets
   `.confirmed`, merges into the speaker that already belongs to that person in this meeting when
   there is one, refreshes both voices, and deletes the clip only when the meeting's master file is
   gone.
4. **Recency.** `MeetingStore.recentPersons() -> [Person]` per Decision 12. `persons()` keeps its name
   order for the CLI and the export.
5. **Clips follow the audio for confirmed speakers.** `RetentionSweep` gathers the clips of
   `.confirmed` speakers per expired asset, removes them with the audio, counts them in `clean`, then
   `MeetingStore.clearSampleClips(meetingID:speakerIDs:)` nulls those columns. Unconfirmed clips are
   untouched. The `RetentionSweep` header doc is rewritten.
6. **Options and excerpts.** `SpeakerOptions.build` (Decision 7) and `SpeakerExcerpts.text(for:in:)`
   (Decision 9) as pure types in StenoCore, tested on Linux.
7. **Vault stale lines.** `ManagedBlock.remove(meetingID:from:)`; `ObsidianFolderDestination.deliver`
   applies it to every `.managedBlock` path of the previous receipt that this delivery did not render,
   keeping the path in the new receipt.
8. **Doc comments** that say "enrol" or "deleted on confirm": `People.swift` (`sampleClipURL`),
   `Audio.swift` (`expirableFiles`), `MeetingStore+People.swift` (`confirm`), `SpeakerMemory.swift`,
   `Diarize.swift`, `MeetingStore.swift` (`nameSuggestions`).
9. **Plan bookkeeping** (this PR): `2026-09-24-initial-scope.md` App UI bullet;
   `2026-09-25-macos-app-and-release.md` step 5, the `SpeakerReviewViewModel` surface row, acceptance
   5 and 8; `2026-09-25-core-foundation.md` API listing (`confirm`, `RetentionSweep`), step 9 sweep
   test note, Deferred ("`forget` stays deferred; recompute replaces enrolment");
   `2026-09-25-speech-and-speakers.md` Enrolment row and the macOS line; `2026-09-25-llm-and-templates.md`
   decision 7 and the macOS line. Unlanded siblings (visual redesign, first-run index, retention plan)
   take their amendments in their own PRs per the execution order.

## App changes (`apps/macos`)

- Delete `Speakers/SpeakerReviewSheet.swift`. Rename `SpeakerReviewViewModel` to
  `Speakers/SpeakersViewModel.swift`: owned by `MeetingDetailViewModel` (`model.speakers`), fed the
  current `MeetingExport` on every observation tick (`update(export:)`) plus `nameSuggestions` and
  `recentPersons` in one cancellable load. API: `rows: [Row]` (speaker, person?, excerpt, canPlay),
  `options(for speakerID, query) -> [Option]`, `select(_:for:)`, `play(_:)`, `stopPlayback()`,
  `error`. `didChange`, `skipped`, `draftNames`, `mergeTargets`, `candidates`, `finish()` go away.
- New `Speakers/SpeakersRow.swift`, `Speakers/SpeakersPopover.swift`, `Speakers/SpeakerPicker.swift`,
  `Speakers/SpeakerPickerState.swift`, `Design/Avatar.swift`.
- `MeetingDetailViewModel`: `speakersDirty`, `pickerClosed()` and the `onDisappear` flush per
  Decision 11; `showsSpeakerReview` removed.
- `MeetingDetailView`: the Speakers row; `.sheet`, "Review speakers", auto-open and
  `reviewCompleted` removed.
- `AppController`: pending reviews cleared from the store on each observation tick.
- `TranscriptTab`: the compact picker trigger and `presentedSpeakerID`.
- Accessibility ids: `speakers-row`, `speaker-row-<uuid>`, `speaker-picker-<uuid>`,
  `speaker-field-<uuid>`, `speaker-option-<personID>`, `speaker-create`, `speaker-play-<uuid>`.

## Implementation steps

Work packages from the execution order, one PR each. `{WP1, WP4, WP6}` run in parallel; then `{WP2,
WP5}` and WP3; then WP7; WP8 last.

1. **WP1 `refactor(core): share speaker merge and person resolution`.** Core change 1; the sheet's
   view model calls `resolvePerson`. Tests: existing `SpeakerReviewViewModelTests` stay green;
   `resolvePersonReusesJeromeForJérôme`, `resolvePersonCreatesWithEmail`, `resolvePersonIgnoresBlank`.
2. **WP2 `feat(core): reassignable confirmations with a recomputed voice`.** Core changes 2, 3, 4, 8.
   Re-pin `confirmSetsConfirmedEnrolsOnceAndRemovesTheClip` → `…RefreshesTheVoiceAndKeepsTheClip`,
   `confirmKeepsAnExistingPersonAndSkipsEnrolWithoutAnEmbedding` → `…LeavesTheVoiceWithoutAnEmbedding`;
   enrol tests in `SpeakerMemoryTests`, `CosineSpeakerMemoryTests`, `ModelIntegrationTests`,
   `SpeakerNameSuggestionTests` reseeded by saving `Person(embedding:sampleCount:)`. Add
   `confirmTwiceWithTheSamePersonIsANoOp`, `confirmAThenBRestoresAExactly` (1e-6),
   `confirmingAwayFromASuggestionLeavesTheSuggestedPersonUntouched`,
   `confirmIntoAPersonOwningAnotherSpeakerMerges`, `confirmDeletesTheClipWhenTheAudioIsGone`,
   `mergeIntoAnUnknownTargetKeepsTheVoice`, `mergePersonsRefreshesTheKeptVoice`,
   `refreshVoiceSkipsAMismatchedDimensionAndCapsAtFifty`, `recentPersonsOrdersByLatestConfirmedMeeting`,
   `PipelineIntegrationTests.confirmThenReassignKeepsTheClipAndMovesTheVoice`.
   `git diff --exit-code Tests/Fixtures` clean. Done: Linux `StenoCoreTests` and the `package` job.
3. **WP3 `feat(core): retention sweep removes named speakers' clips`.** Core change 5. Re-pin
   `removesMasterSidecarsAndMixdownButNeverClipsOrStrangers` →
   `removesMasterSidecarsMixdownAndConfirmedClipsButNeverUnconfirmedClipsOrStrangers`; add
   `anUndeletableClipKeepsExpiresAtAndTheClipColumn`; `retentionZeroSweep…KeepsTheSampleClips` stays
   green (fresh clips are unconfirmed). Depends on the retention plan's step 2 (`expiredAssets` joining
   the meeting state) when that lands first; otherwise builds on today's query. Must be on `main`
   before any release that contains WP2.
4. **WP4 `fix(adapters): drop a meeting's line from pages of removed persons`.** Core change 7. Test
   `redeliverAfterReassignmentRemovesTheOldPersonLine` (bytes outside the block intact).
5. **WP5 `feat(core): ranked speaker options and excerpts`.** Core change 6. `SpeakerOptionsTests`:
   order, the suggested person not repeated under recent, a person owning another speaker here listed
   once as "In this meeting", the speaker's own person excluded, "jerome" finds Jérôme, Create hidden
   for a matching name and for whitespace, LLM guess as a person when it exists, nothing for a nil or
   empty guess, a query searches all persons. `SpeakerExcerptsTests`: two-segment join, nil range →
   longest, no segments → "", partial overlap → "…", 160-character cap.
6. **WP6 `refactor(macos): clear pending reviews from the store`.** Decision 1, controller half; the
   sheet keeps working. Add `testSpeakersNeedReviewClearsWhenTheLastSpeakerIsConfirmed`; keep the
   ghost half of the existing test.
7. **WP7 `feat(macos): inline speakers in the header`.** Everything under App changes except the
   transcript trigger, after spike S1 (a day: keyboard in a popover, Escape order, one `.popover` from
   a deep `LazyVStack` row). `SpeakersViewModelTests` replaces `SpeakerReviewViewModelTests`,
   re-homing `testAssignAttendeeCreatesPersonWithEmail`, `testBlankNamesAndUnknownSpeakersAreIgnored`,
   `testNamingCreatesAPersonAndConfirms`, the LLM suggestion test (as an option),
   `testPlayingAMissingClipReportsAnError` → `canPlay == false`; drop the Skip and Merge-target tests;
   self-merge becomes "assigning the own person is a no-op"; add
   `testRowsUpdateFromTheObservationAfterAssign`, `testBuildingOptionsWritesNothing`.
   `SpeakerPickerStateTests` (pre-filled Return → the suggested person, empty Return → nil, Down past
   the end stays). `MeetingDetailViewModelTests`: drop `showsSpeakerReview`; add
   `testClosingThePickerRedeliversOnceAfterChanges` (obsidian settings arranged),
   `testClosingWithoutChangesDoesNotRedeliver`, `testDisappearFlushesAPendingRedeliver`, and spike S2
   as a test (redeliver while `rerunSummary` is in flight: one delivery after the run, no error).
   `AvatarTests` initials. `LaunchSmokeTests` `[ci hosted]`: `speakers-row` reads "1 to confirm",
   click, `app.popovers.firstMatch` exists, the Speaker 1 row shows "Nicolai", clicking the Speaker 2
   row shows its field with value "Jérôme". Done when
   `grep -rn 'SpeakerReviewSheet\|showsSpeakerReview\|review-speakers\|clusterConfidence' apps/macos/Steno`
   is empty and `ThemeTokensTests` is unchanged.
8. **WP8 `feat(macos): speaker picker on transcript names`.** Transcript trigger with hover state in
   the trigger and one `presentedSpeakerID`. `TabTextSnapshotTests` and
   `git diff --exit-code Tests/Fixtures/snapshots/macos` unchanged; the smoke test clicks the Speaker 1
   trigger, asserts the popover and attaches `XCUIScreen.main.screenshot()` with `.keepAlways`.

## Verification

- CI: `.github/workflows/swift-ci.yml` jobs `package` and `app` (on `vars.MACOS_RUNS_ON`, `macos-15`
  when unset) and `ui-smoke` (hosted `macos-15`) green for every step.
- Local core: `docker run --rm -v "$PWD":/work -w /work steno-swift:6.1 swift test --filter
  StenoCoreTests` (the image exists on atlas).
- Local app: `xcodegen generate --spec apps/macos/project.yml`, `xcodebuild -project
  apps/macos/Steno.xcodeproj -scheme StenoTests test -destination platform=macOS,arch=arm64
  CODE_SIGN_IDENTITY= CODE_SIGN_STYLE=Manual DEVELOPMENT_TEAM=`, then the app with
  `--args -steno-ui-testing` to open the seeded meeting.

## Acceptance

1. `[ci hosted]` A processed meeting with an unnamed speaker shows "Speakers · Nicolai · 1 to confirm"
   in the header; clicking it opens the popover; clicking the unnamed row shows the field with the
   suggested name pre-filled.
2. `[ci]` Typing "Ph", Return, names the speaker "Philipp Schröder" if that person exists, else offers
   Create; the transcript and summary show the name before the popover closes.
3. `[ci]` Clicking a named speaker in the transcript and picking someone else changes every turn of
   that speaker and restores the first person's voice exactly to what it was before the mistake.
4. `[ci]` Picking a person who already owns Speaker 1 for Speaker 3 leaves one speaker with all
   segments; the popover drops the row.
5. `[manual]` An unnamed speaker of a retention-0 meeting still plays after processing; a named speaker
   plays while the audio exists; after the sweep removed the audio the button is gone and the name
   still edits.
6. `[ci]` The vault re-exports once per popover session after a burst of changes; leaving the meeting
   with the popover open exports immediately; no change, no export.
7. `[ci]` No `.sheet` for speakers, no "Review speakers" button, no `confidence` text anywhere in
   `apps/macos/Steno`.

## Reviewer trap

`confirm` that still deletes the clip while the audio exists, or still enrols or adjusts a voice
instead of recomputing it; the merge-on-same-person rule decided in the view model instead of inside
`confirm`; `pendingReviews` cleared from a view; a picker that commits on every keystroke (`select`
runs on Return or click only); the transcript trigger re-rendering the whole `LazyVStack` on hover
(hover state lives in the trigger); `TabText` changed to carry picker text; Recent built from an
outer join.

## Deferred

- Per-person avatar hues (needs the shared achromatic decision reopened).
- Playing a range of the recording when the clip file is gone.
- Merging two unnamed clusters before either has a name.
- Tab between popover rows.
- Refreshing a person's voice when a meeting is deleted (samples drop out at the next refresh).
- Longer sample clips for speakers with only short turns (pad with the next-longest range) in
  `SampleClipPicker`.
- A People screen with cross-meeting merge and email editing.
- Jamie's "turn automatic identification off" preference; `speakerMatchThreshold` stays a Settings
  field.
