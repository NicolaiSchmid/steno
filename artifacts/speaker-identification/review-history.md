# Speaker identification: council review history

Plan: `.plans/2026-11-14-speaker-identification.md` (written after the
baseline audit). Reviewers do not read this file. Each round is a fresh task
with the brief, the screens, that reviewer's own earlier findings and the
responses; never a target score or the other reviewer's findings.

## 2026-11-14 — baseline audit of PR #348 against Jamie (round 1)

Request IDs `steno-speaker-baseline-astra-r1`, `steno-speaker-baseline-fable-r1`.
Evidence: #348 checked out at `origin/feat/speakers-ui`; its 20 speaker
screens (light and dark); the Jamie speaker teardown extracted from the
2026-11-14 bundle. Both kept outside the repository.

**GPT-6-Astra: #348 5.5/10, Jamie 7.5/10 (provisional, from the bundle).**
Ranked problems in #348:
1. Naming hides the evidence: in the editing state the speaker label, quote
   and play disappear (`speakers-popover.tsx:340`); the quote is chosen from
   the transcript independently of the clip (`speaker-names.ts:43`).
2. "Confirm" on a split row hides a destructive merge (moves segments,
   averages embeddings, deletes the row, `people.rs:158`); the row looks
   settled and dimmed; no undo (`speakers-popover.tsx:243`).
3. Transcript "Click to rename" assigns the whole speaker, not the paragraph
   or the person; unmapped turns have no picker (`speaker-picker.tsx:74`,
   `transcript-tab.tsx:101`).
4. Enter before the debounced results creates "Ann" instead of picking Anna;
   assignment advances before success; failures only reach the console
   (`speaker-name-field.tsx:105,132`).
5. "In this meeting" mixes voice guesses, mentions and invitees; the reason
   is hidden until Change; the candidate's avatar replaces the speaker's
   (`speaker-names.ts:152`). "87 % match" is fixture copy; the host says
   "Sounds like" (`steno-host/src/speakers.rs:124`).
6. Visual discipline: repeated labels in `none`, five controls on one line
   in `partly`, illegible dark metadata, the glass panel lets text through.
7. Keyboard stops halfway: no roving rows, Escape loses focus, Home/End
   hijack the caret, auto-advance ignores pending assignments
   (`speaker-name-field.tsx:117`, `speakers-popover.tsx:120`).

Take from Jamie: whole-row interaction, density, clip-linked quote, bounded
scrolling, loading and empty states, focus restoration, rollback-aware
advance. Not: the sideways second popover, hover-only controls, calendar
upsell, silently trusted matches, implicit merging, ambiguous "rename".

Target design proposed: one header control ("Name speakers", "2 to review",
names and +N; amber for open work); an opaque ~400 px panel, unresolved
first, evidence (label, clip quote, time, play) kept above the field;
"Sounds like Anna" with Confirm Anna / Choose someone else; groups
Suggested, People in this meeting (confirmed only), Recent people; explicit
`Add "Ann"`; Enter acts on the current visible selection only; advance after
success. Split: "Speaker 3 · Possibly also Nicolai", both samples, Combine
with Nicolai, then Undo / Separate again. Transcript: paragraph scope by
default, a separate "Identify all Speaker 2 paragraphs (N)"; rename
everywhere separate. Me: "Nicolai · You", or "You · Add your name". States:
identifying, no speech, no matches, sample unavailable; Leave unnamed;
Remove assignment; inline failure with Retry; Undo toast. Full keyboard
model. Needs bridge and store work: paragraph assignment, clearing,
reversible combine, structured suggestion evidence.

**Claude Fable 5.1: #348 5/10, Jamie 7.5/10.** Jamie wins on calm, not
capability. Ranked: the trigger reads as metadata, not a task
(`speakers-popover.tsx:147-156`); four controls per suggested row
(`:286-325`); three vocabularies for a suggestion, and fixtures with copy the
host never sends (`speakers.options.reply.json` vs `host.rs:1709-1722`); the
split row is the noisiest, dimmed to 64 % (`:243`); picking from "In this
meeting" silently merges (`speakers.rs:214-218`, `people.rs:530-538`); no
optimistic or error state (`speaker-name-field.tsx:227-230`); Escape drops
focus, five tab stops per row; a wrong voice match reads as fact in the
transcript (`speaker-picker.tsx:87-94`); play twice per turn
(`transcript-tab.tsx:113-119`); options without avatars, email as noise; no
per-paragraph reassignment (`contract.ts:428`). Both miss: not-a-person,
one-click resolution when counts line up, uncertainty in the transcript,
split or move. Target: four pill states incl. "Check 1 name"; sections
Speakers, "Is this right?" (Yes/No, strength words), Need names; field at
the chip, groups Likely / Also speaking here ("joins with Speaker 2") /
People / Create; optimistic with revert; advance to unknowns only;
"Also Nicolai?" Same person / Someone else; transcript "Rename everywhere"
plus "Only this paragraph…" (new `transcript.setTurnSpeaker`); "?" marker on
suggested names; Me line; states; keyboard with Y/N/P.

### Responses: plan written the same day from both audits

`.plans/2026-11-14-speaker-identification.md`. Where they differed:
- Section order: work first (Astra), confirmed last; S1 for Nicolai.
- Evidence while naming: Astra's (label, quote and play stay; field below),
  with Fable's placeholder = label; no avatar preview.
- Enter: neither stale creation (Astra) nor "unanswered text creates"
  (Fable); Enter waits for the pending answer, then acts on the highlight.
- Saving: Fable's optimistic display and advance, with Jamie's reopen on
  failure and Astra's inline Retry.
- Transcript scope: a segmented This paragraph / All N, preselected by
  whether the voice is named; S2.
- Combining: Fable's Same person / Someone else, Astra's Undo and Separate
  again behind a migration; S3.
- Strength: Fable's words, Astra's no percentage; thresholds from the
  calibration corpus; S4.
- Three pull requests: web only; evidence, reject, not-a-person and clear;
  paragraph moves and reversible combining.

## 2026-11-14 — plan round 2

Request IDs `steno-speaker-plan-astra-r2`, `steno-speaker-plan-fable-r2`.

**GPT-6-Astra: plan 7/10.** Missed or reversed: advance after success (plan
advances at once); paragraph-only is conditional; "No" records a rejection
where "Choose someone else" did not. Problems:
1. Step 1 cannot be web only: `SpeakersViewModel::select` swallows store
   errors and `speakers_select` reports success; add host error propagation,
   keep optimistic display, advance only after acknowledgement.
2. A turn is several segments; `snapshots.rs:291-315` uses the first
   segment's ID as `turn.id`. Define the segment set, stale-snapshot checks,
   and how a new person gets a destination voice without whole-cluster
   learning.
3. Undo needs more than provenance: combining averages embeddings, deletes
   the source speaker, its suggestions and clip. Preserve records,
   embeddings, assignments, clips; recompute person memory; define
   reprocessing vs Separate again; re-export; both hosts.
4. Step 1 ships irreversible combining without the destructive confirm the
   design plan requires, on every merge path.
5. Suggestion rows lack quote and play before Yes; "Yes to all" compounds
   it; rename "Likely" to "Suggested" (mention and last-invitee are not voice
   evidence); paragraph corrections should offer that paragraph's audio.
6. Enter "waits then acts" can commit an unseen choice; "Enter still
   creates" on load failure contradicts highlight-only.
7. Recovery: where a dismissed voice is restored; correcting two collapsed
   legacy voices; clearing Me vs `self_person_id`; step 2 needs a mirrored
   append-only migration; focused tests for transitions and undo.
S1 agree; S2 prefer always This paragraph in the transcript; S3 strong yes,
make it a prerequisite; S4 only voice evidence, omit until calibrated (line
131 mislabels S4); S5 agree with restore path and no learning.

**Claude Fable 5.1: plan 7/10.** Interaction design right; sequencing wrong.
Partly: optimism only for picks, not Yes, No, Same person or combining picks.
Problems: step 1 needs commands it says it does not (No, Not a person, Remove
name, Yes to all, Finding speakers); re-recording fixtures touches Rust and
Swift tests, and the fixture's "Leave unnamed" is never emitted; step 2 needs
a migration (rejections, dismissal); Not a person must be a separate column,
not a fourth `assignment` value (`people.rs:87`, `People.swift:184`); turn
IDs are the first folded segment's (`snapshots.rs:302-311`), a move needs
the segment IDs and staleness, a voiceless turn needs a new voice; Undo must
keep the source row's embedding, clip, confidence and segments, be scoped
to post-migration merges, and the Swift merge must journal too; Yes to all
can merge silently (`people.rs:560-572`); dashed vs amber contradiction;
rows reorder while typing; step 1 calls the transcript snippet the clip's
quote; Remove name cites S4 and has no store operation; Tab vs one stop per
row; the teaching claim implies per-segment embeddings. S1 agree; S2 accept
the hybrid if not remembered; S3 yes, scoped; S4 agree, thresholds in the
PR; S5 yes as a column.

### Responses (plan revised the same day)

Accepted from both:
- Each element marked with its step. Step 1 adds host error replies for
  `speakers.select` (Rust and Swift) and honest fixtures (drops "87%",
  `Add "Anna Berger"`, "Leave unnamed"; keeps the real `unknown`
  assignment); no store change. Step 1 has Yes and Change; No, Not a person,
  Remove name and Finding speakers move to step 2.
- Advance after acknowledgement, optimistic display kept; the same rule for
  Yes, No, Same person and combining picks.
- Enter acts only on a highlighted, visible row; "Searching…" while loading
  and Enter does nothing; on load failure the Create row is highlighted.
- Suggestion rows show their line and play before Yes; "Yes to all" removed.
- "Likely" renamed "Suggested"; only "Sounds like" carries strength.
- Every combine confirms with a red Combine and "cannot be undone" until
  step 3; then Undo toast and Separate again.
- Step 2 migration: `speakerSuggestionRejection` table, `speaker.dismissed`
  column, clear as a store operation in both stores, speaker-stage progress
  in the snapshot, clip words. Me clearing touches the meeting only.
- Step 3: `setTurnSpeaker` carries segment IDs and the snapshot revision,
  stale rejected, new voice row without embedding for voiceless turns;
  combine journal in both stores with embedding, confidence, clip, segments;
  Undo only for post-migration merges; reprocessing drops the journal;
  re-export after reversal.
- Pill: dashed only with nothing named or suggested; otherwise amber.
- Rows stay put while the panel is open.
- Step 1's line is "a line the voice said", not the clip's quote.
- Two-voice rows nest each voice with its own play for separate correction.
- Not-people list with Restore. One tab stop per row; Tab leaves the field.
- Transcript panel plays the turn's own audio; All explains it does not
  rename the person; scope choice not remembered.
- Teaching sentence corrected: voice memory is per speaker row.

## 2026-11-14 — plan round 3

Request IDs `steno-speaker-plan-astra-r3`, `steno-speaker-plan-fable-r3`.

**GPT-6-Astra: plan 8/10.** Mostly resolved; combining and Undo partly;
still prefers always This paragraph (S2). Remaining:
- Undo journals only the source; the merge also changes the target's
  embedding, confidence and sometimes clip. Keep both rows' prior state.
- Separate again returns segments unconditionally; define conflicts with
  later paragraph moves and merges (reject stale or keep later edits).
- Combine confirmation must follow the host's resolved operation: `Create
  "Anna"` after a search failure, or a Suggested/Mentioned option, can
  resolve to an existing owner and merge through `confirm_speaker`.
- Paragraph playback has no step or contract: today's command plays a
  speaker sample. Assign a step; bounded playback, both hosts, missing or
  expired recordings.

**Claude Fable 5.1: plan 8/10.** Buildable except two gaps. Fixtures partly:
name the tests that pin old copy (`steno-host/tests/speakers.rs:213`,
`tests/parity.rs:927`, `BridgeSamples.swift:432-435`,
`transcript-tab.test.tsx:27-29`), and dropping "Leave unnamed" leaves the
`unknown` option uncovered while `host.rs:1687` rejects it. Remaining: the
snapshot has no revision (`snapshots.rs:413-445`); a new voice row needs
`clusterLabel` and `clusterConfidence` (NOT NULL, `001_v1.sql:17`) and an
emptied voice needs handling; the progress topic already carries "Finding
speakers" (`snapshots.rs:141-153`, `labels.rs:27`); clearing Me must say
whether the Me participant reverts (`people.rs:530-538`); merges hand the
source clip back for removal, so "the file is kept" is wrong; "Last invitee
left" has no host logic (`speakers.rs:30-35`); step 1 must say "already owns
another voice" comes from `DetailSpeaker.personId`; step 1 is large, split
the host error reply and fixtures off first.

### Responses (plan revised the same day; council stopped here)

Accepted from both:
- Four steps: (1) host error replies, a `combine` flag on `speakers.select`
  with a `wouldCombine` refusal, honest fixtures, the `unknown` option kind
  removed, and the four pinning tests named; (2) the web interaction;
  (3) evidence, rejection, dismissal, clearing, last invitee; (4) paragraph
  moves, paragraph audio, reversible combining.
- Combine confirmation decided by the host's resolved operation, so Create
  and Suggested picks that resolve to an owner also confirm.
- "Finding speakers…" reads the progress topic (step 2), no snapshot field.
- Clearing Me reverts the meeting's Me participant; `self_person_id` stays.
- "Last invitee left" is host work in step 3; "Also speaking here" comes
  from `DetailSpeaker.personId`.
- Paragraph moves check staleness per segment speaker ID; new voice rows get
  the next free "Speaker N", confidence 0, no embedding or clip; an emptied
  voice is deleted.
- `transcript.playTurn` (≤ 60 s, both hosts), no play once the recording is
  gone.
- The journal keeps both rows' prior state; the source clip file is kept
  while the journal row exists; Undo and Separate again refuse as stale if
  a journaled segment moved; later merges undo latest first.

Final scores: Astra 8, Fable 8 (from 5.5 and 5 for #348). Remaining
disagreement: S2 (Astra: always This paragraph). Handed to Nicolai.

## 2026-11-15 — built speaker step 2 (#360 on #352 on the design stack)

Request IDs `steno-built-speakers2-astra-r1`, `steno-built-speakers2-fable-r1`.
Evidence: `speakers-2/{before,after}/` (31 states per theme) against the
#348 screens and the Jamie teardown.

**GPT-6-Astra: 7/10 (Jamie 7.5); not yet equal.** Resolved: hidden merges,
Enter on an unseen person, advance before success; largely: visual noise.
Partly: evidence (confirmed-person editing drops the quote), suggestion
honesty, keyboard. Not: transcript scope. Ranked:
1. Transcript picker hides that it changes the whole voice
   (`speaker-picker.tsx:165,225`): say "Changes all N paragraphs of
   Speaker 3 in this meeting"; label play "Voice sample".
2. A confirmed row cannot be reopened right away
   (`speakers-popover.tsx:362,759`): freeze placement, not editability.
3. Keyboard: blur cancels and Tab does not move to the next row
   (`speaker-name-field.tsx:326`, `speakers-popover.tsx:263`); no visible
   Y/N/P hints.
4. Confirmed-person editing suppresses the snippet (`speakers-popover.tsx:609`).
5. "No clip" only in the accessible label (`speaker-picker.tsx:70`).
Departures: web combine dialog right; voice-first title right; No clip right
in intent; locking completed rows wrong.

**Claude Fable 5.1: 8/10 (Jamie 7.5); at least as good, ahead on honesty.**
Short of Jamie on paragraph moves (step 4) and clip words (step 3).
Resolved: trigger, controls per row, vocabularies, split row, silent merges,
optimistic and error state, focus and tab stops, play twice, avatars.
Partly: a wrong match reads as fact (the bare "?" is weak). Ranked:
1. Me toast reads "You is now Nicolai." (`speaker-picker.tsx:101`).
2. Out-of-order picks reopen a named voice: stale `layout` after the await
   (`speakers-popover.tsx:306-307`).
3. Transcript "?" as the same grey bubble used in the pill.
4. Combine copy leaves "Their" dangling (`speaker-name-field.tsx:76`).
5. The headline "partly" capture uses the raw fixture (no line, greyed
   play); no capture of a named Me row (`e2e/screens.spec.ts:265-266`).
6. Option avatar colour flips after the pick.
Departures: all right.

### Responses

All accepted, one fix build on #360; Astra's "Changes all N paragraphs"
copy and visible "No clip" are taken. Steps 3-4 must-haves recorded for
those builds.
