# Test-strategy review of the Steno v1 plans

Reviewed 2026-09-25 against `2026-09-25-v1-program.md` and the seven
workstream plans on `plans/v1`. Question: can an implementation agent prove
each step without a human on the hosted `macos-15` job, and can a reviewer
trust that proof? Correctness and elegance are in the sibling reviews.

Severity: blocker = no machine-checkable proof, or it cannot run in CI;
major = a deterministic test is possible but the plan offers only a manual or
opt-in check; minor = fixture, flakiness or reviewer hygiene.

## Findings

1. **blocker — no test runs the real pipeline across modules.** Program
   "Pipeline"; core step 9 (`PipelineIntegrationTests`, all fakes); llm step
   10; adapters step 8; speech step 8 (`--real-speech`, opt-in). Every
   workstream proves its module against fakes of the others; nothing runs WAV
   decode -> `FakeSpeechEngine` -> `FakeDiarizer` -> real `CosineSpeakerMemory`
   -> `LaneMerger` -> `LLMTranscriptCleaner` and `LLMMeetingSummarizer` on the
   stub server -> persist -> `DeliveryCoordinator` -> `ObsidianFolderDestination`
   into a temp vault. Drift between five parallel PRs (`clusterLabel` vs
   `speakerLabel`, summary heading level, `mixdownURL` nil for `.m4aAAC`)
   surfaces only in the macOS manual checklist. Add target
   `Tests/StenoEndToEndTests` (StenoCore, StenoSpeech, StenoLLM, StenoAdapters;
   no StenoAudio, no models, loopback only) with
   `EndToEndTests.testMacCallFixtureLandsInVault`: fixture
   `audio/conversation-two-lane-6s.wav`, canned bodies in `llm/responses/`;
   asserts vault files equal goldens in `snapshots/e2e/`, `meeting.json`
   decodes as `MeetingExport`, meeting `.ready`, one `Delivery` `.delivered`
   with receipt, `Meeting.llmUsage` equals the summed scripted `usage`, event
   order. Owner: core creates it in step 10 with fakes everywhere; audio
   (`AVFoundationAudioCodec` on a generated CAF), speech, llm and adapters
   each replace one fake in their final step. The binary variant (`steno
   process` then `steno deliver`) goes in `stenoTests`.

2. **blocker — audio-capture acceptance checks are hardware-only but unmarked.**
   audio steps 2, 3, 4, 9, 11, 12 entirely; 5 (Instruments), 6 (SIGKILL), 10
   (FaceTime) partly. All read "Check:" like the CI ones; an agent on the
   hosted runner can execute none of them and a reviewer cannot tell which
   were run. Change: tag every check `[ci]`, `[opt-in STENO_AUDIO_TESTS]` or
   `[manual]`. Add `SyntheticCaptureBackend: CaptureBackend` (deterministic
   sines per lane, injectable device-loss, no HAL) in
   `Sources/StenoAudio/Testing/` plus `steno record --backend synthetic`. Then
   on CI: `CaptureSessionTests` runs rings -> processing thread -> writer and
   asserts `droppedFrames == [:]` and `RecordingFiles` durations;
   `testDeviceLostStopsCleanly` asserts `.failed(.deviceLost)` with a readable
   `recording.caf` (replaces the USB unplug, step 11); `LiveAECPathTests`
   feeds a far-end sine and a delayed attenuated copy plus an independent tone
   through the real step-9 path and asserts ERLE >= 15 dB via `EchoMetrics`
   (the speaker test stays manual, for artefacts only);
   `stenoTests.testSIGKILLLeavesReadableCAF` spawns `steno record --backend
   synthetic --seconds 30`, kills it after 2 s and reads the file with
   `AVAudioFile`, duration within 1 s (replaces `afinfo`). Zero-allocation
   (step 5) stays manual; see the reviewer grep in finding 19.

3. **major — CI does not produce the proof the program demands.** Program
   "Verification standard" wants run URL, test count and every skipped test
   with a reason per PR; `.github/workflows/swift-ci.yml` runs `swift test -v`
   and nothing else, so the numbers come from the agent's own reading of the
   log. Change (core step 1): `swift test --parallel --xunit-output
   .build/junit.xml`, then a step that writes totals and the names and
   messages of skipped tests to `$GITHUB_STEP_SUMMARY` and fails when a skip
   message names none of the five env switches. Raise `timeout-minutes` from
   30 before StenoSpeech lands: FluidAudio plus WhisperKit compile cold in 15
   to 25 minutes and the `.build` cache is keyed on `Package.resolved` only.

4. **major — "all fakes live in StenoCore/Testing" cannot hold for module seams.**
   Program "Repository layout"; audio `CaptureBackend` and detector source
   fakes (steps 5, 10); speech "fake downloader" (step 1); llm `StubChatServer`
   (`Tests/StenoLLMTests/Support/`); adapters `FileSink`; handover
   `LoopbackClient`; macOS `Services/Fakes.swift`. StenoCore cannot see these
   types, so each plan quietly puts fakes in a test target where finding 1's
   test and the CLI cannot reuse them. Concrete break: `ModelStore.init(directory:)`
   has no downloader parameter, so speech step 1's "unit test uses a fake
   downloader" cannot be written. Change: program rule becomes "fakes of
   StenoCore protocols live in `StenoCore/Testing/`; each module may add
   `Sources/<Module>/Testing/` for its own seams". Speech adds `protocol
   ModelDownloading` and `ModelStore.init(directory:downloader:)`; llm moves
   `StubChatServer` to `Sources/StenoLLM/Testing/`.

5. **major — handover loopback tests silently depend on a keychain identity.**
   phone-handover M1 to M5. M1's only acceptance runs under
   `STENO_KEYCHAIN_TESTS=1`, so on hosted CI M1 has no proof. M2 to M5 need a
   `sec_identity_t` for the listener; if it can only come from the
   data-protection keychain (`kSecUseDataProtectionKeychain` returns -34018 for
   an unsigned `swift test` binary) the whole suite is keychain-gated in
   disguise. Change: split `ServerIdentity` into `mint()` plus
   `fingerprint(der:)` (pure swift-certificates, CI: stable fingerprint, CN,
   validity, P-256) and `Keychain.store/load` (opt-in). Tests build the
   `SecIdentity` via `SecPKCS12Import` into a file keychain from
   `SecKeychainCreate` under a temp path, deleted in teardown; spike S1
   confirms this path on the hosted runner before M2.

6. **major — the iOS pinning code is never executed by a test.**
   phone-handover decision 2, P2, "Tests: LoopbackClient is the executable
   specification". A specification in another file proves nothing about
   `modules/steno-link/ios/PinnedSessionDelegate.swift`; the accept-iff
   comparison is the security boundary of the feature. Change: put the
   challenge handler in one platform-neutral file
   (`mobile/modules/steno-link/ios/PinnedTrustEvaluator.swift`, Foundation and
   Security only), symlink it into `Tests/StenoHandoverTests/Support/` and use
   it inside `LoopbackClient`. `PinningTests`: right fingerprint -> 200; one
   flipped bit -> handshake error and the listener saw no request line;
   certificate replaced by a second minted one -> rejected.

7. **major — goldens will differ by machine unless time, ids and locale are
   pinned.** core step 2 (`meeting-export.json` byte for byte); adapters steps
   3, 4, 6 (`date` in the user's zone, JSON `exported_at` with offset, `Slug`
   via `.diacriticInsensitive`); llm steps 6, 7 (prompts carry the meeting date
   and `promptName`). `ObsidianFolderDestination.deliver` and
   `ArtifactRenderer` have no clock, so `exported_at` is `Date()`. Change:
   fixtures use sequential UUIDs and fixed `Date(timeIntervalSince1970:)`;
   `RenderOptions` gains `now: Date` fed from the coordinator's `clock()`;
   golden variants pin `Europe/Berlin` and `UTC`; `Slug` folds with
   `locale: nil`; `OutputLanguage.promptName` uses `Locale(identifier:
   "en_US")`; core's export encoder is `.sortedKeys` plus `.iso8601` with
   fractional seconds. `SnapshotTests` adds "two encodes are byte-identical".

8. **major — fixture generation is not shown to be deterministic.** core
   step 7; audio `echo-mic-48k-6s.wav` ("-20 dB noise"), `room-ir-48k.wav`,
   `speech-like-far-48k-6s.wav`; speech `say` fixtures; llm
   `customer-call-60min.json` ("generated"). Unseeded noise, Float32 `sin()`
   across CPUs and `say` (voices and output change per macOS release) all
   change bytes on regeneration, so "regenerate and diff" cannot be a reviewer
   check. Change: fixed-seed SplitMix64, integer phase accumulators, Int16
   WAV; `Tests/Fixtures/MANIFEST.sha256`; `FixtureManifestTests` (core)
   regenerates synthetic fixtures into a temp dir and compares hashes, and
   checks the committed `say` fixtures (generate once, never regenerate,
   `.ref.txt` alongside). CAF and `.m4a` inputs for audio step 7 are in no
   list and AAC is not byte-stable: build them in test setup, never commit.

9. **major — wall-clock timers in six plans, no injected clock anywhere.**
   audio step 10 (2 s debounce, 2 s poll), step 5 (levels at 10 Hz); llm step 2
   (`RetryPolicy` 2 s to 30 s, "cancel stops within 100 ms"); handover M3
   (300 s pairing window), P6 (`backoffMs` jittered); macOS step 6 (60 s
   countdown), step 4 (debounced scratchpad save). As specified these tests
   either sleep for real (RetryTests alone: 6 s per case) or assert a latency
   bound that flakes on a loaded runner. Change: add `ManualClock: Clock` to
   `StenoCore/Testing/`; `MeetingDetector`, `OpenAICompatibleClient`,
   `PairingSession`, `DetectionPromptViewModel` and the scratchpad debounce
   take `any Clock<Duration>`; `backoffMs(attempt:random:)` takes the RNG.
   Replace "within 100 ms" with "throws `CancellationError` and the stub server
   recorded exactly one request".

10. **major — the append-only migration rule has no regression guard.**
    core step 3 asserts `appliedIdentifiers == ["v1"]`; the first v2 PR edits
    that assertion and nothing stops it from also editing v1's SQL. Add
    `SchemaSnapshotTests`: after migrating, dump `SELECT sql FROM sqlite_master
    ORDER BY name` and compare with `Tests/Fixtures/snapshots/schema/v1.sql`
    through `Snapshot`; a later migration adds `v2.sql` and a test that the
    dump after `v1` alone is still byte-identical to `v1.sql`.

11. **major — app configuration facts that break silently have no test.**
    macos steps 1, 12; audio "Needs from other workstreams" warns that Xcode
    ignores `INFOPLIST_KEY_NSAudioCaptureUsageDescription`. A missing literal
    key means no tap prompt and a silent system lane, found only in the manual
    checklist. Add `InfoPlistTests` (StenoTests): `Bundle.main.infoDictionary`
    contains the six usage and Bonjour keys, `NSBonjourServices ==
    ["_steno._tcp"]`, `SUFeedURL`, `SUPublicEDKey` non-placeholder in Release;
    step 12's `codesign -dv --entitlements -` becomes a scripted grep in
    `build-release.sh` that exits non-zero. `KeychainSecretStoreTests` (step
    7) runs unconditionally although the program lists `STENO_KEYCHAIN_TESTS`;
    gate it or state in the plan that it runs hosted.

12. **major — audio deletion (retention sweep) has only a manual check.**
    core step 9 tests `deleteExpiredAudio` returns URLs; the app "sweep"
    that removes files is named in program step 10 but no macOS step or test
    owns it (checklist item 8 only). Add `RetentionSweepTests` (StenoTests or
    core if the sweep moves into the package, preferred): temp folder with
    master, two sidecars, mixdown and one unrelated file; expired asset ->
    exactly the four removed, row updated; `.keepForever` untouched; a missing
    file does not abort the sweep.

13. **major — speech: the word-timing aggregation is untested and the
    mapping test rests on foreign initialisers.** speech steps 3 and 5. All
    segment and clip timings come from aggregating `TokenTiming` on
    SentencePiece `▁`, yet no test names it. Add `TokenAggregationTests`
    (synthetic tokens: word over three tokens, punctuation token, leading `▁`
    only, empty token). `FluidDiarizerMappingTests` "builds a
    `FluidAudio.DiarizationResult` by hand": its memberwise inits may be
    internal; map through StenoSpeech's own `ClusterChunk` and test that
    layer. Step 0 spike B and the step 3 RTF assertion silently need a 1.2 GB
    download and a fast machine: mark both `STENO_MODEL_TESTS`, report RTF
    instead of asserting it. Spike C needs a second same-voice fixture
    (`de-short-2.wav`); none is listed.

14. **minor — llm: three checks lack a fixture or an assertion.** Step 4's
    "1,000-word German sample" has no path (add `llm/text/de-1000-words.txt`);
    step 2's "key never in logs" has no test (`ClientTests.testRedaction`:
    `String(describing:)` of every `LLMError` and the recorded request exclude
    the key); the 60-minute fixture names no generator (commit it, finding 8).
    Mark suites touching `ModelHub.offlineMode` or env `.serialized`.

15. **minor — adapters: renderer changes can slip past the receipt.** Steps 3
    to 7. `rendererVersion` is bumped "when output changes" by convention.
    Add `RendererVersionTests`: sha256 over all goldens in
    `snapshots/obsidian/` recorded next to the version in
    `snapshots/obsidian/VERSION`; a golden diff with an unchanged version
    fails. Step 8's "core fixture database" does not exist; run `steno process`
    then `steno deliver` in `stenoTests` (finding 1). The deferred YAML
    round-trip is cheap on the runner: `/usr/bin/ruby -ryaml` parses the
    emitted frontmatter; add it to `FrontmatterTests`.

16. **minor — mobile: two device-manual checks are unit-testable.** P4
    "killing the app during a write leaves the previous valid index" -> inject
    the file API into `queue-storage.ts` and make `rename` throw; P6 backoff
    jitter -> RNG parameter (finding 9). `vitest` is already configured
    (`mobile/package.json` `test`), so the vitest checks are real; every P1,
    P2, P3, P5, P8 check is device-only and should say `[manual]`.

17. **minor — CLI tests can touch the developer's home.** core step 10:
    `--db` and `--audio-folder` default to `~/Library/...`; a forgotten flag in
    `CLITests` writes to the runner's home and passes. Set `HOME` to a temp
    directory in the child `Process` environment and assert the real home has
    no `Steno/` folder afterwards.

18. **minor — parallel test hazards.** `swift test --parallel` is fine for
    ephemeral loopback ports, not for `chmod` on shared paths,
    `STENO_UPDATE_SNAPSHOTS`, or process globals. Only adapters says "fresh
    temp directory per test"; core, audio and handover should say it too.

19. **Regression traps for reviewers, per workstream.**
    - core: `SchemaSnapshotTests`; any diff above the last `registerMigration`
      or a changed `exports/` golden without a model change is a rejection.
    - audio: grep `RealTime/` and `IOProcRunner.swift` for `[`, `Array`,
      closures, `os_log`, `await`, `lock`; run `LaneRingBuffer` tests with
      `-sanitize=thread`; confirm which `[manual]` checks the PR says it ran.
    - speech: `Package.resolved` model-package bumps; skip messages name
      `STENO_MODEL_TESTS`.
    - llm: a `prompts/*.txt` golden diff needs a sentence in the PR; the 400
      on `response_format` script still exists.
    - adapters: `RendererVersionTests`; re-read the "never deletes"
      assertions when `ObsidianLayout` changes.
    - handover: `HTTPRequestTests` limits and auth-before-body still asserted;
      `advertise: false` in every test.
    - macOS: `InfoPlistTests`; `codesign` grep in `build-release.sh`; UI smoke
      ran or is listed skipped with spike S1's reason.

## Coverage as the plans stand (test files or discrete checks, approximate)

| Workstream | unit (CI) | integration (CI) | opt-in | manual |
|---|---|---|---|---|
| core-foundation | 23 | 2 | 0 | 1 |
| audio-capture | 9 | 0 | 2 | 9 |
| speech-and-speakers | 11 | 0 | 1 file, ~6 checks | 3 |
| llm-and-templates | 11 | 0 (stub server counted as unit) | 1 | 4 |
| adapters-obsidian | 14 | 2 | 0 | 2 |
| phone-handover | 6 Swift + 3 vitest | 0 | 1 | 10 |
| macos-app-and-release | 18 | 1 UI smoke (conditional) | 0 (Keychain should be) | 18 |
| cross-module | 0 | 0 | 0 | checklist only |
