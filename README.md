# Steno

A bot-free meeting recorder for the Mac. Open source, local-first, built for people who
want Jamie-quality meeting notes without a cloud account, a meeting bot, or a subscription.

Steno captures system audio and your microphone directly from macOS, processes everything
on your Mac after the meeting, and writes summary, transcript and tasks into the places you
already work, starting with an Obsidian vault.

> Status: v1 is code-complete and not yet released. The signing certificate, TestFlight
> setup and the manual checks on real hardware that gate the first release are tracked in
> [issue #74](https://github.com/NicolaiSchmid/steno/issues/74). Scope:
> [.plans/2026-09-24-initial-scope.md](.plans/2026-09-24-initial-scope.md).

## Install

Steno is a signed and notarized `Steno.app` for macOS 15+ on Apple Silicon. Every
release publishes `Steno-<version>.dmg`; drag the app to `/Applications`, or use one of the
package managers below. Releases so far are pre-releases.

### Nix

`flake.nix` installs the released `Steno.app` unchanged (no rebuild, no re-signing) on
`aarch64-darwin`:

```sh
nix profile install github:NicolaiSchmid/steno#steno
```

With nix-darwin, add the flake as an input and put `steno.packages.aarch64-darwin.steno`
in `environment.systemPackages`; nix-darwin links `Applications/Steno.app` into
`/Applications/Nix Apps`. With home-manager use `home.packages` and link the app yourself
(`home.file` into `~/Applications`, or the `mac-app-util` module); home-manager does not
link `Applications/` on its own. `nix run` does not apply, the output is an app bundle
with no `bin/`.

The app runs from the read-only Nix store, so Sparkle's "Check for Updates…" can download
a new version but cannot install it. Update by bumping `release.version` and
`release.hash` at the top of `flake.nix` (the release workflow prints both in its job
summary) and rebuilding. To silence the scheduled daily check:

```sh
defaults write uno.schmid.steno.mac SUEnableAutomaticChecks -bool NO
```

nix-darwin users who want in-app updates can use the Homebrew cask through
`homebrew.casks` instead; it installs into `/Applications`, where Sparkle can write.

## What it does

- **Records without a bot.** Core Audio process taps for the other side, your mic for you.
  Works with Zoom, Teams, Meet, FaceTime, Slack, in-person meetings, and phone calls taken
  on the Mac via Continuity.
- **Processes on your Mac.** Transcription with Parakeet or Whisper, speaker diarization
  with voice memory, all on-device. Only transcript text is sent to an LLM endpoint you
  configure. Audio never leaves the machine.
- **Handles German, English and Denglish.** Transcript language is auto-detected and an
  LLM cleanup pass fixes the code-switching that every speech model gets wrong.
- **Remembers voices.** Name a speaker once, Steno recognises them next time.
- **Writes files, not lock-in.** One dated folder per meeting in your Obsidian vault:
  summary note, transcript, tasks in Obsidian Tasks syntax, VTT and JSON. Further
  adapters plug into the same one-way push protocol.
- **Phone companion.** A minimal iOS recorder (Expo) for in-person meetings. It queues
  locally and hands recordings to your Mac over the local network for processing.

## What it deliberately does not do

No Windows. No live transcript during the call. No chat over your meetings. No editing of
generated text inside the app. No team features, no hosted service, no App Store build.
The app stores the canonical data; reading, searching and automation happen in your vault
and with whatever agents you already use.

## Stack

Swift and SwiftUI, macOS 15+ on Apple Silicon. SQLite via GRDB. FluidAudio and WhisperKit
for speech. Any OpenAI-compatible endpoint for post-processing. Signed and notarized DMG
with Sparkle updates.

## Repository

```
AGENTS.md           conventions for coding agents (CLAUDE.md links here)
Package.swift       Swift package manifest; modules under Sources/, tests under Tests/
Sources/            StenoCore, StenoAudio, StenoSpeech, StenoLLM, StenoAdapters,
                    StenoHandover and the `steno` CLI
apps/macos/         SwiftUI app over the package, own README, xcodegen project
mobile/             Expo iOS recorder, own pnpm project, own README
.plans/             scope and planning documents, dated
docs/research/      teardown of Jamie and the open-source landscape
.github/workflows/  repository, Swift and mobile CI; macOS release on `v*` tags
```

## Background

Steno started as an attempt to replicate [Jamie](https://www.meetjamie.ai) as open source.
Along the way it borrows ideas from [anarlog](https://github.com/fastrepl/anarlog),
[Parrot](https://github.com/turantekin/Parrot) and [AudioCap](https://github.com/insidegui/AudioCap),
all rebuilt rather than forked. The name is a nod to stenographers and is known to collide
with a few commercial products; that is acceptable for a personal open-source project.

## License

MIT.
