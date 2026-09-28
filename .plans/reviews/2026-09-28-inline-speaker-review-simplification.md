# Inline speaker assignment: simplification review

Reviewed 2026-09-28 against `.plans/2026-09-28-inline-speaker-assignment.md`, the
scope in `2026-09-24-initial-scope.md`, `2026-09-25-macos-app-and-release.md`,
`2026-09-25-speech-and-speakers.md` (Enrolment row, `CosineSpeakerMemory`), the
code the plan touches (`Sources/StenoCore/Storage/MeetingStore+People.swift`,
`Protocols/SpeakerMemory.swift`, `StenoSpeech/Speakers/CosineSpeakerMemory.swift`,
`Testing/InMemorySpeakerMemory.swift`, `Model/People.swift`, `Storage/Migrations.swift`,
`Storage/RetentionSweep.swift`, `Pipeline/LaneMerger.swift`,
`apps/macos/Steno/Speakers/*`, `Main/MeetingDetailView*.swift`,
`Main/Tabs/TranscriptTab.swift`, `Design/*.swift`) and the sibling plans
`2026-09-28-macos-visual-redesign.md` and `2026-09-28-first-run-feedback.md`.
Dimension: does the plan do more than its goal needs. Correctness and tests are
reviewed elsewhere. Line numbers refer to the plan.

Counts: 0 blocker, 5 major, 6 minor.

**major**: removes a column, a protocol method, a file or a mechanism. **minor**:
one section or one PR smaller.

## Findings

1. **major** — Decision 4 (66-69), Core 2 (197-200), Core 3 first bullet (202-203),
   Core 4 (210-213), Step 1 tests (257-260). `Speaker.enrolledPersonID` plus migration
   `v3`, `SpeakerRow`, `SampleData` and a backfill "from `personID` where
   `assignment = 'confirmed'`". The backfill is the proof the column is derivable:
   `confirm` is the only writer of `.confirmed` that enrols
   (`MeetingStore+People.swift:126-147`), so "the person the embedding was enrolled
   into" is `assignment.isConfirmed ? assignment.personID : nil` before and after this
   plan. The one other writer, `LaneMerger.meSpeaker` (`LaneMerger.swift:33-41`),
   builds the speaker without an embedding, so there is nothing to withdraw under either
   model. The column also opens a hole the plan does not close: it is declared
   `REFERENCES person ON DELETE SET NULL`, and `mergePersons` deletes the removed
   person row (`MeetingStore+People.swift:70`), so after a cross-meeting merge every
   moved speaker forgets its enrolment while `assignment` (re-pointed by the same
   function) stays right. Fix: no column, no `v3`; `confirm` reads the previous person
   from `assignment`. Same-person no-op becomes
   `if case .confirmed(let current) = speaker.assignment, current == person.id`.
   Lost: nothing.

2. **major** — Decision 4 (66-69), Core 1 (189-196), Core 3 second bullet (204-207),
   Core 4 (210-213), Reviewer trap (318-319). `SpeakerMemory.withdraw` as the inverse of
   a capped running mean, "exact while the person is under the cap, approximate above
   it", implemented twice, plus a `mergeSpeakers` path that withdraws the source. The
   inverse is needed only because `Person.embedding` is an opaque accumulator. The
   speaker rows already keep every embedding that was ever enrolled
   (`speaker.embedding`, never cleared), so the person's voice can be recomputed instead
   of adjusted: `Person.embedding = normalise(mean(embedding of confirmed speakers of
   this person, newest 50 by meeting.startedAt))`, `sampleCount = count`. One private
   `refreshVoice(personID, db)` in `MeetingStore`, called for the old and the new person
   inside `confirm`'s write transaction and for both persons touched by
   `mergeSpeakers`. Consequences: `withdraw` is not needed; `enroll` loses its only
   caller (`confirm`, line 145) and leaves the protocol, so `SpeakerMemory` shrinks to
   `candidates` plus the provided `match`; `confirm` drops the `memory:` parameter;
   `InMemorySpeakerMemory.enrolments` and the enrol tests in
   `CosineSpeakerMemoryTests` move to `MeetingStoreTests` as "confirm A then B leaves A
   as it was"; the write is atomic instead of a store write followed by an actor write;
   Acceptance 3 (305-306) holds exactly at any count. Lost, honestly: the speech plan's
   Enrolment row ("running mean, `n` capped at 50 so a voice can drift") becomes a
   50-sample window, which drifts the same way but is a documented model change;
   `mergePersons`' weighted mean becomes redundant (it may stay, or call `refreshVoice`).
   Re-enrol-only (no withdraw, no recompute) is smaller still but breaks Acceptance 3
   and the "no ghost" goal, so it is not an option. If the author keeps `enroll` as the
   voice model, `withdraw` is the minimal correct addition and the approximate-above-cap
   caveat has to be accepted; finding 1 still applies.

3. **major** — Core 3 (205-207) and Core 4 (212) "the person row is deleted" when
   `sampleCount == 0` and no `speaker`, `participant` or `meetingTask` references them.
   A destructive three-table check in two operations, for a UI symptom ("no ghost in
   Recent"). Fix: define Recent as persons that own at least one confirmed speaker,
   which turns Core 5's left join with nulls-last into an inner join with no null
   clause; a person nobody is confirmed to is simply not recent. Typing the same name
   again finds the ghost through the existing name match in `name(_:_:)` and reuses it,
   which is the right outcome. Lost: a nameless row in `person` visible to `persons()`
   and the CLI. Every FK to `person` is already `ON DELETE SET NULL`, so a later
   cleanup command can remove them without a schema change.

4. **major** — Decision 10 (90-92), App changes `Main/Debounce.swift` (236-237),
   `flushRedeliver` and `onDisappear` (233, 243), Step 2 tests "three changes within
   the debounce…" (275-276), Acceptance 6 (311-312). A 3 s debounce on the injected
   clock, an extracted `Debounce` type that also rewrites the scratchpad path, a flush
   on disappear, and three tests. The vault is a file nobody reads while the popover is
   open; the plan already names the moments that matter (popover closes, meeting
   deselected). Fix: a `dirty` flag set by `assign`/`name`/`merge`; `redeliver` once
   when the header popover or a transcript picker closes (`onChange(of: isPresented)`)
   and in `onDisappear`, only when dirty. No clock, no new file, no scratchpad refactor
   in this plan. Lost: a re-export within 3 s while the popover stays open; a transcript
   rename re-exports per change rather than per burst (each is one local write). If the
   debounce stays, the `Debounce` extraction is a separate refactor of working code and
   belongs in its own commit, not in the app PR of this plan.

5. **major** — Decision 2 (60-62), `SpeakerPicker` (141-143, 157), Popover row
   (130-132), Reviewer trap (320-322). One component with two trigger kinds (field for
   unnamed, name button for named), two presentations (sections rendered inside the
   popover, or in a nested `.popover` from the transcript), a draft-text state, Tab
   between rows, and a hover chevron. Fix: one trigger everywhere, a button showing the
   name or the placeholder, and one presentation, its own `.popover` whose first row is
   the search field. The auto-open for `pendingReviews` opens the first unnamed row's
   list directly, so the common path is still "type, Return". Lost: Tab moving between
   fields inside the popover; one click before typing when the popover was opened by
   hand. Halves the picker's states and removes the "list inside a popover inside a
   popover" layout case from the transcript.

6. **minor** — Decision 5 second sentence (71-73), sections table row "Same voice as"
   (151), `merge(_:into:)` (159), `speaker-option-speaker-<uuid>` (247). Two merge
   mechanisms where Jamie has one: the plan adopts "same name twice" and then also lists
   the meeting's other speakers. Fix: drop the section and the id class for v1; merge
   only through choosing a person who already owns a speaker (which already calls
   `mergeSpeakers`). Lost: merging two unnamed clusters without naming either. The
   scope's "user can merge split speakers" is met by the remaining path.

7. **minor** — Decision 6 second sentence (76-77), Playback (179-185), Step 3
   (282-284), App changes `ClipPlayer` (245). Range playback over the recording with a
   clock-scheduled stop and a 10 s cap, for the case "clip file gone, recording still
   there". After this plan that case is only speakers confirmed by the old `confirm`
   (which deleted clips) and the source of a `mergeSpeakers` whose target already had a
   clip. Fix: hide Play when the clip file is gone; keep `ClipPlayer` as it is. Lost:
   Play on speakers named before the plan ships, on a handful of meetings.

8. **minor** — Decision 9 (85-89), Avatars (171-177), `Theme.avatarHues` (240). Eight
   hues in two appearances with a contrast test, and the plan itself records it as the
   one exception to the redesign's achromatic rule. Fix: initials on `secondary` for
   v1, the same fill unnamed speakers get, glyph versus letters telling them apart. The
   avatar never appears without the name beside it except in the three-wide header
   stack. Lost: colour separation in that stack and a piece of Jamie's look; hues can be
   added as a token array later without touching any call site.

9. **minor** — App changes first bullet (231): `SpeakersViewModel` computes
   `candidates` once per export change. The sections table (147-152) never reads them;
   Suggested comes from `.suggested` on the row, the name suggestion and the attendees.
   Drop the call. With finding 2 the view model has no `SpeakerMemory` dependency at
   all.

10. **minor** — Popover (126-127): rows in two groups, unnamed first, re-sorted with
    `Motion.functional` when a row moves. Every pick moves the row the user just used
    and shifts the one under the cursor. Fix: `clusterLabel` order always; the
    placeholder marks unnamed rows, and the auto-open already lands on the first of
    them. Lost: unnamed on top of a long list.

11. **minor** — Implementation steps (250-286), especially "the app cannot ship step 2
    before step 1 is on `main`" (252). The app links the package by path
    (`apps/macos/project.yml`, `packages.StenoPackage.path: ../..`), so the constraint
    is review size, not tooling. With findings 5 and 7 step 3 is a few lines; two PRs
    (core, app) or one. Design also references `IconButton` (134) and the 88 pt Tags
    label (112) from the unlanded redesign plan while Non-goals says "uses what exists
    today" (36-37); today that is `.buttonStyle(.plain)` with an accessibility label.
    Pick one statement.

## Keep

Already minimal; not worth re-litigating.

- `MeetingStore.confirm` as the single write path for `assign`, `name` and the
  person-owns-another-speaker merge; the view model never sets `.confirmed` itself.
- `recentPersons()` as a store query. `export.persons` is meeting-scoped
  (`MeetingStore+Export.swift:49-57`), so recency cannot be derived from data the app
  already has; one join is the floor (inner, after finding 3).
- Clips outliving `confirm`, and `RetentionSweep` removing them with the audio and
  keeping them under `.keepForever`. Once clips persist, the retention promise needs
  this, and the sweep is the one place that already walks expired assets.
- The excerpt from segments overlapping `sampleClipRange` (pure function, no new
  state) replacing `clusterConfidence` in the UI.
- `SpeakersViewModel` fed by `update(export:)` from the detail view model's single
  `observeMeeting` stream; no second observation, no reload after writes.
- Reusing `AppController.pendingReviews` for the auto-open and clearing it from
  `unconfirmedSpeakers.isEmpty` instead of from a sheet closing.
- The Non-goals list, in particular no People screen and no clip picker change.
- Filtering note: `localizedStandardContains` is already case- and
  diacritic-insensitive, and `compare(_:options: [.caseInsensitive, .diacriticInsensitive])`
  covers the Create-row equality; no folding helper is needed.
