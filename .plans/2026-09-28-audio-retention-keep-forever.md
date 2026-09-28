# Steno: keep recordings, explicit audio retention setting

Status: proposal, 2026-09-28. Triggered by first-run feedback.

Scope authority: [`2026-09-24-initial-scope.md`](2026-09-24-initial-scope.md) ("Retention and
privacy"). Binding plans this one refines: [`2026-09-25-core-foundation.md`](2026-09-25-core-foundation.md)
(`AudioRetention`, `RetentionSweep`, the `retention` stage),
[`2026-09-25-adapters-obsidian.md`](2026-09-25-adapters-obsidian.md) (`includeAudio`, `audio.m4a`),
[`2026-09-25-macos-app-and-release.md`](2026-09-25-macos-app-and-release.md) (Settings > Audio,
`setKeepAudio`, acceptance item 8). Nothing here changes the scope's three retention rules or the
"audio never leaves the Mac" rule; it changes the default, the wording, one ordering bug and what the
user can see. This plan owns the retention sentence that
[`2026-09-28-onboarding-vault-and-llm.md`](2026-09-28-onboarding-vault-and-llm.md) places under the
onboarding intro.

The owner's first-run feedback was: "I want a config setting that allows me to save the recordings
(no deletion)."

## Goal

A person who has never read the code opens Steno, understands within one screen what happens to
their recordings, and can choose "Keep forever" (or already has it). Audio is never deleted while the
transcript, summary or an export that needs the audio is still outstanding. The audio folder's disk
usage is visible next to the choice, so keeping everything is an informed decision.

## Findings

What exists today, with evidence. Line numbers are as of commit `bcf5eef` on `main`.

### The setting exists and is reachable

- Model: `Sources/StenoCore/Model/Audio.swift:22-36`. `AudioRetention` has three cases:
  `.deleteAfterProcessing` (the scope's `0`), `.keepDays(Int)`, `.keepForever` (the scope's empty
  value). `expiry(from:)` returns `nil` for `.keepForever`. In SQLite it is the
  `audioAsset.retention` text column (`keepForever`) with `retentionDays` NULL
  (`Sources/StenoCore/Storage/Records.swift:404-431`,
  `Sources/StenoCore/Storage/Migrations.swift:127-128`). In the `setting` table it is one JSON row,
  key `defaultRetention`, value `"keepForever"` or `{"keepDays":30}`
  (`Tests/StenoCoreTests/SettingsStoreTests.swift:36`).
- Setting: `Settings.defaultRetention`, `Sources/StenoCore/Model/Settings.swift:8`.
- UI: Settings > Audio > "Recordings" section, `apps/macos/Steno/Settings/SettingsView.swift:123-143`.
  A `Picker` labelled "Keep audio" with the options "Delete after processing", "Keep for a number of
  days" and "Keep forever" (`apps/macos/Steno/Settings/AudioSettingsViewModel.swift:16-22`), a
  `Stepper` for the days (1 to 3650) shown only in the days mode, and a footnote about speaker sample
  clips. Saving is immediate (`AudioSettingsViewModel.setRetention`, lines 76-86) through
  `AppEnvironment.updateSettings` (`apps/macos/Steno/AppEnvironment.swift:95-100`).
- Reachability: the Settings scene is registered in `apps/macos/Steno/StenoApp.swift:45-49`, so
  Cmd+, works, and the menu bar window has `openSettings` (`apps/macos/Steno/MenuBar/MenuBarView.swift:9`).
- Tests cover the round trip: `apps/macos/StenoTests/SettingsViewModelTests.swift:34-59`.

So the setting already exists: Settings > Audio > "Keep audio" > "Keep forever". The owner did not
find it, for the reasons below.

### The default deletes after 30 days

- `Settings.init(defaultRetention: AudioRetention = .keepDays(30), ...)`,
  `Sources/StenoCore/Model/Settings.swift:27`. Asserted by `Tests/StenoCoreTests/SettingsStoreTests.swift:11,58`,
  `apps/macos/StenoTests/AppEnvironmentTests.swift:96`, `apps/macos/StenoTests/RecordingControllerTests.swift:47`,
  `apps/macos/StenoTests/SettingsViewModelTests.swift:42-43`.
- The default is applied at recording completion for Mac recordings
  (`Sources/StenoCore/Storage/LocalRecordingIntake.swift:133-145`), at admission for phone recordings
  (`Sources/StenoCore/Storage/RecordingIntake.swift:82-89`) and by `steno process`
  (`Sources/steno/Commands/Process.swift:100,106`). Phone handover has no retention of its own: the
  `StenoHandover` module only deletes its upload inbox files after the intake enqueued the meeting
  (`Sources/StenoHandover/Upload/Inbox.swift:10`).
- Nothing in the app tells the user this. Onboarding is permissions only
  (`apps/macos/Steno/Onboarding/OnboardingView.swift`, steps in `OnboardingViewModel.swift:77-92`) and
  says "Audio never leaves this Mac", which a reader can take as "audio stays". The Audio tab shows
  the picker with no sentence saying what the current choice means for the files. Without a visit
  to Settings > Audio, every recording is deleted 30 days after processing.
- Once any setting has been saved, `SettingsStore.save` writes every property as a row
  (`Sources/StenoCore/Storage/SettingsStore.swift:20-25`), so existing installs carry a stored
  `{"keepDays":30}` row. Changing the code default reaches new installs only.

### Wording

The picker copy is understandable ("Keep forever" is explicit). What is missing is a plain sentence
under the control saying what happens under the chosen rule and that transcripts and summaries are
never affected, plus the actual disk cost. The stepper reads "30 days" while the picker reads "Keep
for a number of days", which is two controls for one idea.

### How deletion works, and where it can go wrong

- The pipeline stamps `expiresAt` in the `retention` stage after `deliver`
  (`Sources/StenoCore/Pipeline/ProcessingPipeline.swift:171-172`,
  `Sources/StenoCore/Pipeline/Stages/Retention.swift:8-16`). `RetentionSweep.run(now:)` deletes the
  master, the 16 kHz sidecars and the mixdown of every asset whose `expiresAt` has passed
  (`Sources/StenoCore/Storage/RetentionSweep.swift:30-51`, `AudioAsset.expirableFiles` at
  `Audio.swift:109-114`); it never touches speaker sample clips. The app runs the sweep at launch and
  on `.retentionApplied` (`apps/macos/Steno/AppController.swift:52,66-70`,
  `apps/macos/Steno/AppEnvironment.swift:130-137`).
- Processing failure is safe today: `retention` is only reached after `persist` succeeded
  (`ProcessingPipeline.swift:163-172`); on failure the asset keeps the `expiresAt = nil` the intake
  set (`LocalRecordingIntake.swift:145`; the phone intake never sets it). A failed meeting can be
  re-processed at launch (`resumeUnfinished`, lines 92-107) with its audio intact. No test pins
  this; it is an accident of ordering.
- Delivery failure is not safe. `deliver` never throws (`Sources/StenoCore/Pipeline/Stages/Deliver.swift:6-9`,
  `Sources/StenoAdapters/Runtime/DeliveryCoordinator.swift:37-85` records `.failed` rows instead), and
  `retention` runs regardless. With "Delete after processing" the audio expires at `now` even when
  the Obsidian export failed (vault unmounted, permissions, `audioUnavailable`). The sweep then
  removes the mixdown, and the user's "Re-export" (`MeetingDetailViewModel.reexport`,
  `apps/macos/Steno/Main/MeetingDetailViewModel.swift:149-151`) can never deliver `audio.m4a`:
  `ObsidianFolderDestination` throws `audioUnavailable` when the mixdown is gone and no copy is in the
  vault (`Sources/StenoAdapters/Obsidian/ObsidianFolderDestination.swift:116-128`). The same applies to
  "Keep for N days" with a delivery that stays broken for N days, the realistic case for a vault on
  an external drive.
- The sweep does not check the meeting's state. `MeetingStore.expiredAssets(now:)`
  (`Sources/StenoCore/Storage/MeetingStore.swift:357-365`) is a pure `expiresAt <= now` query. Nothing
  today puts an `expiresAt` on a queued or processing meeting, but nothing guards it either.
- Changing the default does not touch existing assets. Switching to "Keep forever" leaves already
  stamped assets expiring on schedule; the only way to save them is the per-meeting toggle, one
  meeting at a time.

### The per-meeting keep toggle

- Exists, in the detail pane's "Actions" menu: `Toggle("Keep audio")` at
  `apps/macos/Steno/Main/MeetingDetailView.swift:91`, backed by `setKeepAudio`
  (`MeetingDetailViewModel.swift:155-170`) and `keepsAudio` (line 172-174, true when the asset's
  retention is `.keepForever`). Test: `apps/macos/StenoTests/MeetingDetailViewModelTests.swift:133-148`.
- It is hidden behind an ellipsis menu, shows no expiry date, and its semantics break down once the
  default is "Keep forever": `keepsAudio` is then true for every meeting and turning it off restores
  the default, which is again `.keepForever`.
- "Reveal recording in Finder" (`MeetingDetailView.swift:92-95`) is offered whenever the asset has a
  URL, including after the sweep removed the file (the row keeps its URLs, `RetentionSweep.swift:4-6`).

### The vault audio export opt-in

`ObsidianSettings.includeAudio`, default `false` (`Sources/StenoCore/Model/Settings.swift:69,74`).
UI: Settings > Obsidian > "Copy the audio into the vault" (`SettingsView.swift:302`), saved with the
rest of the Obsidian form (`apps/macos/Steno/Settings/ObsidianSettingsViewModel.swift:54-75`).
Correct and reachable; it is the one export that needs the audio file.

### Disk cost of keeping everything

Not shown anywhere in the app. The Mac master is CAF, 48 kHz Float32, one channel per lane
(`Audio.swift:4-5`): 192 KB per second per lane, about 0.69 GB per hour per lane. A one hour call
(two lanes) is about 1.4 GB of master plus about 0.23 GB of 16 kHz sidecars plus a mixdown of tens
of MB; an in-person hour is about half that; a phone recording arrives as AAC (tens of MB per hour)
and has no mixdown. Ten hours of calls a week kept forever is roughly 0.8 TB a year. This is the
one real argument for the 30 day default, and it must be visible to the user rather than decided
for them.

## Non-goals

- No new retention tier such as "keep only the compressed mixdown". It is the obvious follow-up
  (it would cut the cost of "Keep forever" by about fifty times) but it is a scope change and gets
  its own plan; see Open questions.
- No cloud, no sync, no encryption. Audio stays in `Settings.audioFolder` on this Mac.
- No change to speaker sample clips (they follow speaker confirmation, not retention).
- No change to the `RecordingLayout` folder structure or to `meeting.json`.
- No data migration that rewrites a stored `defaultRetention` row (see Decisions).
- No retroactive application of a shorter rule to recordings already on disk.
- No change to the mobile app.

## Decisions

1. Default becomes "Keep forever" for new installs. Reasons: deletion is irreversible and disk is
   not; the owner asked for exactly this after one run; with the disk usage line next to the
   control (decision 5) the cost is visible and the alternative is one click away. The scope
   leaves the default open. Existing installs keep their stored row; the owner flips the picker
   once. A migration cannot tell a deliberate "30 days" from the old default, so it is not
   attempted.
2. Deletion waits for delivery. The `retention` stage stamps `expiresAt` only when every `Delivery`
   row of the meeting is `.delivered` (or there are no destinations). Otherwise the asset stays
   unstamped and `redeliver` and `rerunSummary` stamp it after a later delivery succeeds. Reason: the
   Obsidian `audio.m4a` copy is the one export that needs the file; the app must not make it
   impossible by deleting first.
3. The sweep skips meetings that are recording, queued or processing. Cheap, and it turns an
   accident of ordering into a rule with a test.
4. Switching the default to "Keep forever" clears the expiry of every recording that still has
   files, automatically. Switching to a shorter rule applies to new recordings only. Reason: the
   safe direction should be effortless; the destructive one is not asked for and stays out.
5. Settings > Audio shows the audio folder's size next to the rule, with a "Show in Finder"
   button. Reason: how long and how big are weighed together.
6. The per-meeting control moves out of the ellipsis menu into a visible "Recording" line in the
   detail header with its current status ("Kept forever", "Deleted on 28 Oct 2026", "Kept until the
   export succeeds", "Deleted") and a "Keep this recording" toggle shown only when the default is
   not forever. Reason: the toggle is the scope's per-meeting keep, and it must be discoverable and
   truthful when the default is forever.
7. Copy says "recording" where the user means the file. "Keep recordings" is the picker label;
   footnotes state that transcripts, summaries and exports are never deleted by this rule.
8. The `AudioRetention` enum, its SQLite columns and its JSON encoding do not change. No schema
   migration is needed.

## Spec

### Settings > Audio, "Recordings" section

Order of rows:

1. Folder row as today (path, "Choose…"), plus a second line: "Recordings use 4.2 GB" and a "Show in
   Finder" button. The size is `AudioFolderUsage.measure(folder)`, computed off the main actor on
   `load()` (the tab reloads on appear); while computing it reads "Measuring…". A folder that
   cannot be read shows "Size unavailable".
2. `Picker("Keep recordings")` with exactly three options, in this order:
   - "Forever"
   - "For 30 days" where the number is the stored days; selecting this option shows the `Stepper`
     inline on the same row as "days" (range 1 to 3650, default 30 when switching from another mode)
   - "Until processed, then delete"
   Mapping: "Forever" is `.keepForever`; "For N days" is `.keepDays(N)`; "Until processed, then
   delete" is `.deleteAfterProcessing`. Loading maps back the same way (existing
   `AudioSettingsViewModel.RetentionMode`, retitled).
3. One footnote sentence that follows the selection:
   - Forever: "Recordings stay in the folder above until you delete a meeting."
   - Days: "Each recording is deleted N days after it was processed and exported. Transcripts,
     summaries and exports are never deleted by this rule."
   - Delete: "Each recording is deleted as soon as it was transcribed, summarised and exported.
     Transcripts, summaries and exports stay."
   Followed by the existing sample clip sentence.
4. When the selection changes to Forever, the view model calls
   `MeetingStore.clearExpiry(assetIDs:)` for every asset whose master file exists. No
   confirmation; nothing is deleted.

### Detail header, "Recording" line

Under the meta line (duration, language, tokens) and above the tags row:

- Status text from the asset and the files:
  - files missing: "Recording deleted"
  - `.keepForever`: "Recording kept forever"
  - `expiresAt` set: "Recording deleted on <date>" (or "today")
  - retention not forever, `expiresAt` nil, meeting `.ready`, any delivery not `.delivered`:
    "Recording kept until the export succeeds"
  - retention not forever, `expiresAt` nil, meeting `.failed`: "Recording kept; processing failed"
  - otherwise (queued or processing): "Recording kept while processing"
- `Toggle("Keep this recording")` shown only when `Settings.defaultRetention != .keepForever` and the
  files exist. On: `.keepForever`, `expiresAt = nil` (today's `setKeepAudio(true)`). Off: default rule,
  `expiresAt` from now (today's `setKeepAudio(false)`); when the default is `.deleteAfterProcessing`
  the expiry is now, so the off state confirms ("Delete this recording now?").
- "Reveal recording in Finder" (moved here from the menu) only when the master file exists.

### Deletion guard rules (StenoCore)

1. `ProcessingPipeline.retention(asset:)` reads `store.deliveries(meetingID:)`. If any row is not
   `.delivered`, it saves nothing, posts nothing and returns; `process` still succeeds. Otherwise it
   stamps `expiresAt` as today and posts `.retentionApplied`. `.keepForever` stamps nil and posts as
   today, so the app's launch sweep behaviour does not change.
2. `redeliver` and `rerunSummary` call `retention(asset:)` after `deliver` when the asset's retention
   is not `.keepForever` and `expiresAt` is nil (the deferred case). An asset that is already stamped
   is never restamped by these paths, so a re-export does not extend a 30 day expiry.
3. `MeetingStore.expiredAssets(now:)` joins `meeting` and excludes `recording`, `queued` and
   `processing` states.
4. Processing failure leaves `expiresAt` nil (unchanged behaviour, now tested).
5. `RetentionSweep` is unchanged.

### Model additions (StenoCore)

- `MeetingStore.assets()` (every asset with its URLs; the file check happens in the caller, off the
  database) and `MeetingStore.clearExpiry(assetIDs:)` as one write.
- `AudioFolderUsage` in `Sources/StenoCore/Audio/`: `static func measure(_ folder: URL) throws -> Int64`
  using a `FileManager` enumerator with `.totalFileAllocatedSizeKey`, skipping hidden files. Pure
  function, testable with a temp folder.

## Implementation steps

1. Default and copy (core and app).
   - `Sources/StenoCore/Model/Settings.swift`: `defaultRetention: AudioRetention = .keepForever`.
   - Update the assertions that pin the old default: `Tests/StenoCoreTests/SettingsStoreTests.swift`,
     `apps/macos/StenoTests/AppEnvironmentTests.swift`, `apps/macos/StenoTests/RecordingControllerTests.swift`,
     `apps/macos/StenoTests/SettingsViewModelTests.swift`. Where a test needs a finite rule, set it
     explicitly (`Tests/StenoEndToEndTests/PhoneHandoverEndToEndTests.swift` and
     `Tests/StenoCoreTests/RecordingIntakeTests.swift` already do).
   - `apps/macos/Steno/Settings/AudioSettingsViewModel.swift`: retitle `RetentionMode` cases, reorder
     `allCases` to forever, days, delete; add `footnote` text; keep the `days` clamping.
   - `apps/macos/Steno/Settings/SettingsView.swift`: rebuild the "Recordings" section per the spec
     (picker label, inline stepper, footnote).
   - Tests: `SettingsViewModelTests` covers load and save of all three modes and the footnote per mode.
   Done when the tests pass and a fresh store loads `.keepForever`.

2. Delivery-aware retention (core).
   - `Sources/StenoCore/Pipeline/Stages/Retention.swift`: the guard from rule 1.
   - `Sources/StenoCore/Pipeline/ProcessingPipeline.swift`: `rerunSummary` and `redeliver` call
     `retention(asset:)` under rule 2.
   - `Sources/StenoCore/Storage/MeetingStore.swift`: `expiredAssets(now:)` joins the meeting state
     (rule 3).
   - Tests in `Tests/StenoCoreTests/PipelineIntegrationTests.swift` with `FakeDestination(deliverFailure:)`
     and `FakeDeliveryDispatcher` from `Sources/StenoCore/Testing/FakeDelivery.swift`: a failed
     delivery under `.deleteAfterProcessing` leaves `expiresAt` nil and posts no `.retentionApplied`;
     a later `redeliver` with the failure cleared stamps `expiresAt` and posts it; a second `redeliver`
     does not move an existing `expiresAt`; a `.keepForever` asset is unaffected either way; a
     processing failure (fake speech engine throwing) leaves `expiresAt` nil.
     `Tests/StenoCoreTests/RetentionSweepTests.swift`: an expired asset on a `.processing` meeting is not
     swept; the same asset is swept once the meeting is `.ready`.
   - `Tests/StenoEndToEndTests`: one test that fails the first delivery with `includeAudio` on under
     `.deleteAfterProcessing`, then redelivers and finds `audio.m4a` in the vault.
   - Doc comments on `MeetingEvent.retentionApplied` and `RetentionSweep` say when the event is not
     posted.
   Done when `swift test` is green on Linux and macOS.

3. Clear expiry when switching to Forever (core and app).
   - `MeetingStore`: `assets()` and `clearExpiry(assetIDs:)`.
   - `AudioSettingsViewModel.setRetention`: on `.keepForever`, collect the assets whose master file
     exists and call `clearExpiry`.
   - Tests: `SettingsViewModelTests` with a temp audio folder and two assets (one stamped with a
     file, one stamped without): switching to forever clears the first stamp only; switching to 7
     days changes nothing on disk.

4. Disk usage (core and app).
   - `Sources/StenoCore/Audio/AudioFolderUsage.swift` as specified, with
     `Tests/StenoCoreTests/AudioFolderUsageTests.swift` (temp folder with known sizes, hidden file
     skipped, missing folder throws).
   - `AudioSettingsViewModel.folderUsage: FolderUsage` (`.measuring`, `.bytes(Int64)`, `.unavailable`),
     measured on `load()`.
   - `SettingsView`: "Recordings use …" and "Show in Finder" via `NSWorkspace`. Formatting reuses
     `ByteCountFormatter` as `SpeechSettingsViewModel.formatBytes` does; move that helper to
     `apps/macos/Steno/Design/Labels.swift`.

5. Detail pane "Recording" line (app).
   - `apps/macos/Steno/Main/MeetingDetailViewModel.swift`: `recordingStatus` (the cases in the spec),
     `recordingFilesExist` (checked when `export` changes, `FileManager.fileExists` on the master),
     `showsKeepToggle` (default not forever and files exist), `setKeepAudio` unchanged apart from the
     confirmation when the default is delete-after-processing.
   - `apps/macos/Steno/Main/MeetingDetailView.swift`: the line, the toggle, "Reveal recording in
     Finder" moved and conditional. Remove the toggle from the ellipsis menu.
   - Tests: `apps/macos/StenoTests/MeetingDetailViewModelTests.swift`: status text for each case;
     toggle hidden when the default is forever; the existing `testKeepAudioTogglesRetention`
     kept with an explicit `.keepDays(7)` default. `apps/macos/StenoTests/TabTextSnapshotTests.swift`:
     update if the detail header text is snapshotted.

6. First-run visibility (app). `apps/macos/Steno/Onboarding/OnboardingView.swift`: under the
   page 1 intro (the onboarding plan owns its wording) add one line built from the loaded
   settings: "Recordings are kept forever in <folder name>. Change this any time in Settings >
   Audio." (or the days or delete sentence from the Audio footnote). `OnboardingViewModel` gains a
   `retentionSentence` loaded from `SettingsStore`; `apps/macos/StenoTests/OnboardingViewModelTests.swift`
   covers the three sentences.

7. Plans and docs.
   - This file: mark Status as implemented with the PR number.
   - [`2026-09-25-macos-app-and-release.md`](2026-09-25-macos-app-and-release.md): add a line under
     "Deviations (implementation)" pointing here for the default and the detail line; acceptance item
     8 gains "and a failed export defers deletion".
   - [`2026-09-25-core-foundation.md`](2026-09-25-core-foundation.md): note under the pipeline API that
     `retention` is delivery-aware and that `redeliver` may stamp.
   - Memory note in the release checklist: the owner's install still carries `{"keepDays":30}`; flip
     it in Settings > Audio after updating.

No migration is needed: no table, column or encoding changes.

Commit as `feat(core): defer audio retention until delivery succeeds` (step 2) and
`feat(macos): keep recordings by default, with the rule and disk usage visible` (the rest), or one
PR with those two commits.

## Verification

- `swift test` green on Linux (`StenoCore` and `StenoAdapters` tests) and on the macOS runner
  (`StenoAudio`, `StenoSpeech`, end-to-end), then `xcodebuild test` for `apps/macos/StenoTests`.
- Manual, on a fresh profile (new Application Support folder): the onboarding says recordings are
  kept forever; Settings > Audio shows "Forever" selected and the folder size; record a one minute
  call; after processing the folder size grows and the detail line says "Recording kept forever".
- Manual, delete-after-processing with Obsidian on and `includeAudio` on, vault folder made
  read-only: process a recording; the detail line says "Recording kept until the export succeeds";
  the master is still on disk; make the vault writable, "Re-export"; `audio.m4a` appears in the vault
  and the detail line changes to "Recording deleted on today" and then "Recording deleted" after the
  sweep; the transcript, summary and speaker clips are untouched.
- The owner's existing install: after updating, Settings > Audio still shows "For 30 days" (the stored
  row); selecting "Forever" clears the expiry of every recording still on disk and the detail line of
  each reads "Recording kept forever".

## Open questions

1. Keep only the mixdown. The master and sidecars are the bulk of the cost of "Keep forever"; a
   fourth rule "Keep the compressed recording, delete the master after processing" (or making that
   the meaning of "Forever" once the mixdown exists) would change the scope's three rules and the
   meaning of `expirableFiles`. Worth a follow-up plan once the owner has seen real folder sizes.
2. A per-meeting "Delete recording…" action (files only, meeting kept). With the default at forever
   the only way to reclaim one meeting's disk is to delete the whole meeting
   (`MeetingStore.delete`, `MeetingStore.swift:283-303`). It is a scope addition; the owner decides
   whether it is wanted.
3. Should the launch sweep also be delivery-aware, or is the stamp-time guard enough? Stamp-time is
   enough because nothing else stamps; revisit if another writer of `expiresAt` appears.
4. Whether the onboarding sentence (step 6) is enough, or the first processed meeting should show a
   one-time notice in the detail pane. Start with the sentence.
5. The `steno process` CLI applies `Settings.defaultRetention` but never runs the sweep. Leave as is
   (the CLI is a developer tool) or add `steno sweep`. Not needed for the owner's question.
