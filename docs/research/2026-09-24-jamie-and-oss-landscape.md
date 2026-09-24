# Jamie (meetjamie.ai) teardown and open-source landscape

Date: 2026-09-24. Goal: open-source, macOS-first clone of Jamie with pluggable
transcript/notes destinations ("adapters").

## 1. What Jamie actually is

- Berlin, founded 2022, ~20k users. Native apps: macOS (14.4+), Windows, iOS.
- **Bot-free capture.** Records system audio + microphone at the OS level. No
  virtual audio driver, no meeting bot. Works with any meeting app and in-person.
- **Processing is cloud, not local.** Audio is uploaded to Frankfurt, transcribed
  and summarized there, then deleted. Notes arrive 1-5 min after the meeting ends.
  Local-first is therefore a differentiator we can have that Jamie does not.
- **Meeting detection.** Mic-activity based "Start Jamie?" prompt, auto-start /
  auto-stop, menu bar shows next calendar event with one-click join.
- **Speaker identification with memory.** Diarization on the mixed stream, then a
  human-in-the-loop step: user listens to a short clip per speaker and types a
  name. Voice is remembered and auto-labelled in later meetings. Also infers
  names from conversational context. Caveat in their docs: fast turns or noise
  can split one person into two speakers.
- **Outputs.** Notes (custom templates), action items, transcript, tags,
  cross-meeting "Ask AI" chat. 20+ languages with auto-detect.
- **Integrations (the adapter surface to replicate):**
  - CRM: HubSpot, Salesforce, Dynamics 365, Attio, Affinity, DealCloud
  - Docs: Notion, Google Docs, OneNote
  - Tasks: Asana
  - AI: ChatGPT connector, MCP server (Claude / Cursor / ChatGPT)
  - Developer: webhooks (signed, retried; Make.com recipe), REST API
    (list/get/search/delete meetings, tags, tasks)
  - Workflows: post-meeting automations (CRM update, follow-up draft) with
    optional human review
  - Not native: Slack, Zapier, Pipedrive
- **Pricing (2026):** Free 10 meetings/mo at 30 min; Plus EUR 25; Pro EUR 47;
  Team EUR 39/seat. Widely criticised as expensive now that bot-free is common.
- **Enterprise:** SSO, SCIM, IP allowlisting, audit logs, ISO 27001, EU hosting.

### What "fancy audio processing" most likely means

1. Core Audio process tap for system audio (macOS 14.2+; Jamie's 14.4 minimum
   strongly implies this API rather than ScreenCaptureKit).
2. Simultaneous mic capture, mixed or kept as two lanes.
3. Acoustic echo cancellation so far-end speech played through speakers does not
   get double-transcribed via the mic lane.
4. Diarization + persisted speaker embeddings ("speaker memory").

## 2. Open-source landscape

| Project | Stack | License | Stars | macOS capture | STT | Diarization | Adapters | Notes |
|---|---|---|---|---|---|---|---|---|
| anarlog (ex-Hyprnote, YC S25) | Tauri v2, Rust + React | MIT app, commercial `enterprise/` | ~9.4k | Rust crates incl. `meeting-capture`, `aec` | local (Soniqo, Apple Speech, whisper) or cloud | `api-pyannote` (cloud) | Markdown, Notion, Linear, calendar crates, MCP, CLI, plugin SDK | Most complete. 105+ crates. Team pivoted to "char"; repo maintained but not the focus |
| Meetily | Tauri, Rust + Next.js | MIT community; Pro proprietary | ~31k | mic + system mix, ducking | whisper.cpp, Parakeet ONNX | Pro only | None in community; exports Pro only | Big audience, thin OSS core |
| Parrot | Native SwiftUI, ~13k LOC | GPL-3 (<=0.11.3 MIT) | ~30 | Core Audio taps (15+), SCK fallback (14), AVAudioEngine mic, SpeexDSP AEC | WhisperKit; Groq/Deepgram BYO | FluidAudio + voice memory | None (live copilot focus) | Technically closest to Jamie's audio pipeline |
| Open Granola | Tauri | Apache-2.0 | 6 | claims CoreAudio tap | whisper.cpp | spectral clustering | Obsidian, Notion, Todoist (claimed) | 6 commits, no releases. Aspirational |
| Humla | Swift, Mac only | MIT | small | on-device | whisper | offline | none | |
| Muesli | Apple Silicon | ? | small | | Parakeet v3 | | | |
| Notare | fork of anarlog | MIT | small | | pluggable | | Markdown vault | |
| Scripta, meetink, meeting-noter, whisperASR | Swift + whisper.cpp + SCK | various | tiny | ScreenCaptureKit | whisper.cpp | none | none | Good reference snippets |
| Vexa | self-hosted bot + API | | | bot | | | | Not bot-free, out of scope |

**Gap:** nobody combines (a) native Swift, (b) two-lane capture with AEC and
speaker memory, (c) a real adapter layer to CRMs/docs/webhooks/MCP, under a
permissive license with an active maintainer.

## 3. Building blocks for a native Swift implementation

- **System audio:** Core Audio process taps. `CATapDescription` ->
  `AudioHardwareCreateProcessTap` -> private aggregate device with the default
  output as main sub-device and the tap as sub-tap -> `AudioDeviceCreateIOProcIDWithBlock`.
  Reference: insidegui/AudioCap, AudioTee. Gotchas: AVAudioEngine cannot be
  retargeted to the tap aggregate (silently keeps default input); taps have no
  permission-status API (unauthorised = silence); needs
  `NSAudioCaptureUsageDescription`; exclude own PID from the tap.
- **Mic:** AVAudioEngine input node. Keep as a separate lane ("me").
- **AEC:** Apple's VoiceProcessingIO uses the system output mix as its reference
  and interferes with system-audio capture, so it is a poor fit. Do software AEC
  on the mic lane with the tap audio as far-end reference (SpeexDSP MDF or WebRTC
  AEC3). Sauron PR #4 reports ~38 dB ERLE at 48 ms delay with SpeexDSP.
- **Two-lane labelling:** mic = "me", tap = "them", deterministic. Diarize only
  the "them" lane (Parrot's approach). In-person mode: single mic lane, full
  diarization.
- **STT (on-device, Apple Silicon):**
  - FluidAudio Parakeet TDT v3: ~100x realtime on ANE, 25 languages, 6.3% WER
    (vs 7.8% whisper-large-v3-turbo). Good default.
  - WhisperKit (Argmax): 99 languages, best streaming latency, CoreML/ANE.
    Fallback for non-European languages.
  - whisper.cpp: mature, needs C bridging, no ANE encoder without CoreML build.
- **Diarization:** FluidAudio (Apache-2.0). Offline: pyannote community-1 +
  WeSpeaker + VBx. Streaming: Sortformer / LS-EEND. Speaker memory = persist
  embeddings per named speaker, cosine-match new clusters, confirm via clip UI.
- **VAD:** Silero via FluidAudio (also drives auto-start detection).
- **LLM:** Apple Foundation Models (on-device) / Ollama / BYO key (Claude via
  API). Transcript text only leaves the machine, never audio.
- **Storage:** SQLite (GRDB or SwiftData). Optional sqlite-vec for Ask-AI.

## 4. Open decisions

1. **Native Swift vs Tauri/Rust.** Swift: FluidAudio and WhisperKit are
   Swift-native, taps are trivial, matches the macOS-first goal. Tauri: Windows
   later for free, could fork anarlog. Recommendation: native Swift.
2. **Fork vs greenfield.** anarlog is huge and its team moved on; Parrot is GPL.
   Recommendation: greenfield, borrow patterns from AudioCap and Parrot.
3. **Adapter API.** A `NoteDestination` protocol (auth, map meeting -> payload,
   push, idempotency key) with first adapters: Markdown/Obsidian folder,
   webhook, Notion, Attio. MCP server and REST API as "adapters" too.
4. **Local-first with optional cloud STT** (Deepgram/AssemblyAI) for accuracy.

## Sources

- https://www.meetjamie.ai/  https://docs.meetjamie.ai/llms.txt
- https://docs.meetjamie.ai/getting-started/identify-speaker
- https://docs.meetjamie.ai/enterprise/admins/integrations-and-developers
- https://www.meetjamie.ai/blog/ai-note-taker  https://tldv.io/blog/meetjamie-alternatives/
- https://www.makeuseof.com/jamie-ai-meeting-assistant/
- https://github.com/fastrepl/anarlog  https://github.com/Zackriya-Solutions/meetily
- https://github.com/anshuman-pandey/open-granola  https://github.com/turantekin/Parrot
- https://github.com/abhi-wan-kenobi/notare  https://humla.team/alternatives/granola
- https://github.com/thehwang/Scripta  https://github.com/sservaes/meetink
- https://github.com/deemiles/meeting-noter  https://github.com/insidegui/AudioCap
- https://developer.apple.com/documentation/CoreAudio/capturing-system-audio-with-core-audio-taps
- https://dgrlabs.co/blog/2026-04-25-capturing-system-audio-on-macos-in-2026.html
- https://stronglytyped.uk/articles/audiotee-capture-system-audio-output-macos
- https://github.com/chasebank87/Sauron/pull/4  https://developer.apple.com/forums/thread/733733
- https://github.com/FluidInference/FluidAudio  https://k2-fsa.github.io/sherpa/onnx/speaker-diarization/index.html
- https://github.com/anvanvan/mac-whisper-speedtest  https://spokenly.app/blog/parakeet-vs-whisper
- https://openwhispr.com/blog/local-speaker-diarization

---

## Addendum (same day): anarlog deep-dive, Vercel toolkits, scope options

Sparse clone inspected at `/tmp/anarlog` (fastrepl/anarlog, main, 2026-09-24).

### anarlog by the numbers
- 152 crates, ~1,700 `.rs` files, ~2,300 `.ts/.tsx` files, 75 `.swift`, 50 Tauri plugins,
  Supabase backend (214 files), Expo mobile app (~22k LOC), `enterprise/` (108 files).
- Audio core, confirmed from source:
  - `audio-actual` (5.3k LOC): macOS system audio via **Core Audio process tap** through
    the `cidre` crate (`ca::TapDesc::with_mono_global_tap_excluding_processes` ->
    `ca::AggregateDevice` -> IOProc). Mic via `cpal`. Windows WASAPI, Linux PipeWire/Pulse.
  - `aec` (620 LOC): learned two-stage ONNX echo canceller, models named
    `model_{128,256,512}_{1,2}.onnx` (DTLN-aec naming, ~62 MB bundled), via `anlg-onnx`.
  - `listener2-core` (8.4k LOC): session orchestration; depends on `pyannote-local`
    (diarization), `voiceprint` (speaker memory), `transcribe-soniqo`,
    `transcribe-speechanalyzer` (Apple SpeechAnalyzer), `owhisper-client`, `embedding`.
  - Sync: `e2ee` (age / x25519), `db-sync` (CRDT hints), `cloudsync`, Supabase + R2.
- Licensing (`LICENSING.md`): desktop, mobile, CLI, local capture/transcription/storage/
  export/sync clients are MIT. Only `enterprise/**` (self-hosted orchestration, admin,
  policy, license enforcement) is commercial.

### Vercel toolkits
- `vercel-labs/native` (Aug 2026): Zig engine, `.native` markup + TypeScript subset compiled
  to a single binary, no WebView, Metal renderer. 7.7k stars, pre-1.0, Labs experiment.
  No documented FFI path to Core Audio / CoreML; effects are message-based.
- `zero-native` (Jun 2026): Zig WebView shell, Tauri competitor, experimental, macOS/Linux.
- Verdict: neither fits an app whose core is real-time audio + on-device ML. At most a UI
  shell later.

### The actual decision: Rust core vs Swift core
| | Rust core + Tauri UI | Swift core + SwiftUI |
|---|---|---|
| Reuse anarlog crates verbatim | yes (`audio-actual`, `aec`, `pyannote-local`, `voiceprint`) | reference only |
| Apple ML stack (FluidAudio, WhisperKit, SpeechAnalyzer, Foundation Models) | via bridging | native |
| Learned AEC | DTLN ONNX ready | SpeexDSP, WebRTC AEC3, or port DTLN ONNX to CoreML |
| Windows | yes | no |
| Mobile | Expo app, separate codebase | iOS app sharing the same Swift package |
| Cloud sync with zero backend | needs own server or Supabase | CloudKit |
| Apple API access | `cidre` (single-maintainer crate) | first-party |

### Scope tiers (proposal)
- **Tier 0, local-only core (OSS default):** capture, AEC, STT, diarization, speaker memory,
  notes, SQLite, adapters pushing directly from the Mac. No account. Audio never leaves device.
- **Tier 1, opt-in cloud services:** BYO-key cloud STT/LLM; small relay for OAuth redirects
  and webhook retry; remote MCP endpoint.
- **Tier 2, sync for mobile:** encrypted blob + metadata sync through a dumb store. Desktop
  stays the processing node; phone records in-person audio, uploads encrypted, Mac processes.
  Swift stack: CloudKit, zero own backend. Rust stack: S3/R2 + own auth, or Supabase like anarlog.
