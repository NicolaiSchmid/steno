# Mac cutover: the Tauri app replaces the Swift app

Status (2026-10-18): steps 1 to 6 shipped through
`.plans/2026-10-07-stable-promotion.md`, which extended this plan and changed
them (its S1 to S7, all merged). Its rehearsal (R2 to R5 and R8) replaces this
plan's tests 1 to 7. Left: step 7, the Swift removal, which is the stable
plan's S9 and step 7 of `.plans/2026-10-18-simplify.md`, after the stable
release and its 14-day rollback window. This is WP9b of
`.plans/2026-10-02-rust-core-and-tauri-shell.md`.

## Goal

A user of the Swift app gets the Tauri app as an ordinary Sparkle update and
keeps the database, recordings, settings, API key, paired phones and login
item. From that release on the Tauri updater delivers updates on the Mac too,
and the Swift app, its release workflow and its Sparkle feed retire.

## What steps 1 to 6 became

- **Step 1, bundle id and version:** the app is
  `com.nicolaischmid.steno.desktop` (the stable plan's D5), not the Swift
  app's `uno.schmid.steno.mac`, so TCC grants and keychain items do not carry
  over: the stable plan accepts one re-grant and one keychain prompt per item.
- **Step 2, the Sparkle handoff:** one handoff item on the frozen `appcast`
  branch (D8); the stable plan's "The Sparkle handoff".
- **Step 3, distribution:** the stable plan's "Release mechanics" (S7, which
  also deleted `release.yml`).
- **Steps 4 to 6, preferences, login item, handover identity:** the stable
  plan's S6 import (D4 keeps `SMAppService`; a denied identity export mints
  nothing, and only the user's Pair again mints a new identity).

## Step 7: remove the Swift app

`apps/macos/` except `web/`, the Swift package (`Sources/`, `Tests/` after
`Tests/Fixtures/` moves), `swift-ci.yml`, and the Swift rows in `AGENTS.md`.
The web app moves from `apps/macos/web` to `apps/web`, with every path that
names it: `tauri.conf.json` (`frontendDist`, `beforeDevCommand`,
`beforeBuildCommand`), `build.rs`, `rust-ci.yml`, `web-ci.yml`,
`desktop-release.yml`, the bridge fixtures path in the bridge crate's tests,
`scripts/build-web.sh`, the READMEs. `Sources/StenoBridge` stops being the
oracle; the Rust bridge crate's recorded fixtures are the contract from then
on. The `appcast` branch stays published, so a Swift build that was offline
for months still finds the handoff item.

Done when `swift-ci.yml` is gone and `rg apps/macos` matches nothing outside
`.plans/` and `docs/`.
