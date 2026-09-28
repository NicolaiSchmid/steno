# Test-strategy review: inline speaker assignment

Reviewed 2026-09-28 against `.plans/2026-09-28-inline-speaker-assignment.md`.
Question: can an agent prove each step on CI (`package`, `app`, hosted
`ui-smoke`, StenoCore in the Linux container), and does the plan name every
existing test whose assertion flips? Correctness and elegance are in the
sibling reviews. Severity: blocker = an existing test contradicts the plan
unnamed, or a Done criterion has no machine check; major = a stated behaviour
has no test, or the listed test cannot be written as described; minor =
fixture, naming or reviewer hygiene.

## Findings

1. **blocker — two tests assert the sweep never touches clips.** Core change
   6; step 1. `Tests/StenoCoreTests/RetentionSweepTests.swift`
   `removesMasterSidecarsAndMixdownButNeverClipsOrStrangers` (`fileExists(clip)`)
   and `PipelineIntegrationTests.retentionZeroSweepRemovesTheAudioAndKeepsThe
   SampleClips` (real clips from `Diarize`) pin the opposite. Rename the first
   `removesMasterSidecarsMixdownAndClipsButNeverStrangers`: `removed == [master,
   mic, system, mixdown, clip]`, `speakers(meetingID:)[1].sampleClipURL == nil`,
   `sampleClipRange` kept, stranger and `keptMaster` present. Flip the second's
   clip loops. Add `anUndeletableClipKeepsExpiresAtAndTheClipColumn` (pattern
   of `anUndeletableFileIsRetried...`): read-only `speakers/` ->
   `Incomplete.failures.map(\.url) == [clip]`, column and `expiresAt` still set,
   second run clears both. The null and the unlink need a store operation
   (`clearSampleClips(meetingID:)`) that `RetentionSweep.run` cannot reach
   today; name it.

2. **blocker — the pre-migration insert pattern breaks when `SpeakerRow` gains a
   column.** Core change 2; step 1. The repo's pattern
   (`MigrationsTests.aV1DatabaseUpgradesToTheLatestVersionKeepingItsRows`)
   registers `v1` alone and inserts through `SpeakerRow(...).insert(db)`. With
   `enrolledPersonID` on the record that insert fails on the v1 table, so the
   existing test goes red before v3 is tested. Switch it to raw SQL inserts.
   Add `aV2DatabaseBackfillsEnrolledPersonIDForConfirmedRowsOnly`: v1+v2, raw
   insert of `SampleData.speakers()`, migrate, speaker one `enrolledPersonID ==
   personNicolaiID`, speaker two nil, `Snapshot.assert(dump, matches:
   "snapshots/schema/v3.sql")`, then delete Nicolai -> nil (`SET NULL` holds).
   `migratorRunsOnAnEmptyDatabase` (`["v1", "v2"]`) and `migratingTwiceIsANoOp`
   (`count == 2`) should read `Migrations.identifiers`. `v1.sql`, `v2.sql` must
   not move.

3. **blocker — the pending-review clear lives in a SwiftUI `onChange`, which no
   hostless test drives.** App changes; step 2 "`AppControllerTests...` asserts
   the pending review clears when the last speaker is confirmed". The
   controller has no view in tests, so the assertion can only call
   `reviewCompleted` by hand, which the test already does. Move the rule into
   `AppController` (it already intersects `pendingReviews` with the meeting list,
   `AppController.swift:94`): on each `observeMeetings` tick drop ids whose
   speakers are all confirmed. Then
   `testSpeakersNeedReviewClearsWhenTheLastSpeakerIsConfirmed`: post the event,
   `store.confirm(speakerTwoID, Jérôme)`, `waitUntil` pending empty. Keep the
   ghost half.

4. **major — `withdraw` has one happy path and five undefined edges.** Core
   change 1. Add to `SpeakerMemoryTests` (fake, Linux) and
   `CosineSpeakerMemoryTests` (real): `withdrawAboveTheCapStaysNormalisedAnd
   Counts` (`maxSamples 2`, enrol three, withdraw one -> count 2, magnitude
   ≈ 1); `withdrawFromAnEmptyPersonIsANoOp` (count 0, nil, never negative);
   `withdrawFoldsIntoTheStoredPersonNotTheStaleArgument` (mirror of the enrol
   test); `withdrawOfAMismatchedDimensionClearsTheRow` (the enrol side has a
   rule, the withdraw side needs one). Name the fake's record
   (`withdrawals: [Enrolment]`) so `MeetingStoreTests` can assert on it.

5. **major — `confirm`: orphan rule and suggested-but-never-enrolled untested.**
   Core change 3; step 1. Add to `MeetingStoreTests`:
   `confirmAThenBKeepsAWhenATaskStillReferencesIt` (`SampleData.tasks()[0]`
   assigns Jérôme: confirm Speaker 2 to Jérôme, then Anna -> Jérôme count 0,
   row kept); `confirmAThenBKeepsAWhenAnotherMeetingIsConfirmedToA`;
   `confirmingAwayFromASuggestionWithdrawsNothing` (Speaker 2 `.suggested(
   Jérôme)`, confirm Anna -> Jérôme still 1, `withdrawals` empty). The last is
   Acceptance 3's trap: a withdraw keyed on `personID` instead of
   `enrolledPersonID` corrupts a voice never enrolled. Existing
   `confirmSetsConfirmedEnrolsOnceAndRemovesTheClip` flips its clip assertions
   (rename `...AndKeepsTheClip`); `confirmKeepsAnExistingPersonAndSkipsEnrol
   WithoutAnEmbedding` gains `enrolledPersonID == nil`.

6. **major — `mergeSpeakers` into an `.unknown` target loses the enrolment
   record.** Core change 4. Today an `.unknown` target takes the source's
   assignment (`mergeSpeakersTakesTheSourceAssignmentAndClipWhenTheTargetHas
   None`). A source `.confirmed(A)` with `enrolledPersonID A` leaves the kept
   row `.confirmed(A)`, `enrolledPersonID nil`: the next `confirm(A)` enrols
   twice, the next `confirm(B)` withdraws nothing. Add
   `mergeIntoAnUnknownTargetCarriesTheEnrolment` (kept `enrolledPersonID == A`,
   no withdrawal) and `mergeOfAnUnenrolledSourceWithdrawsNothing`.

7. **major — `recentPersons()` needs a second meeting and a suggestion rule.**
   Core change 5. Arrange meeting `uuid(2)` at `startedAt + 3600` with a speaker
   confirmed to Jérôme; Nicolai confirmed in the sample meeting; Anna never;
   Speaker 2 `.suggested` to Dora. Assert `["Jérôme", "Nicolai", "Anna", "Dora"]`
   and `persons()` still by name. SQLite name order is byte order (`Zoë` before
   `anna`); say `COLLATE NOCASE` if that is meant, and pin it.

8. **major — the sections builder should be pure; half its rules are unlisted.**
   Design "SpeakerPicker"; step 2. `sections(for:query:)` on the view model
   needs a store and an export tick per case. Make `SpeakerSections.build(
   speaker:, speakers:, persons:, participants:, suggestion:, recent:, query:)`
   (like `TranscriptTurns.group`) and test it in `SpeakerSectionsTests` without
   an environment. Beyond the plan's list: `.suggested` person not repeated
   under Recent; an attendee whose person owns another speaker excluded; the
   speaker never under Same voice as; "speaker 1" matches the cluster label;
   Create hidden for "jerome" and for whitespace; LLM guess as a person row
   ("Mentioned") when the name exists, nothing when `name` is nil or empty
   (today's `testLLMNameSuggestionShowsAndNamesOnRequest`); default highlight
   is the first Suggested row, else the first row. App changes says `candidates`
   (cosine top 5) is fetched per export but no section shows them: pick one.

9. **major — the keyboard model is view-only.** Design: Down/Up, Return, Escape,
   Tab. Nothing hostless presses keys in a `.popover`. Put the state in
   `PickerState(sections:, highlighted:, query:)` with `move(.down)` and
   `commit() -> Action?`; test Return on an untouched unnamed speaker yields
   `.assign(Jérôme)` (Acceptance 1), Down past the end stays, Return on an empty
   list is nil. The view only maps key events to these calls.

10. **major — `SpeakerReviewViewModelTests` cases dropped without a home.** Step
    2 lists nine behaviours; these have none: `testAssignAttendeeCreatesPerson
    WithEmail`, `testBlankNamesAndUnknownSpeakersAreIgnored` (`name(id, "  ")`
    no-op; `assign(UUID(), ...)` no error, no person), `testMergeWithItselfIs
    ANoOp`, `testNamingCreatesAPersonAndConfirms` (create path, count 1),
    `testPlayingAMissingClipReportsAnError` (becomes `rows[].canPlay == false`).
    Re-home each under its name in `SpeakersViewModelTests`. `testSkip...` and
    `testMergeNeedsAChosenTarget` go (behaviour removed); add
    `testBuildingSectionsWritesNothing`.

11. **major — `Theme.avatarHues` collides with `ThemeTokensTests`.** Design
    "Avatars". `testEveryCSSColorTokenHasASwiftCounterpart` requires
    `Theme.tokens` ⊆ `--color-*` in `mobile/global.css`; eight `Token`s in
    that array turn step 2 red unless the PR adds `--color-avatar-0..7` to the
    mobile CSS (unmentioned). Keep the hues as `[AvatarHue]` outside
    `Theme.tokens`, or add the CSS deliberately. Contrast runs hostless via
    `NSAppearance(named: .darkAqua)!.performAsCurrentDrawingAppearance { color
    .usingColorSpace(.sRGB) }` and WCAG luminance. Add `AvatarTests`:
    `initials("Philipp Schröder") == "PS"`, `"Jérôme" == "J"`, `"  anna  marie
    x" == "AM"`, `"" == ""`; `hueIndex(SampleData.uuid(10)) == 0`, first byte
    `0xFF` -> 7.

12. **major — the redeliver debounce test needs a vault and a flush rule.**
    Decision 10; step 2. Follow `testScratchpadSavesOnceAfterTheDebounce` plus
    the `obsidian` settings arrange from `testFinishRedeliversOnlyAfterAChange
    AndOnlyOnce`, else `deliveries` stays empty and the test passes vacuously.
    Assert `pendingSleepers == 1` after three `assign`s, no delivery before
    `advance(by: redeliverDebounce)`, one `.delivered` after; `flushRedeliver`
    with a sleeper -> 0 sleepers, one delivery; with nothing pending -> none.
    Add `DebounceTests` for the extracted type (fire once, re-arm, flush,
    cancel) and keep the scratchpad test unchanged as regression proof. A flush
    during an in-flight redeliver throws `PipelineFailure` ("already being
    processed"): decide error or queue, and test it.

13. **major — `enrolledPersonID` reaches three Codable tests and the goldens.**
    Core change 2. `ModelCodableTests.embeddingsAreNotEncoded` and
    `exportMatchesTheGolden` compare decoded speakers to `SampleData` with
    `embedding` nilled; they must nil `enrolledPersonID` too. Add
    `enrolledPersonIDIsNotEncoded`. Goldens that must not move:
    `exports/meeting-export.json`, `snapshots/obsidian/meeting.json`,
    `snapshots/e2e/*`, `snapshots/macos/*`; check with `git diff --exit-code
    Tests/Fixtures` minus the new `schema/v3.sql`.

14. **major — the UI smoke test cannot reach the auto-open or Play paths.**
    Step 2 Done, step 3, Acceptance 1 and 5. In `-steno-ui-testing` mode
    `PreviewSeed` posts no `speakersNeedReview`, so the popover opens only by
    click; the seeded clip and audio URLs (`/tmp/steno/...`) do not exist, so
    Play is hidden. Acceptance 5 is `[manual]` and must say so. What the smoke
    test can assert: `app.buttons["speakers-row"]` reads "1 unnamed"; after the
    click `app.popovers.firstMatch` exists; `speaker-row-00000000-0000-0000-
    0000-000000000014` contains `staticTexts["Nicolai"]`; `...15` contains a
    text field. The id list has no id for the popover field: add
    `speaker-field-<uuid>`. Do not assert focus (`hasKeyboardFocus` is not
    public on macOS); type "Anna" without clicking and check the field's
    `value`. Replace "the screenshot shows a named and an unnamed row" with
    those assertions; keep the `.keepAlways` attachment (the
    `macos-ui-smoke-output` artifact carries the xcresult).

15. **minor — `SpeakerExcerpt` needs a truncation rule a string can pin.**
    Decision 8. "Two lines" is layout; cap characters (say 160), lead with "…"
    when the range starts inside a segment. Tests: range spanning two segments
    joins both; range nil -> longest; no segments -> ""; partial overlap -> "…".

16. **minor — playback: test the clock and the source, mark audibility.** Step
    3. `ClipPlayer(clock: ManualClock)`, `play(mic-6s fixture, range: 1...3)`
    -> one sleeper; `advance(2 s)` -> `playingURL == nil`; `0...30` -> deadline
    10 s; `stop()` drops the sleeper. `AVAudioPlayer.play()` without an output
    device is unproven: skip with a `STENO_AUDIO_TESTS` message if needed. Make
    `PlaybackSource.resolve(speaker:, audio:, fileExists:)` pure; four cases.

17. **minor — end-to-end and pipeline coverage.** `EndToEndTests` never confirms
    and should stay so (People goldens would move). Add `PipelineIntegrationTests.
    confirmThenReassignKeepsTheClipAndMovesTheVoice` on `PipelineHarness` (real
    clips, real cosine memory): Anna then Ben; clip present, Anna 0 and deleted,
    Ben 1, `enrolledPersonID == Ben`.

18. **minor — names, tags, Done criteria.** Core is Swift Testing `@Test func
    lowerCamelSentence()`, app is XCTest `testUpperCamelSentence()`; write out
    the truncated `testSpeakersNeedReviewStaysPendingUntil…`. Tag every check
    `[ci]`, `[ci hosted]` (anything in `LaunchSmokeTests`) or `[manual]`. Add
    to Done: step 2 `grep -rn 'SpeakerReviewSheet\|showsSpeakerReview\|review-
    speakers\|clusterConfidence' apps/macos/Steno` empty (Acceptance 7); step 3
    `git diff --exit-code Tests/Fixtures/snapshots/macos`. The referenced
    `2026-09-28-macos-visual-redesign.md` is not on this branch; do not rely on
    `IconButton` or the Tags label existing.

## Acceptance to test

| # | Proof |
|---|---|
| 1 | `PickerState` default highlight (9); smoke click and field `value` (14) |
| 2 | `SpeakerSectionsTests` Create rule (8); name creates or reuses (10) |
| 3 | `confirmingAwayFromASuggestionWithdrawsNothing`, confirm A then B (5) |
| 4 | owner-of-another-speaker merges (step 2); unknown-target merge (6) |
| 5 | sweep removes clips (1); `PlaybackSource.resolve` (16); audibility `[manual]` |
| 6 | redeliver debounce and flush (12) |
| 7 | grep in step 2 Done (18) |

## Regression traps for reviewers

- Any `Tests/Fixtures` change other than a new `snapshots/schema/v3.sql`;
  `MigrationsTests` still inserting pre-migration rows through `SpeakerRow`.
- A `withdraw` test that covers only `sampleCount 1`; `pendingReviews` cleared
  from a SwiftUI modifier, not the controller; `Theme.tokens` grown without
  matching CSS tokens; a skipped `LaunchSmokeTests` without a `STENO_*` message.
