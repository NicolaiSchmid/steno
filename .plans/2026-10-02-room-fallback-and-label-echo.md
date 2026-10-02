# Fix: a call on speaker is one "Me", and the cleanup pass writes "Me:" into the text

Status: implemented 2026-10-02 (this PR). Refines
`2026-09-24-initial-scope.md` (lane rules: mic is "me", tap is "them") and
`2026-09-29-speaker-calibration.md` (diarization runs over the tap). Neither
plan changes; this one adds a fallback when the tap carried nothing.

## What happened

A phone call recorded with the Mac app while the phone was on speaker
(meeting `Nicolai Schmid und Julien Weber`, 2026-10-02, 11:45). The files
and the database say:

| Lane | Content |
|---|---|
| `system.wav` | digital silence for all 705 s (peak -180 dBFS): nothing on the Mac played audio |
| `mic.wav` | the whole conversation, both voices, 599 s above -45 dBFS |
| `transcriptSegment` | 189 rows, all `lane = mic`, every `text` starts with `Me: `, every `rawText` clean |
| `speaker` | one row, `Me` |

Two defects stack:

1. **Every mic segment is "me".** `LaneMerger` assigns the mic lane to the
   deterministic "me" speaker and diarizes only the tap
   (`ProcessingPipeline.diarizedLane`). With a silent tap the diarizer
   finds no clusters, the whole call becomes one speaker, and the detail
   view joins 189 consecutive "Me" segments into one eleven-minute turn.
   The capture already knows (`CaptureStatistics.systemLaneSilent`, shown
   as a warning at stop) but the pipeline does not.
2. **The cleanup pass stores the speaker label.** The prompt renders each
   segment as `[n] Me: text` (`TranscriptLines.render`). The model answered
   `Me: text`. `CleanupDraft.problems` allows a one-word drift so that
   "Git Hub" can become "GitHub", and `Me:` is exactly one word, so every
   segment passed validation with the label baked in. `rawText` is intact.

## Decisions

1. **Room fallback in the pipeline, not at capture.** After transcription,
   a `.macCall` asset whose system lane holds less than 5 % of the mic
   lane's speech duration (`tapConversationMinimumShare`) and less than
   ten seconds outright (`tapConversationMaximumSeconds`) is treated as a
   room recording: the mic lane is diarized instead of the tap, mic
   segments get clusters, no "me" speaker is created, and the tap's stray
   segments (a chime, a hallucinated word) are kept without a speaker so
   nothing transcribed is lost if the rule misjudges a call. The pipeline
   sees the transcription, so it catches a tap that recorded a notification
   sound as well as pure silence, and the rule also covers `steno process`
   and re-runs. The meeting keeps `source = macCall`: it was a call, just
   not through the Mac. Capture, the asset's lanes and the files stay
   truthful.
2. **Threshold by speech share and an absolute cap, not peak.** The
   capture's -80 dBFS peak rule would miss a tap that heard one chime. The
   share alone would misjudge a partner who mostly listens (60 s against
   2000 s is 3 %), so ten seconds of tap speech is always a conversation.
   Zero mic speech never triggers the fallback.
3. **One voice on the mic is the user alone.** A silent tap also happens
   with headphones when the system audio permission is missing. When the
   diarizer hears fewer than two voices on the mic lane, the stage returns
   no clusters and writes no clip, and the merge keeps the mic as "me" as
   before. A "me" participant an earlier run of the pipeline created
   (deterministic id) is removed when a re-run falls back; one the app
   wrote stays.
4. **Strip echoed labels before validation, and tell the model.** The
   cleaner strips a leading `[n]` index and any known speaker label (or
   `Unknown speaker`) from every returned text
   (`CleanupDraft.strippingSpeakerLabels`), case-insensitively, before the
   count and word checks run. The label may be wrapped in markdown or
   brackets (`**Me:**`, `(Me)`) and end in a colon, a closing bracket or a
   spaced dash. A text that merely starts with a word and a colon
   ("Meeting: agenda", "Me-too products") is left alone unless the word is
   a label. The prompt gains one rule saying the index and label are
   framing. Stripping is deterministic and costs no retry; the prompt rule
   makes the retry rarer. Goldens `Tests/Fixtures/llm/prompts/cleanup-*.txt`
   change by that one line.
5. **No automatic repair of stored transcripts.** The affected meeting
   needs re-diarization anyway, which only a re-run of the pipeline gives.
   The app has no "process again" today; that is a follow-up (bridge
   command + button on a ready meeting). Until then the kept recording can
   be re-run with `steno process <folder>/mic.wav --source mac-in-person`.

## Changes

- `Sources/StenoCore/Pipeline/Stages/Diarize.swift`: `Diarization.lane`,
  `diarizedLane(source:lanes:transcription:)`, `tapCarriedNoConversation`,
  `tapConversationMinimumShare`, `tapConversationMaximumSeconds`; `diarize`
  takes the lane and keeps a one-voice mic lane as "me".
- `Sources/StenoCore/Pipeline/ProcessingPipeline.swift`:
  `transcribeAndDiarize` picks the lane from the transcription and drops a
  handed buffer of another lane before `diarize` decodes the right one.
- `Sources/StenoCore/Pipeline/LaneMerger.swift`: `merge(..., diarizedLane:)`.
- `Sources/StenoCore/Pipeline/Stages/Merge.swift`: no "me" speaker when
  the mic lane is the room; the pipeline's own "me" participant is removed.
- `Sources/StenoCore/Storage/MeetingStore.swift`: `deleteParticipant(id:)`.
- `Sources/StenoCore/Model/Meeting.swift`,
  `Sources/StenoAudio/Capture/CaptureConfiguration.swift`: lane rule doc
  comments name the exception.
- `Sources/StenoLLM/Cleanup/CleanupDraft.swift`,
  `LLMTranscriptCleaner.swift`, `CleanupPromptBuilder.swift`,
  `Transcript/SpeakerLabels.swift`: label stripping and the prompt rule.
- `Sources/StenoCore/Testing/FakeSpeech.swift`: `silentBelowPeak` so a
  test can hand the engine a silent lane.
- Tests: `LaneMergerTests`, `StageTests`, `PipelineIntegrationTests`,
  `CleanupTests`, `CleanupValidationTests`, prompt goldens.

## Follow-ups

- "Process again" for a ready meeting (bridge command, button in the
  detail header) so the affected recording and future ones can be redone
  from the app.
- The stop-time warning "The system audio lane stayed silent. Check the
  system audio permission." should also name the speakerphone case now that
  the pipeline handles it.
