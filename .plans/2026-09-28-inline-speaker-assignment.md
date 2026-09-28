# Inline speaker assignment: the review sheet becomes live selects

Status: proposal, 2026-09-28, third round of the owner's first-run feedback ("the speaker
selection UI needs major rework. See how Jamie does it: all inline and live-updating
selects"). Binding context: [`2026-09-24-initial-scope.md`](2026-09-24-initial-scope.md)
(App UI: "Speaker review sheet after processing for unknown speakers") and
[`2026-09-25-macos-app-and-release.md`](2026-09-25-macos-app-and-release.md) (step 5,
the `SpeakerReviewViewModel` row of the surface table). This plan replaces the sheet with
an inline surface and amends both files (Supersedes, below). It also replaces the
"Speaker review sheet" paragraph and findings F18, F19 (sheet part) and F28 of
[`2026-09-28-macos-visual-redesign.md`](2026-09-28-macos-visual-redesign.md), which restyled
the sheet in place; when that plan lands it points here for speakers.

## Goal

Speakers are named where they are read. The meeting header shows who spoke (avatars,
names, "n unnamed"); clicking it opens a popover with one row per speaker, each an inline
search-or-create select with a transcript excerpt and a play button. The same select sits on
every speaker name in the Transcript tab. A choice applies on click: the transcript, the
summary and the header update from the store observation, the voice is remembered, and the
vault re-exports after a short debounce. Nothing is modal, nothing needs Done, and every
speaker (named or not) can be changed at any time. Picking the same person for two clusters
merges them, which is how Jamie handles a split voice ("just add the same name twice").

## Non-goals

- A People screen, contact photos, email editing, or importing Contacts.app. `Person` stays
  what it is: display name, optional email, voice.
- Cross-meeting person merge UI (`MeetingStore.mergePersons` keeps working from the CLI).
- Changing diarization, the sample clip picker, or the match threshold. The 3 s clips in the
  owner's screenshot (`00:23:29 – 00:23:32`) are `SampleClipPicker` capping the clip to the
  longest contiguous turn; the excerpt text in this plan compensates, a longer clip is a
  StenoSpeech follow-up.
- Live transcription or naming during a recording.
- Restyling anything outside the speaker surfaces. Tokens, buttons and the header layout come
  from the visual redesign plan; this plan uses what exists today and adopts the new
  primitives when they land.

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

1. **Inline, not modal.** The sheet, Later, Done, Skip and "Review speakers (n)" go away.
   The entry points are the header Speakers row and every speaker name in the Transcript
   tab. `AppController.pendingReviews` stays (it drives the badge and the auto-focus below)
   and clears itself when the export shows no unconfirmed speaker, not when a sheet closes.
2. **One select, three places.** `SpeakerPicker` is one SwiftUI component with two triggers:
   a field (unnamed speaker in the popover) and a name button (named speaker in the popover,
   every name in the transcript). Both open the same list with the same keyboard model.
3. **Selection applies immediately** through `MeetingStore.confirm`; the UI re-renders from
   `observeMeeting(id:)`, which the detail view model already follows. No local optimistic
   state beyond the field's draft text.
4. **Any speaker can be reassigned.** Core learns which person a speaker's embedding was
   enrolled into (`enrolledPersonID`) and `SpeakerMemory` learns to withdraw an enrolment,
   so a reassignment moves the voice sample from the wrong person to the right one instead of
   enrolling twice. Same person again is a no-op.
5. **Same person twice merges.** Choosing a person who already owns another speaker of this
   meeting calls `mergeSpeakers(source, into: thatSpeaker)`. The list also offers the other
   speakers of the meeting under "Same voice as", so two unnamed clusters can be merged
   before anyone knows the name. The Merge button and picker are gone.
6. **Clips outlive confirmation.** `confirm` no longer deletes the sample clip; clips go
   with the meeting (`delete(meetingID:)`, already the case) and with the audio when the
   retention sweep removes it. When a clip file is missing but the recording is still there,
   Play uses the clip range on the recording. When neither exists, Play is hidden.
7. **One ranked list.** Under the field: Suggested (voice match, then the LLM's name guess,
   then calendar attendees not yet assigned), Recent (people by last confirmation), Same
   voice as (this meeting's other speakers), Create "typed text". Typing filters every
   section. No Accept or Use buttons; the top row is highlighted and Return takes it.
8. **Excerpt instead of confidence.** Each popover row shows what the speaker said in the clip
   range (the segments overlapping `sampleClipRange`, else the speaker's longest segment),
   two lines, 13 pt `faint`, leading "…" when cut. `clusterConfidence` leaves the UI.
9. **Avatars carry the identity.** Initials on a per-person hue chosen from a fixed set of
   eight muted hues by the person id's first byte, so the colour is stable across launches
   and appearances. Unnamed speakers get a `secondary` veil with a `person` glyph. This is
   the one place the app uses hue to tell things apart; controls stay achromatic, as the
   redesign plan decided.
10. **Re-export is debounced.** Every change schedules `pipeline.redeliver` 3 s later on the
    injected clock, coalesced like the scratchpad save; leaving the meeting flushes it. Done
    on an untouched sheet re-exporting nothing becomes: no change, no redeliver.

## Jamie reference

From the owner's screenshot and https://docs.meetjamie.ai/getting-started/identify-speaker:
speakers live in the meeting header ("Hover over the Speakers list in the header"); each has
a play button for a short clip; the field says "Search or create contact…" and offers
"Recent Contacts", with pre-filled names when Jamie recognised the voice; "you can change the
name at any point", "simply click on the speaker again and rename it"; a split voice is fixed
by adding "the same name twice"; names flow into "both your meeting summary and your
transcript"; a named voice is remembered for future meetings. Everything above maps onto
existing core operations except reassignment and clip retention (Decisions 4 and 6).

## Design

### Header: Speakers row

`MeetingDetailView.header` gains a labelled row under the meta line, before Tags, shown once
the export has at least one speaker:

- Label "Speakers" 12 pt `faint`, 88 pt wide, like the Tags label the redesign plan adds.
- Control: a `Button` (id `speakers-row`) with a hairline pill (`secondary` fill, radius 8,
  height 28): an avatar stack (first three speakers in `clusterLabel` order, 20 pt, 6 pt
  overlap, unnamed ones as the glyph avatar), a "·", the first names of named speakers in
  order, "+n" 13 pt `faint` for the rest, then, when any speaker is unconfirmed, a neutral
  chip "n unnamed" (`warning` text on a 12 % veil), then a 10 pt `chevron.down`.
- Click opens `SpeakersPopover` as a `.popover(arrowEdge: .bottom)`; the pending badge case
  (`controller.pendingReviews.contains(id)` and the window is active) opens it once per
  meeting selection with the first unnamed row's field focused, which replaces today's
  auto-presented sheet.

### Popover: `SpeakersPopover`

360 pt wide, content height up to 440 then scrolls, `popover` fill, padding 16. Title
"Speakers" 16 semibold. Rows in two groups, animated with `Motion.functional` when a row
moves: unnamed first, named after, each in `clusterLabel` order. A row (`speaker-row-<uuid>`):

- `Avatar` 28 pt.
- `SpeakerPicker` trigger: unnamed shows the field (placeholder "Search or create a
  person…"); named shows the name 14 medium `strong` with the person's email 12 `faint`
  after a "·" when known, and a `chevron.down` on hover.
- Excerpt 13 `faint`, two lines, under the trigger.
- Trailing `IconButton` 28 pt: `play.fill` / `stop.fill`, id `speaker-play-<uuid>`, hidden
  when nothing can be played. One clip plays at a time (`ClipPlayer`).

No footer. Escape or clicking outside closes; nothing is lost because nothing is pending.

### `SpeakerPicker`

State per open picker: query text, highlighted row, the ranked sections. Opening the list
(field focus, or clicking a name trigger) shows the sections below the trigger inside the
popover (or, in the transcript, in its own `.popover` anchored to the name). Rows are
24 pt: avatar 18 pt, name 13, a trailing tag 12 `faint` ("Sounds like", "Mentioned in the
conversation", "Attendee", "Speaker 3 in this meeting"). Sections:

| Section | Source | Order |
|---|---|---|
| Suggested | `.suggested(personID, similarity)` on the speaker; `SpeakerNameSuggestion` for the speaker (as a person when a person of that name exists, else as a create row with the tag); meeting participants with `role == .them` whose person is not yet assigned to another speaker | voice match, LLM guess, attendees by name |
| Recent | `MeetingStore.recentPersons()` minus anything already shown | most recently confirmed first, then by name |
| Same voice as | the meeting's other speakers, named or not | `clusterLabel` order |
| Create | shown when the trimmed query is non-empty and matches no person's name case- and diacritic-insensitively | one row: `Create "Anna"` |

Typing filters Suggested, Recent and Same voice as by case- and diacritic-insensitive
substring on the display name (and the cluster label). Down and Up move the highlight,
Return commits the highlighted row (the first row when the query is empty), Escape closes
without change, Tab moves to the next row's field in the popover. Committing a person calls
`assign(speakerID, person)`; a create row calls `name(speakerID, text)`; a speaker row calls
`merge(speakerID, into:)`. The highlighted default for an unnamed speaker is the first
Suggested row when there is one, so Return on an untouched field accepts the voice match.

### Transcript tab

The turn header's name `Text` becomes the compact `SpeakerPicker` trigger (name 13
semibold `strong`, `chevron.down` 9 pt `ghost` on hover, id `speaker-picker-<uuid>`); turns
without a speaker keep the static "Unknown". `TabText.transcript` is unchanged (the trigger's
text is the same name), so `TabTextSnapshotTests` does not move.

### Avatars

`Design/Avatar.swift`: `Avatar(person: Person?, size:)`. Initials are the first letters of
the first two words of `displayName` (one letter for a single word), uppercased, weight
semibold, size `0.42 × size`. Fill for a person: `Theme.avatarHues[Int(id.uuid.0) % 8]`, a
new token array of eight muted hues specified for both appearances with white initials
(light) and near-white initials (dark), contrast at least 4.5:1 in both; the unnamed fill is
`secondary` with a `person` glyph in `faint`. `ThemeTokensTests` asserts eight entries and
the contrast bound.

### Playback

`ClipPlayer.play(_ url: URL)` stays for the clip file. New `play(_ url: URL, range:)`
for the recording fallback: `AVAudioPlayer` with `currentTime = range.lowerBound` and a stop
scheduled on the injected clock after the range length, capped at 10 s. The view model picks
the source: `speaker.sampleClipURL` when the file exists, else `export.audio` (`mixdownURL`
when present, else `url`) with `sampleClipRange`, else no play button.

## Core changes (`Sources/StenoCore`, `Sources/StenoSpeech`)

1. **Withdraw an enrolment.** `SpeakerMemory` gains
   `func withdraw(_ embedding: Embedding, from person: Person) async throws`. With
   `w = min(sampleCount, maxSamples)`: when `w <= 1` the person's embedding becomes nil and
   `sampleCount` drops to `max(0, sampleCount - 1)`; otherwise
   `e' = normalise(e * w − x̂)` and `sampleCount -= 1`. Exact while the person is under the
   cap, approximate above it (documented on the protocol; the common case, a mistake caught
   in the same session, is exact). Implemented in `CosineSpeakerMemory` and
   `InMemorySpeakerMemory` (which records withdrawals like enrolments).
2. **`Speaker.enrolledPersonID`.** Migration `v3` in `Storage/Migrations.swift` adds
   `enrolledPersonID TEXT NULL REFERENCES person ON DELETE SET NULL` to `speaker` and
   backfills it from `personID` where `assignment = 'confirmed'`. `SpeakerRow`, `Speaker`
   (not in `CodingKeys`, like `embedding`) and `SampleData` follow.
3. **`confirm` reassigns.** `MeetingStore.confirm(speakerID:person:memory:)`:
   - Same person as `enrolledPersonID`: writes `.confirmed`, deletes the name suggestion, no
     enrol, no file change (idempotent).
   - Different person (or none yet): withdraws from the previous person when there is one,
     enrols into the new person, sets `enrolledPersonID`. When the previous person ends with
     `sampleCount == 0` and no `speaker`, `participant` or `meetingTask` row references them,
     the person row is deleted: a mistyped name leaves no ghost in Recent.
   - The sample clip is left alone (Decision 6).
   The `.suggested` similarity is kept on the person match, not on the row, as today.
4. **`mergeSpeakers` keeps the memory honest.** After the merge, when the source's
   `enrolledPersonID` is set and differs from the kept speaker's person, the source embedding
   is withdrawn from that person (same orphan rule as above). The kept speaker's
   `enrolledPersonID` is unchanged.
5. **Recency.** `MeetingStore.recentPersons() -> [Person]`: every person, ordered by the
   latest `meeting.startedAt` among the speakers confirmed to them (descending, nulls last),
   then display name. One query with a left join; `persons()` keeps its name order for the
   CLI and the export.
6. **Clips follow the audio.** `RetentionSweep` removes `speaker.sampleClipURL` files of a
   meeting together with its audio and nulls the column (today it never touches clips);
   `.keepForever` keeps them. `Persist`'s `speakersNeedReview` event is unchanged.
7. **Plan bookkeeping.** `2026-09-24-initial-scope.md` App UI bullet "Speaker review sheet
   after processing…" gets an amendment line pointing here; `2026-09-25-macos-app-and-release.md`
   step 5 and the `SpeakerReviewViewModel` surface row get the same note; the redesign plan
   drops its sheet paragraph when it lands.

## App changes (`apps/macos`)

- Delete `Speakers/SpeakerReviewSheet.swift`. Rename `SpeakerReviewViewModel` to
  `Speakers/SpeakersViewModel.swift`: owned by `MeetingDetailViewModel` (`model.speakers`),
  fed the current `MeetingExport` on every observation tick (`update(export:)`), so it never
  reloads speakers or people itself except `recentPersons()` and `candidates` once per export
  change. API: `rows: [Row]` (speaker, person?, excerpt, canPlay), `sections(for speakerID,
  query) -> [Section]`, `assign(_:person:)`, `name(_:_:)`, `merge(_:into:)`, `play(_:)`,
  `stopPlayback()`, `flushRedeliver()`, `error`. `didChange`, `skipped`, `draftNames`,
  `mergeTargets`, `finish()` go away.
- `Main/Debounce.swift`: the scratchpad's sleeper loop extracted into one `Debounce` type on
  the injected clock, used by `saveScratchpad` and by the redeliver.
- New `Speakers/SpeakersRow.swift`, `Speakers/SpeakersPopover.swift`,
  `Speakers/SpeakerPicker.swift`, `Speakers/SpeakerExcerpt.swift` (pure, tested),
  `Design/Avatar.swift`, `Theme.avatarHues`.
- `MeetingDetailView`: the Speakers row; `.sheet` and `showsSpeakerReview` removed; the
  popover auto-open for `pendingReviews`; `onChange(of: model.unconfirmedSpeakers.isEmpty)`
  calls `controller.reviewCompleted(meetingID:)`; `onDisappear` flushes the redeliver.
- `TranscriptTab`: the compact picker trigger.
- `ClipPlayer`: range playback on the recording, `stopAt` via the injected clock.
- Accessibility ids: `speakers-row`, `speaker-row-<uuid>`, `speaker-picker-<uuid>`,
  `speaker-option-<personID>`, `speaker-option-speaker-<uuid>`, `speaker-create`,
  `speaker-play-<uuid>`.

## Implementation steps

Each step is one PR; the app cannot ship step 2 before step 1 is on `main`.

1. **Core: reassignable confirmations.** Core changes 1 to 7. Tests in
   `Tests/StenoCoreTests` and `Tests/StenoSpeechTests`: withdraw is the exact inverse of
   enrol under the cap (enrol two samples, withdraw one, compare to a single enrol within
   1e-5); withdraw at `sampleCount 1` clears the embedding; migration v3 backfills
   `enrolledPersonID` for confirmed rows and leaves suggested rows nil; `confirm` twice with
   the same person enrols once; `confirm` A then B leaves A at `sampleCount 0` and deleted
   when unreferenced, B at 1, the speaker `.confirmed(B)` with `enrolledPersonID B`; `confirm`
   keeps the clip file; `mergeSpeakers` withdraws the source's enrolment when the persons
   differ; `recentPersons()` orders by latest meeting then name; the sweep removes clips with
   the audio and keeps them under `.keepForever`. Done when `swift test` passes in the
   `steno-swift:6.1` container for StenoCore (Linux, fast) and the `package` job passes for
   StenoSpeech.
2. **App: header row, popover, picker.** Everything under App changes except the transcript
   trigger and range playback. `SpeakersViewModelTests` replaces
   `SpeakerReviewViewModelTests`: rows list every speaker with the named ones after the
   unnamed; `sections` puts the `.suggested` person first, the LLM guess second as a create
   row when no person matches, attendees third, recent people after, the meeting's other
   speaker under Same voice as, and a Create row only for an unmatched query; diacritic- and
   case-insensitive filtering ("jerome" finds "Jérôme"); `assign` confirms and the export
   observation updates `rows`; `name` with an existing name reuses the person; `assign` of a
   person who owns another speaker merges the two; reassigning a confirmed speaker changes
   the person without a second enrolment; three changes within the debounce redeliver once
   on `ManualClock`, none without a change, and `flushRedeliver` delivers immediately;
   `SpeakerExcerpt` picks the clip-range text, falls back to the longest segment, and
   truncates with a leading ellipsis. `MeetingDetailViewModelTests` drops
   `showsSpeakerReview`; `AppControllerTests.testSpeakersNeedReviewStaysPendingUntil…`
   asserts the pending review clears when the last speaker is confirmed. Done when
   `StenoTests` passes and `LaunchSmokeTests` finds `speakers-row` in the seeded meeting.
3. **App: transcript trigger and playback fallback.** `TranscriptTab`, `ClipPlayer` range
   playback (`[manual]`: the clip is audible after confirmation once the file is gone),
   `LaunchSmokeTests` attaches a screenshot of the open popover (`lifetime = .keepAlways`).
   Done when `TabTextSnapshotTests` is unchanged and the screenshot shows a named and an
   unnamed row.

## Verification

- CI: `.github/workflows/swift-ci.yml` jobs `package`, `app` and `ui-smoke` (all on
  `vars.MACOS_RUNS_ON`, `macos-15` when unset) green for every step.
- Local core: `docker run --rm -v "$PWD":/work -w /work steno-swift:6.1 swift test
  --filter StenoCoreTests` (memory: the image exists on atlas).
- Local app: `xcodegen generate --spec apps/macos/project.yml`, `xcodebuild -project
  apps/macos/Steno.xcodeproj -scheme StenoTests test -destination platform=macOS,arch=arm64
  CODE_SIGN_IDENTITY= CODE_SIGN_STYLE=Manual DEVELOPMENT_TEAM=`, then the app with
  `--args -steno-ui-testing` to open the seeded meeting.

## Acceptance

1. A processed meeting with two unnamed speakers shows "Speakers · 2 unnamed" in the header;
   clicking it opens the popover with the first field focused and the voice match highlighted.
2. Typing "Ph", Return, names the speaker "Philipp Schröder" if that person exists, else
   offers Create; the transcript and summary show the name before the popover closes.
3. Clicking a named speaker in the transcript and picking someone else changes every turn of
   that speaker and leaves the first person's `sampleCount` where it was before the mistake.
4. Picking a person who already owns Speaker 1 for Speaker 3 leaves one speaker with all
   segments; the popover drops the row.
5. Play works on a named speaker; after the retention sweep removed the audio, the button
   is gone and the name still edits.
6. The vault re-exports once after a burst of three changes; leaving the meeting mid-debounce
   exports immediately; no change, no export.
7. No `.sheet` for speakers, no "Review speakers" button, no `confidence` text anywhere in
   `apps/macos/Steno`.

## Reviewer trap

`confirm` that still deletes the clip or still enrols on a same-person call; `withdraw`
implemented as "set embedding nil" for every count; a picker that commits on every keystroke
(`assign` must run on Return or click only); the transcript trigger re-rendering the whole
`LazyVStack` on hover (hover state lives in the trigger, not the tab); `TabText` changed to
carry picker text.

## Deferred

- Longer sample clips for speakers with only short turns (pad with the next-longest range) in
  `SampleClipPicker`.
- A People screen with cross-meeting merge and email editing.
- Jamie's "turn automatic identification off" preference; `speakerMatchThreshold` stays a
  Settings field.
- Pre-filling a `.suggested` name in the field itself (Jamie shows recognised names filled
  in); v1 highlights it as the first row instead, so a wrong voice match needs one keystroke
  to override rather than a delete.
