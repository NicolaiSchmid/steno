# Steno v1 plans: correctness and feasibility review

Reviewed 2026-09-25 on branch `plans/v1` against `AGENTS.md`,
`.plans/2026-09-24-initial-scope.md`, `.plans/2026-09-25-v1-program.md`
(including its reconciliation log) and the seven workstream plans. API
claims were checked against vendor documentation, package sources and the
packages installed under `mobile/node_modules`; sources are cited per item.
Findings are ordered most severe first. Counts: 1 blocker, 4 major, 12 minor.

## Findings

1. **Blocker.** `2026-09-25-phone-handover.md`, Decisions 6, Spike S1, steps M1 to M4.
   The Mac identity is stored "as a `SecIdentity` in the data-protection
   keychain" (`kSecUseDataProtectionKeychain`). On macOS that keychain is
   gated by the `com.apple.application-identifier` / `keychain-access-groups`
   entitlements, which must be authorised by a provisioning profile; the
   release build is "Developer ID, no provisioning profile" (macOS plan,
   Spike S3) and `swift test` runs unsigned. Every `SecItemAdd` returns
   `errSecMissingEntitlement` (-34018), so M1's acceptance fails, M2 to M4
   have no identity to hand to `sec_identity_create`, and the shipped app
   cannot start the listener. Source: Quinn (Apple DTS),
   https://developer.apple.com/forums/thread/130264 ("you need a
   provisioning profile to whitelist your app identifier entitlement. macOS
   does not support this directly"); field report of exactly this failure on
   a Developer ID lane, https://github.com/Round-Tower/M1K3/issues/319.
   Fix: in Decision 6 replace "Stored as a `SecIdentity` in the
   data-protection keychain" with "Certificate and key stored in the
   file-based login keychain (no `kSecUseDataProtectionKeychain`); fetched
   as `kSecClassIdentity`". For CI, add to Tests: "`Tests/Fixtures/handover/test-identity.p12`
   (test-only, generated once with `openssl`), imported with
   `SecPKCS12Import` and `kSecImportToMemoryOnly` (macOS 15+) so no test
   touches a keychain"; keep the keychain round-trip behind
   `STENO_KEYCHAIN_TESTS=1`.

2. **Major.** `2026-09-25-phone-handover.md`, Decision 2 and step P2; `mobile/app.config.ts`.
   The iOS pinning delegate accepts a self-signed leaf via
   `URLCredential(trust:)`. With App Transport Security active, Apple
   states "you can no longer loosen trust evaluation requirements that way,
   but you can still tighten them", and since iOS 17 "ATS no longer allows
   connections to IP addresses by default". The phone connects to the Mac
   by IP (Spike S2), so every `URLSession` request fails with an ATS error
   before the delegate can pin, right fingerprint or not. Sources:
   https://developer.apple.com/documentation/security/preventing-insecure-network-connections
   and https://developer.apple.com/documentation/bundleresources/information-property-list/nsapptransportsecurity/nsallowslocalnetworking.
   Fix: add to the P8 `app.config.ts` change list
   `ios.infoPlist.NSAppTransportSecurity = { NSAllowsLocalNetworking: true }`
   and state in Decision 2 that this key is required for the delegate
   override to run; it does not itself trust the leaf, the delegate still
   does the pinning. Note it moves the native fingerprint (already the case
   for P8).

3. **Major.** `2026-09-25-v1-program.md` (canonical model `Speaker`, pipeline
   step 10), `2026-09-25-core-foundation.md` step 9,
   `2026-09-25-macos-app-and-release.md` speaker review row.
   The scope's data model gives each speaker a "sample clip"; the program
   reduced it to `Speaker.sampleClipRange`, a time range into the lane
   audio. With `.deleteAfterProcessing` (retention `0`, a supported
   setting), step 10 sets `expiresAt = now` and the sweep removes master,
   sidecars and mixdown before the user opens the review sheet, so
   `play(id)` has nothing to play and the naming flow the scope requires is
   broken for that setting. Independently, the sheet plays `AudioAsset.url`
   (the two-channel master), so the mic lane is audible over the clip.
   Fix: add `sampleClipURL: URL?` to `Speaker`; in pipeline step 8 write a
   10 s 16 kHz WAV per speaker with `personID == nil` from the diarized lane
   into the meeting folder; exclude these files from
   `deleteExpiredAudio` and delete them in `MeetingStore.assign` when
   `personID` becomes non-nil; the review sheet plays `sampleClipURL`.

4. **Major.** `2026-09-25-core-foundation.md` steps 8 and 9;
   `2026-09-25-speech-and-speakers.md` "Language strategy" item 4;
   `2026-09-25-llm-and-templates.md` Design decision 6.
   The speech plan says "`Meeting.language` = duration-weighted dominant
   language; the pipeline owns writing it"; the LLM plan derives the output
   language from `Meeting.language` and the Whisper engine pins to `hint`.
   No core pipeline step sets `Meeting.language`, and StenoCore cannot call
   StenoSpeech's `LanguageTagger`. Result: every summary falls back to
   English regardless of the meeting, and `hint` is always nil.
   Fix: in core step 8 add "Transcribe step: after both lanes,
   `ctx.meeting.language` = the `RawSegment.language` with the largest
   summed duration (nil when all segments are untagged); `hint` passed to
   the second lane is the first lane's result".

5. **Major.** `2026-09-25-audio-capture.md` step 3 check, Spike S1, Tests.
   S1's go criterion is "prompt appears on first start of a signed build
   with `NSAudioCaptureUsageDescription` as a literal Info.plist key", but
   the checks run `steno capture-spike` and `tccutil reset ... <bundle-id>`
   against an unbundled CLI that has no Info.plist and no bundle id. TCC
   attributes a Terminal-launched tool to Terminal, so the prompt, grant and
   denial measured are Terminal's, not Steno's, and the spike cannot answer
   the onboarding question it gates. Source: DGR Labs field notes
   (https://dgrlabs.co/blog/2026-04-25-capturing-system-audio-on-macos-in-2026.html,
   "Terminal-launched processes get the terminal host as responsible
   process"); same attribution rule stated for local network in TN3179.
   Fix: run S1 and the step 3 prompt check from the macOS step 1 skeleton
   app (or from `steno` wrapped in a minimal signed `.app` launched with
   `open`), and change the reset command to `tccutil reset AudioCapture
   <bundle-id>` (see Verified claims).

6. **Minor.** `2026-09-25-speech-and-speakers.md`, Public API.
   `ParakeetEngine` and `WhisperKitEngine` are `actor`s conforming to
   `SpeechEngine`, whose `var id: String { get }` and
   `var supportedLanguages` are synchronous requirements. Swift 6 rejects
   an actor-isolated property satisfying a nonisolated protocol requirement.
   Fix: declare both as `nonisolated public let`.

7. **Minor.** `2026-09-25-phone-handover.md` step M5 acceptance.
   "manual on a Mac: `dns-sd -B _steno._tcp` lists the service and the
   macOS prompt shows once" is run through `steno handover serve`. TN3179:
   command-line tools launched from Terminal are automatically granted
   local network access, so no prompt appears; and "there is no way to
   reset the privilege to undetermined on macOS", so "once" is testable
   only on a fresh user account or VM. Fix: move the prompt check to the
   macOS plan's Spike S4 (already there) and reword M5 to "service listed
   by `dns-sd -B`".

8. **Minor.** `2026-09-25-macos-app-and-release.md` step 7 and Tests.
   `KeychainSecretStoreTests` run unconditionally in `StenoTests`; the
   program's verification standard puts keychain tests behind
   `STENO_KEYCHAIN_TESTS=1`. On the hosted runner the login keychain is
   usable but not guaranteed unlocked, and a locked keychain blocks in a UI
   prompt. Fix: "`KeychainSecretStoreTests` skip unless
   `STENO_KEYCHAIN_TESTS=1`".

9. **Minor.** `2026-09-25-v1-program.md` pipeline preamble vs
   `2026-09-25-core-foundation.md` `PipelineContext`.
   The program says "lanes are processed one at a time so at most one
   two-hour `AudioBuffer16k` (about 460 MB) is alive", but step 1 decodes
   every lane into `PipelineContext.lanes: [AudioLane: AudioBuffer16k]`
   before step 2, so both lanes (about 920 MB) are alive throughout
   transcription and diarization. Fix: make `lanes` a per-lane loader
   (`decode(lane:)` called inside Transcribe and Diarize, buffer released
   after each), or change the program sentence to "at most two".

10. **Minor.** `2026-09-25-speech-and-speakers.md` Decisions "Speaker
    match" vs `2026-09-25-v1-program.md` `Settings`.
    "Default threshold `0.60`, margin `0.05` over the runner-up; both are
    settings", but `Settings` has only `speakerMatchThreshold`. Step 8 wiring
    cannot read a margin. Fix: add `speakerMatchMargin (0.05)` to `Settings`
    with a log entry, or change "both are settings" to "threshold is a
    setting, margin is a constructor constant".

11. **Minor.** `2026-09-25-adapters-obsidian.md`, "JSON" paragraph and
    Public API.
    `renderJSON` writes `delivery.exported_at`, but `ArtifactRenderer` is
    "pure, deterministic, no I/O", takes no clock, and its output is
    golden-snapshotted and SHA-256'd into the receipt. The snapshot test
    cannot pass twice and every re-export rewrites `meeting.json` even when
    nothing changed. Fix: add `exportedAt: Date?` to `RenderOptions` (set by
    the coordinator's `clock`), or drop `exported_at` from `meeting.json`.

12. **Minor.** `2026-09-25-core-foundation.md` step 9.
    "persist calls `decoder.mixdown` for `.caf48kFloat32` assets and skips
    `.m4aAAC`" leaves `.wav16kInt16` (every `steno process` run and every
    core fixture) with `mixdownURL == nil`, so the adapters' `includeAudio`
    path returns `.audioUnavailable` for CLI-processed meetings. Fix:
    "skips only `.m4aAAC`; `WAVAudioDecoder.mixdown` copies the file".

13. **Minor.** `2026-09-25-llm-and-templates.md`, Public API `LLMEndpoint`.
    `LLMEndpoint: Codable` carries `apiKey: String?`. Any future
    persistence or debug dump of the endpoint serialises the key, against
    the program's "API keys never live here; see `SecretStore`" and the
    plan's "key never in logs". Fix: drop `Codable` from `LLMEndpoint`, or
    exclude `apiKey` from `CodingKeys` and give it a `CustomStringConvertible`
    that redacts.

14. **Minor.** `2026-09-25-core-foundation.md` step 6 vs
    `2026-09-25-llm-and-templates.md` step 5.
    Both steps create `Sources/StenoCore/Resources/Templates/*.json` with
    the four templates. Core runs "first and alone", so the LLM step
    overwrites files that already exist and whose `TemplateRegistryTests`
    snapshot then changes without a stated reason. Fix: LLM step 5 becomes
    "PR against core that edits `instructions` and `context` strings only;
    section ids and headings are fixed by core step 6".

15. **Minor.** `2026-09-25-phone-handover.md` Decision 4 and step P2.
    Server-trust challenges do reach a background `URLSession` delegate,
    but each challenge wakes the app and "counts against the resume rate
    limiter" (Quinn, https://developer.apple.com/forums/thread/28713); one
    task per 4 MiB chunk means one wake per chunk for a locked phone. Fix:
    note the cost in Risks and raise `chunkSize` to 16 MiB (one hour of AAC
    at 64 kbps is two chunks), keeping resume granularity acceptable.

16. **Minor.** `2026-09-25-phone-handover.md` Decision 10 and "Files, phone side".
    "require 18.6 or later" has no mechanism: nothing sets the iOS
    deployment target, the module podspec says "iOS 16.4 minimum", and
    `mobile/app.config.ts` forbids `expo-build-properties`, the usual way to
    raise it. Fix: add to P7 "on launch, if `Platform.Version < 18.6` show a
    blocking 'update iOS' screen before the recorder", or list
    `expo-build-properties` with `ios.deploymentTarget: "18.6"` as an
    explicit exception to the config comment.

17. **Minor.** `2026-09-25-speech-and-speakers.md` step 8 acceptance.
    "`steno process` runs end to end ... when the `--real-speech` flag wires
    this module in" adds a flag to `Sources/steno/Commands/Process.swift`,
    which core owns; core's plan says other workstreams add flags "in their
    own command files". Fix: state that speech's PR edits core's
    `Wiring.swift` to accept an `--engine <id>` option, and record the
    ownership exception in one line.

## Verified claims

Each item settles a claim a plan marked unverified, or that this review
needed to check to judge a finding.

- **`CATapDescription(stereoGlobalTapButExcludeProcesses:)`** exists, as
  `convenience init(stereoGlobalTapButExcludeProcesses: [AudioObjectID])`,
  with `mono...`, `stereoMixdownOfProcesses:`, `processes:deviceUID:stream:`
  and `excludingProcesses:deviceUID:stream:` siblings; properties include
  `muteBehavior`, `isPrivate`, `isExclusive`, `isMixdown`, `uuid`,
  `processes`. Source: https://developer.apple.com/documentation/coreaudio/catapdescription.
- **`kAudioProcessPropertyIsRunningInput`** exists (macOS, alongside
  `kAudioProcessPropertyIsRunning`, `IsRunningOutput`, `BundleID`, `PID`,
  `Devices`). Listener behaviour is not documented; the plan's "listeners
  reportedly never fire" stays open. Source:
  https://developer.apple.com/documentation/coreaudio/kaudioprocesspropertyisrunninginput.
- **`kAudioAggregateDeviceTapListKey`, `TapAutoStartKey`, `MainSubDeviceKey`,
  `IsPrivateKey`, `IsStackedKey`** are all public constants. Source:
  https://developer.apple.com/documentation/coreaudio/kaudioaggregatedevicetaplistkey.
- **System-audio permission**: AudioCap's README confirms "There's no
  public API to request audio recording permission or to check if the app
  has that permission" and that the Info.plist key
  `NSAudioCaptureUsageDescription` "is not listed in the Xcode dropdown, you
  have to enter it manually". Source: https://github.com/insidegui/AudioCap.
- **`tccutil` service name**: the TCC service is `kTCCServiceAudioCapture`;
  `tccutil reset AudioCapture <bundle-id>` is reported to re-trigger the
  System Audio Recording prompt (https://github.com/yukij3/atmos-control/issues/1).
  The plan's `SystemAudioCaptureRequests` appears in one blog only. DTS's
  way to list valid names:
  `dyld_info -exports /System/Library/PrivateFrameworks/TCC.framework/Versions/A/TCC | grep kTCCService`
  (https://developer.apple.com/forums/thread/679303).
- **CSpeex (sbooth) exports the echo API**: `Sources/speex/libspeexdsp/`
  contains `mdf.c`, `preprocess.c`, `resample.c`; `include/speex/` has
  `speex_echo.h`, `speex_preprocess.h`, `speex_resampler.h`. The package's
  only product is named **`speex`** (not `CSpeex`); `Package.swift` is
  tools 5.6, one C target with `HAVE_CONFIG_H`. Source:
  https://github.com/sbooth/CSpeex (Package.swift, Sources tree).
- **GRDB FTS5 under SwiftPM**: GRDB's `Package.swift` (tools 6.1,
  `swiftLanguageModes: [.v6]`) always defines `SQLITE_ENABLE_FTS5` in
  `swiftSettings` and links the **system** SQLite via the `GRDBSQLite`
  system-library target; Apple's libsqlite3 ships FTS5. The FTS guide's
  "custom SQLite build" sentence is stale for Apple platforms. Core spike
  S1 is a go; the FTS4 fallback is unnecessary. Sources:
  https://github.com/groue/GRDB.swift/blob/master/Package.swift,
  https://github.com/groue/GRDB.swift/blob/master/Documentation/FullTextSearch.md.
- **GRDB names**: `Database.BusyMode` cases are `immediateError`,
  `timeout(TimeInterval)`, `callback(BusyCallback)` (so `.timeout(5)` is
  right); rank ordering through the query interface is
  `.order(Column.rank)` (`ORDER BY rank`), no `bm25()` wrapper needed;
  `synchronize(withTable:)` and `FTS5Pattern(matchingAllTokensIn:)` exist
  as used. Sources: `GRDB/Core/Database.swift`, FullTextSearch.md.
- **Versions**: GRDB **7.11.1** released 2026-06-18; swift-argument-parser
  **1.8.2** released 2026-06-04; FluidAudio **v0.17.3** released
  2026-09-24 (v0.17.4 followed on 2026-09-25); Sparkle **2.10.0** released
  2026-09-13 (macOS 12+, CocoaPods dropped). Sources: GitHub releases API
  for each repository.
- **WhisperKit repository rename**: `api.github.com/repos/argmaxinc/WhisperKit`
  resolves to `argmaxinc/argmax-oss-swift`; **v1.1.0** released 2026-08-06.
  `Package.swift` is tools **5.10** with `swiftLanguageVersions: [.v5]`,
  platforms macOS 13+; `Package@swift-6.2.swift` is tools 6.2 with
  `swiftLanguageModes: [.v6]` and upcoming features
  `InferIsolatedConformances`, `NonisolatedNonsendingByDefault`. Products:
  `ArgmaxOSS`, `WhisperKit`, `TTSKit`, `SpeakerKit`. On the runner's
  default Xcode 16.4 (Swift 6.1) SwiftPM selects the 5.10 manifest; a
  dependency's language mode does not constrain Steno's Swift 6 mode, so
  speech spike A's "WhisperKit fails to build under Swift 6" no-go cannot
  occur for that reason. Source: repository root and both manifests.
- **FluidAudio packaging**: tools 6.0, platforms macOS 14 / iOS 17, no
  external SwiftPM dependencies, but one **binary target**
  (`NemoTextProcessing` xcframework from `FluidInference/text-processing-rs`)
  plus C targets `FastClusterWrapper`, `MachTaskSelfWrapper`;
  `cxxLanguageStandard: .cxx17`. The binary must be signed and notarised as
  part of the app; Xcode's archive/export does this. Source:
  https://github.com/FluidInference/FluidAudio/blob/v0.17.3/Package.swift.
- **Network.framework TLS with a self-signed identity**:
  `sec_protocol_options_set_local_identity(_: sec_protocol_options_t, _: sec_identity_t)`
  (macOS 10.14+) and `sec_identity_create(_ identity: SecIdentity) -> sec_identity_t?`
  (macOS 10.14+) exist as the plan uses them. Creating the `SecIdentity`
  without a keychain requires `SecPKCS12Import` with
  **`kSecImportToMemoryOnly`, introduced macOS 15.0 / iOS 18.0**. Sources:
  Apple documentation JSON for the three symbols.
- **Data-protection keychain on macOS** requires a provisioning profile
  that authorises the application-identifier entitlement; Developer ID
  without a profile and unsigned test code get -34018. See finding 1.
- **App Transport Security**: with ATS on, a `URLSession` delegate may
  tighten but not loosen server-trust evaluation; since iOS 17 / macOS 14
  ATS applies to IP-address connections by default; `NSAllowsLocalNetworking`
  re-enables unqualified names, `.local` and IP addresses and needs no App
  Review justification. It does not make a self-signed leaf trusted
  (standard trust evaluation still runs), so the delegate is still
  required. See finding 2.
- **Background `URLSession` server-trust challenges** are delivered to the
  session delegate; each costs an app resume against the rate limiter;
  client-certificate challenges are not supported in background sessions
  (not used here). Source: https://developer.apple.com/forums/thread/28713.
- **TN3179 (local network privacy)**: applies to macOS 15; Bonjour
  register, browse and resolve all require the privilege; TCP listen and
  accept do not; Terminal-launched tools, daemons and root are auto-allowed
  (launchd agents are not); no status API (FB8711182); denial surfaces as
  `.waiting(.dns(kDNSServiceErr_PolicyDenied))` on `NWBrowser` and
  `unsatisfiedReason == .localNetworkDenied` on `NWConnection`; **no reset
  on macOS** (FB14944392); macOS 15.1 fixed several bugs; iOS 18 state
  bug fixed in **18.6** (FB14321888); the simulator does not support it; a
  backgrounded iOS app with undetermined state is denied silently and the
  decision is not recorded; `NSBonjourServices` and
  `NSLocalNetworkUsageDescription` are not platform-specific. Source:
  https://developer.apple.com/documentation/technotes/tn3179-understanding-local-network-privacy.
- **Expo (installed packages)**: `expo-modules-core` **57.0.18** forwards
  `application(_:handleEventsForBackgroundURLSession:completionHandler:)` to
  subscribers (`ios/AppDelegates/ExpoAppDelegateSubscriberManager.swift:165-187`);
  `expo-modules-autolinking` **57.0.13** defaults `nativeModulesDir` to
  `./modules` (`build/commands/autolinkingOptions.d.ts:17`);
  `expo-file-system` **57.0.7** `FileSystemUploadTask.swift` and
  `FileSystemDownloadTask.swift` contain no `URLAuthenticationChallenge`
  handling (grep over `ios/`), confirming no trust hook; `expo-audio`
  **57.0.5** `RecordingOptions.directory?: RecordingDirectory` accepts
  `"document"` (`build/Audio.types.d.ts:350-365`). `expo-camera` and
  `expo-crypto` are not installed yet, as the plan expects.
  `mobile/app.config.ts` already sets `NSLocalNetworkUsageDescription`,
  `NSBonjourServices: ["_steno._tcp"]` and `UIBackgroundModes: ["audio"]`;
  `.github/workflows/mobile-cd.yml:201` hard-codes
  `runs-on: [self-hosted, macOS, ARM64]` as the macOS plan says.
- **Sparkle**: non-sandboxed apps need no XPC services or `SUEnable*Service`
  keys; the `--deep` warning is about signing flags, not `codesign --verify
  --deep`; the SPM CLI tools live at `../artifacts/sparkle/Sparkle/bin/`
  relative to `checkouts` (the macOS plan's unverified path is right);
  Library Validation (hardened runtime) is what stops an ad-hoc-signed
  Debug app from loading Sparkle, so the plan's "hardened runtime off in
  Debug" is the correct condition. Sources:
  https://sparkle-project.org/documentation/sandboxing/,
  https://sparkle-project.org/documentation/.
- **XcodeGen**: top-level `packages: { Name: { path: ../.. } }` for local
  packages, target `dependencies: [{ package:, product: }]`,
  `info.properties`, `entitlements.properties` (all entitlement keys must
  be listed) and product type `bundle.ui-testing` all exist. Source:
  https://github.com/yonaskolb/XcodeGen/blob/master/Docs/ProjectSpec.md.
- **GitHub `macos-15` runner** (image 20260824): macOS 15.7.9; Xcode
  **16.4 default**, 26.0.1 to 26.3 and 16.0 to 16.3 installed; **xcodegen is
  not preinstalled** (the plan's `brew install xcodegen` is needed).
  Source: https://github.com/actions/runner-images/blob/main/images/macos/macos-15-Readme.md.
- **`swift format lint --strict --recursive`**: `-s/--strict`,
  `-r/--recursive`, `-p/--parallel` exist; `swift format` (space) is the
  toolchain entry point from Xcode 16. Source:
  https://github.com/swiftlang/swift-format/blob/main/README.md.
