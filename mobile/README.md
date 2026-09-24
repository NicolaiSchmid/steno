# Steno — iOS recorder

The phone side of Steno. It does one thing: record audio, keep it queued on
the device, and hand it to the paired Mac over the local network for
processing. No account, no backend, no notes on the phone. Scope and the
decisions behind it: [`.plans/2026-09-24-initial-scope.md`](../.plans/2026-09-24-initial-scope.md).

Status: scaffold only. The app builds a themed shell with one placeholder
screen. Recording, the local queue and Bonjour handover are separate plans.

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
