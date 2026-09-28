# Inline speaker assignment: necessary refactoring review

Reviewed 2026-09-28 against `.plans/2026-09-28-inline-speaker-assignment.md` and the
code it touches: `apps/macos/Steno/Speakers/{SpeakerReviewViewModel,SpeakerReviewSheet,ClipPlayer}.swift`,
`Main/{MeetingDetailViewModel,MeetingDetailView,MainWindow}.swift`,
`Main/Tabs/{TranscriptTab,TabText}.swift`, `AppController.swift`, `Design/{Components,Theme}.swift`,
`Sources/StenoCore/Storage/{MeetingStore,MeetingStore+People,Records,Migrations,RetentionSweep,Observation}.swift`,
`Model/{People,StageIO,Audio}.swift`, `Pipeline/LaneMerger.swift`, `Protocols/SpeakerMemory.swift`,
`Sources/StenoSpeech/Speakers/CosineSpeakerMemory.swift`,
`Sources/StenoCore/Testing/{InMemorySpeakerMemory,SampleData,ManualClock,Snapshot}.swift`, and the
tests `Tests/StenoCoreTests/{MigrationsTests,SchemaSnapshotTests,SpeakerMemoryTests,RetentionSweepTests,MeetingStoreTests}.swift`,
`apps/macos/StenoTests/{SpeakerReviewViewModelTests,MeetingDetailViewModelTests,AppControllerTests,ThemeTokensTests,TabTextSnapshotTests,TestSupport}.swift`,
`apps/macos/StenoUITests/LaunchSmokeTests.swift`. Correctness of the new behaviour and test
coverage are reviewed elsewhere; this pass lists the refactors each step depends on.

13 findings: 8 **required** (the step cannot land cleanly without them), 4 **recommended**,
1 **optional**. Most severe first; the dependency sequence is at the end.

## Findings

1. **required before step 1** — `Tests/StenoCoreTests/MigrationsTests.swift`
   `aV1DatabaseUpgradesToTheLatestVersionKeepingItsRows`, `SchemaSnapshotTests`,
   `Storage/Records.swift` `SpeakerRow`, `Testing/SampleData.swift`. `SpeakerRow` is
   `Codable + PersistableRecord` (`StenoRecord`), so adding `enrolledPersonID` to it makes
   the test's `SpeakerRow(speaker).insert(db)` into the v1-only queue fail with "no column
   named enrolledPersonID" before the migration under test runs. The same test then pins
   `snapshots/schema/v2.sql` after migrating to latest, and `SpeakerRow...map(\.speaker) ==
   SampleData.speakers()` only holds if `SampleData.speakers()[0]` (confirmed, has an
   embedding) carries `enrolledPersonID: personNicolaiID`, which the backfill will set.
   Smallest: insert the v1 speaker rows in that test with SQL (or a local v1-shaped record),
   point the assertion at `v3.sql`, regenerate `v1..v3` with `STENO_UPDATE_SNAPSHOTS=1`
   (`Snapshot.assert` throws `Missing` otherwise; schema dumps are not in
   `MANIFEST.sha256`), and set the field in `SampleData`.

2. **required before step 1** — `MeetingStore+People.swift` `mergePersons`, `confirm`,
   `mergeSpeakers`; `Migrations.swift` v3 backfill; `Pipeline/LaneMerger.swift`
   `meSpeaker`. Two lifecycle gaps the plan does not name. (a) `mergePersons` repoints
   `speaker.personID`, `participant.personID` and `meetingTask.assigneePersonID` but would
   leave `enrolledPersonID = remove`; deleting `remove` then fires `ON DELETE SET NULL`, and
   the next reassignment cannot withdraw, so `keep`'s mean keeps a voice nobody owns. Add
   `UPDATE speaker SET enrolledPersonID = ? WHERE enrolledPersonID = ?`. (b) The "Me"
   speaker is `.confirmed(personID:)` with no embedding (`LaneMerger.meSpeaker`), so a
   backfill `WHERE assignment = 'confirmed'` marks it enrolled; a later reassignment would
   withdraw a sample never added. Backfill `AND embedding IS NOT NULL`, and make `confirm`
   and `mergeSpeakers` set or withdraw `enrolledPersonID` only when the speaker has an
   embedding, mirroring today's "no enrol without an embedding"
   (`MeetingStoreTests.confirmKeepsAnExistingPersonAndSkipsEnrolWithoutAnEmbedding`).

3. **required before step 1** — `Model/People.swift` `Embedding.weightedMean`,
   `CosineSpeakerMemory.enroll`, `InMemorySpeakerMemory.enroll`,
   `Tests/StenoCoreTests/SpeakerMemoryTests.swift` line 56. The two enrol implementations
   already disagree: Cosine normalises the sample, weights by `min(sampleCount, cap)` and
   keeps counting ("the count keeps counting" test); InMemory folds the raw sample, sets
   `sampleCount = min(count, cap) + 1` (Nicolai with 3 samples and cap 2 becomes 3, not 4)
   and ranks against an unnormalised probe. The plan's `withdraw` (`w = min(sampleCount,
   maxSamples)`, `sampleCount -= 1`) and its "exact inverse under the cap" test would be
   written twice against two count semantics. Smallest: two pure statics next to
   `weightedMean`, `Embedding.enrolling(_ known: Embedding?, count: Int, cap: Int, sample:)
   -> (Embedding, Int)` and `withdrawing(...)`, both actors reduced to load, apply, save;
   InMemory adopts Cosine's counting (update the `== 3` expectation) and records withdrawals
   in a signed `enrolments` entry as the plan asks.

4. **required before step 1** — `Storage/RetentionSweep.swift`, `Model/Audio.swift`
   `expirableFiles`, `MeetingStore.swift` `expiredAssets`,
   `RetentionSweepTests.removesMasterSidecarsAndMixdownButNeverClipsOrStrangers`. Three
   places pin "never sample clips" in doc comments and a test name; the sweep sees only
   `AudioAsset` rows and has no write path for speakers. Smallest: for each expired asset
   also gather `store.speakers(meetingID:).compactMap(\.sampleClipURL)`, count them in the
   `clean` check, and null them through one new `MeetingStore.clearSampleClips(meetingID:)`
   (`UPDATE speaker SET sampleClipURL = NULL WHERE meetingID = ?`) after the files are gone.
   State in the header that asset rows keep their URLs while speaker rows lose theirs (the
   app tests file existence for Play either way), rename the test, and update the
   `sampleClipURL` doc in `People.swift` ("deleted on confirm" becomes wrong).

5. **required before step 2** — `MeetingStore+People.swift` `confirm`, `mergeSpeakers`;
   `SpeakerReviewViewModel.name`, `assign(_:attendee:)`. Decision 5 ("a person who already
   owns another speaker of this meeting merges") is a store invariant, one person per
   speaker per meeting, and the plan puts it in the view model as a read of the last export
   followed by a second store call: two quick clicks, or a transcript trigger and the
   popover racing, produce two confirmed speakers for one person. CLAUDE.md also forbids
   logic in the app target the CLI could need, and `Sources/steno` has no speaker command
   only because there is nothing in core to call. Smallest: inside `confirm`'s write, look
   for another speaker of the meeting `.confirmed(person.id)`; when found, run the merge
   body on the same `db` and return. That needs `mergeSpeakers`' transaction body extracted
   to `static func mergeSpeakerRows(_ source: SpeakerRow, into target: SpeakerRow, _ db:
   Database) throws -> URL?` used by both. Move `name -> existing person or new` and
   `attendee -> person (id, then name, then new with email)` to core as
   `MeetingStore.resolvePerson(named:email:now:)` so the app and a future CLI share one
   rule; the view model keeps error strings and the debounce.

6. **recommended before step 2** — new `Sources/StenoCore/Model/SpeakerSections.swift`
   (name open), `SpeakerReviewViewModel` `caseInsensitiveCompare` sites. The plan's ranked
   list, its filter and the Create-row rule are pure functions of (speaker, export, name
   suggestion, matches, recent people, query) and the plan tests them in `StenoTests`,
   which runs only on macOS. Today's matching is `caseInsensitiveCompare`, which does not
   fold diacritics, so "jerome finds Jérôme" needs a new fold anyway. Put one
   `String.stenoFolded` (`folding(options: [.caseInsensitive, .diacriticInsensitive],
   locale: nil)`) and `SpeakerSections.build(...) -> [Section]` in StenoCore, test them in
   `StenoCoreTests` (Linux, fast), and let `SpeakersViewModel.sections(for:query:)` be one
   call. Section rows carry ids (`personID`, `speakerID`, `.create(text)`) so the
   accessibility ids fall out.

7. **required before step 2** — `MeetingDetailViewModel.saveScratchpad`/`flushScratchpad`,
   new `Main/Debounce.swift`, `MeetingDetailViewModelTests.testScratchpadSavesOnceAfterTheDebounce`.
   The loop is extractable, but four properties must survive or the existing test and the
   cancellation comment break: one sleeper re-armed by an edit counter (the test asserts
   `clock.pendingSleepers == 1` across three edits and again after a late edit); the task
   handle cleared before the action runs (the comment: `flushScratchpad` cancels the
   pending task, and cancelling the running one would abort the GRDB write); `flush()` is
   cancel-sleeper-then-run-now; and the action-specific "failed write keeps the text
   pending" stays in the view model, not in the helper. The redeliver action is longer
   (file writes into the vault), so the same hazard applies to it. Shape: `@MainActor final
   class Debounce { init(_ interval: Duration, clock: any Clock<Duration>, action:
   @escaping @MainActor () async -> Void); func schedule(); func flush() async; func
   cancel() }`. `reexport()`'s comment "The only re-export entry point" moves to the one
   private method both paths call.

8. **recommended before step 2** — `MeetingDetailViewModel.observe()`, `MainWindow.swift`
   line 45, `MeetingStore+Export.swift` `exportRows`. No split is needed: the model is
   glue plus one debounce, and `MainWindow` recreates it per selection with `.id(detail.id)`,
   so `onDisappear` fires and a child `SpeakersViewModel` dies with it. What it needs is a
   single fan-out: `observe()` assigns `export` and calls `speakers.update(export:)`. Two
   things the plan's "never reloads except recentPersons and candidates" misses:
   `SpeakerNameSuggestion` is not in `MeetingExport` (`exportRows` never reads
   `speakerNameSuggestion`), so `update` must also call `nameSuggestions(meetingID:)`; do
   not add it to the export, that changes `Tests/Fixtures/exports/meeting-export.json`
   and every vault `meeting.json`. And `update` must run its loads in one cancellable Task
   keyed by the speakers' ids and embeddings so a burst of ticks (three assigns, each a
   speaker-table write) does not rank candidates three times.

9. **required before step 2** — `Design/Components.swift`, the missing
   `.plans/2026-09-28-macos-visual-redesign.md`. The plan borrows `IconButton`, the 88 pt
   labelled row and the `popover` treatment from a redesign plan that is not on `main` or
   this branch (only in t3 checkpoint refs); `Components.swift` has
   `StenoPrimaryButtonStyle`, `StenoSecondaryButtonStyle`, `StatusChip`, `Card`,
   `SectionLabel`, `MessageRow`, `PendingText` and nothing icon-shaped. Either sequence
   step 2 after the redesign lands or define `IconButton` (28 pt, `.plain`, `secondary`
   hover veil, `accessibilityLabel`) in `Components.swift` in step 2 and let the redesign
   adopt it. The plan should say which; "adopts the new primitives when they land" does not
   compile.

10. **required before step 3** — `Speakers/ClipPlayer.swift`,
    `SpeakerReviewViewModel.playing`. `ClipPlayer()` takes no dependencies and its only
    identity is `playingURL`, which the view model matches against `card.clipURL`. With
    the recording fallback every speaker plays the same `export.audio.url`, so two speakers
    become indistinguishable and the stop button lights on both. Change: `init(clock: any
    Clock<Duration>)` (the detail model already holds `clock`), `enum Source: Equatable {
    case clip(URL); case range(URL, ClosedRange<TimeInterval>) }`, `func play(_ source:
    Source, for speakerID: UUID) -> Bool`, `private(set) var playingSpeakerID: UUID?`, and
    a `stopTask` that sleeps `min(range length, 10 s)` on the clock and is cancelled in
    `stop()` and `finished()`. Set `currentTime` before `play()`; keep the
    `fileExists` guard and Bool return. Tests drive it with `ManualClock`.

11. **recommended** — `Design/Theme.swift` `tokens`, `ThemeTokensTests`. `Theme.tokens` is
    asserted 1:1 against `--color-*` in `mobile/global.css` in both directions, so
    `avatarHues` as eight `Token`s inside `tokens` fails the mobile mirror, and adding eight
    `--color-avatar-*` entries to the mobile CSS for a macOS feature is the wrong coupling.
    Keep `static let avatarHues: [Token]` outside `tokens` with its own test, and add a
    `Theme.contrastRatio(_:_:)` over `RGBA` (relative luminance) for the 4.5:1 assertion;
    nothing in the app computes contrast today.

12. **recommended** — `Main/Tabs/TabText.swift`, `TranscriptTab.swift` `TranscriptTurns`,
    `MeetingDetailView.swift`, `LaunchSmokeTests.swift`. Contracts that must not move: the
    ids `tab-<tab>` (the only ids the UI smoke test reads) and `tab-content-<tab>`;
    `TabText.transcript` lines `"<name> <timestamp> <lane>"` then text, with "Unknown" for
    a nil speaker, pinned by `snapshots/macos/tab-transcript.txt`; `TranscriptTurns.group`
    unchanged. The transcript trigger renders exactly `export.displayName(forSpeaker:)`
    and nothing else in its label, so a `Text` snapshot of the tab stays byte-equal.
    `review-speakers` has no reference outside `MeetingDetailView` and can go. Add
    `speakers-row` to the smoke test in step 2 as the plan says.

13. **optional** — `Model/People.swift` `Speaker`, its `CodingKeys` and init; callers in
    `Pipeline/Stages/Diarize.swift`, `LaneMerger.meSpeaker`, `StenoLLM/Testing/LLMFixtures.swift`,
    `StenoAdaptersTests/Support/FixtureMeeting.swift`. Keeping `enrolledPersonID` out of
    `CodingKeys` (like `embedding`) leaves `meeting-export.json`, `ModelCodableTests` and
    the vault JSON untouched; declare it as a defaulted `= nil` init parameter and every
    constructor keeps compiling, so only `SpeakerRow.init(_:)`, `SpeakerRow.speaker` and
    `SampleData` change. One sentence for the plan: `sampleClipURL` is exported (it is in
    `CodingKeys`), so with clips outliving confirmation more vault exports carry a local
    file path; harmless, but a reader of `meeting.json` should not expect the file.

## Dependency sequence

Step 1 (core PR): 3 (shared running mean) -> 2 (lifecycle rules) -> 1 (migration test
insert, `v3.sql`, `SampleData`) -> 4 (sweep contract) -> 5 (merge-if-owned inside
`confirm`, shared merge body, `resolvePerson`) -> 6 (`SpeakerSections` and the fold, core
tests) -> 13 (init default, doc comments). 3 must precede 2 because `withdraw`'s
implementation decides what `confirm` and `mergeSpeakers` call; 5 must precede step 2
because the view model's `assign` is one store call only once core merges.

Step 2 (app PR): 9 (decide `IconButton`/redesign order, blocks compilation) -> 7
(`Debounce`, with the scratchpad test green before the redeliver uses it) -> 8 (fan-out in
`observe()`, `nameSuggestions` fetch, cancellable loads) -> 11 (`avatarHues` outside
`tokens`) -> 12 (ids and `TabText` unchanged, `speakers-row` in the smoke test).

Step 3 (app PR): 10 (`ClipPlayer` identity and clock) -> transcript trigger, which needs
nothing beyond 12.
