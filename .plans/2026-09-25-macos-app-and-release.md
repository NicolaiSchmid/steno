# Steno v1: macOS app and release

Status: implementation plan, 2026-09-25. Binding context:
[`2026-09-25-v1-program.md`](2026-09-25-v1-program.md) (boundaries, model,
protocols) and [`2026-09-24-initial-scope.md`](2026-09-24-initial-scope.md).
Owns `apps/macos/`, the release workflow, and Forge runner adoption in CI.
Implemented last, after the module workstreams have merged.

## Goal

Ship the SwiftUI shell that turns the Swift package into a product: a menu bar
recorder with a main window (meeting list, Summary, Transcript, Tasks,
Scratchpad), the speaker review sheet, the meeting-detection prompt, settings,
permission onboarding, EventKit titles and attendees, Sparkle updates, and a
Developer ID signed, notarised DMG built by a tag-triggered GitHub Actions
workflow that runs on the Forge runner when it is online and on GitHub-hosted
`macos-15` otherwise. The app target contains AppKit/SwiftUI glue and view
models only; every decision that the CLI or iOS could need lives in the package.

## Non-goals

- Editing summary, transcript or task text; custom templates; audio import UI.
- App Sandbox, Mac App Store, Intel, macOS 14. Localised UI strings.
- Hiding the Dock icon (`LSUIElement`). v1 is a regular app whose window can
  be closed while the menu bar item stays.
- Sparkle deltas, phased rollouts, channels, signed feeds; Homebrew cask;
  release notes beyond the GitHub Release body.
- Pixel design. Tokens are dark-first and mirror `mobile/global.css`
  (luminance ladder, alpha-veil surfaces, hairline borders, achromatic CTA).

## Platform facts the plan relies on

| Fact | Source | Status |
|---|---|---|
| `MenuBarExtra` is macOS 13+, `.menuBarExtraStyle(.window)` renders a popover-like window, coexists with `Window` scenes | Apple SwiftUI docs | verified |
| `SMAppService.mainApp.register()/unregister()`, `status` in `.notRegistered/.enabled/.requiresApproval/.notFound`, `openSystemSettingsLoginItems()`; macOS 13+ | Apple ServiceManagement docs | verified |
| Process taps need `NSAudioCaptureUsageDescription`; prompt appears on first record from an aggregate device containing a tap; no public status API; own TCC category, not Screen Recording | Apple Core Audio taps article, AudioCap README | verified |
| macOS 15 local network privacy: registering a Bonjour service triggers the prompt, plain TCP listen/accept does not; needs `NSLocalNetworkUsageDescription` and `NSBonjourServices` (`_name._tcp`); no status API, detect via `kDNSServiceErr_PolicyDenied`; test on 15.1+ | TN3179 | verified |
| EventKit on macOS 14+: `requestFullAccessToEvents()`, `NSCalendarsFullAccessUsageDescription`, status `.fullAccess` | Apple EventKit docs, WWDC23 sample | verified |
| Sparkle 2.10.0 via SPM: package `https://github.com/sparkle-project/Sparkle`, product `Sparkle`, macOS 12+; non-sandboxed apps need no XPC services or `SUEnable*Service` keys; `SUFeedURL`, `SUPublicEDKey`, increasing `CFBundleVersion`; never sign with `--deep`; Xcode archive/export re-signs Sparkle helpers correctly | Sparkle docs, Package.swift | verified |
| `generate_appcast --ed-key-file -` reads the private key from stdin; `--download-url-prefix`, `--link`, `-o`, `--maximum-deltas` exist | Sparkle 2.x `generate_appcast/main.swift` | verified |
| `xcrun notarytool submit <dmg> --key <p8> --key-id <id> --issuer <uuid> --wait`, then `xcrun stapler staple <dmg>`; DMG, ZIP, PKG accepted; 75 submissions/day | notarytool man page, Apple notarisation article | verified |
| Temporary keychain recipe for CI (`security create-keychain`, `import -f pkcs12`, `set-key-partition-list`, `list-keychain -d user -s`, `delete-keychain` in an `always()` step on self-hosted) | GitHub Docs | verified |
| XcodeGen `packages: {Name: {path: ../..}}`, `dependencies: [{package:, product:}]`, `info.properties`, `entitlements.properties`, `type: bundle.ui-testing` | XcodeGen ProjectSpec | verified |
| Hardened runtime entitlement identifiers `com.apple.security.device.audio-input`, `com.apple.security.personal-information.calendars`, `com.apple.security.cs.disable-library-validation` | Apple entitlements docs (fetch blocked) | unverified, confirm in Xcode capability editor |
| Sparkle CLI tools live under `<DerivedData>/SourcePackages/artifacts/sparkle/Sparkle/bin/` after SPM resolution | Sparkle docs mention `../artifacts/sparkle/Sparkle/bin/` | unverified path, spike S2 |
| XCUITest launches on GitHub-hosted `macos-15` without the "Timed out while enabling automation mode" failure | actions/runner-images history is mixed | unverified, spike S1 |

## Decisions

- Bundle id `uno.schmid.steno.mac`, team `KQB68F43PW` (same team as mobile,
  which owns `uno.schmid.steno`). Immutable once v1 ships; TCC and Sparkle key
  off it.
- `MARKETING_VERSION` comes from the tag (`v1.2.3` -> `1.2.3`);
  `CURRENT_PROJECT_VERSION` (`CFBundleVersion`) is `git rev-list --count` of
  the tagged commit, monotonic on `main`. Local builds use `0.0.0` / `0`.
- Debug: ad-hoc signing (`CODE_SIGN_IDENTITY=-`), hardened runtime off, so PR
  builds need no certificate and Sparkle still loads. Developers who test
  permissions locally set `DEVELOPMENT_TEAM` and an Apple Development identity
  in the git-ignored `apps/macos/Config/Local.xcconfig`, because TCC forgets
  ad-hoc signed apps on every rebuild. Release: manual signing, `Developer ID
  Application`, hardened runtime on, timestamp. No sandbox; entitlements carry
  only audio-input and calendars.
- Appcast is a release asset. `SUFeedURL` is
  `https://github.com/NicolaiSchmid/steno/releases/latest/download/appcast.xml`;
  each release regenerates an appcast containing only that release
  (`--maximum-deltas 0`, `--link` to the Releases page). Pre-releases are
  flagged on GitHub so `latest` skips them.
- DMG via `hdiutil` (staging folder with `Steno.app` and an `Applications`
  symlink, `UDZO`); no AppleScript layout, no third-party DMG tool.
- Signing secrets are imported into a temporary keychain per run on both
  runner kinds; nothing release-related is pre-installed in the Forge runner
  account. The keychain is deleted in an `always()` step.
- The LLM API key lives in the login keychain as a generic password (service
  `uno.schmid.steno.mac`, account `llm-api-key`); settings keep only URL and
  model. The CLI reads the same secret from an environment variable through
  the `SecretStore` protocol requested below.
- Calendar attendees are resolved at recording start: the app writes title
  and `Participant` rows from the matching EventKit event (overlapping now,
  else the next within 15 minutes). The pipeline needs no calendar access.
- Detection prompt is a floating `NSPanel` (`.nonactivatingPanel`, `.floating`)
  that auto-dismisses after 60 s and never appears while recording.

## Public API of the app target

Nothing imports the app target; these are the seams the tests use.

```swift
@MainActor @Observable final class AppEnvironment {           // composition root
    init(store: MeetingStore, settings: SettingsStore, secrets: any SecretStore,
         capture: any CaptureControlling, detector: any MeetingDetecting,
         pipeline: any PipelineControlling, engines: SpeechEngineFactory.Type, models: ModelStore,
         speakerMemory: any SpeakerMemory, delivery: any DeliveryDispatcher, handover: any HandoverControlling,
         calendar: any CalendarProviding, loginItem: any LoginItemControlling,
         permissions: any PermissionsChecking, updater: any UpdaterControlling)
    static func live() throws -> AppEnvironment
    static func preview() -> AppEnvironment                    // in-memory GRDB, fakes
}
protocol CaptureControlling: Sendable { func start(mode: CaptureMode, meetingID: UUID) async throws; func stop() async throws -> AudioAsset; var states: AsyncStream<CaptureState> { get }; var levels: AsyncStream<LaneLevels> { get } }
protocol MeetingDetecting: Sendable { var events: AsyncStream<MeetingDetector.Event> { get } }
protocol LoginItemControlling: Sendable { var status: LoginItemStatus { get }; func setEnabled(_: Bool) throws; func openSystemSettings() }
protocol PermissionsChecking: Sendable {
    func microphone() async -> PermissionState                 // AVCaptureDevice
    func calendar() async -> PermissionState                   // EKEventStore, .fullAccess
    var systemAudio: PermissionState { get }                   // .unknown until SystemAudioPermission.request() ran
    var localNetwork: PermissionState { get }                  // .unknown / .denied from kDNSServiceErr_PolicyDenied
    func request(_ kind: PermissionKind) async -> PermissionState
}
protocol CalendarProviding: Sendable { func currentOrNextEvent(within: TimeInterval) async throws -> CalendarEvent?; func attendees(of: CalendarEvent) -> [Attendee] }
protocol UpdaterControlling: AnyObject { var canCheckForUpdates: Bool { get }; var automaticallyChecks: Bool { get set }; func checkForUpdates() }
struct KeychainSecretStore: SecretStore { }                    // Security framework, generic password
```

## Window and view-model inventory

| Scene | View model | State | Actions |
|---|---|---|---|
| `MenuBarExtra` (`.window` style; icon changes while recording) | `MenuBarViewModel` | `recording: .idle / .recording(since:, levels:) / .stopping`, `queue: [QueueItem(meeting, stage, progress)]`, `nextEvent: CalendarEvent?`, `launchAtLogin: LoginItemStatus` | `toggleRecording()`, `startInPerson()`, `openMeeting(id)`, `openMain()`, `openSettings()`, `setLaunchAtLogin(Bool)`, `checkForUpdates()`, `quit()` |
| `Window("Steno", id: "main")`: sidebar list + detail | `MeetingListViewModel` | `meetings` (GRDB `ValueObservation`), `query` (FTS), `stateFilter`, `tagFilter`, `selection` | `select(id)`, `delete(id)` with confirmation, `revealAudio(id)` |
| Detail with tabs Summary, Transcript, Tasks, Scratchpad | `MeetingDetailViewModel` | `meeting`, `segments`, `tasks`, `speakers`, `deliveries`, `templates`, `tab` | `setTags`, `setTemplate(id)` + `rerunSummary()`, `setKeepAudio(Bool)`, `reexport()`, `openSpeakerReview()`, `saveScratchpad(String)` debounced (requested change 2) |
| Speaker review `.sheet` on detail | `SpeakerReviewViewModel` | `unresolved: [SpeakerCard(clipRange, suggestions: person / attendee / inferredName)]`, `playing: id?` | `play(id)` (AVAudioPlayer over `AudioAsset.url` bounded to `sampleClipRange`), `name(id, String)`, `assign(id, personID)`, `merge(a, b)`, `skip(id)`, `finish()` enrols and re-exports |
| Detection prompt (floating `NSPanel`) | `DetectionPromptViewModel` | `trigger: .micOpened(bundleID:) / .calendarStart(event:)`, `countdown` | `start()`, `ignoreOnce()`, `ignoreApp()`, `dismiss()` |
| `Settings` scene, one tab each | `GeneralSettingsViewModel` (launch at login, detection on/off, ignored apps), `AudioSettingsViewModel` (input device, recordings folder), `SpeechSettingsViewModel` (engine id, `ModelStore.ensure` progress), `LLMSettingsViewModel` (base URL, model, key in Keychain, `test()`), `TemplatesSettingsViewModel`, `RetentionSettingsViewModel`, `ObsidianSettingsViewModel` (`ObsidianSettings` fields, `validate()`), `PhonesSettingsViewModel` (paired devices, `startPairing()` QR, `forget(id)`), `UpdatesSettingsViewModel` | per tab | per tab |
| Onboarding `Window(id: "onboarding")`, shown until required permissions are granted | `OnboardingViewModel` | steps microphone -> system audio -> calendar (optional) -> local network (optional, deferred to first pairing), each with `PermissionState` | `request(step)`, `openSystemSettings(step)`, `continue()` |
| `commands`: `CommandGroup(after: .appInfo)` Check for Updates; `Record` menu with start/stop shortcut | | | |

Design: `Theme.swift` mirrors `mobile/global.css` (dark-first, `NSColor`
dynamic providers), `Motion.swift` mirrors `mobile/src/lib/motion.ts`.

## Files to create

```
apps/macos/project.yml                         xcodegen spec: Steno, StenoTests, StenoUITests; local package at ../..; Sparkle 2.10.0
apps/macos/Config/{Base,Debug,Release}.xcconfig team, bundle id, macOS 15.0, Swift 6 strict; Debug ad-hoc, Release Developer ID
apps/macos/Config/Local.xcconfig.example       template for the git-ignored developer override
apps/macos/Config/Steno.entitlements           audio-input, calendars
apps/macos/Config/ExportOptions.plist          method developer-id, signingStyle manual, teamID
apps/macos/Steno/StenoApp.swift                @main, scenes, commands, updater controller
apps/macos/Steno/AppEnvironment.swift          composition root, live() and preview()
apps/macos/Steno/Resources/Assets.xcassets     app icon, menu bar template icons (idle, recording)
apps/macos/Steno/Resources/InfoPlist.strings   usage descriptions
apps/macos/Steno/Design/{Theme,Motion}.swift   tokens mirroring mobile
apps/macos/Steno/MenuBar/{MenuBarViewModel,MenuBarView}.swift
apps/macos/Steno/Main/{MeetingListViewModel,MeetingListView,MeetingDetailViewModel,MeetingDetailView}.swift
apps/macos/Steno/Main/Tabs/{Summary,Transcript,Tasks,Scratchpad}Tab.swift   markdown via AttributedString, grouped segments, read-only tasks, TextEditor
apps/macos/Steno/Speakers/{SpeakerReviewViewModel,SpeakerReviewSheet,ClipPlayer}.swift
apps/macos/Steno/Detection/{DetectionPromptViewModel,DetectionPanel}.swift   NSPanel host + SwiftUI content
apps/macos/Steno/Settings/<Tab>SettingsViewModel.swift, <Tab>SettingsView.swift   nine tabs listed above
apps/macos/Steno/Onboarding/{OnboardingViewModel,OnboardingView}.swift
apps/macos/Steno/Services/LoginItemController.swift     SMAppService
apps/macos/Steno/Services/PermissionsService.swift      AVCaptureDevice, EKEventStore, SystemAudioPermission, local network probe
apps/macos/Steno/Services/CalendarService.swift         EventKit, one shared EKEventStore
apps/macos/Steno/Services/KeychainSecretStore.swift
apps/macos/Steno/Services/UpdaterController.swift       SPUStandardUpdaterController wrapper
apps/macos/Steno/Services/Fakes.swift                   fakes for previews and UI tests (DEBUG only)
apps/macos/StenoTests/*Tests.swift             one file per view model, plus AppEnvironment, KeychainSecretStore, ThemeTokens
apps/macos/StenoUITests/LaunchSmokeTests.swift
apps/macos/scripts/{build-release,make-dmg,make-appcast}.sh   archive+export+verify; hdiutil+codesign+notarise+staple; generate_appcast
apps/macos/README.md                           generate the project, sign locally, cut a release
.github/workflows/release.yml                  tag-triggered release
.github/workflows/swift-ci.yml                 modified: runner variable, app job
.gitignore                                     add apps/macos/*.xcodeproj, apps/macos/Config/Local.xcconfig, apps/macos/build/
```

## Steps

Each step is at most one day and ends in a PR with a green CI run.

1. Project skeleton: `project.yml`, xcconfigs, entitlements, Info.plist keys
   (`NSMicrophoneUsageDescription`, `NSAudioCaptureUsageDescription`,
   `NSCalendarsFullAccessUsageDescription`, `NSLocalNetworkUsageDescription`,
   `NSBonjourServices`, `SUFeedURL`, `SUPublicEDKey` placeholder,
   `SUEnableAutomaticChecks`), empty `StenoApp` with a `Window` and a
   `MenuBarExtra`, Sparkle dependency, test targets, `app` job in
   `swift-ci.yml`.
   Accept: `xcodegen generate && xcodebuild -scheme Steno build` passes on
   CI; `.xcodeproj` is git-ignored.
2. Composition root and tokens: `AppEnvironment.live()` wires the package
   types, `preview()` uses in-memory GRDB and fakes; `Theme`, `Motion`.
   Accept: `AppEnvironmentTests` builds both roots; `ThemeTokensTests` reads
   `mobile/global.css` and checks every token name has a Swift counterpart.
3. Menu bar: start/stop, elapsed time, recording icon, queue rows from
   pipeline events, launch at login via `LoginItemController` (register on
   first launch, surface `.requiresApproval` with a System Settings link).
   Accept: state-transition and queue-ordering tests with fakes; manual: the
   item appears in System Settings > Login Items.
4. Main window: list with FTS search and filters over `ValueObservation`,
   detail with four tabs, tags, template change and re-run, keep toggle,
   re-export, delivery footer.
   Accept: list tests (filtering, selection survives update), detail tests
   (re-run calls pipeline with `.reexport`); UI smoke test passes.
5. Speaker review sheet: bounded clip playback, name entry, suggestions from
   `CosineSpeakerMemory.rankedCandidates`, calendar attendees and the
   inferred name; merge; skip; finish enrols and re-exports. Opens when a
   meeting turns `.ready` with unresolved speakers and the window is
   frontmost, else a badge.
   Accept: tests for merge, assign, skip (`personID` stays nil); manual: the
   clip is audible.
6. Detection prompt: subscribe to `MeetingDetector.events`, resolve bundle id
   to app name, show panel, start `.macCall`, per-app ignore list in
   settings, suppressed while recording.
   Accept: view-model tests (ignore list, countdown, suppression); manual:
   opening FaceTime shows the panel within 2 s.
7. Settings part one: General, Audio (device list, recordings folder via
   `NSOpenPanel`), Speech (engine, `ModelStore.ensure` progress), LLM (URL,
   model, key to Keychain, Test through `LanguageModel`).
   Accept: `KeychainSecretStoreTests` round-trip on a throwaway account;
   `LLMSettingsViewModelTests` against a stub `LanguageModel`.
8. Settings part two: Templates, Retention, Obsidian (`ObsidianSettings`,
   `Destination.validate`), Phones (paired list, pairing QR, forget),
   Updates (auto-check bound to Sparkle, Check now).
   Accept: Obsidian tests surface validation errors; Phones tests with a
   fake handover.
9. Onboarding and permissions: `PermissionsService`, ordered steps, deep
   links to the right panes; the system audio step runs
   `SystemAudioPermission.request()` and reports silence versus signal.
   Accept: onboarding tests (order, optional steps skippable, done only when
   required ones granted); manual on a fresh macOS user.
10. Calendar: `CalendarService`; at recording start resolve the event, write
    title and participants; menu bar shows the next event; detection prompt
    offers "start for <event>" at event start.
    Accept: tests with an injected event source (no EventKit in tests);
    manual: a recorded test call carries the event title.
11. Sparkle wiring: `UpdaterController`, Check for Updates command, updates
    settings, Debug `SUFeedURL` overridable from `Local.xcconfig`.
    Accept: a Debug build installs an update served by `python3 -m
    http.server` (spike S2 artefacts).
12. Release workflow, dry run: `release.yml` on `workflow_dispatch` and `v*`
    tags; keychain import, xcodegen, `xcodebuild archive` with versions from
    the tag, `-exportArchive`, `codesign --verify --deep --strict`, zip as a
    workflow artifact. Run once on GitHub-hosted and once on Forge.
    Accept: both green; `codesign -dv --entitlements -` shows Developer ID,
    hardened runtime, the two entitlements.
13. Release workflow, full: DMG, notarise, staple, `spctl -a -t open
    --context context:primary-signature -v`, appcast, draft release, upload,
    publish. Switch `swift-ci.yml` and `release.yml` to the runner variable.
    Update `apps/macos/README.md`.
    Accept: `v0.9.0-rc.1` pre-release has DMG and appcast; a fresh Mac opens
    the DMG without a Gatekeeper warning.
14. Release rehearsal: install `v0.9.0`, tag `v0.9.1`, confirm Sparkle
    installs it; run the manual checklist; fix; tag `v1.0.0`.
    Accept: checklist signed off in the version-bump PR.

## CI and release

### `swift-ci.yml` changes

- Both jobs: `runs-on: ${{ fromJSON(vars.MACOS_RUNS_ON || '"macos-15"') }}`.
- New `app` job, gated on `apps/macos/project.yml`, `needs: package`:
  checkout, setup-xcode `latest-stable`, `brew install xcodegen`, `xcodegen
  generate --spec apps/macos/project.yml`, `xcodebuild -project
  apps/macos/Steno.xcodeproj -scheme Steno -destination platform=macOS
  -derivedDataPath apps/macos/build/DerivedData test`, upload `.xcresult` on
  failure. UI tests run in the same invocation unless spike S1 fails, then
  only when `vars.MACOS_RUNS_ON` is set (Forge has a GUI session).
- Fork PRs must never reach the self-hosted runner: keep "Require approval
  for all outside collaborators" on (human step) and force `macos-15` when
  `github.event.pull_request.head.repo.fork` is true.

### `release.yml`

Trigger: tags `v*` and `workflow_dispatch` (dry run: no notarisation, no
release). `permissions: contents: write`. Concurrency group `release`, no
cancellation. 60-minute timeout.

1. Checkout `fetch-depth: 0`, setup-xcode, xcodegen.
2. Import `MACOS_CERTIFICATE_P12_BASE64` with `MACOS_CERTIFICATE_PASSWORD`
   into `$RUNNER_TEMP/steno-release.keychain-db` (random per-run password).
3. `build-release.sh <version> <build>`: archive, export, verify.
4. `make-dmg.sh`: staging, `hdiutil create -format UDZO`, `codesign --sign
   "Developer ID Application" --timestamp`, write `ASC_PRIVATE_KEY` to
   `$RUNNER_TEMP/AuthKey.p8`, `notarytool submit --key --key-id --issuer
   --wait`, `stapler staple`, `spctl` assessment.
5. `make-appcast.sh`: `echo "$SPARKLE_PRIVATE_KEY" | generate_appcast
   --ed-key-file - --download-url-prefix
   https://github.com/NicolaiSchmid/steno/releases/download/<tag>/ --link
   https://github.com/NicolaiSchmid/steno/releases --maximum-deltas 0 -o
   dist/appcast.xml dist/`.
6. `gh release create <tag> --draft --generate-notes`, `gh release upload`
   DMG and appcast, `gh release edit --draft=false` (`--prerelease` when the
   tag contains a hyphen).
7. `always()`: `security delete-keychain`, `rm -f $RUNNER_TEMP/AuthKey.p8`.

Secrets: `MACOS_CERTIFICATE_P12_BASE64`, `MACOS_CERTIFICATE_PASSWORD`,
`ASC_KEY_ID`, `ASC_ISSUER_ID`, `ASC_PRIVATE_KEY`, `SPARKLE_PRIVATE_KEY`.
Variable: `MACOS_RUNS_ON`. Team id and public EdDSA key are committed.

### One-time human steps (CI never does these)

Apple:
- Create a Developer ID Application certificate, export as `.p12` with a
  password, base64-encode, store both in 1Password, delete local copies.
- App Store Connect > Users and Access > Integrations > Team Keys: create a
  Developer-role key, download the `.p8` once, note Key ID and Issuer ID.
- Run Sparkle's `generate_keys` once on a Mac, `generate_keys -x <file>` to
  export, store in 1Password, paste the public key into `project.yml`.

GitHub (`NicolaiSchmid/steno`):
- Add the six secrets. Set `MACOS_RUNS_ON` to
  `["self-hosted","macOS","ARM64"]` once the runner exists (the watchdog
  maintains it afterwards).
- Settings > Actions: "Require approval for all outside collaborators";
  workflow permissions allow `contents: write`. Optional tag protection `v*`.

Forge (per `agent-infra/runbooks/forge.md`):
- Runner in `~/actions-runner-macos-steno`: `./config.sh --url
  https://github.com/NicolaiSchmid/steno --token <token> --name
  forge-macos-steno --labels forge,macos,arm64,ios,xcode --unattended`, then
  `./svc.sh install && ./svc.sh start`.
- `brew install xcodegen` (CI installs it too; pre-installing saves time).
- Add `steno` to `hosts/atlas/forge-runner-watchdog.nix`, redeploy atlas.
- No certificates or keys in the runner keychain; the workflow brings and
  removes its own.

CI does everything in `release.yml` on every tag. GitHub-hosted `macos-15` is
the fallback, free on this public repository, roughly 3x slower than Forge.

## Tests

Unit (`StenoTests`, XCTest, no network, no TCC): one class per view model
using fakes for every protocol in the public API section plus `SpeechEngine`,
`LanguageModel`, `Destination`, `DeliveryDispatcher`, `PipelineControlling`;
`AppEnvironmentTests`; `KeychainSecretStoreTests` (unique account, deleted in
tearDown); `ThemeTokensTests`.

UI smoke (`StenoUITests`): `LaunchSmokeTests.testMainWindowOpens` launches
with `-steno-ui-testing` (preview environment, no permission requests),
asserts `app.state == .runningForeground`, `app.windows["main"]
.waitForExistence(timeout: 10)`, the fixture meeting is listed, and each of
the four tabs is selectable. One test, under 30 seconds.

Manual checklist, run by a human on a Mac before tagging v1, recorded in
the release PR:
1. Fresh macOS 15.1+ user or VM: download the DMG from the pre-release, drag
   to Applications, first open shows no Gatekeeper warning.
2. Onboarding: microphone and calendar prompts, system audio prompt on the
   test recording, buttons open the right System Settings panes.
3. Login item appears in System Settings > Login Items; the toggle reflects
   it after relaunch.
4. FaceTime or Zoom call: detection panel appears, record two minutes, stop;
   processing completes; menu bar queue empties.
5. Speaker review plays a ten-second clip; naming and merging persist; a
   second call with the same person is auto-labelled.
6. Four tabs render; template re-run changes the summary; scratchpad text
   survives relaunch.
7. Obsidian folder matches the documented layout; re-export overwrites only
   app-written files.
8. Retention `0` deletes audio after processing; keep toggle prevents it.
9. Phone pairing: local network prompt on first pairing; a phone recording
   arrives and is processed.
10. Sparkle: an installed older build offers and installs the new version;
    after relaunch no permission re-prompts.

## Spikes

- S1 (before the `app` job becomes required): XCUITest launches on
  GitHub-hosted `macos-15` without the automation-mode timeout. If not, the
  UI smoke test is Forge-only (requested change 7).
- S2 (before step 11): `generate_appcast --ed-key-file -` from the SPM
  artifacts path under `-derivedDataPath` produces an appcast that a Debug
  build accepts from a local HTTP server; confirms the artifacts path.
- S3 (before step 13): one `workflow_dispatch` run on a `v0.0.1-spike` tag
  notarises with Sparkle embedded, manual signing, no provisioning profile.
  Fallback: re-sign Sparkle helpers by hand in `build-release.sh`, or strip
  the XPC services (allowed when not sandboxed).
- S4 (before step 8): Bonjour registration through StenoHandover triggers
  the local network prompt on 15.1+ and the listener accepts after approval;
  `PolicyDenied` maps to `.denied`.

## Needs from other workstreams

- StenoCore: `MeetingStore` with GRDB `ValueObservation` for meetings,
  segments, tasks, speakers, deliveries; `SettingsStore` with typed keys for
  every settings tab; `ProcessingPipeline` exposing `AsyncStream<PipelineEvent>`
  plus `enqueue`, `rerunSummary(meetingID, templateID)`; `Template`
  catalogue; FTS query; `SecretStore` and `mergeSpeakers` (requested below).
- StenoAudio: `CaptureSession` (`start(meetingID:)`, `stop()`, `states`,
  `levels`), `MeetingDetector.events` with bundle id, `SystemAudioPermission`,
  an input-device list with change notifications.
- StenoSpeech: `SpeechEngineFactory`, `ModelStore.ensure` progress,
  `CosineSpeakerMemory.rankedCandidates` and `merge(_:into:)` for people.
- StenoLLM: a `LanguageModel` factory from (baseURL, model, secret), a cheap
  validation request for the Test button, inferred speaker names on `Speaker`.
- StenoAdapters: `DeliveryCoordinator.reexport(meetingID:destinationID:)`,
  `ObsidianSettings`, user-readable `ObsidianError` from `validate`.
- StenoHandover: `HandoverControlling` (start/stop listener, `pairingPayload()`
  for the QR, `pairedDevices`, `forget(id)`), the Bonjour service type for
  `NSBonjourServices`, an `AsyncStream` of received recordings for the queue.

## Requested changes to the program document

1. Add `public protocol SecretStore: Sendable { func get(_ key: SecretKey) throws -> String?; func set(_ value: String?, for key: SecretKey) throws }` to StenoCore; the app implements it with the Keychain, the CLI with environment variables. Settings must not hold the API key.
2. Scratchpad: the scope calls the four tabs "display only" and also defines Scratchpad as "free text typed by the user". This plan builds Scratchpad as an editable text area with debounced save and the other three tabs read-only. Confirm or strike the field.
3. Add `mergeSpeakers(_:into:)` (two clusters inside one meeting: moves segments, averages embeddings, deletes the merged `Speaker`) to the StenoCore store API. This is distinct from the person merge the speech plan added to `CosineSpeakerMemory`.
4. Make `ProcessingPipeline` observability explicit: `AsyncStream<PipelineEvent>` with queued, progress(stage, fraction), ready, failed. The menu bar queue and the detail footer depend on it.
5. Record the macOS bundle id `uno.schmid.steno.mac` and team `KQB68F43PW` next to the mobile identifiers as immutable.
6. `mobile-cd.yml` hard-codes `runs-on: [self-hosted, macOS, ARM64]`; switch it to `${{ fromJSON(vars.MACOS_RUNS_ON || '"macos-15"') }}` in the handover workstream so one variable governs every macOS job.
7. Verification standard: if spike S1 fails, the UI smoke test is required only on the Forge runner and reported as skipped on GitHub-hosted runs.
