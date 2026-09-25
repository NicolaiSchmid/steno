# Steno v1: macOS app and release

Status: implementation plan, 2026-09-25, reconciled and then revised the same day after the three reviews (program
review application log). Binding context: [`2026-09-25-v1-program.md`](2026-09-25-v1-program.md) (boundaries, model,
protocols) and [`2026-09-24-initial-scope.md`](2026-09-24-initial-scope.md). Owns `apps/macos/`, the release workflow,
and the `MACOS_RUNS_ON` runner variable in every macOS workflow (including `mobile-cd.yml`). Implemented last, after
the module workstreams have merged; its step 1 skeleton ships early because the audio workstream's permission spike
needs a bundled, signed app.

## Goal

Ship the SwiftUI shell that turns the Swift package into a product: a menu bar recorder with a main window (meeting
list, Summary, Transcript, Tasks, Scratchpad), the speaker review sheet, the meeting-detection prompt, settings,
permission onboarding, EventKit titles and attendees, Sparkle updates, and a Developer ID signed, notarised DMG built
by a tag-triggered GitHub Actions workflow that runs on the Forge runner when it is online and on GitHub-hosted
`macos-15` otherwise. The app target contains AppKit/SwiftUI glue and view models only; it injects the module types
directly and adds protocols only where a system framework has no seam.

## Non-goals

- Editing summary, transcript or task text; custom templates; audio import UI.
- App Sandbox, Mac App Store, Intel, macOS 14. Localised UI strings.
- Hiding the Dock icon (`LSUIElement`); v1 is a regular app whose window can be closed while the menu bar item stays.
  Sparkle deltas, phased rollouts, channels, signed feeds; Homebrew cask; release notes beyond the Release body.
- Pixel design. Tokens are dark-first and mirror `mobile/global.css` (luminance ladder, alpha-veil surfaces, hairline
  borders, achromatic CTA).
- App-side wrappers around package types (`CaptureSession`, `MeetingDetector`, `ProcessingPipeline`, `HandoverService`
  are injected as they are; their fakes come from the modules' `Testing/`).

## Platform facts the plan relies on

| Fact | Source | Status |
|---|---|---|
| `MenuBarExtra` is macOS 13+, `.menuBarExtraStyle(.window)` renders a popover-like window, coexists with `Window` scenes | Apple SwiftUI docs | verified |
| `SMAppService.mainApp.register()/unregister()`, `status` in `.notRegistered/.enabled/.requiresApproval/.notFound`, `openSystemSettingsLoginItems()`; macOS 13+ | Apple ServiceManagement docs | verified |
| Process taps need `NSAudioCaptureUsageDescription` as a literal Info.plist key (Xcode's dropdown lacks it); prompt appears on first record from an aggregate device containing a tap; no public status API; TCC service `kTCCServiceAudioCapture`, reset with `tccutil reset AudioCapture <bundle-id>`; TCC attributes Terminal-launched tools to Terminal | Apple Core Audio taps article, AudioCap README, correctness review | verified |
| macOS 15 local network privacy: registering a Bonjour service triggers the prompt, plain TCP listen/accept does not; needs `NSLocalNetworkUsageDescription` and `NSBonjourServices` (`_name._tcp`); no status API, detect via `kDNSServiceErr_PolicyDenied`; no reset on macOS (FB14944392), so test on a fresh user or VM, 15.1+ | TN3179 | verified |
| EventKit on macOS 14+: `requestFullAccessToEvents()`, `NSCalendarsFullAccessUsageDescription`, status `.fullAccess` | Apple EventKit docs, WWDC23 sample | verified |
| Sparkle 2.10.0 (released 2026-09-13) via SPM: package `https://github.com/sparkle-project/Sparkle`, product `Sparkle`, macOS 12+; non-sandboxed apps need no XPC services or `SUEnable*Service` keys; `SUFeedURL`, `SUPublicEDKey`, increasing `CFBundleVersion`; never sign with `--deep` (the warning is about signing flags, `codesign --verify --deep` is fine); Library Validation is what stops an ad-hoc Debug build from loading Sparkle, so hardened runtime off in Debug is the right condition | Sparkle docs, Package.swift, correctness review | verified |
| `generate_appcast --ed-key-file -` reads the private key from stdin; `--download-url-prefix`, `--link`, `-o`, `--maximum-deltas` exist; the SPM CLI tools live at `<DerivedData>/SourcePackages/artifacts/sparkle/Sparkle/bin/` | Sparkle 2.x `generate_appcast/main.swift`, Sparkle docs | verified |
| `xcrun notarytool submit <dmg> --key <p8> --key-id <id> --issuer <uuid> --wait`, then `xcrun stapler staple <dmg>`; DMG, ZIP, PKG accepted; 75 submissions/day | notarytool man page, Apple notarisation article | verified |
| Temporary keychain recipe for CI (`security create-keychain`, `import -f pkcs12`, `set-key-partition-list`, `list-keychain -d user -s`, `delete-keychain` in an `always()` step on self-hosted) | GitHub Docs | verified |
| XcodeGen `packages: {Name: {path: ../..}}`, `dependencies: [{package:, product:}]`, `info.properties`, `entitlements.properties` (every entitlement key listed), `type: bundle.ui-testing` | XcodeGen ProjectSpec | verified |
| GitHub `macos-15` image: macOS 15.7.x, Xcode 16.4 default (26.x installed), xcodegen not preinstalled | actions/runner-images readme | verified |
| FluidAudio ships a binary xcframework target (`NemoTextProcessing`); the archive/export must sign and notarise it | FluidAudio v0.17.3 `Package.swift` | verified |
| Hardened runtime entitlement identifiers `com.apple.security.device.audio-input`, `com.apple.security.personal-information.calendars`, `com.apple.security.cs.disable-library-validation` | Apple entitlements docs (fetch blocked) | unverified, confirm in Xcode capability editor |
| XCUITest launches on GitHub-hosted `macos-15` without the "Timed out while enabling automation mode" failure | actions/runner-images history is mixed | unverified, spike S1 |

## Decisions

- Bundle id `uno.schmid.steno.mac`, team `KQB68F43PW` (same team as mobile, which owns `uno.schmid.steno`). Immutable
  once v1 ships; TCC and Sparkle key off it.
- `MARKETING_VERSION` comes from the tag (`v1.2.3` -> `1.2.3`); `CURRENT_PROJECT_VERSION` (`CFBundleVersion`) is `git
  rev-list --count` of the tagged commit, monotonic on `main`. Local builds use `0.0.0` / `0`.
- Debug: ad-hoc signing (`CODE_SIGN_IDENTITY=-`), hardened runtime off, so PR builds need no certificate and Sparkle
  still loads. Developers who test permissions locally set `DEVELOPMENT_TEAM` and an Apple Development identity in the
  git-ignored `apps/macos/Config/Local.xcconfig`, because TCC forgets ad-hoc signed apps on every rebuild. Release:
  manual signing, `Developer ID Application`, hardened runtime on, timestamp. No sandbox, no provisioning profile;
  entitlements carry only audio-input and calendars. The handover identity therefore lives in the login keychain
  (`IdentityKeychain.loadOrCreate` at launch), never the data-protection keychain.
- Appcast is a release asset. `SUFeedURL` is
  `https://github.com/NicolaiSchmid/steno/releases/latest/download/appcast.xml`; each release regenerates an appcast
  containing only that release (`--maximum-deltas 0`, `--link` to the Releases page). Pre-releases are flagged on
  GitHub so `latest` skips them.
- DMG via `hdiutil` (staging folder with `Steno.app` and an `Applications` symlink, `UDZO`); no AppleScript layout, no
  third-party DMG tool.
- Signing secrets are imported into a temporary keychain per run on both runner kinds; nothing release-related is
  pre-installed in the Forge runner account. The keychain is deleted in an `always()` step.
- The LLM API key lives in the login keychain as a generic password (service `uno.schmid.steno.mac`, account
  `llm-api-key`) behind StenoCore's `SecretStore`; `Settings` keeps only URL, model and context tokens.
- Calendar attendees are resolved at recording start: `CalendarService` lists today's events, core's pure
  `CalendarMatch.pick` chooses the one overlapping now or the next within 15 minutes, and the app writes the title and
  `Participant` rows (`role == .them`) before calling `ProcessingPipeline.enqueue`. The pipeline needs no calendar
  access; the calendar never starts a recording.
- Detection prompt is a floating `NSPanel` (`.nonactivatingPanel`, `.floating`) that auto-dismisses after 60 s on an
  injected clock and never appears while recording.
- Settings has seven tabs: General, Audio, Speech, LLM, Obsidian, Phones, Updates. Default template lives in General,
  retention in Audio.
- `RetentionSweep` (core) runs at launch and after every processed meeting; the app owns no deletion code.

## Public API of the app target

Nothing imports the app target; these are the seams the tests use. Module types are injected as they are; protocols
exist only over system frameworks.

```swift
@MainActor @Observable final class AppEnvironment {           // composition root
    init(store: MeetingStore, settings: SettingsStore, secrets: any SecretStore, events: MeetingEventBus,
         captureBackend: any CaptureBackend, detector: MeetingDetector, pipeline: ProcessingPipeline,
         makeSpeechEngine: @Sendable (SpeechEngineID) throws -> any SpeechEngine, models: ModelStore,
         speakerMemory: any SpeakerMemory, handover: HandoverService, sweep: RetentionSweep,
         calendar: any CalendarProviding, loginItem: any LoginItemControlling,
         permissions: any PermissionsChecking, updater: any UpdaterControlling, clock: any Clock<Duration>)
    static func live() throws -> AppEnvironment
    static func preview() -> AppEnvironment                    // in-memory GRDB, StenoCore/StenoAudio/StenoSpeech/StenoLLM Testing/ fakes, SyntheticCaptureBackend
}
protocol LoginItemControlling: Sendable { var status: LoginItemStatus { get }; func setEnabled(_: Bool) throws; func openSystemSettings() }
protocol PermissionsChecking: Sendable {
    func microphone() async -> PermissionState                 // AVCaptureDevice
    func calendar() async -> PermissionState                   // EKEventStore, .fullAccess
    var systemAudio: PermissionState { get }                   // .unknown until SystemAudioPermission.request() ran
    var localNetwork: PermissionState { get }                  // .unknown / .denied from kDNSServiceErr_PolicyDenied
    func request(_ kind: PermissionKind) async -> PermissionState
}
protocol CalendarProviding: Sendable { func events(on day: Date) async throws -> [CalendarEvent]; func attendees(of: CalendarEvent) -> [Attendee] }   // CalendarMatch.pick does the choosing
protocol UpdaterControlling: AnyObject { var canCheckForUpdates: Bool { get }; var automaticallyChecks: Bool { get set }; func checkForUpdates() }
struct KeychainSecretStore: SecretStore { }                    // Security framework, generic password; program's async secret(for:) / setSecret
```

## Window and view-model inventory

| Scene | View model | State | Actions |
|---|---|---|---|
| `MenuBarExtra` (`.window` style; icon changes while recording) | `MenuBarViewModel` | `recording: .idle / .recording(since:, levels:) / .stopping` from a `CaptureSession` built per recording over `captureBackend`, `queue: [QueueItem(meeting, stage, fraction)]` from `MeetingEvent.progress` joined with `observeMeetings()`, `launchAtLogin: LoginItemStatus` | `toggleRecording()` (stop → set retention from `Settings`, write title and participants from the calendar, `pipeline.enqueue`), `startInPerson()`, `openMeeting(id)`, `openMain()`, `openSettings()`, `setLaunchAtLogin(Bool)`, `checkForUpdates()`, `quit()` |
| `Window("Steno", id: "main")`: sidebar list + detail | `MeetingListViewModel` | `meetings` (`observeMeetings()`), `query` (FTS), `stateFilter`, `tagFilter`, `selection` | `select(id)`, `delete(id)` with confirmation, `revealAudio(id)` |
| Detail with tabs Summary, Transcript, Tasks, Scratchpad | `MeetingDetailViewModel` | `export` (`observeMeeting(id:)`), `deliveries` (`observeDeliveries`), `templates` (`SummaryTemplate.bundled`), `tab`; Summary tab shows `SummaryMarkdown.render(export)` | `setTags`, `setTemplate(id)` + `rerunSummary()`, `setKeepAudio(Bool)` (retention `.keepForever`, clears `expiresAt`), `reexport()` (`pipeline.redeliver`), `openSpeakerReview()`, `saveScratchpad(String)` debounced on the injected clock (scratchpad is the one editable text) |
| Speaker review `.sheet` on detail | `SpeakerReviewViewModel` | `unresolved: [SpeakerCard]` for speakers whose `assignment` is `.unknown` or `.suggested` (clip, suggestions from `SpeakerMemory.candidates(for:limit:)`, calendar participants, `SpeakerNameSuggestion`), `playing: id?` | `play(id)` (`AVAudioPlayer` over `Speaker.sampleClipURL`), `name(id, String)` (new `Person` then `confirm`), `assign(id, person)` and `acceptSuggestion(id)` via `MeetingStore.confirm(speakerID:person:memory:)`, `mergeSpeakers(a, b)` via `MeetingStore.mergeSpeakers`, `mergePersons(a, b)` via `MeetingStore.mergePersons`, `skip(id)`, `finish()` calls `pipeline.redeliver` (enrolment already happened in `confirm`) |
| Detection prompt (floating `NSPanel`) | `DetectionPromptViewModel` | `trigger: .micOpened(bundleID:)`, `countdown` on `clock` | `start()`, `dismiss()` |
| `Settings` scene, one tab each | `GeneralSettingsViewModel` (launch at login, detection on/off, default template), `AudioSettingsViewModel` (input device, recordings folder, retention), `SpeechSettingsViewModel` (engine id, `ModelStore.ensure` progress), `LLMSettingsViewModel` (base URL, model, context tokens, key in Keychain, `test()` via `OpenAICompatibleClient.probe`), `ObsidianSettingsViewModel` (edits `Settings.obsidian`, `ObsidianFolderDestination(settings:).validate()`), `PhonesSettingsViewModel` (`HandoverService.pairedDevices()`, `beginPairing()` QR, `revoke(_:)`, `states`, `receipts`), `UpdatesSettingsViewModel` | per tab | per tab |
| Onboarding `Window(id: "onboarding")`, shown until required permissions are granted | `OnboardingViewModel` | steps microphone -> system audio -> calendar (optional) -> local network (optional, deferred to first pairing), each with `PermissionState` | `request(step)`, `openSystemSettings(step)`, `continue()` |
| `commands`: `CommandGroup(after: .appInfo)` Check for Updates; `Record` menu with start/stop shortcut; Debug-only `Capture Spike` item running `dev capture-spike` in-process for the audio S1 check | | | |

Design: `Theme.swift` mirrors `mobile/global.css` (dark-first, `NSColor` dynamic providers), `Motion.swift` mirrors
`mobile/src/lib/motion.ts`.

## Files to create

```
apps/macos/project.yml                         xcodegen spec: Steno, StenoTests, StenoUITests; local package at ../..; Sparkle 2.10.0
apps/macos/Config/{Base,Debug,Release}.xcconfig team, bundle id, macOS 15.0, Swift 6 strict; Debug ad-hoc, Release Developer ID
apps/macos/Config/Local.xcconfig.example       template for the git-ignored developer override
apps/macos/Config/Steno.entitlements           audio-input, calendars
apps/macos/Config/ExportOptions.plist          method developer-id, signingStyle manual, teamID
apps/macos/Steno/StenoApp.swift                @main, scenes, commands, updater controller, RetentionSweep at launch
apps/macos/Steno/AppEnvironment.swift          composition root, live() and preview()
apps/macos/Steno/Resources/Assets.xcassets     app icon, menu bar template icons (idle, recording)
apps/macos/Steno/Resources/InfoPlist.strings   usage descriptions
apps/macos/Steno/Design/{Theme,Motion}.swift   tokens mirroring mobile
apps/macos/Steno/MenuBar/{MenuBarViewModel,MenuBarView}.swift
apps/macos/Steno/Main/{MeetingListViewModel,MeetingListView,MeetingDetailViewModel,MeetingDetailView}.swift
apps/macos/Steno/Main/Tabs/{Summary,Transcript,Tasks,Scratchpad}Tab.swift   SummaryMarkdown via AttributedString, grouped segments, read-only tasks, TextEditor
apps/macos/Steno/Speakers/{SpeakerReviewViewModel,SpeakerReviewSheet,ClipPlayer}.swift
apps/macos/Steno/Detection/{DetectionPromptViewModel,DetectionPanel}.swift   NSPanel host + SwiftUI content
apps/macos/Steno/Settings/<Tab>SettingsViewModel.swift, <Tab>SettingsView.swift   seven tabs listed above
apps/macos/Steno/Onboarding/{OnboardingViewModel,OnboardingView}.swift
apps/macos/Steno/Services/LoginItemController.swift     SMAppService
apps/macos/Steno/Services/PermissionsService.swift      AVCaptureDevice, EKEventStore, SystemAudioPermission, local network probe
apps/macos/Steno/Services/CalendarService.swift         EventKit, one shared EKEventStore; no matching logic (CalendarMatch is core's)
apps/macos/Steno/Services/KeychainSecretStore.swift
apps/macos/Steno/Services/UpdaterController.swift       SPUStandardUpdaterController wrapper
apps/macos/Steno/Services/Fakes.swift                   fakes for the four app protocols only (DEBUG); module fakes come from the modules' Testing/
apps/macos/StenoTests/*Tests.swift             one file per view model, plus AppEnvironment, InfoPlist, KeychainSecretStore (gated), ThemeTokens
apps/macos/StenoUITests/LaunchSmokeTests.swift
apps/macos/scripts/{build-release,make-dmg,make-appcast}.sh   archive+export+verify (scripted codesign grep); hdiutil+codesign+notarise+staple; generate_appcast
apps/macos/README.md                           generate the project, sign locally, cut a release
.github/workflows/release.yml                  tag-triggered release
.github/workflows/swift-ci.yml                 modified: runner variable, app job
.github/workflows/mobile-cd.yml                modified: runner variable only
.gitignore                                     add apps/macos/*.xcodeproj, apps/macos/Config/Local.xcconfig, apps/macos/build/
```

## Steps

Each step is at most one day and ends in a PR with a green CI run. Checks are `[ci]` (the `app` job), `[opt-in:
STENO_KEYCHAIN_TESTS]` or `[manual]`.

1. Project skeleton (early, so the audio workstream can run its permission spike from it): `project.yml`, xcconfigs,
   entitlements, Info.plist keys (`NSMicrophoneUsageDescription`, `NSAudioCaptureUsageDescription`,
   `NSCalendarsFullAccessUsageDescription`, `NSLocalNetworkUsageDescription`, `NSBonjourServices`, `SUFeedURL`,
   `SUPublicEDKey` placeholder, `SUEnableAutomaticChecks`), empty `StenoApp` with a `Window`, a `MenuBarExtra` and the
   Debug-only `Capture Spike` command, Sparkle dependency, test targets, `app` job in `swift-ci.yml`. Accept `[ci]`:
   `xcodegen generate && xcodebuild -scheme Steno build` passes; `.xcodeproj` is git-ignored; `InfoPlistTests` asserts
   `Bundle.main.infoDictionary` contains the six usage and Bonjour keys as literals, `NSBonjourServices ==
   ["_steno._tcp"]`, `SUFeedURL` set, and `SUPublicEDKey` is not the placeholder in Release.
2. Composition root and tokens: `AppEnvironment.live()` wires the package types, `preview()` uses in-memory GRDB, the
   modules' `Testing/` fakes and `SyntheticCaptureBackend`; `Theme`, `Motion`. Accept `[ci]`: `AppEnvironmentTests`
   builds both roots; `ThemeTokensTests` reads `mobile/global.css` and checks every token name has a Swift
   counterpart.
3. Menu bar: start/stop over `CaptureSession`, elapsed time, recording icon, queue rows from `progress` joined with
   `observeMeetings()`, `enqueue` on stop, launch at login via `LoginItemController` (register on first launch,
   surface `.requiresApproval` with a System Settings link). Accept `[ci]`: state-transition and queue-ordering tests
   with the synthetic backend and a fake pipeline dependency set. `[manual]`: the item appears in System Settings >
   Login Items.
4. Main window: list with FTS search and filters over `observeMeetings()`, detail with four tabs (Summary via
   `SummaryMarkdown.render`), tags, template change and `rerunSummary`, keep toggle, `redeliver`, delivery footer from
   `observeDeliveries`. Accept `[ci]`: list tests (filtering, selection survives update), detail tests (re-run calls
   `rerunSummary`, re-export calls `redeliver`, scratchpad save fires once after the debounce on `ManualClock`); UI
   smoke test passes.
5. Speaker review sheet: clip playback from `sampleClipURL`, name entry, suggestions from `SpeakerMemory.candidates`,
   calendar participants and `SpeakerNameSuggestion`; accept suggestion; in-meeting cluster merge and person merge as
   two distinct actions; skip; finish re-exports. Opens on `speakersNeedReview` when the window is frontmost, else a
   badge. Accept `[ci]`: tests for both merges, `confirm` for new and existing persons, accept-suggestion, skip
   (`assignment` unchanged), `finish` calling `redeliver` exactly once. `[manual]`: the clip is audible.
6. Detection prompt: subscribe to `MeetingDetector.events`, resolve bundle id to app name, show panel, start
   `.macCall`, suppressed while recording and when `Settings.meetingDetectionEnabled` is off. Accept `[ci]`:
   view-model tests on `ManualClock` (countdown, suppression, disabled). `[manual]`: opening FaceTime shows the panel
   within 2 s.
7. Settings part one: General (launch at login, detection, default template), Audio (device list, recordings folder
   via `NSOpenPanel`, retention), Speech (engine, `ModelStore.ensure` progress), LLM (URL, model, context tokens, key
   to Keychain, Test through `OpenAICompatibleClient.probe`). Accept `[opt-in: STENO_KEYCHAIN_TESTS]`:
   `KeychainSecretStoreTests` round-trip on a throwaway account (skipped otherwise; the hosted login keychain may be
   locked). `[ci]`: `LLMSettingsViewModelTests` against `FakeLanguageModel`.
8. Settings part two: Obsidian (`Settings.obsidian`, `ObsidianFolderDestination.validate()`), Phones (paired list,
   pairing QR, revoke), Updates (auto-check bound to Sparkle, Check now). Accept `[ci]`: Obsidian tests surface
   `ObsidianError` messages verbatim; Phones tests with a `HandoverService` over `MeetingStore.inMemory()`,
   `FakeHandoverIntake`, `TestIdentity` and `advertise: false`.
9. Onboarding and permissions: `PermissionsService`, ordered steps, deep links to the right panes; the system audio
   step runs `SystemAudioPermission.request()` and reports silence versus signal. Accept `[ci]`: onboarding tests
   (order, optional steps skippable, done only when required ones granted). `[manual]`: on a fresh macOS user.
10. Calendar: `CalendarService` plus `CalendarMatch.pick`; at recording start resolve the event, write title and
    participants. Accept `[ci]`: tests with an injected event source (no EventKit in tests) covering overlapping,
    upcoming within 15 minutes, none. `[manual]`: a recorded test call carries title and attendees.
11. Sparkle wiring: `UpdaterController`, Check for Updates command, updates settings, Debug `SUFeedURL` overridable
    from `Local.xcconfig`. Accept `[manual]`: a Debug build installs an update served by `python3 -m http.server`
    (spike S2 artefacts).
12. Release workflow, dry run: `release.yml` on `workflow_dispatch` and `v*` tags; keychain import, xcodegen,
    `xcodebuild archive` with versions from the tag, `-exportArchive`, `codesign --verify --deep --strict`,
    `build-release.sh` greps `codesign -dv --entitlements -` for Developer ID, hardened runtime and exactly the two
    entitlements and exits non-zero otherwise; zip as a workflow artifact; once on GitHub-hosted, once on Forge.
    Accept `[ci]`: both runs green with the grep step passing.
13. Release workflow, full: DMG, notarise, staple, `spctl -a -t open --context context:primary-signature -v`, appcast,
    draft release, upload, publish; every macOS workflow on the runner variable; `apps/macos/README.md`. Accept
    `[manual]`: `v0.9.0-rc.1` pre-release has DMG and appcast; a fresh Mac opens the DMG without a Gatekeeper warning.
14. Release rehearsal: install `v0.9.0`, tag `v0.9.1`, confirm Sparkle installs it; run the manual checklist; fix; tag
    `v1.0.0`. Accept `[manual]`: checklist signed off in the version-bump PR.

## CI and release

### `swift-ci.yml` and `mobile-cd.yml` changes

- Every macOS job, including `mobile-cd.yml` (today hard-coded to `[self-hosted, macOS, ARM64]`): `runs-on: ${{
  fromJSON(vars.MACOS_RUNS_ON || '"macos-15"') }}`.
- New `app` job, gated on `apps/macos/project.yml`, `needs: package`: checkout, setup-xcode `latest-stable`, `brew
  install xcodegen` (not preinstalled), `xcodegen generate --spec apps/macos/project.yml`, `xcodebuild -project
  apps/macos/Steno.xcodeproj -scheme Steno -destination platform=macOS -derivedDataPath apps/macos/build/DerivedData
  test`, upload `.xcresult` on failure, test totals and skips into `$GITHUB_STEP_SUMMARY` like the package job. UI
  tests run in the same invocation unless spike S1 fails, then only when `vars.MACOS_RUNS_ON` is set (Forge has a GUI
  session).
- Fork PRs must never reach the self-hosted runner: keep "Require approval for all outside collaborators" on (human
  step) and force `macos-15` when `github.event.pull_request.head.repo.fork` is true.

### `release.yml`

Trigger: tags `v*` and `workflow_dispatch` (dry run: no notarisation, no release). `permissions: contents: write`.
Concurrency group `release`, no cancellation. 60-minute timeout.

1. Checkout `fetch-depth: 0`, setup-xcode, xcodegen.
2. Import `MACOS_CERTIFICATE_P12_BASE64` with `MACOS_CERTIFICATE_PASSWORD` into
   `$RUNNER_TEMP/steno-release.keychain-db` (random per-run password).
3. `build-release.sh <version> <build>`: archive, export, verify, entitlement grep.
4. `make-dmg.sh`: staging, `hdiutil create -format UDZO`, `codesign --sign "Developer ID Application" --timestamp`,
   write `ASC_PRIVATE_KEY` to `$RUNNER_TEMP/AuthKey.p8`, `notarytool submit --key --key-id --issuer --wait`, `stapler
   staple`, `spctl` assessment.
5. `make-appcast.sh`: `echo "$SPARKLE_PRIVATE_KEY" | generate_appcast --ed-key-file - --download-url-prefix
   https://github.com/NicolaiSchmid/steno/releases/download/<tag>/ --link
   https://github.com/NicolaiSchmid/steno/releases --maximum-deltas 0 -o dist/appcast.xml dist/`.
6. `gh release create <tag> --draft --generate-notes`, `gh release upload` DMG and appcast, `gh release edit
   --draft=false` (`--prerelease` when the tag contains a hyphen).
7. `always()`: `security delete-keychain`, `rm -f $RUNNER_TEMP/AuthKey.p8`.

Secrets: `MACOS_CERTIFICATE_P12_BASE64`, `MACOS_CERTIFICATE_PASSWORD`, `ASC_KEY_ID`, `ASC_ISSUER_ID`,
`ASC_PRIVATE_KEY`, `SPARKLE_PRIVATE_KEY`. Variable: `MACOS_RUNS_ON`. Team id and public EdDSA key are committed.

### One-time human steps (CI never does these)

Apple:
- Create a Developer ID Application certificate, export as `.p12` with a password, base64-encode, store both in
  1Password, delete local copies.
- App Store Connect > Users and Access > Integrations > Team Keys: create a Developer-role key, download the `.p8`
  once, note Key ID and Issuer ID.
- Run Sparkle's `generate_keys` once on a Mac, `generate_keys -x <file>` to export, store in 1Password, paste the
  public key into `project.yml`.

GitHub (`NicolaiSchmid/steno`):
- Add the six secrets. Set `MACOS_RUNS_ON` to `["self-hosted","macOS","ARM64"]` once the runner exists (the watchdog
  maintains it afterwards).
- Settings > Actions: "Require approval for all outside collaborators"; workflow permissions allow `contents: write`.
  Optional tag protection `v*`.

Forge (per `agent-infra/runbooks/forge.md`):
- Runner in `~/actions-runner-macos-steno`: `./config.sh --url https://github.com/NicolaiSchmid/steno --token <token>
  --name forge-macos-steno --labels forge,macos,arm64,ios,xcode --unattended`, then `./svc.sh install && ./svc.sh
  start`.
- `brew install xcodegen`; add `steno` to `hosts/atlas/forge-runner-watchdog.nix`, redeploy atlas. No certificates or
  keys in the runner keychain.

CI does everything in `release.yml` on every tag. GitHub-hosted `macos-15` is the fallback, free on this public
repository, roughly 3x slower than Forge.

## Tests

Unit `[ci]` (`StenoTests`, XCTest, no network, no TCC): one class per view model using the modules' `Testing/` fakes
(`StenoCore`, `SyntheticCaptureBackend`, `FakeModelDownloader`, `StubChatServer`, `TestIdentity`) and app-local fakes
for the four app protocols; `AppEnvironmentTests`; `InfoPlistTests`; `ThemeTokensTests`; every timer on `ManualClock`.
`KeychainSecretStoreTests` is `[opt-in: STENO_KEYCHAIN_TESTS]` (unique account, deleted in tearDown).

UI smoke (`StenoUITests`): `LaunchSmokeTests.testMainWindowOpens` launches with `-steno-ui-testing` (preview
environment, no permission requests), asserts `app.state == .runningForeground`,
`app.windows["main"].waitForExistence(timeout: 10)`, the fixture meeting is listed, and each of the four tabs is
selectable. One test, under 30 seconds. The app cannot join the SwiftPM `StenoEndToEndTests` target; this smoke test
and the manual checklist are its end-to-end proof.

Manual checklist `[manual]`, run by a human on a Mac before tagging v1, recorded in the release PR:
1. Fresh macOS 15.1+ user or VM: download the DMG from the pre-release, drag to Applications, first open shows no
   Gatekeeper warning.
2. Onboarding: microphone and calendar prompts, system audio prompt on the test recording, buttons open the right
   System Settings panes.
3. Login item appears in System Settings > Login Items; the toggle reflects it after relaunch.
4. FaceTime or Zoom call: detection panel appears, record two minutes, stop; processing completes; menu bar queue
   empties.
5. Speaker review plays a ten-second clip from `sampleClipURL`; naming and merging persist; a second call with the
   same person shows a suggestion.
6. Four tabs render; template re-run changes the summary; renaming a speaker updates the summary without a re-run;
   scratchpad text survives relaunch.
7. Obsidian folder matches the documented layout; re-export overwrites only app-written files.
8. Retention `0` deletes audio after processing but keeps the speaker clips until confirmed; keep toggle prevents
   deletion.
9. Phone pairing: local network prompt on first pairing; a phone recording arrives and is processed.
10. Sparkle: an installed older build offers and installs the new version; after relaunch no permission re-prompts.

Reviewer trap: `InfoPlistTests` weakened; the `codesign` grep removed from `build-release.sh`; a new protocol in the
app target wrapping a package type; the UI smoke test neither run nor listed skipped with spike S1's reason.

## Spikes

- S1 (before the `app` job becomes required): XCUITest launches on GitHub-hosted `macos-15` without the
  automation-mode timeout. If not, the UI smoke test is Forge-only.
- S2 (before step 11): `generate_appcast --ed-key-file -` from the SPM artifacts path under `-derivedDataPath`
  produces an appcast that a Debug build accepts from a local HTTP server.
- S3 (before step 13): one `workflow_dispatch` run on a `v0.0.1-spike` tag notarises with Sparkle and FluidAudio's
  xcframework embedded, manual signing, no provisioning profile. Fallback: re-sign helpers and the framework by hand
  in `build-release.sh`, or strip Sparkle's XPC services (allowed when not sandboxed).
- S4 (before step 8): Bonjour registration through StenoHandover triggers the local network prompt on a fresh 15.1+
  user and the listener accepts after approval; `PolicyDenied` maps to `.denied`.

## Needs from other workstreams

- StenoCore: `MeetingStore.observeMeetings()`, `observeMeeting(id:)`, `observeDeliveries`, `search`,
  `confirm(speakerID:person:memory:)`, `mergeSpeakers`, `mergePersons`; `SettingsStore` with the program's `Settings`
  fields including `obsidian`; `ProcessingPipeline.enqueue`, `process`, `rerunSummary`, `redeliver`;
  `MeetingEventBus.subscribe()` with `progress` and `speakersNeedReview`; `SummaryTemplate.bundled`;
  `SummaryMarkdown.render`; `CalendarMatch.pick`; `RetentionSweep`; `SecretStore`; `ManualClock` and the `Testing/`
  fakes.
- StenoAudio: `CaptureSession(configuration:backend:echoCanceller:)` with `states` and `levels`, `CaptureBackend`,
  `SyntheticCaptureBackend`, `MeetingDetector(source:clock:)` with bundle id, `SystemAudioPermission`, an input-device
  list with change notifications, the in-process capture spike for the Debug menu item.
- StenoSpeech: `makeSpeechEngine`, `ModelStore.ensure` progress, `SpeakerMemory.candidates(for:limit:)`.
- StenoLLM: `LLMEndpoint(settings:)`, `OpenAICompatibleClient(endpoint:apiKey:)` and `probe()` for the Test button,
  `SpeakerNameSuggestion` in `SummaryOutput`.
- StenoAdapters: `ObsidianFolderDestination(settings:).validate()` with user-readable `ObsidianError`;
  `DeliveryCoordinator` for `live()`.
- StenoHandover: `HandoverService` (`start`, `stop`, `beginPairing()`, `pairedDevices()`, `revoke(_:)`, `states`,
  `receipts`), `IdentityKeychain.loadOrCreate`, `TestIdentity`, the Bonjour service type for `NSBonjourServices`.

## Deferred

- Per-application ignore list for the detection prompt (start and dismiss only).
- Calendar-triggered recording prompt and "next event" in the menu bar; the calendar supplies titles and attendees
  only, per scope.
- Separate Templates and Retention settings tabs (folded into General and Audio).

## Deviations (implementation)

Recorded 2026-09-25 while implementing steps 1 to 13 in PR #75 (`feat/macos-app`). Reasons in one line each.

- **Package clients and schemes.** The hostless `StenoTests` bundle has its own scheme and no dependency on the `Steno` target: when two targets of one build link the same package products, Xcode 16.4 wraps every product in a dynamic `PackageFrameworks/*_PackageProduct.framework` and links `Crypto` before it exists. One client per build graph keeps the products static. CI runs `xcodebuild build -scheme Steno`, then `test -scheme StenoTests`, then `test -scheme Steno -only-testing:StenoUITests`; `InfoPlistTests` reads `Steno.app` from the shared products directory instead of `Bundle.main`.
- **`app` job runs beside `package`, not `needs: package`**, so a lint failure never hides an app build failure and the two builds overlap. xcodegen comes from the GitHub release zip (2.46.0) cached under `$RUNNER_TEMP` on every runner kind, not `brew`, so Forge and hosted behave the same.
- **Generated files are ignored**: `Steno/Info.plist` and `Config/Steno.entitlements` are written by xcodegen from `project.yml` (`info.properties`, `entitlements.properties`) and are not committed.
- **`AppEnvironment` seams.** `makeCaptureSession: (CaptureConfiguration) throws -> CaptureSession` replaces `captureBackend` (the synthetic backend is single-use per recording); `makeDependencies: (Settings, apiKey) throws -> PipelineDependencies` replaces `makeSpeechEngine`, and `reloadPipeline()` rebuilds the `ProcessingPipeline` after `waitUntilIdle()` when the Speech or LLM settings change, so neither needs a relaunch. `handover` is optional (nil when the login keychain refuses the identity; the Phones tab says so). `live()` and `preview()` are `async` because `SettingsStore.load()` and the seed are; `AppBootstrap` builds the environment at launch and every scene renders a spinner until then.
- **`AppController`** owns the running object graph (menu bar view model, `DetectionController`, pending reviews, retention sweep after finished meetings, first-launch login item registration, handover listener when phones are paired). Interrupted `.recording` rows become `.failed` at launch.
- **Recording flow.** The meeting row is written `.recording` with the calendar title and attendee `Participant` rows when the recording starts (not at stop), so the list shows it immediately; stop writes the duration, sets `asset.retention` from `Settings` and calls `enqueue`. The listener for `CaptureState.failed(_, recording:)` calls `stop()`, which returns the partial recording after a device loss.
- **Detection.** `DetectionController.handle(_:)` holds the suppression rules (disabled, recording, one prompt at a time, released microphone dismisses); the detector is stopped while Steno records; `DetectionPromptViewModel` counts down on the injected clock. App names resolve through `NSWorkspace`; tests inject the resolver.
- **Speaker review.** `SpeakerNameSuggestion`s are not persisted by the pipeline (core follow-up), so the sheet's suggestions are the `.suggested` assignment, `SpeakerMemory.candidates(for:limit:)` and the calendar participants. Naming an existing display name reuses that person. Clip playback is `ClipPlayer` over `AVAudioPlayer`, no protocol.
- **Calendar protocol** is `events(on:) -> [CalendarEvent]` with attendees embedded (no `attendees(of:)`); attendees flagged `isCurrentUser` are not written as participants (the pipeline adds "me").
- **Permissions protocol** is `state(of:)`, `request(_:)`, `openSystemSettings(for:)` over one `PermissionKind`; the system audio state is what the last probe found, remembered in `UserDefaults`; local network stays `.unknown` (no status API) and the onboarding step only explains it.
- **Delete meeting** is not offered: it needs `MeetingStore.delete(meetingID:)` in StenoCore (follow-up), and the app owns no SQL. The list offers "Reveal recording in Finder" instead.
- **LLM wiring** lives in `Services/LLMWiring.swift` (PR #5 merged during this workstream): `LLMEndpoint(settings:)` decides whether the real `LLMTranscriptCleaner` and `LLMMeetingSummarizer` run on one shared `OpenAICompatibleClient`, else the pipeline runs `PassthroughCleaner` and `FakeSummarizer` as the CLI does without an endpoint. The Test button calls `probe()` with `RetryPolicy.none` and shows `LLMError.description` on failure.
- **Debug menu** offers "Run System Audio Probe" (`SystemAudioPermission.request()` in-process, result in the menu bar item) instead of an in-process `dev capture-spike`, which is a CLI command, not a library call.
- **Settings tabs** apply on change (General, Audio, Speech) or on Save (LLM, Obsidian, which validate first).
- **Sparkle feed override** for the local spike is `defaults write uno.schmid.steno.mac STENO_FEED_URL …` read by `UpdaterController` in Debug (`SPUUpdaterDelegate.feedURLString`), not an xcconfig key.
- **Version constants**: `MARKETING_VERSION`/`CURRENT_PROJECT_VERSION` are passed on the `xcodebuild archive` command line by `release.yml`; local builds stay `0.0.0` (`0`). Xcode resolves the package graph itself at generate time (the root `Package.resolved` does not bind the xcodeproj); Sparkle is `from: 2.10.0`.
- **`release.yml`** checks the six secrets first and fails with their names (`MACOS_CERTIFICATE_P12_BASE64` and `MACOS_CERTIFICATE_PASSWORD` do not exist yet); `workflow_dispatch` is a dry run (sign and verify, no notarisation, no release) by default. Steps 12 and 13 are one workflow with the dry-run switch rather than two rounds; the first real run needs the certificate secrets.
- **`mobile-cd.yml`** also guards `setup-xcode` on `runner.environment == 'github-hosted'`, the same pattern as the other macOS jobs.
- **Tests** are XCTest (`@MainActor` classes with async methods) in the hostless bundle; the handover test identity import is repeated in `TestSupport` because the app cannot import the handover test target. Timers run on `ManualClock`; store observations and pipeline runs are awaited with a bounded poll.
- **Step 14 (release rehearsal)** and every `[manual]` check remain for a human on a Mac; none were run here.

### Testing pass (2026-09-25, PR #75)

Seams added so the behaviours above are proven through fakes, one line each:

- **`AppEnvironment.preview(makeCaptureSession:processActivity:)`**: tests inject a `CaptureSession` factory (a synthetic backend with `loseDeviceAfter`, a throwing factory, a backend whose `start` refuses) and the `FakeProcessAudioActivity` the detector polls, so device loss, start failures and the detector's 2 s debounce run on `ManualClock` with no HAL.
- **`TabText`** (`Main/Tabs/TabText.swift`): the four tabs as plain text lines, composed from the pieces the views lay out (`MarkdownBlocks`, `TranscriptTurns`, `MeetingExport.assigneeName(for:)`, `timestampText`, `PendingText.text`); `TabTextSnapshotTests` pins them for the fixture meeting under `Tests/Fixtures/snapshots/macos/` and asserts the strings the UI smoke test clicks for. `TasksTab` and `PendingText` call the shared helpers; pixels stay untested.
- **`MenuBarViewModel.RecordingState.label`** replaces the view's private status switch so every state's text is asserted.
- **`ui-smoke` job.** The UI smoke test moved out of the `app` job into its own GitHub-hosted `macos-15` job: with the `app` job on the Forge runner, Xcode 27.0 (`27A266a`) aborts inside `IDELaunchServicesLauncher.m:445` (`INTERNAL ERROR: childPID > 0`) when `xcodebuild test` launches `StenoUITests-Runner.app`, reproducibly from the runner's LaunchAgent and from an SSH session, with and without `arch=arm64`; the runner app starts but `xcodebuild` never learns its PID. Hosted `macos-15` (Xcode 16.4) passed spike S1, so the smoke test stays hard-required there. Follow-up for agent-infra: a second Xcode on Forge (16.4 or 26.x) selected through `DEVELOPER_DIR` would let the job come back to the runner variable. The `app` job's summary now reads `unit.xcresult` only.
- **`scripts/check-release-secrets.sh`** holds the secrets guard that was inline in `release.yml`; the workflow runs it right after checkout, and `ReleaseScriptsTests` runs it under `/bin/bash` for every missing-secret and dry-run combination, checks it stays ahead of every tool and build step, and greps `build-release.sh` for the `codesign` guards (the reviewer trap).

Still `[manual]`: the ten checklist items, spikes S2 to S4, `KeychainSecretStoreTests` (`STENO_KEYCHAIN_TESTS`), the first-launch login item registration (`AppController` skips it in the preview environment), `ClipPlayer` audibility, `NSWorkspace` app-name resolution, the SwiftUI sheet and window plumbing beyond the one UI smoke test.

### Review application (2026-09-26, PR #75)

The correctness and elegance reviews of PR #75 applied, over the core additions of PR #82 (`LocalRecordingIntake`, `MeetingStore.failInterruptedRecordings`, `ProcessingPipeline.resumeUnfinished`, `SummaryMarkdown.sections(for:)`, `MeetingEvent.retentionApplied`, `MeetingStore.delete`, persisted `SpeakerNameSuggestion`s). What changed against the deviations above, one line each:

- **Launch** also calls `resumeUnfinished()`: a meeting left `.queued` or `.processing` by Quit, a relaunch or a crash is processed again; interrupted `.recording` rows are failed by core's one-write `failInterruptedRecordings`.
- **Retention sweep** runs at launch and on `MeetingEvent.retentionApplied`, not on the `.ready` row change (persist writes `.ready` before deliver and retention run, so that sweep found nothing).
- **Detection** keeps the detector running through Steno's own recordings; it ignores Steno's PID and `handle` drops events while recording. A restarted detector reported the microphone the call still held as newly opened and re-prompted after every Stop.
- **`reloadPipeline()`** swaps first and retains the retired pipeline until idle; Save never waits, later enqueues land on the replacement, meetings in flight finish on the old dependencies.
- **Quit** waits for a `start()` or `stop()` in flight (`RecordingController.awaitSettled()`) and then stops and enqueues; the capture is never abandoned mid-start.
- **Recording flow** is `RecordingController` (owned by `AppController` like `DetectionController`) over core's `LocalRecordingIntake`: `begin` writes the `.recording` row with the calendar title and attendees, `complete` sets retention from `Settings` at stop time, writes the duration and enqueues, `fail` marks a capture that never made it. The menu bar, the Record menu, the Debug probe and the detection prompt drive the recorder, not each other; the app carries no copy of the row transaction and the default title is core's.
- **Observation** is view-driven: every view model exposes `observe()` (and `observeDeliveries()`, `observeProgress()`, `observeReceipts()`, `observePairing()`) that a `.task` runs and cancels; `AppController` cancels its observers in `shutdown()`; `SettingsView` builds its view models once. The Phones pairing poll runs on the injected clock.
- **Summary tab** renders `SummaryMarkdown.sections(for:)`; the Markdown is never parsed back.
- **Speaker review** shows the summarize stage's persisted name suggestion with a Use button (never applied on its own); draft names and merge targets live on the view model; the merge picker starts unselected; Done re-exports only after a confirm or a merge.
- **Delete meeting** is offered (row context menu and toolbar, confirmation) through `MeetingStore.delete(meetingID:)`; disabled while recording or processing.
- **Labels**: `PipelineStage.label`, `AudioLane.label`, `MeetingSource.label`, `LanguageTag.localizedName` replace raw values; one `AppEnvironment.setLaunchAtLogin` writer; `Binding.action` for the save-on-change controls; accessibility labels on the icon-only controls and a value per level bar.
- **Release scripts**: `build-release.sh` checks team, hardened runtime and timestamp on every nested code item including Sparkle's `Autoupdate`, `Updater.app` and XPC services, reads entitlements with `codesign -d --entitlements - --xml`; `install-xcodegen.sh` pins the zip's SHA-256 and accepts an `xcodegen` on PATH only at the pinned version; `release.yml` restores the runner's keychain search list from what it saved and reads one job-level `DRY_RUN`; `xcodebuild-quiet.sh` is the shared xcodebuild runner; the `app` job's DerivedData cache is scoped per self-hosted runner and hashes the source folders only.
- **Deferred from the elegance review** (follow-ups, not done here): `Theme.Space.xxs` and `StatusDot`, the unused `Color` shortcuts and `lineHeight` tuple, `DetectionPanel` sizing from `fittingSize` (9); one file per settings tab and the `AppProtocols.swift` split (17); one test class per view model (15); `startupWarnings` now shows in the menu bar, `pendingReviews` stays a set (13).
