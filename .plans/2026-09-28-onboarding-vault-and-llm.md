# Steno: configure summaries and the vault during onboarding

Status: proposal, 2026-09-28, revised after the 2026-09-28 reviews. Triggered by first-run feedback.

Binding plans: scope authority [`2026-09-24-initial-scope.md`](2026-09-24-initial-scope.md);
app plan [`2026-09-25-macos-app-and-release.md`](2026-09-25-macos-app-and-release.md) (onboarding
row in the window inventory, "Settings tabs" and "LLM wiring" deviations); LLM plan
[`2026-09-25-llm-and-templates.md`](2026-09-25-llm-and-templates.md); adapters plan
[`2026-09-25-adapters-obsidian.md`](2026-09-25-adapters-obsidian.md) ("no destination configured"
is an empty delivery list). This plan amends the onboarding row and the LLM wiring note of the app
plan; nothing else in those plans changes. This plan owns the onboarding window's pages, rows and
copy; the retention sentence under the intro comes from
[`2026-09-28-audio-retention-keep-forever.md`](2026-09-28-audio-retention-keep-forever.md) (step 6
there) and the window's look from
[`2026-09-28-macos-visual-redesign.md`](2026-09-28-macos-visual-redesign.md).

## Goal

The owner ran the app for the first time and asked: "the onboarding didn't ask me to configure the
vault or the post-processing LLM APIs. I assume that will be flagged as missing once the first
recording is completed?"

The answer today is no. Nothing flags it, and the first meeting gets a fabricated summary. This
plan records what happens today with file and line evidence, stops the pipeline from running a
test double in the product, makes the missing configuration visible where the user looks
(onboarding, main window, meeting detail), and reuses the existing re-run and re-export paths so
a meeting processed before configuration can still get its summary and its export.

## Findings

Line numbers are as of commit `9cd7cf5` (source identical to `bcf5eef` on `main`). What happens
today when the first recording completes with no LLM endpoint and no vault:

1. Onboarding is permissions only. `OnboardingViewModel.steps` is built from
   `PermissionKind.allCases` (`apps/macos/Steno/Onboarding/OnboardingViewModel.swift:17`); the
   window closes itself the moment every step is granted or skipped
   (`OnboardingView.swift:42-44`). The opener runs once per launch and opens the window only
   while a required permission is missing (`apps/macos/Steno/StenoApp.swift:132-143`). Once
   microphone and system audio are granted the window never appears again.

2. Without an endpoint the product runs test doubles. `AppEnvironment.live` builds the pipeline
   with `LLMWiring.passes(settings:apiKey:)` (`apps/macos/Steno/AppEnvironment.swift:207`), which
   is nil until `Settings.llmBaseURL` and `llmModel` are both set
   (`apps/macos/Steno/Services/LLMWiring.swift:18-19`, `Sources/StenoLLM/LLMEndpoint.swift:60-62`).
   The fallbacks are `PassthroughCleaner()` and `FakeSummarizer()`
   (`AppEnvironment.swift:213-214`), both from `Sources/StenoCore/Testing/FakeLLM.swift`. The CLI
   does the same (`Sources/steno/Wiring.swift:74-75`).

3. The fake summarizer fabricates content. `FakeSummarizer.output(for:usage:)`
   (`Sources/StenoCore/Testing/FakeLLM.swift:72-99`) returns: the title "Summary of <title>", one
   bullet per template section whose lead is the first speaker's cluster label and whose text is
   the first transcript segment, one decision "Decision from Speaker 1.", one task "Follow up on
   Default." assigned to Speaker 1, and a usage of 200 prompt and 100 completion tokens. The
   summarize stage replaces the title when the meeting has no calendar event
   (`Sources/StenoCore/Pipeline/Stages/Summarize.swift:30-31`) and sums the usage (:34), on top
   of the passthrough cleaner's fixed 100 plus 50 (`FakeLLM.swift:31`, folded in at
   `Sources/StenoCore/Pipeline/ProcessingPipeline.swift:159`). The meeting lands `.ready`.

4. What the user sees: a "Ready" chip (`apps/macos/Steno/Main/MeetingDetailView.swift:54`), a
   header line "450 tokens" although no request was made (:61-62), a Summary tab with a heading
   per section and a bullet quoting the first sentence (`Main/Tabs/SummaryTab.swift:49-77`), a
   Tasks tab with the fake task, a Decisions block with the fake decision, and a meeting renamed to
   "Summary of Untitled" unless the calendar supplied a title. No error, no warning, no chip says
   any of this is a placeholder. The transcript is real but uncleaned: `PassthroughCleaner`
   returns segments untouched (`FakeLLM.swift:38-42`), so the Denglish and casing cleanup the scope
   promises did not run.

5. Delivery is silently absent. `DeliveryCoordinator.destinations(for:)` returns `[]` when
   `settings.obsidian == nil` (`Sources/StenoAdapters/Runtime/DeliveryCoordinator.swift:32-35`)
   and `deliverAll` returns without writing a row (:56). The footer shows "Not delivered yet"
   (`MeetingDetailView.swift:176-190`), the same text a meeting shows in the seconds before a
   configured delivery lands. Nothing distinguishes "no vault" from "not yet".

6. The only hint in the app is in Settings > LLM: "No endpoint configured: summaries use a
   placeholder until a base URL and model are saved." (`apps/macos/Steno/Settings/SettingsView.swift:262-268`),
   visible only if the user opens that tab. The Obsidian tab has no equivalent when the toggle is
   off (:284-327). The menu bar item (`MenuBar/MenuBarView.swift`) and the main window
   (`Main/MainWindow.swift`) carry no setup state at all.

7. The recovery paths exist but are undiscoverable and incomplete. Actions > "Re-run summary" and
   "Re-export" (`MeetingDetailView.swift:86-88`) are enabled for `.ready` and failed meetings
   (`Main/MeetingDetailViewModel.swift:100-103`). Saving the LLM tab rebuilds the pipeline
   (`Settings/LLMSettingsViewModel.swift:84`) and the coordinator reads settings per delivery
   (`DeliveryCoordinator.swift:41`), so after configuring, both actions do the right thing. But the
   user has to know the summary was fake, and `rerunSummary` summarizes the stored segments only
   (`ProcessingPipeline.swift:179-194`): the cleanup pass is never repeated, so a meeting processed
   before the endpoint was configured keeps its raw transcript for good.

8. Minor: `Sources/StenoCore/Model/Settings.swift:6` says the audio folder is picked in onboarding;
   it is not (it lives in Settings > Audio).

The tests that pin the fake output (`Tests/StenoCoreTests/PipelineIntegrationTests.swift:31`,
`FakesTests.swift:63`, `StageTests.swift:378`) inject the fake explicitly and stay valid.

## Non-goals

- New LLM providers, provider pickers or anything beyond base URL, model name and API key. The LLM
  client keeps sending text only.
- A second destination, or moving the people folder, task tag and audio copy options into
  onboarding. Those stay in Settings > Obsidian.
- Bulk re-processing of every unsummarised meeting after the endpoint is configured (open
  question).
- Re-running the cleanup pass on an already processed meeting. That needs a "process again from
  decode" path and its own plan.
- Retroactively detecting summaries the fake produced under earlier builds. The owner's first
  meeting is fixed with one Re-run summary once the endpoint exists.
- The audio folder in onboarding. The default location is fine for a first run.

## Decisions

1. The pipeline no longer runs `FakeSummarizer` or `PassthroughCleaner` in the product. The
   fakes stay test doubles under `Sources/StenoCore/Testing/`. `PipelineDependencies.cleaner` and
   `.summarizer` become optional (`(any TranscriptCleaner)?`, `(any MeetingSummarizer)?`, default
   nil), which existing call sites that pass a value still satisfy. Nil means "no LLM endpoint":
   the cleanup stage posts its progress event and persists nothing (the merge stage already wrote
   the transcript in its own transaction, `Sources/StenoCore/Pipeline/Stages/Merge.swift:9-11`
   and `:32`); the summarize stage posts its progress event and leaves `summary`, `llmUsage`,
   tasks and decisions empty and the title untouched. The meeting still lands `.ready` and still
   delivers. Reason: a `.ready` meeting whose summary is nil is an honest state the UI already has
   words for ("No summary"), and it needs no migration. A fabricated summary is a data integrity
   problem in SQLite and in every exported file.

2. `.ready` with `summary == nil` means "summary skipped for lack of an endpoint", by invariant.
   `process` only leaves that pair when the summarizer is nil (a configured endpoint that fails
   marks the meeting `.failed(reason)` with the transcript kept, as today). A pipeline test pins
   the invariant so no schema column is needed. Whether the endpoint is configured now comes from
   `Settings`, not from the meeting.

3. `rerunSummary` with a nil summarizer throws `PipelineFailure(stage: .summarize, reason: "no LLM
   endpoint is configured")` instead of writing a fake. The app disables the action first, so the
   throw is for the CLI and for races between Save and Re-run.

4. Onboarding asks, and the app nudges afterwards. Asking during onboarding matters because the
   first recording cannot be repaired: the cleanup pass runs once, at processing time (finding
   7). Nudging afterwards is still needed because both steps are optional (a local-only
   transcript recorder is a legitimate way to use Steno) and because existing installs have
   already passed onboarding.

5. The two setup steps reuse `LLMSettingsViewModel` and `ObsidianSettingsViewModel` as they are.
   Same validation, same `Test connection`, same Save that rebuilds the pipeline, same
   `ObsidianFolderDestination.validate()`. Onboarding shows fewer fields (no context window, no
   people folder, no task tag, no audio copy) and says where the rest lives. Reason: one code path
   for writing these settings; the onboarding view is layout only.

6. The setup steps are a second page, not rows on the permissions screen. Owner instruction,
   2026-09-28: "additional onboarding steps, later on, not in the first screen". Page 1 is the
   permissions screen as it is; page 2, "Summaries and export", holds the two optional steps and
   appears when page 1 finishes by Done or Later. Reason: the first screen stays about the one
   thing recording cannot do without, and a local-only user is one click from done on page 2.

7. Onboarding opens once more for installs that have not seen page 2. A `UserDefaults` flag
   `steno.onboardingCompleted` (same pattern as `AppController.loginItemRegisteredKey`) is set
   when the window finishes by any route. The opener shows the window when a required permission
   is missing or the flag is unset; existing installs with the flag unset open straight on page
   2, which is the owner's situation.

8. The main window gets one setup banner, the detail pane gets per-meeting status rows. The banner
   is a launch-time reminder and hides for the rest of the launch on "Not now"; it comes back on the
   next launch while the configuration is still missing. The detail rows are factual per-meeting
   states and are never dismissed. Reason: a permanently dismissable banner would need a second
   flag and a reset rule when configuration changes; the per-meeting rows already carry the signal
   permanently. "Configured" is a pure function of `Settings`, not a mirrored class:
   `extension Settings { var llmConfigured: Bool; var vaultConfigured: Bool }`
   (`LLMEndpoint(settings:) != nil`, `obsidian != nil`) in the app, and the one real piece of state,
   `AppController.setupBannerDismissed: Bool`. The view models already observe `Settings`, so no
   second observer task is needed.
8a. The copy for a `.ready` meeting without a summary, and the export footer, is owned here and
   selected by `MeetingDetailViewModel.summaryStatus` and `exportStatus`. The redesign plan folds
   these rows into its states table verbatim and keeps the two selectors; its `PendingText` keys
   off `Meeting.state` only for queued, processing and failed. One vocabulary in user copy:
   "export" (the Actions menu already says "Re-export"), never "delivered".

9. Settings gets deep links. `SettingsView` binds its `TabView` to a `SettingsTab` selection and
   `AppController` gets `requestedSettingsTab` plus `openSettings(_ tab:)`, which sets the request
   and returns it, mirroring `requestedMeetingID`. Every "Open Settings" button in this plan lands
   on the right tab. The request is set on the controller (unit-tested) and cleared in the view
   (covered by the UI smoke test that opens Settings on the right tab).

10. The CLI follows the same rule. `Wiring.dependencies` passes nil passes when
    `Wiring.llmComponents` is nil and `steno process` reports the summary as skipped. The fakes
    stay reachable for tests through the existing injection points.

## UX spec

### Onboarding window

Two pages in the same window, with a "Step 1 of 2" / "Step 2 of 2" caption above the title.

Page 1: title stays "Welcome to Steno". Intro copy becomes:

> A few permissions, then where summaries come from and where meetings go. Audio never leaves this Mac.

Directly under the intro sits the retention plan's one-line sentence ("Recordings are kept forever
in <folder name>. Change this any time in Settings > Audio.", or its days or delete variant; the
retention plan owns the wording). Rows 1 to 4 are today's
Microphone (required), System audio (required), Calendar (optional) and Local network (optional,
"Got it"), unchanged. Bottom buttons: "Later" until the required permissions are granted, then
"Done"; both advance to page 2 instead of closing. The auto-close on page 1 is replaced by the
advance to page 2.

Page 2: title "Summaries and export", intro "Optional. Steno works as a local transcript recorder
without either." Two rows, each with the "Optional" chip:

5. Summaries. Explanation:

   > Steno sends the transcript text, never audio, to an OpenAI-compatible endpoint to clean it up and write the summary, tasks and decisions. Without one, meetings keep a raw transcript and no summary.

   Fields: "Base URL" (prompt `http://127.0.0.1:1234/v1`), "Model" (prompt `gpt-4.1-mini`),
   "API key" secure field (prompt "optional for local servers"). Buttons: "Test connection"
   (secondary, disabled until the URL validates), "Save" (primary, disabled while
   `validationMessage` is set), "Skip" (plain). The test result line and the validation message
   are the view model's strings, shown as in Settings. Footnote: "The context window and the rest
   live in Settings > LLM." When saved the row collapses to a check and "Saved: <model> at <host>".

6. Obsidian vault. Explanation:

   > Steno writes each meeting into Meetings/<date>-<slug>/ inside the vault: a folder note, transcript, tasks, VTT and JSON. It never touches files it did not write. Without a vault, meetings stay in Steno.

   Controls: the vault path field with "Choose…" (NSOpenPanel, directories only, prompt "Use
   vault"), "Save" (primary, validates through the destination and shows `ObsidianError` verbatim
   on failure), "Skip" (plain). Footnote: "People pages, the task tag and the audio copy live in
   Settings > Obsidian." When saved the row collapses to a check and "Saved: <vault folder name>".

Page 2 bottom buttons: "Back" (plain) and "Finish" (primary, always enabled). The window closes
when both rows are saved or skipped, or on Finish. Finishing by any route sets
`steno.onboardingCompleted`.

### Main window banner

Shown as row 1 of the redesign plan's detail header stack (above the header, full width,
dismissable), over the selected meeting or the detail empty state, when at least one meeting
exists, the configuration is incomplete and the banner was not dismissed this launch. The preview
environment has no endpoint and no vault, so the banner shows there and the UI smoke test uses
that. Button ids: `setup-summaries`, `choose-vault`, `banner-not-now`; the smoke test matches ids,
not the copy.

- Both missing:

  > Summaries and export are off. Steno has no LLM endpoint and no Obsidian vault yet, so meetings keep a raw transcript on this Mac.

  Buttons: "Set up summaries" (Settings > LLM), "Choose a vault" (Settings > Obsidian), "Not now".

- Endpoint missing only:

  > Summaries are off. Steno has no LLM endpoint yet, so meetings keep a raw transcript.

  Buttons: "Set up summaries", "Not now".

- Vault missing only:

  > Export is off. Steno has no Obsidian vault yet, so meetings stay on this Mac.

  Buttons: "Choose a vault", "Not now".

Styling: a `Card` with a `MessageRow(kind: .info)` and the buttons in a trailing `HStack`, 32 pt
side insets like the header; motion from `Motion.functional` on appear and dismiss. No new colours.
The redesign plan's detail pane spec carries this row so the banner is not left unstyled.

### Meeting detail

Summary tab, replacing the "No summary" text when the meeting is `.ready` and `summary == nil`:

- Endpoint not configured now:

  > Summary skipped: no LLM endpoint is configured. The transcript is complete.

  Button "Set up summaries" (Settings > LLM).

- Endpoint configured now:

  > No summary yet: this meeting was processed before an LLM endpoint was configured.

  Button "Run summary" (calls `rerunSummary()`), footnote "Summary only; the transcript stays as recorded."

Tasks tab in the same two states: "No tasks: the summary was skipped." with the same button as the
Summary tab. Queued and processing keep today's pending texts. All four strings go through
`TabText` (a `setup` argument beside the state) so `TabTextSnapshotTests` keeps describing what the
tabs show; the redesign's `EmptyState` gets an optional action so the button survives its restyle.

Header: the "N tokens" line is absent because `llmUsage` stays nil. Actions menu: "Re-run summary"
is disabled with help "Set up an LLM endpoint in Settings > LLM first" while unconfigured;
"Re-export" is disabled with help "Choose an Obsidian vault in Settings > Obsidian first" while no
vault is configured.

Footer:

- No delivery rows and no vault configured: "Not exported: no Obsidian vault is configured." with a
  plain button "Choose a vault" (Settings > Obsidian).
- No delivery rows and a vault configured: "Not exported yet." with a plain button "Export now"
  (calls `reexport()`).
- Delivery rows present: the badges as today.

### Settings copy

- LLM tab info row becomes: "No endpoint configured: new meetings get a transcript but no summary
  until a base URL and model are saved."
- Obsidian tab gains an info row while the toggle is off: "No vault configured: meetings stay in
  Steno until a vault is chosen."

### Settings deep links

`SettingsTab` cases `general, audio, speech, llm, obsidian, phones, updates`. Callers call
`controller.openSettings(.llm)` (sets `requestedSettingsTab`), then the `openSettings` environment
action and `NSApp.activate()`. `SettingsView` applies the request in
`onChange(of: controller.requestedSettingsTab, initial: true)` and clears it, like `MainWindow`
does for `requestedMeetingID`.

## Implementation steps

1. Core: optional LLM passes.
   Files: `Sources/StenoCore/Pipeline/ProcessingPipeline.swift:7-40` (`PipelineDependencies.cleaner`
   and `.summarizer` optional with default nil; the struct lives there, not in `PipelineStage.swift`), `Sources/StenoCore/Pipeline/Stages/Cleanup.swift`
   (nil cleaner: post `.cleanup`, return the input segments with `usage: .zero`, no store write),
   `Sources/StenoCore/Pipeline/Stages/Summarize.swift` (nil summarizer: post `.summarize`, return
   the meeting unchanged, clear tasks, decisions and name suggestions through `replaceSummary` with
   `summary` nil so a re-run of `process` never leaves stale rows),
   `Sources/StenoCore/Pipeline/ProcessingPipeline.swift` (`process` leaves `llmUsage` nil when
   both passes are nil; `rerunSummary` throws the typed reason from decision 3).
   Tests in `Tests/StenoCoreTests/PipelineIntegrationTests.swift`: a `PipelineHarness` with nil
   passes lands `.ready`, `summary == nil`, `llmUsage == nil`, no tasks or decisions, title still
   "Untitled", every segment `text == rawText`, `.cleanup` and `.summarize` progress events posted,
   the dispatcher called once; `rerunSummary` throws a `.summarize` failure whose reason contains
   "LLM endpoint"; `redeliver` succeeds. `PipelineHarness` (`Tests/StenoCoreTests/Support`) types
   `cleaner` and `summarizer` as non-optional today; make them optional so the nil-passes case is
   constructible. Add `PipelineIntegrationTests.aConfiguredSummarizerNeverLeavesReadyWithoutASummary`
   with a `FakeSummarizer` that returns an empty document, expecting a non-nil `summary`, so
   decision 2's invariant is pinned from both sides (today only the contrapositive,
   `summarizeFailureMarksFailedAndKeepsTheTranscript`, exists). `Tests/StenoCoreTests/StageTests.swift`:
   the skipped summarize stage clears rows a previous fake run wrote. Existing tests that inject
   the fakes are untouched.

2. CLI: `Sources/steno/Wiring.swift` passes `llm?.cleaner` and `llm?.summarizer` without
   fallbacks. `Sources/steno/Commands/Process.swift` keeps printing only the meeting id on stdout
   (`Process.swift:136`, scripts depend on it) and writes "summary skipped: no LLM endpoint
   configured" to stderr when the meeting lands ready without a summary.
   `CLITests.migrateGenerateProcessAndExport` (`Tests/stenoTests`, line 113) asserts
   `decoded.meeting.title == "Summary of Sweep"`, the fake's title, and fails once the CLI stops
   wiring the fake: change it to `title == "Sweep"`, `summary == nil`, `llmUsage == nil`, and
   stderr containing "summary skipped: no LLM endpoint configured". End-to-end tests already use
   the stub server for real summaries.

3. App wiring: `apps/macos/Steno/AppEnvironment.swift` `live()` passes `llm?.cleaner` and
   `llm?.summarizer` without fallbacks. `preview()` keeps its explicit fakes so seeded meetings
   render summaries in UI tests. Fix the comment at `Sources/StenoCore/Model/Settings.swift:6`.

4. Setup state: a new `apps/macos/Steno/Services/Settings+Setup.swift` with
   `extension Settings { var llmConfigured: Bool { LLMEndpoint(settings: self) != nil }; var
   vaultConfigured: Bool { obsidian != nil } }` and `AppController.setupBannerDismissed: Bool` with
   `dismissBanner()`. No class, no observer task: the banner and the detail view model read the
   current `Settings` the view models already observe.
   Tests: `apps/macos/StenoTests/SettingsSetupTests.swift` covers the four combinations of the two
   properties; `AppControllerTests`: after `launch()`, saving the LLM tab makes the loaded
   settings report `llmConfigured`, saving the Obsidian tab `vaultConfigured`, `dismissBanner()`
   sets the flag and a fresh controller starts with it false.

5. Settings deep links: `SettingsTab` and the `TabView(selection:)` binding in
   `apps/macos/Steno/Settings/SettingsView.swift`; `requestedSettingsTab` and
   `openSettings(_ tab:) -> SettingsTab` on `apps/macos/Steno/AppController.swift`; a small `View`
   extension `openSettings(_ tab:, controller:)` in `apps/macos/Steno/Design/Components.swift` used
   by the banner, the footer, the tabs and the menu bar. Test in
   `apps/macos/StenoTests/AppControllerTests.swift` that `openSettings(.llm)` sets and returns the
   request; the clearing happens in `SettingsView.onChange`, which the hostless bundle never runs,
   so it is covered by the UI smoke test in step 7 (`app.tabs["LLM"].isSelected` after clicking
   `setup-summaries`).

6. Detail pane states: `apps/macos/Steno/Main/MeetingDetailViewModel.swift` reads the current
   `Settings` (it already observes them) and exposes `summaryStatus` (`pending`, `present`,
   `skippedUnconfigured`, `skippedRunnable`), `exportStatus` (`noVault`, `notExported`,
   `exported([Delivery])`), `canRerunSummary` and `canReexport`.
   `apps/macos/Steno/Main/Tabs/SummaryTab.swift`, `Tabs/TasksTab.swift` and
   `Main/MeetingDetailView.swift` (actions menu, footer) render the copy above. `TabText` gains a
   `setup` argument carrying the four new strings and `TabTextSnapshotTests` is regenerated once,
   so the snapshot and the tabs describe the same words.
   Tests in `apps/macos/StenoTests/MeetingDetailViewModelTests.swift`: each status for (ready with
   summary), (ready without summary, unconfigured), (ready without summary, configured),
   (processing); `canRerunSummary` false while unconfigured; `exportStatus` for empty deliveries
   with and without a vault.

7. Banner: `apps/macos/Steno/Main/SetupBanner.swift` (view over `Settings`,
   `controller.setupBannerDismissed` and `MeetingListViewModel.all.isEmpty`), mounted in
   `apps/macos/Steno/Main/MainWindow.swift` as row 1 of the detail header stack. UI smoke test in
   `apps/macos/StenoUITests`: in the preview environment `setup-summaries` and `choose-vault`
   exist, clicking `setup-summaries` opens Settings with the LLM tab selected, `banner-not-now`
   hides the banner.

8. Onboarding: `apps/macos/Steno/Onboarding/OnboardingViewModel.swift` gets `enum Page { case
   permissions, setup }` with `page`, a `StepKind` enum (`permission(PermissionKind)`,
   `summaries`, `vault`) so the two setup rows sit in the same `steps` array as the permission
   rows, owns an `LLMSettingsViewModel` and an `ObsidianSettingsViewModel` from the environment,
   `advance()` (page 1 Done or Later), `back()`, `isFinished` requires the setup steps saved or
   skipped, `markCompleted()` writes `steno.onboardingCompleted`, and a static
   `shouldOpen(permissions:defaults:)` replaces the loop in `OnboardingOpener` (opening on page 2
   when the permissions are granted and the flag is unset). The constructor is the
   `init(environment:defaults:)` the retention plan introduced (index step 5); this plan extends
   that initialiser only and leaves the `init(permissions:)` convenience the 81 x 4 matrix test uses
   untouched, so the constructor is rewritten once, not twice. `OnboardingView.swift` renders the two
   pages, the caption and the two new rows. `apps/macos/Steno/StenoApp.swift`: `OnboardingOpener`
   uses `shouldOpen`, `OnboardingWindowContent` calls `markCompleted()` on finish.
   Tests in `apps/macos/StenoTests/OnboardingViewModelTests.swift`: the step order; the 81 x 4
   permission matrix still holds with the setup steps skipped; Done and Later on page 1 advance
   instead of finishing; `isFinished` stays false with all permissions granted until Summaries and
   Obsidian vault are saved or skipped; saving Summaries with a valid URL and model marks the step
   done and rebuilds the pipeline; saving Obsidian vault with a temp directory marks it done and an
   unwritable path shows the destination's message; `shouldOpen` is true when the flag is unset
   even with every permission granted and false once `markCompleted()` ran; the preview guard stays
   in the opener.

9. Settings copy from the UX spec in `apps/macos/Steno/Settings/SettingsView.swift`.

10. Plan cross-references: add a one-line pointer to this plan in the onboarding row and the "LLM
    wiring" deviation of `.plans/2026-09-25-macos-app-and-release.md` in the same PR.

Steps 1 to 3 are one PR (`fix(core): skip the LLM passes instead of running the fakes`). Steps 4 to
7 are one PR (`feat(macos): show missing summary and vault configuration`). Steps 8 to 10 are one
PR (`feat(macos): configure summaries and the vault in onboarding`). Each PR passes CI before the
next opens.

## Verification

- `swift test --filter StenoCoreTests` and `swift test --filter stenoTests` in the local
  `steno-swift` container (CI has no Linux job; the PR body states the run and its test count) and
  on the macOS runner; the new pipeline test is the one that pins decision 2.
- `xcodebuild test -scheme StenoTests` and `-scheme Steno` (UI smoke test with the banner) on the
  macOS runner.
- Manual, fresh user account: launch; page 1 shows the four permission rows; grant microphone and
  system audio; skip Calendar and Local network; Done advances to page 2; enter a local LM Studio
  URL and model, Test connection reports "Connected: ...", Save; choose a vault folder, Save; the
  window closes. Record a two-minute call; the meeting lands Ready with a real summary, a token
  count and an "obsidian-folder: delivered" badge.
- Manual, existing install (the owner's): launch; onboarding opens on page 2 with both rows open;
  press Finish; the main window shows the "Summaries and export are off" banner; open the first
  meeting; the Summary tab reads "Summary skipped: no LLM endpoint is configured."; the footer
  reads "Not exported: no Obsidian vault is configured." Configure the endpoint from the banner;
  the Summary tab switches to "No summary yet" with Run summary; run it; the summary appears and
  the header shows the token count. Configure the vault; the footer switches to "Not exported yet"
  with Export now; run it; the badge reads delivered and the folder opens from the Finder button.
  Press Not now on the banner; it hides; relaunch with the vault removed from settings and the
  banner returns.
- Manual, no configuration at all: record a call; the meeting lands Ready with the transcript, no
  summary, no tasks, no token line and the original title. Nothing in the export folder because
  there is none.

## Open questions

- Should the banner offer "Don't remind me again" for users who run Steno as a transcript-only
  recorder on purpose? Deferred until someone asks; the per-launch reminder is cheap.
- After the endpoint is configured, should the banner offer "Run summaries for N meetings"? It
  would call `rerunSummary` per meeting sequentially. Left out of v1 because each run costs tokens
  and the owner has one meeting.
- Should "Run summary" on a meeting processed without cleanup warn more loudly that the transcript
  was never cleaned, or should a "Process again" action (decode onward) exist? Needs its own plan;
  it re-transcribes and re-diarizes.
- Should an app menu item reopen the onboarding window later ("Set Up Steno…")? Not added; the
  banner and the Settings tabs cover the same ground after the first run.
- Does `steno process` need a `--fake-llm` flag for local development without an endpoint? The
  end-to-end tests inject the stub server directly, so probably not.
