# Steno

A bot-free meeting recorder for the Mac. Open source, local-first, built for people who
want Jamie-quality meeting notes without a cloud account, a meeting bot, or a subscription.

Steno captures system audio and your microphone directly from macOS, processes everything
on your Mac after the meeting, and writes summary, transcript and tasks into the places you
already work, starting with an Obsidian vault.

> Status: pre-alpha. Scope is settled, code is not written yet. See [docs/SCOPE.md](docs/SCOPE.md).

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
- **Phone companion.** A minimal iOS recorder for in-person meetings. It queues locally
  and hands recordings to your Mac over the local network for processing.

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
docs/SCOPE.md       what is being built and why, decision by decision
docs/research/      teardown of Jamie and the open-source landscape
```

Core package, macOS app and iOS app targets will follow the layout described in the scope.

## Background

Steno started as an attempt to replicate [Jamie](https://www.meetjamie.ai) as open source.
Along the way it borrows ideas from [anarlog](https://github.com/fastrepl/anarlog),
[Parrot](https://github.com/turantekin/Parrot) and [AudioCap](https://github.com/insidegui/AudioCap),
all rebuilt rather than forked. The name is a nod to stenographers and is known to collide
with a few commercial products; that is acceptable for a personal open-source project.

## License

MIT.
