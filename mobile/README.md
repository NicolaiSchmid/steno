# Steno — iOS recorder

The phone side of Steno. It does one thing: record audio, keep it queued on
the device, and hand it to the paired Mac over the local network for
processing. No account, no backend, no notes on the phone. Scope and the
decisions behind it: [`.plans/2026-09-24-initial-scope.md`](../.plans/2026-09-24-initial-scope.md).

Status: the phone side of
[`.plans/2026-09-25-phone-handover.md`](../.plans/2026-09-25-phone-handover.md)
is implemented and waits for the Mac listener (`Sources/StenoHandover`). The
app records AAC `.m4a` with expo-audio, keeps a JSON-indexed queue in
`Documents/queue/`, pairs with the Mac by scanning its QR code, and uploads in
16 MiB chunks over a background `URLSession` that pins the Mac's certificate.

## How the handover works

- `modules/steno-link/` is a local Expo module (Swift, autolinked from
  `./modules`). It browses `_steno._tcp` with `NWBrowser`, resolves the Mac,
  performs small pinned JSON requests, runs the background upload session
  `uno.schmid.steno.upload`, and hashes files with streaming SHA-256.
  `ios/PinnedTrustEvaluator.swift` is the canonical pinning code; the Swift
  package's handover tests symlink to it. The Swift compiles only during
  `expo prebuild` on a Mac; CI checks the TypeScript around it.
- `modules/steno-link/src/wire.ts` mirrors the StenoCore wire types by name
  (`RecordingMetadata`, `RecordingStatus`, `PairRequest`, `PairResponse`),
  camelCase JSON, `Data` as standard base64. Only the QR URL uses base64url.
- `src/features/queue/` holds the queue index (pure state machine) and its
  atomic storage; `src/features/sync/` the planner and coordinator;
  `src/features/pairing/` the QR parser, pairing sequence and sheet;
  `src/features/recorder/` the screen, the expo-audio hook and crash
  recovery. `@modules/steno-link` is the pure entry (types and wire);
  `@modules/steno-link/native` the module and the pinned transport.
- Everything deterministic is under vitest (`pnpm test`): wire encoding,
  queue index and storage, pairing payload parsing and fingerprint comparison,
  discovery registry, pairing sequence, planner and backoff, version gate,
  formatters, recovery. Anything touching a device (recording an hour locked,
  a real pairing, uploads with the phone in a pocket) is a manual check listed
  in the plan.
- iOS 18.6 or later is required at runtime (Local Network privilege bug in
  earlier 18.x); older builds see an update screen.

## Requirements

This is an **Expo Dev Client** app (Expo SDK 57), **not** Expo Go: it depends
on native modules (expo-audio, reanimated, screens, secure-store). iOS only.

- iOS builds need macOS with Xcode and cannot run on a Linux box; only
  typecheck, lint and tests run there.
- Node 24, pnpm 11 (`packageManager` in `package.json`).

## Bootstrap

```bash
cd mobile
pnpm install
cp .env.example .env
pnpm ios                 # prebuild --clean + expo run:ios (needs macOS/Xcode)
pnpm check               # biome + tsc + vitest
```

Config is read from `EXPO_PUBLIC_*` env vars via `src/lib/env.ts`. There are
none yet. `APP_VARIANT` (`development` | `preview` | `production`) selects the
bundle id, scheme and display name in `app.config.ts`.

## Design system

Tokens live in `global.css` (the house five-tier luminance ladder, alpha-veil
surfaces, hairlines, semantic statuses) and reach components through uniwind
classes or `useThemeColor("--color-…")`. Type scale is `AppText`. Motion
tokens are `src/lib/motion.ts`. No hard-coded colours or durations at call
sites.

## Delivery

CD runs on GitHub Actions: `.github/workflows/mobile-cd.yml`, on push to
`main` touching `mobile/**`. It computes the Expo native fingerprint and picks
the lane:

- **OTA update** when the newest `ios-fp-*` tag equals the current
  fingerprint. JS-only changes ship in minutes, live on next launch.
- **TestFlight build** otherwise: `eas build --local` on the macOS runner,
  `eas submit`, OTA baseline, then the `ios-fp-<hash>` tag is pushed.

PRs get the same decision as a sticky comment from `mobile-ci.yml`, computed
by the same script (`scripts/ci/mobile-delivery-lane.sh`), so the preview
cannot disagree with CD. The manual fallback when Actions cannot run is the
EAS Workflow in `.eas/workflows/submit-ios.yml`.

Anything that moves the fingerprint (a dependency, config plugin, permission,
icon, `version`, `updates`) forces a native build. Check the PR comment before
merging such a change.

### Apple one-time setup (not done yet)

1. `cd mobile && eas init` against account `nicolaischmid`. Commit the printed
   project id as the `easProjectId` default in `app.config.ts`; OTA updates are
   disabled until then.
2. Confirm the bundle id `uno.schmid.steno` (and `.dev`, `.preview`). It is
   immutable after the first build.
3. Create the App Store Connect record, pin its numeric id as
   `submit.production.ios.ascAppId` in `eas.json` (replaces `TODO_ASC_APP_ID`;
   CD refuses to build until then).
4. `APP_VARIANT=production eas credentials -p ios`, "Set up all required
   credentials", so the non-interactive CD build can fetch provisioning.
5. Add the `EXPO_TOKEN` repository secret.
6. Add an app icon (`ios.icon` in `app.config.ts`) and splash images before the
   first TestFlight build.

### After a fallback build

The EAS Workflow bypasses CD, so CD does not know a binary exists. Record it
by hand or the next JS-only push rebuilds:

```bash
git tag -f ios-fp-<fingerprint printed by verify_ios>
git push -f origin ios-fp-<fingerprint>
cd mobile && APP_VARIANT=production eas update --platform ios --branch production --environment production --message "baseline"
```
