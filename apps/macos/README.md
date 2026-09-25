# Steno for macOS

The SwiftUI app over the Swift package at the repository root. Plan:
[`.plans/2026-09-25-macos-app-and-release.md`](../../.plans/2026-09-25-macos-app-and-release.md).

## Generate and build

The Xcode project is generated, never committed.

```sh
brew install xcodegen            # or apps/macos/scripts/install-xcodegen.sh
cd apps/macos
xcodegen generate                # writes Steno.xcodeproj, Steno/Info.plist, Config/Steno.entitlements
open Steno.xcodeproj
```

Two schemes:

- `Steno`: the app plus the UI smoke test (`StenoUITests`). Debug builds are ad-hoc signed
  with the hardened runtime off, so no certificate is needed and Sparkle loads.
- `StenoTests`: the hostless unit tests over the view models and services. They link the
  package products directly and never launch the app. Keep it a separate scheme: when two
  targets of one build link the same package products, Xcode 16.4 wraps them in dynamic
  `PackageFrameworks` and links `Crypto` before it is built.

From the command line, the way CI does it:

```sh
xcodebuild build -project Steno.xcodeproj -scheme Steno -configuration Debug \
  -destination platform=macOS,arch=arm64 -derivedDataPath build/DerivedData CODE_SIGN_IDENTITY=- DEVELOPMENT_TEAM=
xcodebuild test  -project Steno.xcodeproj -scheme StenoTests -configuration Debug \
  -destination platform=macOS,arch=arm64 -derivedDataPath build/DerivedData CODE_SIGN_IDENTITY=- DEVELOPMENT_TEAM=
xcodebuild test  -project Steno.xcodeproj -scheme Steno -only-testing:StenoUITests ...
```

CI runs the first two on `MACOS_RUNS_ON` (the `app` job) and the UI smoke test in its own
GitHub-hosted `macos-15` job (`ui-smoke`): Xcode 27 on the Forge runner aborts inside
`IDELaunchServicesLauncher` (`INTERNAL ERROR: childPID > 0`) when `xcodebuild test` launches
an XCUITest runner, so the smoke test stays where spike S1 passed.

xcodebuild does not hand its own environment to the test process; prefix a variable with
`TEST_RUNNER_` to pass it through. `TEST_RUNNER_STENO_UPDATE_SNAPSHOTS=1 xcodebuild test …`
rewrites the tab goldens under `Tests/Fixtures/snapshots/macos/`, and
`TEST_RUNNER_STENO_KEYCHAIN_TESTS=1` runs the two login-keychain tests.

Launch the app with `-steno-ui-testing` for the preview environment: in-memory database seeded
with StenoCore's sample meeting, synthetic capture backend, fake engines, every permission
granted, no Sparkle, no keychain.

### Signing Debug builds locally

TCC forgets an ad-hoc signed app on every rebuild, so permission testing needs a stable
identity. Copy `Config/Local.xcconfig.example` to `Config/Local.xcconfig` (git-ignored), set
`DEVELOPMENT_TEAM` and `CODE_SIGN_IDENTITY = Apple Development`, regenerate. Reset the tap
permission with `tccutil reset AudioCapture uno.schmid.steno.mac`.

### Local Sparkle feed

`defaults write uno.schmid.steno.mac STENO_FEED_URL http://127.0.0.1:8000/appcast.xml` makes
a Debug build read that feed instead of `SUFeedURL`. Serve `dist/` with
`python3 -m http.server 8000 --directory dist`.

## Layout

| Path | What |
|---|---|
| `project.yml` | xcodegen spec: targets, Info.plist keys, entitlements, schemes |
| `Config/*.xcconfig` | team, bundle id, deployment target, Swift 6 strict; Debug ad-hoc, Release Developer ID |
| `Config/ExportOptions.plist` | `developer-id`, manual signing |
| `Steno/StenoApp.swift` | scenes, commands, `AppBootstrap` |
| `Steno/AppEnvironment.swift` | composition root: `live()` and `preview()` |
| `Steno/AppController.swift` | the running object graph over one environment |
| `Steno/Design/` | `Theme` (tokens mirroring `mobile/global.css`), `Motion`, shared controls |
| `Steno/MenuBar/` | recording, queue, launch at login |
| `Steno/Main/` | meeting list, detail with Summary, Transcript, Tasks, Scratchpad |
| `Steno/Speakers/` | the speaker review sheet and clip player |
| `Steno/Detection/` | the detection prompt (floating panel) |
| `Steno/Settings/` | General, Audio, Speech, LLM, Obsidian, Phones, Updates |
| `Steno/Onboarding/` | permission onboarding |
| `Steno/Services/` | the four app protocols over system frameworks, their live types and fakes |
| `StenoTests/` | hostless XCTest unit tests, one file per view model |
| `StenoUITests/` | `LaunchSmokeTests` |
| `scripts/` | `install-xcodegen.sh` (release zip pinned by version and SHA-256; an `xcodegen` on PATH counts only at the pinned version), `xcodebuild-quiet.sh` (log to file, diagnostics to the console, fails without the `** … SUCCEEDED **` marker; used by CI and `build-release.sh`), `xcresult-summary.py`, `build-release.sh`, `make-dmg.sh`, `make-appcast.sh` |

Where things live at runtime: the database in `~/Library/Application Support/Steno/steno.sqlite`,
recordings in the folder chosen in Audio settings (default `…/Steno/Audio`), models in
`…/Steno/Models`, the LLM API key in the login keychain (service `uno.schmid.steno.mac`,
account `llm-api-key`), the handover identity in the login keychain.

## Cutting a release

`.github/workflows/release.yml` runs on every `v*` tag:

1. `Check secrets` (`scripts/check-release-secrets.sh`, unit-tested in `ReleaseScriptsTests`)
   fails early with the missing names; a dry run needs only the certificate pair.
2. The Developer ID certificate is imported into a throwaway keychain.
3. `scripts/build-release.sh <version> <build>` archives and exports with Developer ID and the
   hardened runtime, then verifies: `codesign --verify --deep --strict`, the Developer ID
   authority, the runtime flag, a secure timestamp, exactly the two entitlements
   (audio-input, calendars, read with `codesign -d --entitlements - --xml`), and that every
   nested code item (Sparkle.framework with its `Autoupdate`, `Updater.app` and XPC services,
   FluidAudio's framework) carries the same team, the runtime flag and a timestamp.
   `<version>` is the tag without `v`; `<build>` is `git rev-list --count HEAD`.
4. `scripts/make-dmg.sh <version>` builds `Steno-<version>.dmg` with `hdiutil` (UDZO, an
   `Applications` symlink), signs it, submits it to `notarytool --wait`, staples the ticket
   and runs `spctl -a -t open --context context:primary-signature`.
5. `scripts/make-appcast.sh <tag>` runs Sparkle's `generate_appcast --ed-key-file -` with
   `SPARKLE_PRIVATE_KEY` on stdin and writes `appcast.xml` for this release alone.
6. A draft GitHub release is created, the DMG and appcast uploaded, and the release
   published (`--prerelease` when the tag contains a hyphen, so `releases/latest` skips it).
7. `always()`: the keychain is deleted and the App Store Connect key removed.

`workflow_dispatch` with `dry_run` builds, signs and verifies without notarising or
publishing.

### One-time setup (a human, once)

Apple:

- Create a Developer ID Application certificate in the developer portal, export it from
  Keychain Access as `.p12` with a password, `base64 < cert.p12 | pbcopy`, store both in
  1Password, delete the local files.
- App Store Connect > Users and Access > Integrations > Team Keys: create a Developer-role
  key, download the `.p8` once, note Key ID and Issuer ID.
- Sparkle: `generate_keys` once on a Mac, `generate_keys -x sparkle.key` to export the
  private key, store it in 1Password. The public key is committed in `project.yml`
  (`SUPublicEDKey`).

GitHub (`NicolaiSchmid/steno`, Settings > Secrets and variables > Actions):

| Secret | Value |
|---|---|
| `MACOS_CERTIFICATE_P12_BASE64` | the base64 `.p12` |
| `MACOS_CERTIFICATE_PASSWORD` | its password |
| `ASC_KEY_ID`, `ASC_ISSUER_ID`, `ASC_PRIVATE_KEY` | the App Store Connect key (contents of the `.p8`) |
| `SPARKLE_PRIVATE_KEY` | the exported EdDSA private key |

Variable `MACOS_RUNS_ON`: `"macos-15"` (default) or `["self-hosted","macOS","ARM64"]` once
the Forge runner is registered. Settings > Actions: keep "Require approval for all outside
collaborators" on, so a fork never reaches the self-hosted runner.

### Release rehearsal

Tag `v0.9.0-rc.1` (pre-release), install the DMG on a fresh macOS 15.1+ user, run the manual
checklist from the plan, tag `v0.9.0`, then `v0.9.1` and confirm Sparkle offers and installs
it before tagging `v1.0.0`.
