# Steno: configure summaries and the vault during onboarding

Status: proposal, 2026-09-28. Triggered by first-run feedback.

Binding plans: scope authority [`2026-09-24-initial-scope.md`](2026-09-24-initial-scope.md);
app plan [`2026-09-25-macos-app-and-release.md`](2026-09-25-macos-app-and-release.md) (onboarding
row in the window inventory, "Settings tabs" and "LLM wiring" deviations); LLM plan
[`2026-09-25-llm-and-templates.md`](2026-09-25-llm-and-templates.md); adapters plan
[`2026-09-25-adapters-obsidian.md`](2026-09-25-adapters-obsidian.md) ("no destination configured"
is an empty delivery list). This plan amends the onboarding row and the LLM wiring note of the app
plan; nothing else in those plans changes.

## Goal

The owner ran the app for the first time and asked: "the onboarding didn't ask me to configure the
vault or the post-processing LLM APIs. I assume that will be flagged as missing once the first
recording is completed?"

The answer today is no. Nothing flags it, and worse, the first meeting gets a fabricated summary.
This plan (a) records what happens today with file and line evidence, (b) stops the pipeline from
running a test double in the product, (c) makes the missing configuration visible where the user
looks (onboarding, main window, meeting detail), and (d) reuses the existing re-run and re-export
paths so a meeting processed before configuration can still get its summary and its export.

## Findings

What happens today when the first recording completes with no LLM endpoint and no vault:

1. Onboarding is permissions only. `OnboardingViewModel.steps` is built from
   `PermissionKind.allCases` (`apps/macos/Steno/Onboarding/OnboardingViewModel.swift:17`); the
   window closes itself the moment every step is granted or skipped
   (`OnboardingView.swift:138-140`). The opener runs once per launch and opens the window only
   while a required permission is missing (`apps/macos/Steno/StenoApp.swift:132-143`). Once
   microphone and system audio are granted the window never appears again, so a user never sees a
   hint that anything else needs setting up.

2. Without an endpoint the product runs test doubles. `AppEnvironment.live` builds the pipeline
   with `LLMWiring.passes(settings:apiKey:)` (`apps/macos/Steno/AppEnvironment.swift:207`), which
   is nil until `Settings.llmBaseURL` and `llmModel` are both set
   (`apps/macos/Steno/Services/LLMWiring.swift:211-218`, `Sources/StenoLLM/LLMEndpoint.swift:167-172`).
   The fallbacks are `PassthroughCleaner()` and `FakeSummarizer()`
   (`AppEnvironment.swift:213-214`), both from `Sources/StenoCore/Testing/FakeLLM.swift`. The CLI
   does the same (`Sources/steno/Wiring.swift:74-75`).

3. The fake summarizer fabricates content. `FakeSummarizer.output(for:usage:)`
   (`Sources/StenoCore/Testing/FakeLLM.swift:72-99`) returns: the title "Summary of <title>", one
   bullet per template section whose lead is the first speaker's cluster label and whose text is
   the first transcript segment, one decision "Decision from Speaker 1.", one task "Follow up on
   Default." assigned to Speaker 1, and a usage of 200 prompt and 100 completion tokens. The
   summarize stage replaces the title when the meeting has no calendar event
   (`Sources/StenoCore/Pipeline/Stages/Summarize.swift:276-278`) and sums the usage (:280), on top
   of the passthrough cleaner's fixed 100 plus 50 (`FakeLLM.swift:31`, folded in at
   `Sources/StenoCore/Pipeline/ProcessingPipeline.swift:159`). The meeting lands `.ready`.

4. What the user sees: a "Ready" chip (`apps/macos/Steno/Main/MeetingDetailView.swift:290`), a
   header line "450 tokens" although no request was made (:297-299), a Summary tab with a heading
   per section and a bullet quoting the first sentence (`Main/Tabs/SummaryTab.swift:49-77`), a
   Tasks tab with the fake task, a Decisions block with the fake decision, and a meeting renamed to
   "Summary of Untitled" unless the calendar supplied a title. There is no error, no warning and
   no chip that says any of this is a placeholder. The transcript itself is real but uncleaned:
   `PassthroughCleaner` returns segments untouched (`FakeLLM.swift:38-42`), so the Denglish and
   casing cleanup the scope promises did not run.

5. Delivery is silently absent. `DeliveryCoordinator.destinations(for:)` returns `[]` when
   `settings.obsidian == nil` (`Sources/StenoAdapters/Runtime/DeliveryCoordinator.swift:32-35`)
   and `deliverAll` returns without writing a row (:56). The footer shows "Not delivered yet"
   (`MeetingDetailView.swift:415-418`), the same text a meeting shows in the seconds before a
   configured delivery lands. Nothing distinguishes "no vault" from "not yet".

6. The only hint in the app is in Settings > LLM: "No endpoint configured: summaries use a
   placeholder until a base URL and model are saved." (`apps/macos/Steno/Settings/SettingsView.swift:262-268`),
   visible only if the user opens that tab. The Obsidian tab has no equivalent when the toggle is
   off (:284-327). The menu bar item (`MenuBar/MenuBarView.swift`) and the main window
   (`Main/MainWindow.swift`) carry no setup state at all.

7. The recovery paths exist but are undiscoverable and incomplete. Actions > "Re-run summary" and
   "Re-export" (`MeetingDetailView.swift:322-325`) are enabled for `.ready` and failed meetings
   (`Main/MeetingDetailViewModel.swift:100-103`). Saving the LLM tab rebuilds the pipeline
   (`Settings/LLMSettingsViewModel.swift:84`) and the coordinator reads settings per delivery
   (`DeliveryCoordinator.swift:41`), so after configuring, both actions do the right thing. But the
   user has to know the summary was fake, and `rerunSummary` summarizes the stored segments only
   (`ProcessingPipeline.swift:179-194`): the cleanup pass is never repeated, so a meeting processed
   before the endpoint was configured keeps its raw transcript for good.

8. Minor: `Sources/StenoCore/Model/Settings.swift:6` says the audio folder is picked in onboarding;
   it is not (it lives in Settings > Audio).

The tests that pin the fake output (`Tests/StenoCoreTests/PipelineIntegrationTests.swift:31`,
`FakesTests.swift:63`, `StageTests.swift:378`) inject the fake explicitly and stay valid; they are
not evidence that the product should run it.

## Non-goals

- New LLM providers, provider pickers or anything beyond base URL, model name and API key. The LLM
  client keeps sending text only.
- A second destination, or moving the people folder, task tag and audio copy options into
  onboarding. Those stay in Settings > Obsidian.
- Bulk re-processing of every unsummarised meeting after the endpoint is configured. Each meeting
  has its own Run summary action; a bulk action is an open question below.
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
   and `:32`, which is why a cleanup failure keeps the transcript today); the summarize stage posts its progress event and leaves `summary`, `llmUsage`, tasks
   and decisions empty and the title untouched. The meeting still lands `.ready` and still
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

4. Onboarding asks, and the app nudges afterwards. Both, not one. Asking during onboarding is
   justified because the first recording is the one that cannot be repaired: the cleanup pass runs
   once, at processing time (finding 7), so a user who configures the endpoint after the first
   meeting keeps a raw transcript for that meeting. Nudging afterwards is still needed because both
   steps are optional (a local-only transcript recorder is a legitimate way to use Steno, and a
   local LLM server may not be running on day one), and because existing installs have already
   passed onboarding.

5. The two new steps reuse `LLMSettingsViewModel` and `ObsidianSettingsViewModel` as they are.
   Same validation, same `Test connection`, same Save that rebuilds the pipeline, same
   `ObsidianFolderDestination.validate()`. Onboarding shows fewer fields (no context window, no
   people folder, no task tag, no audio copy) and says where the rest lives. Reason: one code path
   for writing these settings; the onboarding view is layout only.

6. Onboarding opens once more for installs that have not seen the setup steps. A `UserDefaults`
   flag `steno.onboardingCompleted` (same pattern as `AppController.loginItemRegisteredKey`) is set
   when the window finishes, by Done, by Later or by the auto-close. The opener shows the window
   when a required permission is missing or the flag is unset. Existing installs therefore see the
   window one more time with the permission rows already green and only the two setup rows open,
   which is exactly the owner's situation. An app menu item "Set Up Steno…" reopens it at any time.

8. The setup steps are a second page, not rows on the permissions screen. Owner instruction,
   2026-09-28: "additional onboarding steps, later on, not in the first screen". The window
   becomes two pages: page 1 is the permissions screen exactly as it is; page 2, "Summaries and
   export", holds the two optional steps and appears when page 1 finishes by Done or Later.
   Existing installs with the flag unset open straight on page 2. Reason: the first screen
   stays about the one thing recording cannot do without, and a user who wants a local-only
   recorder is one click from done on page 2 as well.

7. The main window gets one setup banner, the detail pane gets per-meeting status rows. The banner
   is a launch-time reminder and hides for the rest of the launch on "Not now"; it comes back on the
   next launch while the configuration is still missing. The detail rows are factual per-meeting
   states and are never dismissed. Reason: a permanently dismissable banner would need a second
   flag and a reset rule when configuration changes; the per-meeting rows already carry the signal
   permanently, so the banner can stay simple.

8. Settings gets deep links. `SettingsView` binds its `TabView` to a `SettingsTab` selection and
   `AppController` gets `requestedSettingsTab`, mirroring `requestedMeetingID`. Every "Open
   Settings" button in this plan lands on the right tab.

9. The CLI follows the same rule. `Wiring.dependencies` passes nil passes when
   `Wiring.llmComponents` is nil and `steno process` reports the summary as skipped. One rule for
   the app and the CLI; the fakes stay reachable for tests through the existing injection points.

## UX spec

### Onboarding window

Title stays "Welcome to Steno". Intro copy becomes:

> A few permissions, then where summaries come from and where meetings go. Audio never leaves this Mac.

Directly under the intro copy sits the one-line retention sentence from
[`2026-09-28-audio-retention-keep-forever.md`](2026-09-28-audio-retention-keep-forever.md)
(step 7 there), for example "Recordings are kept forever in Steno. Change this any time in
Settings > Audio." Visual treatment of the whole window (hidden title bar, raised cards,
neutral chips) comes from
[`2026-09-28-macos-visual-redesign.md`](2026-09-28-macos-visual-redesign.md); this plan adds
rows and copy only.

Two pages in the same window, with a "Step 1 of 2" / "Step 2 of 2" caption above the title.
Page 1 keeps the current title and rows 1 to 4. Page 2 is titled "Summaries and export" with
the intro "Optional. Steno works as a local transcript recorder without either." and holds
rows 5 and 6, each with the "Optional" chip. Page 2 appears after page 1 finishes by Done or
Later, or on its own for installs whose flag is unset. Rows, in order:

1. Microphone (required, unchanged).
2. System audio (required, unchanged).
3. Calendar (optional, unchanged).
4. Local network (optional, unchanged, "Got it").
5. Summaries (optional). Explanation:

   > Steno sends the transcript text, never audio, to an OpenAI-compatible endpoint to clean it up and write the summary, tasks and decisions. Without one, meetings keep a raw transcript and no summary.

   Fields: "Base URL" (prompt `http://127.0.0.1:1234/v1`), "Model" (prompt `gpt-4.1-mini`),
   "API key" secure field (prompt "optional for local servers"). Buttons: "Test connection"
   (secondary, disabled until the URL validates), "Save" (primary, disabled while
   `validationMessage` is set), "Skip" (plain). The test result line and the validation message
   are the view model's strings, shown as in Settings. Footnote: "The context window and the rest
   live in Settings > LLM." When saved and configured the row collapses to a check and
   "Saved: <model> at <host>".

6. Obsidian vault (optional). Explanation:

   > Steno writes each meeting into Meetings/<date>-<slug>/ inside the vault: a folder note, transcript, tasks, VTT and JSON. It never touches files it did not write. Without a vault, meetings stay in Steno.

   Controls: the vault path field with "Choose…" (NSOpenPanel, directories only, prompt "Use
   vault"), "Save" (primary, validates through the destination and shows `ObsidianError` verbatim
   on failure), "Skip" (plain). Footnote: "People pages, the task tag and the audio copy live in
   Settings > Obsidian." When saved the row collapses to a check and "Saved: <vault folder name>".

Page 1 bottom buttons unchanged: "Later" until the required permissions are granted, then
"Done"; both advance to page 2 instead of closing. Page 2 bottom buttons: "Back" (plain) and
"Finish" (primary, always enabled); the window closes when both rows are saved or skipped or on
Finish. Finishing by any route sets `steno.onboardingCompleted`. The auto-close on page 1 is
replaced by the advance to page 2.

App menu (after "About"): "Set Up Steno…" opens the onboarding window.

### Main window banner

Shown at the top of the detail column, above the selected meeting or the "Select a meeting" empty
state, when at least one meeting exists, the configuration is incomplete and the banner was not
dismissed this launch. Never in the preview environment's onboarding sense; the banner does show in
preview because preview settings have no endpoint and no vault, and the UI smoke test uses that.

- Both missing:

  > Summaries and export are off. Steno has no LLM endpoint and no Obsidian vault yet, so meetings keep a raw transcript on this Mac.

  Buttons: "Set up summaries" (Settings > LLM), "Choose a vault" (Settings > Obsidian), "Not now".

- Endpoint missing only:

  > Summaries are off. Steno has no LLM endpoint yet, so meetings keep a raw transcript.

  Buttons: "Set up summaries", "Not now".

- Vault missing only:

  > Export is off. Steno has no Obsidian vault yet, so meetings stay on this Mac.

  Buttons: "Choose a vault", "Not now".

Styling: a `Card` with a `MessageRow(kind: .info)` and the buttons in a trailing `HStack`; motion
from `Motion.functional` on appear and dismiss. No new colours.

### Meeting detail

Summary tab, replacing the "No summary" text when the meeting is `.ready` and `summary == nil`:

- Endpoint not configured now:

  > Summary skipped: no LLM endpoint is configured. The transcript is complete.

  Button "Set up summaries" (Settings > LLM).

- Endpoint configured now:

  > No summary yet: this meeting was processed before an LLM endpoint was configured.

  Button "Run summary" (calls `rerunSummary()`), footnote "Summary only; the transcript stays as recorded."

Tasks tab in the same two states: "No tasks: the summary was skipped." with the same button as the
Summary tab. Queued and processing keep today's pending texts.

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

`SettingsTab` cases `general, audio, speech, llm, obsidian, phones, updates`. Callers set
`controller.requestedSettingsTab`, call the `openSettings` environment action and `NSApp.activate()`.
`SettingsView` applies the request in `onChange(of: controller.requestedSettingsTab, initial: true)`
and clears it, like `MainWindow` does for `requestedMeetingID`.

## Implementation steps

1. Core: optional LLM passes.
   Files: `Sources/StenoCore/Pipeline/PipelineStage.swift` (`PipelineDependencies.cleaner` and
   `.summarizer` optional with default nil), `Sources/StenoCore/Pipeline/Stages/Cleanup.swift`
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
   "LLM endpoint"; `redeliver` succeeds. `Tests/StenoCoreTests/StageTests.swift`: the skipped
   summarize stage clears rows a previous fake run wrote. Existing tests that inject the fakes are
   untouched.

2. CLI: `Sources/steno/Wiring.swift` passes `llm?.cleaner` and `llm?.summarizer` without
   fallbacks. `Sources/steno/Commands/Process.swift` keeps printing only the meeting id on stdout
   (`Process.swift:136`, scripts depend on it) and writes "summary skipped: no LLM endpoint
   configured" to stderr when the meeting lands ready without a summary. Update any
   `Tests/stenoTests` expectation that relies on the fake summary (a title starting "Summary of",
   a task, a decision) from the default wiring; end-to-end tests already use the stub server for
   real summaries.

3. App wiring: `apps/macos/Steno/AppEnvironment.swift` `live()` passes `llm?.cleaner` and
   `llm?.summarizer` without fallbacks. `preview()` keeps its explicit fakes so seeded meetings
   render summaries in UI tests. Fix the comment at `Sources/StenoCore/Model/Settings.swift:6`.

4. Setup state: a new `apps/macos/Steno/Services/SetupState.swift`, `@MainActor @Observable final
   class SetupState` with `llmConfigured`, `vaultConfigured` (both from `Settings` via
   `LLMEndpoint(settings:) != nil` and `settings.obsidian != nil`), `bannerDismissed`, and a pure
   `static func derive(_ settings: Settings)` for tests. `AppEnvironment` owns one instance;
   `AppController.launch()` adds an observer over `environment.settings.observe()` that updates it,
   alongside the existing observers, and `shutdown()` cancels it with the rest.
   Tests in `apps/macos/StenoTests/AppControllerTests.swift`: after `launch()`, saving the LLM tab
   flips `llmConfigured`, saving the Obsidian tab flips `vaultConfigured`, `dismissBanner()` hides
   the banner and a fresh controller shows it again.

5. Settings deep links: `SettingsTab` and the `TabView(selection:)` binding in
   `apps/macos/Steno/Settings/SettingsView.swift`; `requestedSettingsTab` on
   `apps/macos/Steno/AppController.swift`; a small `View` extension `openSettings(_ tab:, controller:)`
   in `apps/macos/Steno/Design/Components.swift` used by the banner, the footer, the tabs and the
   menu bar. Test in `apps/macos/StenoTests/AppControllerTests.swift` that the request is set and
   cleared.

6. Detail pane states: `apps/macos/Steno/Main/MeetingDetailViewModel.swift` takes `SetupState` in
   its designated init (the convenience init reads `environment.setup`) and exposes
   `summaryStatus` (`pending`, `present`, `skippedUnconfigured`, `skippedRunnable`),
   `exportStatus` (`noVault`, `notExported`, `delivered([Delivery])`), `canRerunSummary` and
   `canReexport`. `apps/macos/Steno/Main/Tabs/SummaryTab.swift`, `Tabs/TasksTab.swift` and
   `Main/MeetingDetailView.swift` (actions menu, footer) render the copy above. `TabText` is left
   alone so `TabTextSnapshotTests` keeps its fixture lines.
   Tests in `apps/macos/StenoTests/MeetingDetailViewModelTests.swift`: each status for (ready with
   summary), (ready without summary, unconfigured), (ready without summary, configured),
   (processing); `canRerunSummary` false while unconfigured; `exportStatus` for empty deliveries
   with and without a vault.

7. Banner: `apps/macos/Steno/Main/SetupBanner.swift` (view over `SetupState` and
   `MeetingListViewModel.all.isEmpty`), mounted in `apps/macos/Steno/Main/MainWindow.swift` above
   the detail column content. UI smoke test in `apps/macos/StenoUITests`: in the preview
   environment the banner is present with both buttons, "Not now" hides it.

8. Onboarding: `apps/macos/Steno/Onboarding/OnboardingViewModel.swift` gets a `StepKind` enum
   (`permission(PermissionKind)`, `summaries`, `vault`), builds the six steps in the order above,
   owns an `LLMSettingsViewModel` and an `ObsidianSettingsViewModel` from the environment,
   `isFinished` requires the setup steps saved or skipped, `markCompleted()` writes
   `steno.onboardingCompleted`, and a static `shouldOpen(permissions:defaults:)` replaces the loop
   in `OnboardingOpener`. `OnboardingView.swift` renders the two new rows.
   `apps/macos/Steno/StenoApp.swift`: `OnboardingOpener` uses `shouldOpen`,
   `OnboardingWindowContent` calls `markCompleted()` on finish, `AppCommands` adds "Set Up Steno…".
   Tests in `apps/macos/StenoTests/OnboardingViewModelTests.swift`: the step order; the 81 x 4
   permission matrix still holds with the setup steps skipped; `isFinished` stays false with all
   permissions granted until Summaries and Obsidian vault are saved or skipped; saving Summaries
   with a valid URL and model marks the step done and rebuilds the pipeline; saving Obsidian vault
   with a temp directory marks it done and an unwritable path shows the destination's message;
   `shouldOpen` is true when the flag is unset even with every permission granted and false once
   `markCompleted()` ran; the preview guard stays in the opener.

9. Settings copy from the UX spec in `apps/macos/Steno/Settings/SettingsView.swift`.

10. Plan cross-references: add a one-line pointer to this plan in the onboarding row and the "LLM
    wiring" deviation of `.plans/2026-09-25-macos-app-and-release.md` in the same PR.

Steps 1 to 3 are one PR (`fix(core): skip the LLM passes instead of running the fakes`). Steps 4 to
7 are one PR (`feat(macos): show missing summary and vault configuration`). Steps 8 to 10 are one
PR (`feat(macos): configure summaries and the vault in onboarding`). Each PR passes CI before the
next opens.

## Verification

- `swift test --filter StenoCoreTests` and `swift test --filter stenoTests` on the Linux
  container; the new pipeline test is the one that pins decision 2.
- `xcodebuild test -scheme StenoTests` and `-scheme Steno` (UI smoke test with the banner) on the
  macOS runner.
- Manual, fresh user account: launch; onboarding shows six rows; grant microphone and system audio;
  skip Calendar and Local network; enter a local LM Studio URL and model, Test connection reports
  "Connected: ...", Save; choose a vault folder, Save; the window closes. Record a two-minute call;
  the meeting lands Ready with a real summary, a token count and an "obsidian-folder: delivered"
  badge.
- Manual, existing install (the owner's): launch; onboarding opens once with the four permission
  rows green and the two setup rows open; press Later; the main window shows the "Summaries and
  export are off" banner; open the first meeting; the Summary tab reads "Summary skipped: no LLM
  endpoint is configured."; the footer reads "Not exported: no Obsidian vault is configured."
  Configure the endpoint from the banner; the Summary tab switches to "No summary yet" with Run
  summary; run it; the summary appears and the header shows the token count. Configure the vault;
  the footer switches to "Not exported yet" with Export now; run it; the badge reads delivered and
  the folder opens from the Finder button. Press Not now on the banner; it hides; relaunch with the
  vault removed from settings and the banner returns.
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
- Does `steno process` need a `--fake-llm` flag for local development without an endpoint? The
  end-to-end tests inject the stub server directly, so probably not.
