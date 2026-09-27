# iOS bundle id: `com.nicolaischmid.steno`

Amends one identifier in `.plans/2026-09-25-v1-program.md` ("Identifiers,
immutable once a build ships") and in `.plans/2026-09-25-macos-app-and-release.md`
(the aside that the mobile team owns `uno.schmid.steno`). Both plans otherwise
stand.

## Decision

The iOS recorder ships as `com.nicolaischmid.steno`. Variants:

| Variant | Bundle id | Scheme (unchanged) |
|---|---|---|
| production | `com.nicolaischmid.steno` | `steno` |
| preview | `com.nicolaischmid.steno.preview` | `steno-preview` |
| development | `com.nicolaischmid.steno.dev` | `steno-dev` |

The macOS app keeps `uno.schmid.steno.mac`; nothing on the Mac side changes.
Reverse-DNS identifiers inside the iOS app that are not bundle ids (the
background `URLSession` `com.nicolaischmid.steno.upload`, the Bonjour browser
queue label) follow the new prefix so the app carries one namespace.

## Why it changed before the first build

Nicolai registered `com.nicolaischmid.steno` as the App ID under team
`KQB68F43PW` and created the App Store Connect record against it while
completing the Apple one-time setup (issue #60). Nothing had ever been built,
uploaded or provisioned for `uno.schmid.steno`: no EAS credentials, no
TestFlight build, no installed tester. Changing the id at this point costs
nothing; changing it after the first TestFlight build would orphan testers,
keychain-backed data and the server-side build number. The Apple record is the
source of truth now, so the code follows it rather than the other way round.

## What exists on the Apple side (2026-09-27)

- App ID `com.nicolaischmid.steno` (id `R3YTUG98Z8`, team `KQB68F43PW`).
- App Store Connect app `6816745548` ("Steno – Transcripts", SKU `steno-ios`,
  primary locale `en-US`) at https://appstoreconnect.apple.com/apps/6816745548.
  Pinned as `submit.production.ios.ascAppId` in `mobile/eas.json`.
- EAS project `@nicolaischmid/steno`, id `0bfb34f4-1f48-4286-a576-32dcbde71b15`,
  pinned as the `easProjectId` default in `mobile/app.config.ts`.
- `.dev` and `.preview` App IDs are not registered yet. The App Store Connect
  API key on file has the Developer role and Apple refuses `POST /v1/bundleIds`
  with it; EAS registers them during the first interactive
  `eas credentials -p ios` for those profiles.

## From here on

From the first TestFlight build onwards the production bundle id, the EAS
project id, the ASC app id and the slug `steno` are immutable. A change needs
a new plan that says why and accepts losing every installed build.
