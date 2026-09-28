# Inline speaker assignment: conflicts with decisions, code and in-flight plans

Reviewed 2026-09-28 against `.plans/2026-09-28-inline-speaker-assignment.md` (line numbers
below are its lines), the binding plans (`2026-09-24-initial-scope.md`,
`2026-09-25-macos-app-and-release.md`, `2026-09-25-core-foundation.md`,
`2026-09-25-speech-and-speakers.md`, `2026-09-25-adapters-obsidian.md`,
`2026-09-25-llm-and-templates.md`), the code on `main` (`Sources/StenoCore/Storage/*`,
`Sources/StenoAdapters/**`, `Sources/steno/**`, `apps/macos/Steno/**`, every test under
`Tests/` and `apps/macos/StenoTests`), the uncommitted sibling plans in worktree
`t3code-7f265610` (`macos-visual-redesign`, `first-run-feedback`, `audio-retention-keep-forever`,
`start-recording-from-main-window`, `onboarding-vault-and-llm`), the branch
`origin/docs/plan-processing-progress` (PR #101) and `gh pr list` (open: #101, #102 = this plan).
The elegance and simplification reviews of the same date cover interaction design and
code shape; this file covers only what the plan contradicts or must merge with.
Counts: 1 blocker, 6 major, 7 minor.

## Findings

1. **Blocker.** Decision 6 (74-77), Core 6 (218-220), Acceptance 5 (309-310): "clips follow the
   audio", the sweep removes `sampleClipURL` files with the asset. Other side: scope line 72-73
   ("Unknown speakers get a ten-second sample clip for manual naming"), macOS acceptance 5 (311)
   and 8 (316, "Retention `0` deletes audio after processing but keeps the speaker clips until
   confirmed"), core plan `RetentionSweep` "never sample clips" (169, 325-327), and the code:
   `AppController.swift:66-70` runs the sweep on `.retentionApplied`, which `Retention.swift:15`
   posts right after processing. Under `.deleteAfterProcessing` the audio and, with this plan,
   every clip are gone before the popover opens for the first time, so an unnamed speaker has
   neither clip nor recording-range fallback and Play is hidden exactly when naming happens.
   The retention plan's keep-forever default softens this for new installs only; `0` stays a
   supported rule. Winner: the scope. Smallest edit to Core 6: "the sweep removes clips of
   `.confirmed` speakers only; unconfirmed speakers keep theirs as today. `confirm` deletes the
   clip when the meeting's audio has already been swept (`expiresAt == nil`, retention not
   `.keepForever`, master missing), otherwise leaves it." Acceptance 5 then adds "an unnamed
   speaker of a retention-`0` meeting can still play its clip".

2. **Major.** Core 6 (218-220) and Core 7 (221-224) against the sibling
   `2026-09-28-audio-retention-keep-forever.md`: its Non-goals line 149 ("No change to speaker
   sample clips (they follow speaker confirmation, not retention)"), Deletion guard rule 5
   (244, "`RetentionSweep` is unchanged"), step 1 (209, keeps "the existing sample clip
   sentence", which is `SettingsView.swift:141` "Speaker sample clips stay until the speaker is
   named, whatever the retention."), and step 7 (329-331) amending macOS acceptance 8. Both plans
   edit `RetentionSweep.run` / `MeetingStore.expiredAssets(now:)` and acceptance 8; neither
   names the other on this point. Winner: this plan for the rule (finding 1's version), the
   retention plan for the Audio tab copy it owns (first-run Ownership table, line 63). Edits:
   this plan's Core 7 adds "amend the retention plan: drop Non-goal 149, rule 5 becomes 'the
   sweep also removes clips of confirmed speakers', the Audio footnote reads 'Sample clips of
   unnamed speakers stay until you name them; named speakers' clips go with the recording.'";
   Step 1 (254) adds "lands after the retention plan's step 2 (delivery-aware `retention`), on
   top of its `expiredAssets` join".

3. **Major.** Core 3 (204-207, orphan person deleted), Decision 10 (redeliver) against the vault.
   `PersonPageRenderer.swift:14-47` and `ObsidianFolderDestination.swift:95-113` write one
   `- <date> [[slug|title]] %%steno:<meeting uuid>%%` line into the page of every person in
   `export.persons`; `ManagedBlock.merge` replaces or inserts that uuid's line, it never removes
   one. Reassigning Speaker 2 from Anna to Bea and redelivering adds the line to `Bea.md` and
   leaves it in `Anna.md`; deleting the orphan person makes the stale line permanent (the export
   no longer lists Anna, so her page is never visited). The adapters plan (199-200) only
   foresaw renames ("the old one stays"). This is exported data integrity, a review priority.
   Winner: the vault must not claim a meeting a person was never in. Smallest edit, new item in
   Core changes: "`ObsidianFolderDestination.deliver` removes the `%%steno:<uuid>%%` line
   (`ManagedBlock.remove`) from every `.managedBlock` path in the previous receipt that this
   delivery did not render; the path stays in the receipt. Test in `StenoAdaptersTests`:
   reassign, redeliver, the old page has no line and its bytes outside the block are intact."

4. **Major.** Lines 9-12 and 35-37 ("uses what exists today and adopts the new primitives when
   they land"; the redesign "when that plan lands points here") against
   `2026-09-28-macos-visual-redesign.md`: detail header spec (317-318) still places "Review
   speakers (n)" primary on the tags row; step 10 (504-506) restyles `SpeakerReviewSheet.swift`
   and requires `SpeakerReviewViewModelTests` unchanged; step 12 screenshots; F18, F19, F28.
   `2026-09-28-first-run-feedback.md` Order (22-40) puts the redesign last "because the plans
   above change copy and controls it restyles" and does not list this plan at all. Meanwhile
   this plan already depends on redesign primitives: `IconButton` (134, 245; not in
   `apps/macos/Steno/Design/`, only `Card` exists, `Components.swift:60`) and "the Tags label
   the redesign plan adds" at 88 pt (112; the redesign has no such label, its tags row is chips
   plus an "Add tag" ghost button, 315-317). Each plan expects the other to adapt. Winner: the
   index decides. Edits: add this plan to the first-run index as a tenth row (after
   processing-progress's ninth) with Order "9. Inline speakers, core step any time after the
   retention plan; app steps after redesign steps 1, 2 and 7"; the redesign drops 317-318's
   "Review speakers (n)", the sheet paragraph (413-418), step 10, F28 and the sheet half of
   F19, and its transcript turn header (331-332) becomes "the `SpeakerPicker` trigger from the
   inline speaker plan"; this plan replaces line 112 with "Label 'Speakers' 12 pt `faint`,
   width shared with the retention plan's 'Recording' label" and line 35-37 with the order above.

5. **Major.** Header (109-121): a "Speakers" row "under the meta line, before Tags". The
   retention plan Decision 6 (175-179) and spec (214-230) put a "Recording" line "under the meta
   line and above the tags row"; the start-recording and redesign plans add a Stop control to
   the title row; the first-run Ownership table (56-64) gives the header's "Recording" line to
   the retention plan and lists no owner for speakers. Two rows claim the same slot with no
   stated order. Also, speakers are written by the Merge stage (`Stages/Merge.swift`,
   `replaceTranscript`) before `.ready`, so "shown once the export has at least one speaker"
   (109-110) renders the Speakers row while the processing-progress card (PR #101) is on
   screen, with names the summary has not seen. Winner: one document. Edits: line 109 becomes
   "gains a labelled row after the meta line and the retention plan's 'Recording' line, before
   Tags, shown once the meeting is `.ready` and has a speaker"; the Ownership table gains
   "Speakers row, popover, picker, avatars | inline speaker plan".

6. **Major.** Core 1, 3 and 4 (189-213) reverse a recorded deferral without saying so:
   `2026-09-25-core-foundation.md` Deferred (393-394) "`SpeakerMemory.forget(personID:)` and
   person deletion: not in v1 scope; merging split speakers is covered by `mergePersons` and
   `mergeSpeakers`". `withdraw` is `forget` for one sample and Core 3 deletes person rows.
   Core 7's bookkeeping (221-224) names only the scope bullet, macOS step 5 and the surface row.
   Missing amendments, each one line: core plan 169 and 393-394; core plan 157 (`confirm` "deletes
   sampleClipURL"); macOS acceptance 5 and 8 (311, 316); speech plan 340-341 ("the speaker review
   sheet calls … `mergePersons`"); LLM plan 84 and 342 ("the review sheet prefills from them");
   and the doc comments that now lie: `Model/People.swift:235` ("deleted on confirm"),
   `Model/Audio.swift:107-108`, `Storage/RetentionSweep.swift:3-4`,
   `Storage/MeetingStore+People.swift:123-126`, `Protocols/SpeakerMemory.swift:3-4` ("feeds the
   review sheet"), `Pipeline/Stages/Diarize.swift:25`. Winner: this plan, once it says so. Edit:
   extend Core 7 with the list above and "Deferred in core-foundation: `forget(personID:)` stays
   deferred; single-sample `withdraw` and orphan-person removal are added here."

7. **Major.** Decision 9 (85-89) "the one place the app uses hue to tell things apart" against
   the first-run index Shared decisions (47-48, "The accent stays achromatic. A brand hue is an
   open question … to be decided together if at all") and redesign Decision 3 (150-155, "hue only
   for status"). A plan cannot grant itself an exception to a shared decision; the owner
   document must change. Winner: the shared decision until amended. Edit: either add to the
   first-run Shared decisions "identity avatars may use a fixed muted palette; status and
   controls stay achromatic (inline speaker plan)" in the same PR, or make Decision 9 neutral
   (initials on `secondary`, which the elegance review's item 11 also prefers) and drop
   `Theme.avatarHues` and its `ThemeTokensTests` assertion.

8. **Minor.** Core 2 (197-200), migration `v3`. `SchemaSnapshotTests.fullMigratorMatchesTheLatestGolden`
   (`Tests/StenoCoreTests/SchemaSnapshotTests.swift:26-31`) needs a new
   `Tests/Fixtures/snapshots/schema/v3.sql`; `MigrationsTests.aV1DatabaseUpgradesToTheLatestVersionKeepingItsRows:60`
   pins `snapshots/schema/v2.sql` by name and must move to `v3.sql`; `rowsRoundTripThroughTheirRecords`
   gains the column. The backfill "from `personID` where `assignment = 'confirmed'`" marks
   speakers that never enrolled: `MeetingStoreTests.confirmKeepsAnExistingPersonAndSkipsEnrolWithoutAnEmbedding`
   (416-460) confirms a speaker with `embedding == nil`, and `confirm` (People.swift:144-146) skips
   `enroll` then. A later reassignment would withdraw a sample the person never received. Edit:
   backfill `WHERE assignment = 'confirmed' AND embedding IS NOT NULL`; Core 3 sets
   `enrolledPersonID` only when an embedding was enrolled; name the two snapshot files in step 1.

9. **Minor.** Decision 1 (57-59) says `pendingReviews` "clears itself when the export shows no
   unconfirmed speaker"; App changes (242-243) put the clearing in `MeetingDetailView`'s
   `onChange` calling `controller.reviewCompleted`, as today (`MeetingDetailView.swift:29-44`);
   step 2 (279-280) says `AppControllerTests.testSpeakersNeedReviewStaysPendingUntilTheReviewCompletes`
   "asserts the pending review clears when the last speaker is confirmed". That test
   (`AppControllerTests.swift:198-226`) drives the controller with no view and no store change;
   `AppController` observes only `observeMeetings()` (line 94). Pick one: either the controller
   watches unconfirmed counts (then the test claim holds and the view code goes) or the view
   keeps it (then the test is a `MeetingDetailViewModelTests` case on `unconfirmedSpeakers`).

10. **Minor.** "Unnamed" (58, 115-117, 130-131) is `!isConfirmed`, which includes `.suggested`.
    Core already names those: `MeetingExport.displayName(forSpeaker:)` (`Model/StageIO.swift:161-167`)
    returns the suggested person's name and `SummaryMarkdown` bolds it, so the transcript
    trigger and summary show "Philipp" while the header says "1 unnamed" and the popover shows an
    empty field. The Deferred bullet (331-333) defers exactly the pre-fill that would reconcile
    this. Edit: the popover field of a `.suggested` speaker shows the suggested name as its
    draft text (Return confirms, typing replaces), matching what the transcript already prints.

11. **Minor.** Non-goals (29) "`MeetingStore.mergePersons` keeps working from the CLI": no
    `steno` subcommand calls it (`Sources/steno/**` has no `mergePersons`, `confirm` or
    `mergeSpeakers`), and neither does `apps/macos/Steno` despite the macOS surface row (117).
    Edit: "`mergePersons` stays a `MeetingStore` operation without a UI or CLI caller."

12. **Minor.** Decision 6 keeps `sampleClipURL` on confirmed speakers and Core 6 nulls it in the
    sweep. `Speaker.CodingKeys` (`Model/People.swift:262-270`) includes `sampleClipURL`, so the
    vault's `meeting.json` (`Tests/Fixtures/snapshots/obsidian/meeting.json:305`) now carries a
    live local clip path for every named speaker, and a sweep changes export bytes so the next
    redeliver rewrites `meeting.json` for an otherwise untouched meeting. Not a privacy breach
    (a path, not audio) but undocumented. Edit: say so under Decision 6, or drop
    `sampleClipURL` from `CodingKeys` like `embedding` (then `exportMatchesTheSampleExport`,
    `meeting-export.json:175` and the two `meeting.json` fixtures change).

13. **Minor.** Tests the plan breaks but does not name (it names `SpeakerReviewViewModelTests`,
    `MeetingDetailViewModelTests`, `AppControllerTests`, `TabTextSnapshotTests`,
    `ThemeTokensTests`, `LaunchSmokeTests`):
    `Tests/StenoCoreTests/MeetingStoreTests.swift` `confirmSetsConfirmedEnrolsOnceAndRemovesTheClip`
    (270-298, asserts `sampleClipURL == nil` and the file gone);
    `RetentionSweepTests.removesMasterSidecarsAndMixdownButNeverClipsOrStrangers` (7-59, asserts
    the clip survives; under finding 1's rule only a confirmed speaker's clip changes);
    `PipelineIntegrationTests.retentionZeroSweepRemovesTheAudioAndKeepsTheSampleClips` (491-532);
    `SchemaSnapshotTests` and `MigrationsTests` per finding 8; `SpeakerMemoryTests` and
    `CosineSpeakerMemoryTests` gain `withdraw` cases (additive);
    `apps/macos/StenoTests/MeetingDetailViewModelTests.testScratchpadSavesOnceAfterTheDebounce`
    (58-94) must survive the `Debounce` extraction (236-237) unchanged. Add them to step 1 and 2.

14. **Minor.** Merge seams with other in-flight plans, no contradiction: the onboarding plan
    step 6 (336-341) changes `MeetingDetailViewModel`'s designated init (`SetupState`) and this
    plan gives it `model.speakers` (229-230); the processing-progress plan adds the ninth index
    row this plan follows; the start-recording plan's `stop-recording-header` shares the header.
    State the order once (below) instead of in three places.

## Merge order

1. Retention plan step 2 (delivery-aware `retention`, `expiredAssets` join) before this plan's
   step 1, which then adds the confirmed-clip removal on top (findings 1, 2).
2. This plan's step 1 (core) can land right after that; it needs nothing from the app plans.
3. Onboarding app PRs (first-run order 4) before this plan's step 2, so `MeetingDetailViewModel`
   is edited once for `SetupState` and once for `speakers` (finding 14).
4. Redesign steps 1, 2 and 7 before this plan's steps 2 and 3, so `IconButton`, the header
   geometry and the transcript turn header exist to be replaced; redesign step 10 is deleted,
   not executed (finding 4). If the owner wants speakers before the redesign, this plan must
   drop `IconButton` for a plain `Button` and specify its own label width.
5. The first-run index, the retention plan and the redesign plan are amended in the same PR as
   this plan's bookkeeping, before any code (findings 2, 4, 5, 7).
