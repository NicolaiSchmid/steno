# Steno: Settings redesign and a working update feed

> Superseded in part by [`2026-09-29-macos-webview-ui.md`](2026-09-29-macos-webview-ui.md)
> (2026-09-29): the Settings window is rebuilt as web UI in `WKWebView`. The sections, their
> subtitles, copy, view models and the update feed decisions stand; the SwiftUI layout spec
> (UX spec, `NavigationSplitView`, `Form(.grouped)`) does not.

Status: accepted, 2026-09-28. Triggered by the owner's review of the Settings window after the
first-run feedback round ("everything is not user-facing designed", "updates are broken").

Binding context: [`2026-09-24-initial-scope.md`](2026-09-24-initial-scope.md) (scope, pluggable
boundaries, audio never leaves the device) and
[`2026-09-25-macos-app-and-release.md`](2026-09-25-macos-app-and-release.md) (app structure,
view models, release workflow).

This plan **supersedes** two parts of
[`2026-09-28-macos-visual-redesign.md`](2026-09-28-macos-visual-redesign.md): Decision 12
("Settings stays native, light touch") and implementation step 11 (Settings, notes to `faint`).
That plan's tokens, components and every other screen stand. It also **amends** the release
plan's appcast decision (appcast as a release asset behind `releases/latest`): the feed moves to
a rolling appcast on a branch, see Decision 9. Both files carry a note at the top.

Siblings this plan absorbs or defers to:

- [`2026-09-28-audio-retention-keep-forever.md`](2026-09-28-audio-retention-keep-forever.md)
  owns the retention rule's behaviour (the forever default, `RetentionSweep.keepAll()`, the
  detail header line). This plan adopts its Settings copy (picker label "Keep recordings", the
  three option titles, the per-option footnote, the folder size and "Show in Finder") so that
  plan only adds behaviour when it lands.
- [`2026-09-28-onboarding-vault-and-llm.md`](2026-09-28-onboarding-vault-and-llm.md) specifies
  `SettingsTab` and `AppController.openSettings(_:)`. This plan implements the deep link as
  `SettingsSection` with the new section names; the onboarding plan's callers use it as is.

## Goal

Settings a friend can read without knowing what Sparkle, an appcast, an LLM, a token, a
listener or a diarizer is. Sections named by outcome, status visible before a section is opened,
one save model, no identifiers in copy, permissions reachable after onboarding. And an update
check that succeeds on every installed build, including release candidates.

## Findings

Settings today (`apps/macos/Steno/Settings/SettingsView.swift`, seven tabs in a `TabView` over
grouped `Form`s, 560 pt wide):

| # | Where | What is wrong |
|---|---|---|
| S1 | Tab titles | "LLM", "Speech", "Obsidian", "Phones", "Updates" name subsystems, not outcomes |
| S2 | Phones > Listener | Port number, the Mac's UUID in monospace, recording UUID prefixes while receiving |
| S3 | Speech > Models | Licence strings and Hugging Face repository names under every model; phase strings like "starting" |
| S4 | LLM | "Context window (tokens)", placeholder `gpt-4.1-mini`, probe report "structured output json_schema", "summaries use a placeholder until…" |
| S5 | Obsidian | "People folder (relative, optional)", "Task tag", "Meetings/<date>-<slug>/ with a folder note, transcript, tasks, VTT and JSON", "Takes effect on the next delivery" |
| S6 | Updates | "delivered by Sparkle", the repository URL |
| S7 | Every tab | `"Settings could not be loaded: \(error)"`: the raw Swift error description is the message |
| S8 | LLM, Obsidian vs the rest | Two save models: Save buttons here, save-on-change there |
| S9 | Audio, Obsidian | Full path strings; the vault is a free-text field |
| S10 | Whole window | No permissions after onboarding: nothing shows microphone, system audio or calendar state, nothing grants Calendar later, nothing re-runs the system audio test |
| S11 | Whole window | No status at a glance: whether summaries or export are set up is only visible inside the tab |

Updates (`apps/macos/project.yml` `SUFeedURL`, `.github/workflows/release.yml`):

| # | What | Why it breaks |
|---|---|---|
| U1 | `SUFeedURL` is `releases/latest/download/appcast.xml` | GitHub never points `latest` at a pre-release. The only release is `v0.9.0-rc.1`, a pre-release, so the feed is a 404 (verified 2026-09-28). Scheduled checks fail silently; "Check now" shows Sparkle's generic error |
| U2 | Each appcast holds one release | A release candidate can never be offered a newer candidate; only a stable release ends the 404 |
| U3 | Launch checklist items #69 and #72 are open | The Sparkle install path has never been exercised end to end |

## Non-goals

- New settings (no shortcut editor, no appearance override, no per-template editing).
- Changes to `Settings` storage, `SettingsStore` or any StenoCore type. The retention default
  and `keepAll()` stay in the retention plan.
- Sparkle deltas, phased rollouts, a user-visible channel picker. The pre-release lane is
  automatic and invisible.
- Localisation.
- Rewriting the onboarding window (the onboarding plan).

## Decisions

1. **A sidebar window, not tabs.** `NavigationSplitView` with a `List` of six sections on the
   left and one grouped `Form` on the right, 760 by 520 points. Reason: the Summaries and
   Transcription content is tall, the QR code needs room, and a sidebar row can carry a status
   subtitle. Apple's own System Settings established the idiom.
2. **Six sections named by outcome**: General, Recording, Transcription, Summaries, Export,
   iPhone. The enum is `SettingsSection` (the onboarding plan's `SettingsTab`, renamed), with
   `AppController.requestedSettingsSection` and `openSettings(_:)` mirroring `requestedMeetingID`.
   Updates fold into General. Reason: the user asks "how do I get summaries", not "where is the
   LLM".
3. **Status in the sidebar.** Each row's subtitle is computed by `SettingsOverviewViewModel`
   from `Settings`, the model store, the paired devices and the permissions: General shows the
   update status or the version, Recording "Ready" or "Permission needed", Transcription "Ready",
   "Download needed" or "Downloading", Summaries the preset or model name or "Not set up",
   Export the vault folder name or "Off", iPhone the paired count. Reloaded whenever the
   selection changes and on appear. Reason: setup state is visible before a section is opened;
   the onboarding plan's banner and the sidebar say the same thing from the same `Settings`.
4. **One save model.** Every control saves on change; text fields commit when focus leaves them
   or on Return. Summaries and Export lose their Save buttons; a status row shows the
   consequence ("Connected", "Not set up", the validation message). The view models keep
   `save()` for the tests and gain `commit()`; `commit()` saves only when the draft differs from
   what is stored and validates.
5. **No identifiers in copy.** No library names, ports, UUIDs, repository names, licence strings,
   engine ids or raw errors. Every view model error is a plain sentence in `error` plus the
   original text in `errorDetails`, rendered by `SettingsErrorRow` with a "Details" disclosure.
   The model licences and source repositories, which attribution requires somewhere, move to an
   Acknowledgements sheet reachable from General.
6. **Summaries get presets.** `LLMPreset`: LM Studio (`http://127.0.0.1:1234/v1`), Ollama
   (`http://127.0.0.1:11434/v1`), OpenRouter (`https://openrouter.ai/api/v1`), OpenAI
   (`https://api.openai.com/v1`), Anthropic (`https://api.anthropic.com/v1`), Custom. Selecting a
   preset fills the server address and a model placeholder; the preset is inferred from the
   stored URL on load. The context size sits under an "Advanced" disclosure with the default
   shown. After a successful commit the view model probes the endpoint on its own and shows
   "Connected" or the failure; "Test again" repeats it. Reason: most people pick one of five
   servers; the URL is an implementation detail of that choice.
7. **Permissions live where they matter.** Recording shows Microphone and System audio rows with
   state and an action (Allow, Run the test recording, Open System Settings); General shows the
   Calendar row under "Meetings"; iPhone keeps the local network sentence. The rows reuse
   `PermissionKind.title` and `explanation` from onboarding. Reason: the permission explains
   itself next to the feature it unlocks.
8. **Recording folder as a folder.** Folder name with a folder glyph, the full path as a tooltip,
   "Show in Finder" and "Choose…", and the folder's size measured off the main actor.
9. **A rolling appcast on the `appcast` branch, with an automatic pre-release lane.** The feed
   URL becomes `https://raw.githubusercontent.com/NicolaiSchmid/steno/appcast/appcast.xml`. The
   release workflow, after publishing the GitHub release, merges the new release's item into the
   branch's `appcast.xml` (`apps/macos/scripts/merge-appcast.py`, newest first, one item per
   build number, capped at twenty) and pushes. Pre-release items carry
   `<sparkle:channel>beta</sparkle:channel>`. `UpdaterController.allowedChannels(for:)` returns
   `["beta"]` only when the running build's own version string contains a hyphen
   (`UpdateChannels.allowed(forVersion:)`), so release candidates see candidates and stable
   builds never do. The per-release appcast is still uploaded to each release so `v0.9.0-rc.1`
   installs, which read `releases/latest`, hop to the first stable release and pick up the new
   feed URL from it. Reason: no channel UI, no 404, no dependence on `latest`, and the feed
   survives a future move of the DMGs.
10. **General shows update status in words.** `UpdaterControlling` gains
    `automaticallyDownloadsUpdates` and `lastOutcome` (`upToDate`, `available(version)`,
    `failed(message)`), fed by the Sparkle delegate. General reads "Steno 0.9.0 · Up to date,
    checked today at 10:06" with "Check for Updates" and "Install updates automatically".

## UX spec

Window: sidebar 200 pt (rows: 28 pt icon well with `secondary` fill and radius 6, title 13
medium `foreground`, subtitle 11 `faint`), detail column fills the rest, `Form` `.grouped`,
one header per section (title 18 semibold `strong`, one purpose sentence 13 `muted-foreground`).

**General.** Header "General", "Steno runs in the menu bar and records when you ask it to."
Rows: "Open Steno at login" toggle (with the approval hint and "Open Login Items" when the
system asks); "Offer to record when a call starts" toggle, footnote "Steno notices when another
app opens the microphone and asks before recording."; Section "Meetings": "Name meetings from
your calendar" permission row (Calendar) with state and Allow / Open System Settings; "Summary
template" picker with the template's description as footnote. Section "Updates": one line
"Steno 0.9.0 · Up to date, checked today at 10:06" (or "Update available: 0.9.1", "Could not
check for updates" with details, "Not checked yet"), "Check for Updates" button, "Install
updates automatically" toggle, footnote "Updates are checked once a day." Footer link
"Acknowledgements…" opens the sheet.

**Recording.** Header "Recording", "Audio is recorded and kept on this Mac only." Section
"Permissions": Microphone and System audio rows, each with a state glyph (green check, red
cross, grey circle), the explanation as footnote when not granted, and the action. Section
"Microphone": picker with "System default" and the devices, a refresh glyph button
(accessibility label "Refresh microphones"). Section "Recordings": folder row (glyph, name,
tooltip path, "Show in Finder", "Choose…"), "Recordings use 4.2 GB" / "Measuring…" / "Size
unavailable"; "Keep recordings" picker with "Forever", "For N days" (stepper inline when
selected), "Until processed, then delete"; the footnote for the selection as the retention plan
words it; then "Short voice samples stay until you have named the speaker."

**Transcription.** Header "Transcription", "Speech is turned into text on this Mac. Nothing is
uploaded." Section "Language model" picker only when more than one engine is selectable, with
friendly names ("Parakeet · fast · 25 languages", "Whisper · slower · 99 languages"). Section
"On this Mac": one row per component, "Speech recognition" and "Speaker recognition", with
"Installed · 485 MB", "Downloading… 40%", "Not downloaded · 485 MB" and Download / Remove /
Retry; a failure as `SettingsErrorRow`. No licence or repository text.

**Summaries.** Header "Summaries", "Meeting summaries and tasks are written by an AI model you
choose. Only the transcript text is sent to it." Rows: "Service" preset picker; "Server
address" (hidden for a preset with a fixed address, shown for Custom and the two local
servers); "Model" with a preset placeholder; "API key" secure field (hidden for the two local
presets, footnote "Stored in your login keychain."); "Advanced" disclosure with "Context size"
and its footnote "Leave the default unless the model reports a shorter limit."; status row:
"Not set up" (info), "Checking…", "Connected" with the round trip, or the failure with details;
"Test again" button when configured.

**Export.** Header "Export", "Finished meetings can be written into an Obsidian vault as notes
you own." Rows: "Export to Obsidian" toggle; when on: "Vault" folder row (name, tooltip path,
"Choose…"); footnote "Each meeting becomes a folder with a note, the transcript and the tasks.
Steno never edits files it did not create."; "Advanced" disclosure: "People folder" text field
(placeholder "People", footnote "One page per person, inside the vault."), "Tag for tasks"
(placeholder "task"), "Copy the recording into the vault" toggle; status row with the
validation message when saving failed.

**iPhone.** Header "iPhone", "Record on your iPhone when you are away from the Mac. Recordings
travel over your Wi-Fi only, encrypted to this Mac." Section "Paired phones": rows with name,
"Paired 12 Sep · last seen today" and Remove; empty "No iPhone paired yet."; "Pair an iPhone…"
opens a sheet with the QR code (radius 8, hairline), "Scan this code with the Steno app on your
iPhone. It expires in 4 minutes.", Cancel. Transfers in flight: "Receiving from Nicolai's
iPhone… 40%" with a progress bar. When the handover identity is unavailable: one warning row
"Pairing is unavailable right now. Relaunch Steno to try again." with details. No port, no Mac
ID, no listener state.

**Acknowledgements sheet.** Two lists: the speech models (`ModelAsset.allCases`: display name,
licence, source repository) and the libraries (Sparkle, GRDB, FluidAudio, WhisperKit, SwiftNIO,
Swift Crypto, Speex) with licences. Close button.

## Implementation steps

1. **Update feed.** `apps/macos/scripts/merge-appcast.py` (stdlib XML; merges one release
   appcast into a rolling one, optional `--channel`, cap), `apps/macos/scripts/publish-appcast.sh`
   (fetches or creates the orphan `appcast` branch in a worktree, merges, commits, pushes),
   the "Rolling appcast" step in `.github/workflows/release.yml` after "Publish GitHub release",
   `SUFeedURL` in `apps/macos/project.yml`. `apps/macos/Steno/Services/UpdateChannels.swift`
   (pure), `UpdaterController` delegate methods, `UpdaterControlling` additions and `FakeUpdater`.
   Tests: `UpdateChannelsTests`, `InfoPlistTests.testSparkleKeys`, `ReleaseScriptsTests` (the
   merge script over fixtures, the workflow step order).
2. **Sections and deep link.** `apps/macos/Steno/Settings/SettingsSection.swift`;
   `requestedSettingsSection` and `openSettings(_:)` on `AppController`; `SettingsView` applies
   and clears the request. Test in `SettingsSectionTests`.
3. **View models.** Additions listed under Decisions 4 to 8 in the six existing view models plus
   `SettingsOverviewViewModel`. Tests extend `SettingsViewModelTests`: presets, `commit()` for
   Summaries and Export, retention titles and footnotes, permission rows, overview subtitles.
4. **Views.** `SettingsView.swift` rebuilt; `SettingsComponents.swift` (`SettingsHeader`,
   `SettingsErrorRow`, `PermissionRow`, `FolderRow`, `StatusText`); `AcknowledgementsView.swift`.
5. **Docs.** `apps/macos/README.md` release steps and the rehearsal section; the amendment notes
   at the top of the two superseded plans.

## Verification

- `xcodebuild test -scheme StenoTests` on the macOS runner: every test above.
- `xcodebuild test -scheme Steno` (UI smoke) unchanged.
- The merge script under `python3` on any platform: `ReleaseScriptsTests` runs it; the same
  invocation runs locally.
- Manual, after the next tag: the `appcast` branch holds the new item; `curl` of the feed URL
  returns it; a Debug build with `STENO_FEED_URL` at the raw URL offers the update; a release
  candidate build lists the beta item, a stable build does not.
- Manual, the window: every section in light and dark at 760 by 520, no text truncates, no
  identifier visible without holding a disclosure open.

## Open questions

- Whether the two local presets should probe on appear so "Connected" shows without an edit.
  Not in this plan; the status shows "Not checked" until the first commit or "Test again".
