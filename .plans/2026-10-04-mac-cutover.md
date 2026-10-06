# Mac cutover: the Tauri app replaces the Swift app

Extended by `.plans/2026-10-07-stable-promotion.md` (2026-10-07). That plan:

- replaces the opening gate (every unticked parity line) with its blocking list,
  and this plan's one pull request with its packages S6 to S9;
- replaces step 2's beta staging and signing and all of step 3 (distribution);
  step 2's frozen `appcast` branch stands, with one handoff item (its D8), and
  step 7's removal of `release.yml` moves to its S7;
- answers the open choices in step 1 (its D5) and step 5 (its D4, which keeps
  `SMAppService`, so the two-login-items risk is moot);
- carries step 4 as two preference keys plus Sparkle's update flags.

The inventory, steps 4, 6 and 7, and the tests stand, with three corrections
(that plan's Facts):

- test 1's premise that a Swift release build reads no channel matches no
  installed build: every one is a release candidate, and from `v0.9.0-rc.2`
  on each reads `beta` too, while `v0.9.0-rc.1` reads
  `releases/latest/download/appcast.xml`;
- test 1's `codesign -dr -` texts never match, so `codesign --verify -R` is
  the check;
- Sparkle does not refuse a bundle for its id, so the first risk keeps only the
  missing key and the signature.

Status: planned 2026-10-04, not started. WP9b, the second half of WP9 in
`.plans/2026-10-02-rust-core-and-tauri-shell.md`; WP9a (signed, notarised
bundles that carry the speech sidecar, `cargo deny`, the `desktop-v*`
release workflow and the updater lanes) is #184
(`feat/rust-release-signing`). It supersedes nothing; it expands the
cutover paragraph of WP9 in the Rust plan, whose progress table tracks it
as WP9b.
This plan is one pull request, opened only once the parity list in the
Rust plan is empty, apart from its two Rust-only "Speech" lines
(`SpeechSettings` and "One model store"), which do not gate it. That list
includes four gaps of the shell's ("Open after the port" in the Rust
plan): the tray's badge for pending speaker reviews, the QR encoder, the
clip player and the update schedule (Swift checks daily).

## Goal

A user of the Swift app gets the Tauri app as an ordinary Sparkle update and
notices nothing but the new version: same app name and place, same
microphone, system audio and calendar permissions, same login item, same
database, recordings and settings, same API key, same paired phones. From
that release on the Tauri updater delivers updates on the Mac too, and the
Swift app, its release workflow and its Sparkle feed retire.

## What the Swift app owns today

Each row is something the cutover has to carry over, retire or decide.

| Item | Swift app | Rust app before the cutover |
|---|---|---|
| Bundle id | `uno.schmid.steno.mac` (`apps/macos/project.yml`) | `uno.schmid.steno.desktop` (`tauri.conf.json`), a separate app to macOS; also `Steno.app`, so in `/Applications` it replaces the Swift app |
| App | `/Applications/Steno.app`, executable `Steno` | `Steno.app`, executable `steno-desktop`, sidecar beside it |
| Version | `CFBundleShortVersionString` from the `v*` tag, `CFBundleVersion` the commit count | Both from `[workspace.package]` (0.1.0) |
| Updates | Sparkle, `SUFeedURL` the rolling `appcast` branch, `SUPublicEDKey` (`apps/macos/project.yml`), daily checks | Tauri updater, the `desktop-stable` and `desktop-beta` lanes, on request only |
| Login item | `SMAppService.mainApp` (`LoginItemController.swift`) | A Launch Agent from `tauri-plugin-autostart` |
| Data | `~/Library/Application Support/Steno/` (database, audio, models) | The same directory (`steno_core::paths`) |
| Preferences | `UserDefaults` of `uno.schmid.steno.mac`: `steno.onboardingCompleted`, `steno.floatingPanel.anchor`, `steno.systemAudioGranted`, `steno.loginItemRegistered` (`AppController.swift`: the first-launch login-item registration has run) | `preferences.json` in the support directory; the panel anchor in `panel-anchor.json` in Tauri's app config directory, which is named after the bundle id |
| Secrets | Keychain generic passwords, service `uno.schmid.steno.mac`, account the `SecretKey` | The same service and accounts through `keyring` (`KEYRING_SERVICE`) |
| Phone handover identity | A `SecIdentity` labelled `Steno handover identity` (`IdentityKeychain.swift`) | A PEM bundle in the keyring entry `handover-identity` |
| Permissions | TCC grants for `uno.schmid.steno.mac` under team `KQB68F43PW` | Grants for `uno.schmid.steno.desktop` |
| Distribution | `v*` releases (`release.yml`), "latest release" links, the Homebrew cask, the Nix flake | `desktop-v*` pre-releases, never "latest" |

## Steps

1. **Bundle id and version.** `tauri.conf.json` `identifier` becomes
   `uno.schmid.steno.mac`, and `.github/workflows/desktop-release.yml`
   sets `bundle.macOS.bundleVersion` to the commit count, the Swift
   scheme, through the configuration merge. Sparkle orders updates by
   `CFBundleVersion` alone, so the cutover build must have a higher one
   than the last Swift build; the commit count only grows.
   `desktop-release.yml` checks out with `fetch-depth: 0` (the commit
   count needs the history) and fails when the build number is not above
   the newest `sparkle:version` on the `appcast` branch. The marketing
   version moves past the last Swift tag (the workspace version becomes
   the next minor after it), so the Tauri updater, which compares
   marketing versions, never offers an older build.
   Installs of the `uno.schmid.steno.desktop` build read the same
   `desktop-*` lanes, so the cutover build reaches them as an ordinary
   Tauri update and converts them in place to `uno.schmid.steno.mac`,
   wherever they sit. This step decides which of two outcomes ships:
   either the conversion happens, and a Mac that had both apps ends with
   two copies of the cutover app on one database; or the cutover build
   reads new lane names, and the old lanes leave those installs at the
   last desktop-id build, a second app on the same database. Either way
   the release notes (step 3) tell users to delete the extra copy.
2. **Sparkle handoff.** The last Swift release ships unchanged. The cutover
   release adds one item to the rolling appcast on the `appcast` branch: the
   Tauri `.dmg`, signed with `SPARKLE_PRIVATE_KEY`, `sparkle:version` the
   build number from step 1. The macOS Bundle job signs the `.dmg` with
   Sparkle's `sign_update` from a pinned Sparkle release (the Xcode build
   that provided `generate_appcast` goes away) and uploads the signature
   with the bundles; publish writes the item to the `appcast` branch after
   the release is public, since the item's URL resolves only then.
   `desktop-release.yml` gains `SPARKLE_PRIVATE_KEY` in Check secrets, and
   the desktop README's secrets table lists it. The item first carries
   `<sparkle:channel>beta</sparkle:channel>` and a pre-release version;
   after test 4, a release item without the channel follows. The cutover
   build's `Info.plist` (`apps/desktop/src-tauri/`) carries the Swift
   app's `SUPublicEDKey` from `apps/macos/project.yml`: Sparkle 2 refuses
   an update whose new bundle drops the key the running app has (it
   supports rotation, not removal). The key is inert in the Tauri app.
   Sparkle installs the item over `Steno.app` because the bundle id
   matches and the EdDSA signature verifies against that key (the
   Developer ID team matching the running app's designated requirement is
   the other check that would pass). After the release item, `release.yml`,
   the Sparkle scripts and the `appcast` branch stop moving; the branch
   stays published so a Swift build that was offline for months still
   finds the handoff item.
3. **Distribution.** The desktop workflow takes over what `release.yml`
   did: the Mac release becomes GitHub's "latest" (drop `--prerelease` and
   `--latest=false` for releases without a hyphen), the Homebrew cask bump
   and the Nix flake hash move to the desktop `.dmg`. Whether the tag prefix
   becomes `v*` again is decided then: the installed apps read the rolling
   lanes, not a tag name, so either works. The cutover's release notes
   tell users of the `uno.schmid.steno.desktop` build what step 1 decided
   for it, and say whether phones pair again (step 6, test 3).
4. **Preferences.** On first launch, with no `preferences.json` yet, the
   Rust app reads the four `UserDefaults` keys from
   `~/Library/Preferences/uno.schmid.steno.mac.plist` (through
   `CFPreferences` with the app's own domain, which after step 1 is that
   one) and writes them into `preferences.json`, so onboarding does not
   reopen. The panel anchor is converted to `panel-anchor.json` or dropped
   (the panel then opens at its default place).
5. **Login item.** On first launch the Rust app checks
   `SMAppService.mainApp.status`; when it is `enabled` it unregisters it
   first, then enables its own Launch Agent, so the user keeps exactly one
   login item. When the status is not `enabled` but
   `steno.loginItemRegistered` (step 4) is set, the user turned the item
   off after the Swift app registered it, so the Rust app registers
   nothing. With neither, it does what a fresh install does. This needs
   a small `objc2` call in the shell (`autostart.rs`). Keeping
   `SMAppService.mainApp` instead of the Launch Agent on macOS is the
   alternative, decided by test 1. The `requiresApproval` copy in the
   General section becomes unreachable and goes.
6. **Handover identity.** Phones pin the Mac's certificate, so a new
   identity forces every phone to pair again. The cutover imports the Swift
   identity: on first launch, when `handover-identity` is empty, find the
   certificate labelled `Steno handover identity` and its identity through
   `SecIdentityCreateWithCertificate`, as `IdentityKeychain.load` does
   (`Sources/StenoHandover/Identity/IdentityKeychain.swift`; an identity
   query ignores the label and returns every identity in the keychain).
   Export that identity with `SecItemExport` as PKCS#12, convert it to the
   PEM bundle `steno-handover` reads, store it under `handover-identity` and
   leave the Swift item in place. If the export fails (a key marked
   non-extractable, a denied prompt), the app mints a new identity and
   phones pair again. The paired devices are rows in the shared database,
   so nothing else changes.
7. **Remove the Swift app.** `apps/macos/` except `web/`, the Swift package
   targets the Rust crates replace, `swift-ci.yml`, `release.yml`, the
   Swift rows in `AGENTS.md`. The web app moves from `apps/macos/web` to
   `apps/web`, with every path that names it: `tauri.conf.json`
   (`frontendDist`, `beforeDevCommand`, `beforeBuildCommand`), `build.rs`,
   `rust-ci.yml`, `web-ci.yml`, `desktop-release.yml`, the bridge fixtures
   path in the bridge crate's tests, `scripts/build-web.sh`, the READMEs.
   `Sources/StenoBridge` stops being the oracle; the Rust bridge crate's
   recorded fixtures are the contract from then on.

## Risks

- **Sparkle refuses the update** when the new bundle lacks the key (step
  2) or the bundle id (step 1), or passes neither the EdDSA check nor the
  designated requirement (an ad hoc signed sidecar fails the latter). The
  Swift app then retries daily without telling the user why. Covered by
  test 1.
- **Version ordering.** A `CFBundleVersion` lower than the last Swift
  build's means the item is never offered; a marketing version lower than
  the last Swift tag confuses the Tauri updater after the handoff. Step 1
  fixes and checks both.
- **TCC grants.** Microphone, system audio and calendar grants are stored
  per bundle id and checked against the code requirement recorded at grant
  time. Same id and team should keep them. If the requirement differs,
  the user is asked again, and system audio records silence until they
  agree. The executable's name is not part of the requirement; the
  bundle's identifier is. Covered by test 2.
- **Keychain prompts.** The generic passwords and the identity trust the
  app that created them through its designated requirement. If the Rust
  app's differs, macOS asks once per item on first access; for the API key
  that happens in the middle of a summary. Same id and team avoids it.
- **The handover identity export** can raise a keychain prompt or fail;
  then phones pair again (step 6). Step 3's notes cover it.
- **Two login items** if step 5 fails. `SMAppService.mainApp` registers
  the app bundle (its id and path), not the executable, so after the
  in-place replacement the Swift entry most likely still launches the new
  app; with the Launch Agent beside it, two items both start Steno at
  login (the single-instance guard keeps one running). Test 1 checks
  which happens.
- **Data directory.** Both apps use the same database. A user who runs the
  `uno.schmid.steno.desktop` build beside the Swift app before the cutover
  already shares it, and after the cutover it leaves a second Steno on the
  same database either way (step 1). Tauri's own directories (WebKit data,
  caches, the panel anchor) are named after the bundle id and start empty
  under the new one.
- **Rollback.** Once Sparkle has replaced the app there is no way back
  through Sparkle; a broken cutover build is fixed forward through the
  Tauri updater, which only works if the cutover build's updater works.
  Step 2 publishes to the beta channel first.

## Tests

All on a real Apple-silicon Mac (the self-hosted runner's machine or a
second user account), never on CI alone.

1. **Handoff.** Install the last Swift release from its DMG, grant every
   permission, set an API key, pair a phone, record a meeting, enable launch
   at login. Point the Swift app at a local appcast that carries the
   cutover item without `sparkle:channel`: a Swift release build reads no
   channel (`UpdateChannels.allowed`), and its own feed override
   `STENO_FEED_URL` exists in DEBUG builds only (`UpdaterController.swift`).
   Use Sparkle 2's `SUFeedURL` user default
   (`defaults write uno.schmid.steno.mac SUFeedURL <url>` on a test
   account), after checking that a release build still honours it; if it
   does not, use a Developer ID test build of the last Swift release whose
   `Info.plist` names the local appcast. Check for updates, install. Then:
   the app launches as the Tauri app, `codesign -dr -` on it shows the
   same designated requirement as the Swift build's, its `Info.plist` has
   the Swift `SUPublicEDKey`, the meeting is listed, the API key works
   without a prompt, onboarding does not open, Login Items shows one Steno
   entry.
2. **Permissions.** After test 1, record a call: both lanes carry audio, no
   TCC prompt appeared, and `tccutil` was not needed.
3. **Phone.** After test 1, the paired phone uploads a recording without
   pairing again (the identity was imported), or, in the export-failure
   case, pairing again works and the old pairing is gone.
4. **Updater after the handoff.** Publish a second pre-release on the beta
   lane; the cutover build finds it through the Tauri updater, installs it
   and relaunches, and the sidecar still runs from the new bundle.
5. **Fresh install.** A Mac that never had the Swift app installs the
   cutover DMG: onboarding opens, Gatekeeper passes the image offline (it
   is stapled), and nothing references the Swift paths.
6. **Offline Swift build.** A Swift build two releases behind still finds
   the handoff item on the `appcast` branch after `release.yml` is gone.
7. **Distribution.** After the cutover release, `releases/latest` resolves
   to it, and the cask and the flake install the desktop `.dmg`.
8. **Removal.** `swift-ci.yml` and `release.yml` are gone, and
   `rg apps/macos` matches nothing outside `.plans/` and `docs/`.
