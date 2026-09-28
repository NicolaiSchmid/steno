# Inline speaker assignment: elegance review

Date: 2026-09-28. Reviewer: elegance pass over
`2026-09-28-inline-speaker-assignment.md` (product interaction model and the
code design it implies), against the owner's two screenshots, Jamie's
"Identify speaker" doc, the visual redesign plan's Decisions and token spec,
and the current code in `apps/macos/Steno/Speakers`, `Main` and
`Sources/StenoCore/Storage/MeetingStore+People.swift`. Correctness and tests
are reviewed elsewhere. Nothing below edits the plan.

**major**: shapes the model or the schema; expensive once code compiles
against it. **minor**: one PR.

The plan's one idea is right and is stated in the Goal: "Speakers are named
where they are read", one select, applied on click, everywhere. Most findings
below are places where the old sheet still shows through that idea.

## Findings

1. **major** — Decision 5 (line 70-73), Decision 7 (line 78-80), picker table
   row "Same voice as" (line 151), tag "Speaker 3 in this meeting" (line 145).
   The Merge picker survives as a list section. Jamie's rule, which the plan
   adopts in the same decision, already covers a split voice: pick the same
   person twice and the clusters merge. A section that lists the meeting's
   other clusters is the old "Same voice as / Choose a speaker" control
   relabelled, and it makes the picker offer two kinds of thing (people and
   clusters) with two commit paths (`assign`, `merge`). The one case it
   serves, merging two clusters before either has a name, is rare and still
   possible by naming both. Drop the section and the `merge(_:into:)` API from
   the picker; keep `mergeSpeakers` as the store-level consequence of picking a
   person who already owns a speaker here. Named speakers of this meeting
   still appear in the list as people, tagged "In this meeting". Cost: no
   merge-before-naming; one fewer section, one fewer commit verb, one fewer
   accessibility id (`speaker-option-speaker-<uuid>`).

2. **major** — Decision 7 (line 78-80), `SpeakerPicker` table (line 147-152),
   `sections(for:query:) -> [Section]` (line 232). Four labelled sections plus a
   trailing tag on every row labels each row twice. Jamie's screenshot shows
   one flat list ("Recent Contacts") with avatar and name, nothing else; the
   plan's deviation adds real information (the voice match, the LLM guess, the
   attendees) but pays for it in headers. A flat list ranked by
   voice match, LLM guess, attendees, then recency, with a tag only where it
   changes the decision ("Sounds like", "Mentioned", "Attendee") and Create
   last, carries the same information in one column. The view-model API
   becomes `options(for speakerID:, query:) -> [Option]` where
   `enum Option { case person(Person, tag: Tag?); case create(String) }` and
   `select(_ option: Option, for speakerID:)`; the row model is the command, so
   `assign`, `name` and (after finding 1) `merge` collapse into one verb and
   the picker cannot call the wrong one. Cost: none in behaviour; the test list
   in step 2 reads the same with "section" replaced by "position".

3. **major** — Decision 1 (line 57-59), Header (line 118-121), App changes
   (line 242), Acceptance 1. The popover opens itself when a pending meeting is
   selected, with a field focused. That is the sheet with the chrome removed:
   it takes focus from whatever the user came to read, and `pendingReviews`
   keeps a per-meeting "already auto-opened" flag alive in the view. Jamie
   never opens anything; the header row is the affordance and the doc says
   "hover over the Speakers list". The redesign plan's list badge plus the
   row's unnamed avatars are enough of a nudge, and a first-run onboarding
   line can say where speakers are named. Remove the auto-open;
   `pendingReviews` drives the badge and nothing else; `reviewCompleted` still
   fires from `onChange(of: unconfirmedSpeakers.isEmpty)`. Cost: one less
   prompt on the first processed meeting, which the owner's feedback about
   modal interruptions argues for anyway.

4. **major** — `SpeakerPicker` (line 155-160), Deferred (line 331-333).
   "Return on an untouched field accepts the voice match" commits something
   the field does not show and enrols the voice into that person. With Tab
   moving between fields, a Tab-Return habit names speakers nobody looked at,
   and the plan's own repair path (reassign, withdraw) is invisible. Jamie
   avoids this by writing the recognised name into the field, so Return
   confirms what is on screen. Either do the pre-fill now (select-all on
   focus, so one keystroke overrides, which answers the deferred item's
   objection), or highlight nothing until the user types or presses Down, so
   Return on an empty untouched field is a no-op. The first is closer to Jamie
   and to the "nothing hidden" idea. Cost: one deferred item moves into scope;
   Acceptance 1 loses "the voice match highlighted".

5. **major** — Core change 2 (line 197-200), Core 3 and 4 (line 201-213),
   step 1 tests. `Speaker.enrolledPersonID` is a second source of truth beside
   `SpeakerAssignment`, which was introduced (2026-09-25 elegance review,
   finding 4) precisely so one field could not drift from another. The column
   is written in the same transaction as `.confirmed(personID:)`, before
   `memory.enroll` runs, so it records intent, not enrolment, and it can only
   ever equal the confirmed person id or be stale. Its one job, "which person
   do I withdraw from", is answered by the previous `assignment`: `confirm`
   withdraws `speaker.embedding` from `old.personID` when `old` is
   `.confirmed`, and `mergeSpeakers` does the same for the source. After any
   `mergeSpeakers` the kept embedding is an average, so `withdraw` is
   approximate there whichever field records it; the column buys nothing
   exact. Drop the column, migration `v3`, the `SpeakerRow` and `SampleData`
   changes and the backfill test. Cost: none; the "confirm failed after the
   row write" gap is exactly today's.

6. **minor** — Core change 1 (line 189-196), Reviewer trap (line 318-319).
   `withdraw` is a fair protocol addition, but the smell it inherits is that
   `enroll` is already the same running-mean math written twice
   (`CosineSpeakerMemory.enroll`, `InMemorySpeakerMemory.enroll`, and they
   differ: one caps `sampleCount`, the other lets it grow). Adding the inverse
   to both doubles the place a sign error can hide. Put the math on the model
   once, `Person.enrolling(_ sample: Embedding, cap: Int) -> Person` and
   `Person.withdrawing(_:cap:) -> Person` next to `Embedding.weightedMean`,
   with the inverse test on those pure functions; both memories become
   load-apply-save. `SpeakerMemory` keeps `enroll` and `withdraw` as the
   persistence seam. Naming is fine: `withdraw(_:from:)` reads against
   `enroll(_:as:)` and the codebase already uses "withdraw" for a Bonjour
   record. Cost: one small refactor in step 1.

7. **minor** — Core change 3 (line 205-207). `confirm` deleting a person as a
   side effect ("a mistyped name leaves no ghost in Recent") gives one store
   operation five effects. The ghost is a query problem: `recentPersons()`
   omits persons with `sampleCount == 0` and no `speaker`, `participant` or
   `meetingTask` reference. Rows stay; nothing is deleted by a naming action.
   Cost: unreferenced rows accumulate (bytes), cleaned by a later People
   screen or `steno dev db`.

8. **minor** — Decision 10 (line 90-92), App changes (line 228-237,
   `flushRedeliver`, `Main/Debounce.swift`). The debounce itself is right (the
   vault is a mirror, the names already changed on screen at click time, and
   the footer's `observeDeliveries` shows the receipt change, which is
   indication enough; a spinner for a 3 s background sync would be noise).
   What is misplaced is the owner of the clock.
   `MeetingDetailViewModel.reexport()` is documented as "the only re-export
   entry point" and already owns the clock, the pipeline handle, the scratchpad
   debounce and the `onDisappear` flush. A child view model that also calls
   `pipeline.redeliver` gives the detail screen two delivery clocks and two
   flush calls. Let `SpeakersViewModel` expose `onChange: () -> Void` (or the
   parent observes `rows`), and let the parent run one `Debounce` for the
   redeliver beside the scratchpad one. Cost: `flushRedeliver` moves up one
   level; the step 2 debounce tests move to `MeetingDetailViewModelTests`.
   Name nit: the codebase names things by what they are (`ClipPlayer`,
   `RetentionSweep`); `Debouncer`, not `Debounce`.

9. **minor** — Header (line 114-117). The "n unnamed" chip in `warning` on a
   12 % veil is a status chip inside a select trigger. Jamie's row is avatars,
   names, "+4", chevron, nothing else; the unnamed speakers are visible as the
   glyph avatars in the stack. Say it in the name list instead ("Nicolai,
   Thomas, 2 unnamed" in `faint`) and keep the `warning` hue for the list badge
   the redesign plan owns. This also keeps the header's hue count at "avatars
   only" (finding 11). Cost: none.

10. **minor** — Popover (line 130-133), Decision 8. The excerpt earns its
    space on an unnamed row: the clips are 3 s and two lines of text answer
    "who is this" faster than Play. On a named row it is dead weight; Jamie
    shows named rows as name · email and nothing under them. Seven named
    speakers with two-line excerpts is a 450 pt popover of prose. Show the
    excerpt only under a field (unnamed row, or a named row whose picker is
    open). Cost: `Row.excerpt` stays; the view decides.

11. **minor** — Decision 9 (line 85-89), Avatars (line 171-177). Eight hues is
    a deliberate exception to the redesign plan's Decision 3 ("the one hue
    budget goes to status") and the plan is honest about it. It is the right
    call: an avatar stack of three 20 pt "N T J" initials cannot be told apart
    without hue, and Jamie leans on it. Two things make it sit well. First,
    the plan says it replaces F18, F19 and F28 but not Decision 3's sentence;
    add the amendment there ("identity hues on avatars are the second use of
    hue") so the rule stays true after this lands. Second, finding 9: with the
    `warning` chip gone the header has exactly one coloured element class.
    `Theme.avatarHues` as a tested token array with the 4.5:1 bound is the
    right shape. Cost: one line in the redesign plan.

12. **minor** — Popover (line 126-127). Regrouping rows into unnamed-then-
    named "animated when a row moves" makes the row you just named jump below
    the ones you have not, so the next unnamed row slides into the spot you
    were reading. The trigger style already says which rows are settled.
    Stable `clusterLabel` order, no regroup, no motion. Cost: none; one
    `Motion.functional` call fewer.

13. **minor** — naming across the plan. "Person" everywhere, including the
    placeholder, matches `Person` and `persons()`; good that "contact" from
    Jamie did not leak in. `recentPersons()` fits beside `persons()` and
    `MenuBarViewModel.recent`. `SpeakersViewModel`, `SpeakersRow`,
    `SpeakersPopover` (the set) versus `SpeakerPicker`, `SpeakerExcerpt` (one
    speaker) is a consistent plural rule. `SpeakerExcerpt` "(pure, tested)" is
    an enum of functions like `TranscriptTurns`; name it so it cannot be
    mistaken for a view (`SpeakerExcerpts.text(for:in:)` or fold it into
    `TranscriptTurns`). `Avatar` in `Design/` matches `StatusChip`, `Card`.

## Keep

- Decision 3 and `update(export:)`: selection goes through `confirm`, the
  UI re-renders from `observeMeeting(id:)`, and the child view model is fed by
  its parent instead of reloading speakers and people itself. This deletes
  `reload()`, `didChange`, `skipped`, `draftNames`, `mergeTargets` and
  `finish()` and leaves one truth. It is the cleanest ownership in the app.
- Decision 5's first sentence: "same person twice merges". Jamie's rule as a
  store consequence, no Merge control, no picker. (Findings 1 and 2 remove
  what was added around it.)
- Decision 6 and Core change 6: clips outlive confirmation and follow the
  audio's retention. One lifecycle rule replaces two, and the play button
  stops lying after a name is set.
- Decision 8: the excerpt replaces `clusterConfidence`. A number nobody could
  act on becomes text anyone can, at zero new data.
- The two trigger styles (field for unnamed, name for named) are one
  component with one list and one keyboard model, and they match Jamie's
  screenshot exactly. This is not a leak of the old model; keep it.
