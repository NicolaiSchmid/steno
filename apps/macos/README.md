# Steno for macOS

The Mac app over the Swift package at the repository root: a Swift shell (`AppController`, the
view models, the services, Sparkle, the iPhone listener) whose three windows, the main window,
Settings and onboarding, are pages of the web UI in [`web/`](web/README.md) rendered by
`WKWebView` from the app bundle, with one bridge per window mapping view model state to
snapshots and page commands to view model calls. SwiftUI draws only the menu bar item and the
floating panels (the recording bubble, the detection prompt). Plans:
[`.plans/2026-09-25-macos-app-and-release.md`](../../.plans/2026-09-25-macos-app-and-release.md)
(app target, CI, release) and
[`.plans/2026-09-29-macos-webview-ui.md`](../../.plans/2026-09-29-macos-webview-ui.md) (the web
windows, the bridge, the design language).

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
an XCUITest runner, so the smoke test stays where spike S1 passed. That runner's display is
1024 x 768, the layout budget: every window must fit it at its minimum size (the main window's
960 by 600, Settings' 960 by 640 by default, resizable down to 760 by 520, onboarding's 560 by 620); the pages lay out inside those
frames, and the Playwright screens in `web/` review them at the same sizes.

xcodebuild does not hand its own environment to the test process; prefix a variable with
`TEST_RUNNER_` to pass it through. `TEST_RUNNER_STENO_UPDATE_SNAPSHOTS=1 xcodebuild test …`
rewrites the tab goldens under `Tests/Fixtures/snapshots/macos/`, and
`TEST_RUNNER_STENO_KEYCHAIN_TESTS=1` runs the two login-keychain tests.

Launch the app with `-steno-ui-testing` for the preview environment: in-memory database seeded
with StenoCore's sample meeting, synthetic capture backend, fake engines, every permission
granted, no Sparkle, no keychain. Add `-steno-start-recording` to start a call recording from the
window at launch (the live row selected, the header and sidebar Stop controls showing),
`-steno-show-onboarding` to open the onboarding window over unknown permissions, or
`-steno-ui-testing-hold-transcribe` to queue the sample
meeting for processing at launch over a synthetic recording, with the fake speech engine
holding each lane for sixty seconds, so the processing card can be watched; the UI smoke test
uses it. `-steno-appearance=light|dark` renders the app in that appearance whatever the
system setting, `-steno-window=960x600` sizes the main window at launch and
`-steno-settings-section=recording` (any `SettingsSection` raw value) is the section Settings
opens on (the page reads it from the `app` snapshot's `requestedSettingsSection` once ⌘, opens
the window, which is `settings-window` to XCUITest); the screenshot matrix
(`testScreenshotMatrixLight` and `Dark`) uses them. A value
always sits in the flag's own argument: AppKit opens a separate token as a document at launch
and SwiftUI then leaves the main window closed. The
`ui-smoke` job exports the attachments beside the result bundle
(`apps/macos/build/attachments`, named in `manifest.json`; `scripts/attachment-names.py` lists
them and fails when one of the matrix's twelve is missing). A misspelt `-steno-*` flag or a
bad value shows a launch error instead of the app.

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
| `Steno/Design/` | `Theme` and `Motion` (the tokens the native surfaces draw with, mirroring `mobile/global.css`), `Components` (the button styles, chip, card, dot and message row the menu bar and panels share), `Labels` (user-facing words for core's enums, read by the snapshots too) |
| `Steno/Resources/` | `AppIcon.svg`, the icon's source of truth, and `Assets.xcassets` with the `AppIcon` set it renders to |
| `Steno/MenuBar/` | The menu bar item (SwiftUI): recording, queue, launch at login |
| `Steno/Panels/` | The floating panels (SwiftUI): the recording bubble and the detection prompt |
| `Steno/Recording/` | `RecordingController`, auto-stop, the countdown and clock, the Stop and level views the bubble and the menu bar share |
| `Steno/Main/` | The main window (`MainWindow`, the web page at `#/main` over `Web/MainWindowBridge`, 960 by 600 minimum) and its models: `MeetingListViewModel`, `MeetingDetailViewModel`, `ProcessingProgressModel` with the `ProcessingPresentation` card, `SetupStatus` |
| `Steno/Speakers/` | `SpeakersViewModel` and `SpeakerPickerState` behind the detail's speaker rows, the clip player |
| `Steno/Detection/` | Meeting detection and the prompt's model |
| `Steno/Settings/` | The Settings window (`SettingsWindow`, the web page at `#/settings` over `Web/SettingsBridge`, 960 by 640 by default, resizable down to 760 by 520): General, Recording, Transcription, Summaries, Export, iPhone (`SettingsSection`), one view model each plus the sidebar status (`SettingsOverviewViewModel`) |
| `Steno/Web/` | The web host: `AppSchemeHandler` (the bundle over `steno-app://`), `WebBridge` and `BridgeDispatcher` (the message runtime), `WebWindowView` (one `WKWebView` per window; `CanvasWebView` paints the window in the page's canvas colour), `TopicPublisher` (the one publishing loop: tracked snapshots, coalescing, page readiness), and one `*Bridge` plus `*Snapshots` per window (`MainWindowBridge`, `SettingsBridge`, `OnboardingBridge`) |
| `Steno/Onboarding/` | The onboarding window (`OnboardingWindow`, the web page at `#/onboarding` over `Web/OnboardingBridge`, 560 by 620) and `OnboardingViewModel`, the two pages' rules: permissions, then Summaries and the Obsidian vault over the Settings view models |
| `Steno/Services/` | the four app protocols over system frameworks, their live types and fakes |
| `StenoTests/` | hostless unit tests: one file per view model, the bridges' snapshots and command routing (`*SnapshotsTests` over `BridgeTestSupport`), the web host's pure rules (`WebHostTests`), the theme tokens against both CSS files |
| `StenoUITests/` | `LaunchSmokeTests` |
| `scripts/` | `install-xcodegen.sh` (release zip pinned by version and SHA-256; an `xcodegen` on PATH counts only at the pinned version), `install-gh.sh` (the GitHub CLI for the publish step, same pinned-zip scheme; a `gh` already on PATH is used as is, so hosted runners skip the download and Forge needs nothing preinstalled), `xcodebuild-quiet.sh` (log to file, diagnostics to the console, fails without the `** … SUCCEEDED **` marker; used by CI and `build-release.sh`), `xcresult-summary.py`, `build-release.sh`, `make-dmg.sh`, `make-appcast.sh`, `make-app-icon.sh` |

### App icon

Edit `Steno/Resources/AppIcon.svg`, run `scripts/make-app-icon.sh` (ImageMagick 7 with librsvg,
`brew install imagemagick`) and commit the SVG together with everything it wrote: the ten PNGs and
`Contents.json` in `Assets.xcassets/AppIcon.appiconset`, `Steno/Resources/AppIcon.sha256` and
`mobile/assets/icon.png`. `AppIconTests` fails on CI when the SVG and `AppIcon.sha256` disagree.
`scripts/make-app-icon.sh --check` compares bytes; it is exact only for the ImageMagick and librsvg
pair that produced the committed files (7.1.2 and 2.62 today), and it also reports a `Contents.json`
that Xcode's asset editor reformatted. A diff after a Homebrew upgrade or an Xcode edit means
re-render and commit, not a bug. Design and geometry:
[`.plans/2026-09-28-app-icon.md`](../../.plans/2026-09-28-app-icon.md).

Where things live at runtime: the database in `~/Library/Application Support/Steno/steno.sqlite`,
recordings in the folder chosen in Audio settings (default `…/Steno/Audio`), models in
`…/Steno/Models`, the LLM API key in the login keychain (service `uno.schmid.steno.mac`,
account `llm-api-key`), the handover identity in the login keychain. With the ChatGPT (Codex)
summaries choice, Steno reads and refreshes the Codex CLI's own sign-in in `~/.codex/auth.json`
(or `$CODEX_HOME/auth.json`) and stores nothing of it elsewhere; the choice is off until the
user confirms it in onboarding or Settings > Summaries
([`.plans/2026-09-29-codex-chatgpt-provider.md`](../../.plans/2026-09-29-codex-chatgpt-provider.md)).

## Releases (retired)

The Swift app ships no further release: S7 of
[`.plans/2026-10-07-stable-promotion.md`](../../.plans/2026-10-07-stable-promotion.md) removed
`.github/workflows/release.yml`, and a `v*` tag now releases the Tauri app through
`.github/workflows/desktop-release.yml` (`apps/desktop/README.md`, Release). That workflow
still calls `scripts/publish-appcast.sh`, `scripts/merge-appcast.py` and
`scripts/bump-homebrew-cask.sh`, which stay here until the Swift app's removal. If a bridge
release is ever needed, a dedicated pull request restores the workflow with a `swift-v*`
trigger (the plan's "`release.yml`" section). The removed workflow ran these steps:

1. `Check secrets` (`scripts/check-release-secrets.sh`, unit-tested in `ReleaseScriptsTests`)
   failed early with the missing names; a dry run needed only the certificate pair.
2. The Developer ID certificate was imported into a throwaway keychain. A Swift release ran
   only while no Desktop release run with macOS was in progress, since both imported the same
   Developer ID identity (see `apps/desktop/README.md`, Signing).
3. `scripts/build-release.sh <version> <build>` archived and exported with Developer ID and the
   hardened runtime, then verified: `codesign --verify --deep --strict`, the Developer ID
   authority, the runtime flag, a secure timestamp, exactly the two entitlements
   (audio-input, calendars, read with `codesign -d --entitlements - --xml`), and that every
   nested code item (Sparkle.framework with its `Autoupdate`, `Updater.app` and XPC services,
   the Swift compatibility dylib Xcode embeds) carries the same team and a timestamp, and
   every nested bundle and executable the runtime flag (dylibs never carry it; notarisation
   requires it on executables and bundles only). `build-release.sh --verify-only <Steno.app>`
   runs only these checks against an app exported earlier; `ReleaseScriptsTests` does so
   against a `codesign` shim. `<version>` is the tag without `v`; `<build>` is
   `git rev-list --count HEAD`.
4. `scripts/make-dmg.sh <version>` built `Steno-<version>.dmg` with `hdiutil` (UDZO, an
   `Applications` symlink), signed it, submitted it to `notarytool --wait`, stapled the ticket
   and ran `spctl -a -t open --context context:primary-signature`.
5. `scripts/make-appcast.sh <tag>` ran Sparkle's `generate_appcast --ed-key-file -` with
   `SPARKLE_PRIVATE_KEY` on stdin and wrote `appcast.xml` for this release alone.
6. A draft GitHub release was created, the DMG and appcast uploaded, and the release
   published (`--prerelease` when the tag contained a hyphen).
   The build number was the commit count, so a tag was refused when another `v*` tag already
   pointed at the same commit: promoting `v0.9.0-rc.1` to `v0.9.0` needs a new commit (the
   version bump), otherwise Sparkle, which orders by build number alone, would never offer
   the stable build to candidate installs.
7. `scripts/publish-appcast.sh <tag> <prerelease>` folded the release's item into the rolling
   `appcast.xml` on the `appcast` branch (`scripts/merge-appcast.py`: newest first, one item
   per build number, at most twenty) and pushed. That branch is what `SUFeedURL` reads
   (`https://raw.githubusercontent.com/NicolaiSchmid/steno/appcast/appcast.xml`), so the feed
   never depends on `releases/latest`, which GitHub never points at a pre-release. A
   pre-release item carries `<sparkle:channel>beta</sparkle:channel>`; the app allows that
   channel only when its own version string has a hyphen (`Services/UpdateChannels.swift`),
   so release candidates are offered candidates and stable builds never are. The
   per-release appcast stays on the release too, so `v0.9.0-rc.1` installs (which read
   `releases/latest`) hop to the first stable release and pick up the new feed URL from it.
8. `scripts/bump-homebrew-cask.sh <version> <dmg>` rewrote `version` and `sha256` in
   `Casks/steno.rb` of [NicolaiSchmid/homebrew-tap](https://github.com/NicolaiSchmid/homebrew-tap)
   and pushed `steno <version>` to its `main`; pre-releases bumped too (the desktop release
   bumps stable releases only). Without `HOMEBREW_TAP_TOKEN` the step printed a notice and
   the release stood; with it, a failed push was a warning (`continue-on-error`), never a
   failed release.
9. `always()`: the keychain was deleted and the App Store Connect key removed.

`workflow_dispatch` with `dry_run` built, signed and verified without notarising or
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
| `HOMEBREW_TAP_TOKEN` | optional: fine-grained PAT, repository access `homebrew-tap` only, permission Contents read and write; the cask bump is skipped without it |

Variable `MACOS_RUNS_ON`: `"macos-15"` (default) or `["self-hosted","macOS","ARM64"]` once
the Forge runner is registered. Settings > Actions: keep "Require approval for all outside
collaborators" on, so a fork never reaches the self-hosted runner.

### Release rehearsal

Tag `v0.9.0-rc.1` (pre-release), install the DMG on a fresh macOS 15.1+ user, run the manual
checklist from the plan, tag `v0.9.0`, then `v0.9.1` and confirm Sparkle offers and installs
it before tagging `v1.0.0`. After each tag, `curl -s
https://raw.githubusercontent.com/NicolaiSchmid/steno/appcast/appcast.xml` should list the
new build at the top; a candidate build (`-rc`) must be offered a newer candidate, a stable
build must not see candidates.

### Homebrew

`brew tap nicolaischmid/tap && brew install --cask steno` installs the DMG the cask points at;
`brew upgrade --cask steno` follows the tap, Sparkle follows the `appcast` branch. Plan:
[`.plans/2026-09-28-homebrew-and-nix.md`](../../.plans/2026-09-28-homebrew-and-nix.md).
