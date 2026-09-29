# Speaker calibration on real calls: fewer phantom speakers, working suggestions

Status: decision plan, 2026-09-29. Resolves the calibration that
[`2026-09-25-speech-and-speakers.md`](2026-09-25-speech-and-speakers.md) deferred to issue #34 and
narrows issue #31. Scope authority: [`2026-09-24-initial-scope.md`](2026-09-24-initial-scope.md).

## Problem

Nicolai's first week of real calls: every 1:1 comes out as three remote speakers, two group calls with
seven remote people each come out as eight and nine, and no known person is ever suggested. Seven calls
(four hours, five 1:1s and two group calls) were copied to Forge with the attendee list per call and used
as the calibration corpus. The recordings stay outside the repository; only counts, durations and
embeddings are reported here.

## What the corpus shows

Measured with `steno dev diarize-sweep` (this plan's PR) on FluidAudio 0.17.4, community-1 pipeline.

**Where the extra speakers come from.** Not fragments of a rambling speaker, and not echo of the mic
lane (cosine to the mic-lane embedding is about 0): the small clusters in every 1:1 are the same person's
short turns. Tim's "Speaker 1" says "Guten Morgen, wie geht's dir?"; Sebastian's asks "Thomas Göb oder
Thomas Fuchs?". A one-minute cluster of two-second interjections embeds far from the same voice speaking
for twenty minutes, because each ten-second window holds little of it.

| Call | Remote people | Clusters at 0.6 (today) | 0.7 | 0.8 | 0.9 | 1.0 |
|---|---|---|---|---|---|---|
| 1:1 Tim | 1 | 3 | 3 | 2 | 2 | 2 |
| 1:1 Mo | 1 | 3 | 2 | 2 | 2 | 1 |
| 1:1 Moritz | 1 | 1 | 1 | 1 | 1 | 1 |
| 1:1 Thomas | 1 | 3 | 2 | 2 | 2 | 2 |
| 1:1 Sebastian | 1 | 3 | 3 | 3 | 2 | 2 |
| Team kickoff | 7 | 8 | 7 | 7 | 7 | 1 |
| Lost deals review | 7 | 9 | 9 | 8 | 5 | 2 |

No FluidAudio clustering threshold fixes both shapes: 1.0 finally merges a 1:1 but collapses the kickoff
to one speaker; 0.9 already folds two people of the lost deals review together. The threshold is a
Euclidean cut on unit embeddings (larger merges more); `clusterConfidence` is 1.0 for almost every cluster
and separates nothing.

**Re-embedding fixes it.** Cutting a cluster's own speech out of the lane, concatenating it (up to three
minutes) and embedding that as one speaker gives vectors that behave like the long clusters:

| Comparison (re-embedded) | Cosine |
|---|---|
| Small cluster vs the same person's main cluster, 1:1s (8 cases) | 0.50, 0.54, 0.68, 0.69, 0.74, 0.92, 0.94, 0.94 |
| Different people, 1:1 main clusters (10 pairs) | 0.02 to 0.50 |
| Different people inside the group calls (all pairs over 20 s) | -0.11 to 0.39 |
| Sebastian's 1:1 voice vs his clusters in the two group calls | 0.75, 0.90 |
| Thomas's 1:1 voice vs his clusters in the two group calls | 0.78, 0.83 |
| Best wrong person for those four clusters | 0.25 to 0.54 |

Within the group calls, once the clusters under 30 s of speech are set aside (7 s, 2 s, 28 s, 1 s), the
kickoff has six substantive clusters for seven people and the lost deals review exactly seven for seven.
The diarizer was nearly right there; the excess was noise clusters and the 1:1 short-turn split.

**The matcher was starved, not broken.** The database held two people with one sample each and Nicolai
with none: the "Me" speaker of a call carries no embedding, so confirming yourself enrols nothing. The one
suggestion that fired (Philip, 0.62) was right. The 0.2 to 0.4 scores against Moritz that stayed below the
threshold were other people, correctly rejected. Nicolai's own mic-lane embedding is 0.85 to 0.97
consistent across calls where he spoke fifteen minutes or more, but in one call the mic lane was the
remote person's echo (0.66 to their cluster), so enrolling it needs a guard.

## Decisions

1. **Cluster refinement pass in `StenoSpeech`, after the FluidAudio mapping.** For every cluster with at
   least 30 s of speech: slice its ranges from the lane, concatenate up to 180 s, run the diarizer over
   that slice with `numSpeakers = 1` and take the resulting embedding as the cluster's embedding. Merge
   clusters whose re-embedded cosine is 0.60 or higher, greedily by highest pair first, re-embedding the
   union (ranges join, label of the earlier speaker survives). Clusters under 30 s join the cluster with
   the highest cosine when that is at least 0.30, otherwise their ranges are dropped and their segments
   stay speaker-less. Cost: about one second per cluster on an M4; the whole pass is bounded by the number
   of clusters, not the meeting length. Pure decision logic lives in a `ClusterRefinement` type given an
   `(AudioBuffer16k) async throws -> Embedding?` closure, so the merge rules are unit-tested without models.
   On the corpus this yields 1, 2, 1, 1, 2 remote speakers for the 1:1s (no false merge anywhere) and 6
   and 7 for the group calls.
2. **FluidAudio clustering threshold 0.8** (`FluidDiarizerConfig.default`). Same substantive clusters as
   0.6 on every call, fewer fragments for the refinement pass to handle. Issue #31 (a user setting) stays
   open but is no longer needed for correctness.
3. **Match threshold 0.55, margin 0.05 unchanged.** Same person across recordings scores 0.75 to 0.90
   with re-embedded vectors, different people at most 0.54. 0.55 catches every true match in the corpus;
   the runner-up margin rejects the 0.50 to 0.54 near-misses. `Settings.speakerMatchThreshold` default and
   the settings migration for stored rows at the old default.
4. **"Me" gets a voice.** For a call, embed the mic lane with `numSpeakers = 1` (the same re-embedding
   call) and store it on the "Me" speaker, unless the mic lane has under 60 s of speech or its embedding
   is within 0.50 of any system-lane cluster (echo guard, the Mo call). Confirming "Me" then enrols
   Nicolai like anyone else, and in-person recordings can suggest him.
5. **Confirmations survive reprocessing.** `replaceTranscript` currently deletes every speaker row. A
   re-run keeps `.confirmed` assignments for speaker ids that come back (ids derive from the meeting id
   and cluster label, so they do), and refreshes the voices of persons whose speakers vanished.
6. **Not doing:** attendee-count caps (calendars are often missing), PLDA-space comparison (raw space
   separates people fine once clusters are clean), a duration-based speaker drop above 30 s (kickoff
   speakers real at 55 s).

## Work packages

| WP | Where | Test |
|---|---|---|
| 0 | `Sources/steno/Commands/DevDiarizeSweep.swift` (this PR), this plan | Runs on Forge over the corpus; report tables reproduce the numbers above |
| 1 | `Sources/StenoSpeech/Diarization/ClusterRefinement.swift` (pure), `FluidDiarizer.diarize` calls it, `FluidDiarizerConfig.default` threshold 0.8 | Unit tests for merge order, the 30 s rule, the 0.30 absorb rule, label survival; opt-in model test on `two-speakers-mf.wav` unchanged (two speakers) |
| 2 | `Settings.speakerMatchThreshold` 0.55, `SettingsStore` migration of stored 0.60 | `SettingsStoreTests` |
| 3 | `Diarizer` protocol gains `embedding(of:)`; `Diarize` stage embeds the mic lane with the echo guard and writes it on the "Me" speaker (`Merge` stage) | Stage test with `FakeDiarizer`; `refreshVoice` picks the "Me" row up |
| 4 | `MeetingStore.replaceTranscript` keeps confirmed assignments by speaker id, refreshes voices of dropped persons | Store test: confirm, reprocess, still confirmed; person voice refreshed when their speaker vanishes |

Each WP is one PR on top of the previous one; WP1 is the user-visible fix and goes first.
