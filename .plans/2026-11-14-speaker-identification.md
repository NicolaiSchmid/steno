# Speaker identification: calm like Jamie, honest about voices
**Status:** decided 2026-11-15 (S1–S5 at the recommendations); building in four pull requests on top of #348

Builds on #348 ("name speakers in place, from one header button", branch
`feat/speakers-ui`), which moved naming onto Jamie's interaction model. Two
independent audits (GPT-6-Astra and Claude Fable 5.1,
`artifacts/speaker-identification/review-history.md`) scored #348 at 5.5 and
5, and Jamie's speaker UX at 7.5 from both. Jamie wins on calm and speed,
not on capability: it has no suggestion handling, no Me, and moves single
paragraphs only for "Unknown Speaker". Steno has the richer model and buries
it under more controls. Visual rules (opaque panels, Geist, the colour rule,
contrast) come from `.plans/2026-11-14-design-contrast-and-calm.md`.

## What is wrong with #348

1. **The header does not look like a task.** "1 needs a name" is a grey
   ghost button beside the tags (`speakers-popover.tsx:147-156`).
2. **Naming hides the evidence.** While the field is open the speaker's
   label, quote and play button are gone (`speakers-popover.tsx:340`); the
   quote is picked from the transcript independently of the clip
   (`speaker-names.ts:43`).
3. **Too many controls per row.** A suggested row carries name, "Suggested",
   Confirm, pencil and play (`speakers-popover.tsx:286-325`); email sits
   beside every confirmed name.
4. **Merging is hidden.** Confirming a dimmed duplicate, or picking a person
   from "In this meeting" who already owns another speaker, merges the two
   (moves segments, averages the voice, deletes the row:
   `people.rs:158`, `people.rs:530-538`), with no label and no undo.
5. **Three words for one suggestion.** The row says "Suggested", the host
   "Sounds like" (`steno-host/src/speakers.rs:41`), the fixtures "Suggested,
   87% match" and `Add "Anna Berger"`, which the host never sends
   (`host.rs:1709-1722`). The screens show copy the app does not.
6. **Enter changes meaning with timing.** Typing "Ann" and pressing Enter
   before the debounced answer creates Ann instead of picking Anna
   (`speaker-name-field.tsx:105,132`).
7. **No saving or failure state.** A pick closes the field at once, the row
   still says "Name Speaker 1" until the host republishes, advance can wrap
   back to it, and a rejected pick reaches only the console
   (`speaker-name-field.tsx:227-230`).
8. **Focus and keyboard stop halfway.** Escape drops focus
   (`speaker-name-field.tsx:265-270`); each row is five tab stops; Home and
   End move the highlight instead of the caret (`:117`).
9. **The transcript overstates and duplicates.** "Click to rename" assigns
   every turn of the speaker, neither renames the person nor fixes the
   paragraph (`speaker-picker.tsx:74-94`); a suggested speaker's name reads
   as fact; unmapped turns have no picker (`transcript-tab.tsx:101`); every
   unnamed turn repeats a "Play" link (`:113-119`).
10. **Visual noise.** Repeated headings in the empty state, the 64 % dimmed
    duplicate (`:243`), illegible dark metadata, a glass panel the transcript
    shows through, options without avatars.

## Taken from Jamie, and not

Taken: one pill in the header, amber only while work is left; rest rows that
are a name and one icon; the dashed chip, quote and clip for unknowns; a
nested duplicate on a left rule; the speaker's label as the field's
placeholder; advancing through unknowns only; showing the pick at once and
reverting on failure; focus back to the row; one tab stop per row; bounded
scrolling; loading and empty states.

Not taken: the second popover sliding out sideways (an in-row list fits 368
px better); controls shown only on hover; the calendar upsell footer;
white initials on hashed colours (contrast); trusting voice matches
silently. Jamie can hide its matcher because it is server-side and
workspace-wide. Steno's voice memory is local and learns from each
confirmation (`people.rs:238-241`), so an accept or reject is both honest
and useful.

What both miss and Steno adds: three separate things, a **voice** (a
cluster in this meeting), a **person**, and **who said one paragraph**, each
with its own action; a way to dismiss a voice that is not a person; and
undo.

## Target design

Each element below names the step that delivers it (see Steps).

### Header pill

One control left of the tags, 28 px, fully rounded, 13/500:

| State | Look | Step |
|---|---|---|
| Finding speakers (the progress topic reports the Diarize stage for this meeting, which the host already labels "Finding speakers", `labels.rs:27`) | "Finding speakers…" with a spinner, disabled | 2 |
| Nobody named and nothing suggested | dashed outline, "Name speakers" | 2 |
| Any voice unnamed (and at least one named or suggested) | amber tint and text, up to two avatars, a "?" count, "2 need names" | 2 |
| Only suggestions left | neutral, avatars, "Check 1 name" | 2 |
| All confirmed | avatars and first names, muted, `+N` past four | 2 |
| No speech | no pill | 2 |

Dashed wins only when there is nothing on screen yet to anchor the amber
state; once anything is named or suggested, open work is amber.

### Panel

Opaque, 368 px wide, at most 440 px tall with its own scroll. Sections in
this order, each hidden when empty, no heading repeated (S1). Rows do not
move while the panel is open: a voice confirmed during this visit stays in
its section, marked done, until the panel is closed and opened again.

1. **Is this right?** Voice matches. Row: avatar, name, the muted reason
   ("Sounds like Anna"), a line the voice said and play, then **Yes** and
   **Change**. Yes confirms. Change opens the field. From step 3, **No**
   replaces Change: it records a rejection and opens the field with Anna
   removed. No "Yes to all": every confirmation sees its own evidence.
2. **Need names.** Unknowns. Row: "?" avatar, the dashed chip "Speaker 1",
   a line the voice said at 12 muted, play. The whole row opens naming.
3. **Speakers.** Confirmed people: avatar, name 14/500, play. Me reads
   "Nicolai" with a muted "You" and no play. No email.

Unnamed Me is one line above all sections: "You spoke on the mic. Name
yourself", and naming opens there.

"A line the voice said" is today's transcript snippet (`speaker-names.ts:43`)
in step 2 and the clip's own words from step 3, when it may be called the
clip's quote.

Two confirmed voices of one person (old data, or a merge that predates step
3) show as the person's row with each voice nested beneath it on a 1 px left
rule, each with its own line and play, so either can be corrected on its own.

### Naming

- The speaker stays on screen: avatar, label and play keep their line, the
  line they said keeps its line, and the field opens under them with the
  label as placeholder ("Speaker 1"). The avatar does not preview candidates.
- Options under the field, avatars on every person. Groups:
  - **Suggested**: voice match ("Sounds like"), mentioned ("Mentioned"),
    calendar invitee ("Invited"), and from step 3 "Last invitee left" when
    one unnamed voice and one unassigned invitee remain (a host
    computation; today's tags stop at Attendee, `speakers.rs:30-35`); the reason right-aligned and
    muted. Only "Sounds like" is voice evidence and only it carries a
    strength word (S4);
  - **Also speaking here**: people who already own another voice in this
    meeting (known from `DetailSpeaker.personId` in the snapshot), each row
    saying what happens: "Nicolai · combines with Speaker 2";
  - **People** ("Recent" while the field is empty);
  - `Create "Ann"`, always its own row.
- Enter acts only on the row that is highlighted and visible. While the
  options for the typed text are still loading, the list shows "Searching…"
  and Enter does nothing; nothing is chosen that the user has not seen. If
  the options fail to load, the list says "People could not be loaded" and
  keeps the `Create "Ann"` row, highlighted, so Enter creates visibly.
- A pick shows the name at once with a muted "Saving" mark. The field moves
  to the next unknown (never to a suggestion, never wrapping) when the host
  acknowledges the pick, not before, so a failure never interrupts typing in
  the next row. On failure the row reopens with its text and a red inline
  line with Retry. The same rule covers Yes, No, Same person and combining
  picks; a combining pick reconciles by removing the merged row from the
  next snapshot.
- Escape closes the field and returns focus to the row; a second Escape
  closes the panel and returns focus to the pill.
- From step 3, "Remove name" in a confirmed voice's options clears that
  voice's assignment in this meeting. For Me it clears the meeting's
  assignment and the meeting's Me participant reverts to unnamed Me (the
  participant `confirm_speaker` named, `people.rs:530-538`); who you are
  (`self_person_id`) is changed in Settings.

### One person, two voices

A suggested duplicate nests under its person on a 1 px left rule, at full
contrast: "Also Nicolai?", the line it said and play, and **Same person** /
**Someone else**. Someone else opens the field with Nicolai excluded.

Whether a pick combines is decided by the host, not by the option's group:
`Create "Anna"` or a Suggested pick can resolve to an Anna who already owns
a voice. From step 1, `speakers.select` takes `combine: true|false`; without
it the host refuses a pick that would combine and replies which voice it
would join. Every combining action is destructive until step 4 and confirms
first, as the design plan requires:
"Combine Speaker 3 with Nicolai? Their paragraphs become Nicolai's. This
cannot be undone." with a red **Combine** button. From step 4 the confirm
goes and the result toast offers **Undo**; the combined person's row offers
"Separate again" for each nested voice.

### Transcript

- The speaker name on a turn opens a small panel: that turn's own audio
  from step 4 (the voice's clip before), then two scopes as a segmented choice,
  **This paragraph** and **All 12 paragraphs of Speaker 2**, then the same
  field. All is explained under the choice: "Names this voice Anna in this
  meeting. Anna's name is not changed." The scope preselects All while the
  voice is unnamed and This paragraph once it is confirmed; a suggested
  voice counts as unnamed. The choice is not remembered between turns (S2).
- Until step 4 only All exists, and the panel shows no scope choice.
- Turns without a voice get the panel with only This paragraph (step 4).
- A suggested speaker's name carries a small "?" and the tooltip "Sounds
  like Anna. Click to confirm or change". The per-turn "Play" link goes;
  play lives in the panel.
- Renaming a person everywhere is not offered; it belongs to a people view
  (out of scope).

### States and copy

"Type a name. Steno remembers their voice next time." for a first meeting
with no people; no play icon when a voice has no clip; "Searching…" and
"People could not be loaded" in the field; **Not a person** (step 3, S5) in
an unknown row's options for music or noise. A dismissed voice leaves the
counts and sections and is listed at the bottom of the panel under "Not
people" with **Restore**; its turns keep their text and read "Not a person"
in the transcript. Every result is announced; Undo comes as a toast,
failures inline in red.

### Keyboard

Enter or Space on the pill opens the panel on the first row with work. Up
and Down move between rows; each row is one tab stop. On a row: Enter names,
Y confirms, N rejects (Change in step 2), P plays or stops. In the field:
Up, Down, Enter, Escape; Home and End move the caret; an input method's
composing Enter is left alone; Tab leaves the field for the next row. Play
while naming is P after Escape, or the mouse.

## Steps

Each step is one pull request on top of #348, with `pnpm screens` for every
speaker state in light and dark, the keyboard walk, and focused tests for
each transition (pick, failure and revert, combine, undo, restore). Each
waits for Nicolai's OK. Every contract change keeps the fixtures round-trip
byte for byte in the Rust fixture tests, the host parity test and the Swift
`BridgeSamples`.

1. **Host errors, the combine guard and honest fixtures** (contract, both
   hosts, fixtures; no web design change, no schema change). Small and first.
   - A failed `speakers.select` replies as an error: today
     `SpeakersViewModel::select` turns store errors into model state and the
     bridge replies success (`host.rs:2026-2047`, `speakers.rs:515-516`;
     `SpeakersViewModel.swift:138` in Swift).
   - `speakers.select` gains `combine`; without it a pick that would join
     another voice is refused with a `wouldCombine` reply naming that voice.
   - The fixtures are re-recorded with the copy the hosts send: no "87%
     match", no `Add "Anna Berger"`. The `unknown` option kind ("Leave
     unnamed"), which the hosts never emit and `host.rs:1687` rejects, leaves
     the contract. The tests that pin the old copy change with them:
     `crates/steno-host/tests/speakers.rs:213`, the documented fixture
     exception in `tests/parity.rs:927`, `BridgeSamples.swift:432-435`,
     `transcript-tab.test.tsx:27-29`. The `unknown` assignment stays: it is a
     real state.
2. **The interaction** (web only, on step 1's contract). The pill, the
   panel, naming, the keyboard and the transcript marker as marked step 2
   above, "Finding speakers…" from the progress topic, combining behind its
   confirm.
3. **Evidence, rejection, dismissal, clearing** (contract, both hosts, an
   append-only migration in SQL mirrored in `Migrations.swift` and the Rust
   `.sql` files).
   - Options carry a reason kind (voice, mentioned, invited, last invitee)
     and, for voice, a strength word, "strong" or "likely", from thresholds
     measured on the calibration corpus and written into the pull request;
     below the lower threshold no voice suggestion is shown. No strength is
     shown until the thresholds are measured.
   - New table `speakerSuggestionRejection` (meeting, speaker, person): a
     rejected person is not suggested again for that voice, and a rejection
     never touches voice memory.
   - New column `speaker.dismissed` for Not a person, separate from the
     `assignment` column, which both stores parse into a closed three-case
     enum (`store/people.rs:87`, `People.swift:184`); a fourth value would
     break the Swift app's reads. A dismissed voice is excluded from voice
     memory.
   - Clearing an assignment as a store operation in both stores, beside
     `confirm_speaker`, which is the only operation that confirms today.
   - The host computes "Last invitee left".
   - The clip's own words come with the clip.
4. **Paragraph moves, paragraph audio and reversible combining** (contract, both hosts, a
   mirrored migration).
   - A turn is the run of segments the snapshot folds together
     (`main_window/snapshots.rs:302-311`), and its ID is only the first
     segment's. `transcript.setTurnSpeaker` therefore carries the turn's
     segment IDs, each with the speaker ID it shows; the snapshot has no
     revision, so the host rejects the move as stale if any segment's
     speaker differs. The destination is an existing voice in the meeting,
     or a new voice row created from those segments: label the next free
     "Speaker N", confidence 0, no embedding and no clip, so it does not join
     voice memory until confirmed. A voice left with no segments is deleted
     with its clip. Moving segments changes no voice's embedding, since voice
     memory is kept per speaker row.
   - `transcript.playTurn` plays the turn's span from the meeting's audio,
     at most 60 seconds, in both hosts. When the recording is gone (not
     kept, or past its deletion date) the panel shows no play.
   - Combining writes a journal row before it averages and deletes, with
     both rows' prior state: the source speaker's embedding, confidence,
     clip, assignment and segment IDs, and the target's embedding,
     confidence and clip. The merge keeps the source's clip file while the
     journal row exists, instead of handing it back for removal; once the
     meeting's recording is gone the clip may be too, and Separate again
     then shows no play. Undo and Separate again restore both rows from the
     journal, return the segments, recompute the person's voice and
     re-export the meeting. They are refused as stale if any journaled
     segment has since moved, and later merges into the same person undo
     latest first. Both stores
     write and read the journal (`people.rs:158-162`,
     `MeetingStore+People.swift:171-179`). Merges from before the migration
     have no journal and show no Separate again. Reprocessing a meeting
     drops its journal.

## Decisions for Nicolai

- **S1: section order.** Recommendation: work first (Is this right?, Need
  names), confirmed people last. Both reviewers agree.
- **S2: transcript scope.** Recommendation: the segmented choice,
  preselected All for an unnamed or suggested voice and This paragraph for a
  confirmed one. Astra prefers always This paragraph; Fable accepts this.
- **S3: undoable combining.** Recommendation: yes (step 4). Until then,
  every combine confirms and says it cannot be undone.
- **S4: suggestion strength.** Recommendation: two words for voice matches
  only, from measured thresholds, nothing until measured, never a
  percentage.
- **S5: Not a person.** Recommendation: yes, as a separate column, listed
  under "Not people" with Restore, excluded from voice memory, transcript
  text kept.

## Out of scope

- A people view (rename a person everywhere, forget a voice, merge people).
- Splitting one voice into two inside a meeting beyond moving single
  paragraphs.
- Live naming while recording.
