# Speech and speakers: StenoSpeech

Status: workstream plan, 2026-09-25. Binding program:
[`2026-09-25-v1-program.md`](2026-09-25-v1-program.md). Scope authority:
[`2026-09-24-initial-scope.md`](2026-09-24-initial-scope.md).
Owns `Sources/StenoSpeech`, `Tests/StenoSpeechTests`, the `steno bakeoff`
and `steno models` subcommands.

API names below were checked against FluidAudio `v0.17.3` (source of
2026-09-24) and WhisperKit `v1.1.0` (repository now named `argmax-oss-swift`,
old URL redirects). Anything marked **unverified** was seen only in a README,
model card or doc page, or not seen at all, and must be confirmed in step 0.

## Goal

Turn a 16 kHz mono lane into timed, language-tagged `RawSegment`s with two
interchangeable on-device engines, turn the "them" lane (or the whole mix)
into speaker clusters with L2-normalised 256-dim embeddings and a ten-second
sample clip each, match those clusters against remembered people, and give
the project a repeatable bake-off so the default engine is chosen from
measurements on real German/English/Denglish meetings, not from a README.

## Non-goals

- Live or streaming transcription and streaming diarization (`SlidingWindowAsrManager`,
  `DiarizerManager`, `LSEENDDiarizer`, Sortformer are not wrapped).
- Any model conversion or fine-tuning; we consume published CoreML repos.
- Speaker name inference from context (StenoLLM), speaker review UI (app).
- Custom vocabulary boosting (FluidAudio CTC rescoring); revisit after v1.
- Apple `SpeechAnalyzer`, whisper.cpp, cloud STT.
- Audio decode and resampling to 16 kHz (StenoCore pipeline step 1).
- Persisting `Person` rows (StenoCore storage); we only compute and match.

## Decisions

| Topic | Decision |
|---|---|
| Default engine candidate | Parakeet TDT v3 via FluidAudio (`parakeet-v3`). Final default set by the bake-off (step 9) and recorded in a follow-up plan. |
| Second engine | WhisperKit `openai_whisper-large-v3-v20240930_turbo` (`whisperkit-large-v3-turbo`). |
| Optional engines | `parakeet-ultra` (same FluidAudio API, `AsrModelVersion.ultra`); `parakeet-de` (German fine-tune, custom directory, German-only). Both bake-off only unless the bake-off promotes one. |
| Language handling | No engine can be forced per segment. Parakeet has no language control at all (its `Language` parameter only filters Latin vs Cyrillic script; `de` and `en` are both Latin). Whisper can be pinned per call, not per segment. So: engines run unpinned (Parakeet) or pinned to the meeting language (Whisper); every `RawSegment` gets `language` from `NLLanguageRecognizer` constrained to `{de, en}`; the LLM cleanup pass fixes the rest. |
| Diarizer | FluidAudio `OfflineDiarizerManager`, pyannote community-1 offline pipeline, community defaults, `exposeChunkEmbeddings = true`. |
| Cluster embedding | Mean of the cluster's `ChunkEmbedding.embedding256` values weighted by chunk duration, then L2-normalised. Not `speakerDatabase`/`TimedSpeakerSegment.embedding`: those are VBx centroids, an un-normalised mean of unit vectors, so cosine against stored people would be biased by cluster purity. |
| Speaker match | Cosine similarity on unit vectors. Default threshold `0.60`, margin `0.05` over the runner-up; both are settings. Calibrated in step 6. |
| Enrolment | Running mean: `e' = normalise((e * n + x) / (n + 1))`, `n` capped at 50 so a voice can drift. |
| Merge | Merging person B into A: sample-count-weighted mean, renormalise, B's `Speaker` and `Participant` rows repointed to A, B deleted. Within a meeting, merging clusters unions their ranges and recomputes the embedding from their chunks. |
| Sample clip | Longest contiguous single-speaker segment of the cluster, capped to 10 s centred on the highest-`qualityScore` chunk; if the longest segment is under 3 s the speaker is flagged `lowConfidence`. |
| Model location | `~/Library/Application Support/Steno/Models/`. FluidAudio repos as `fluidaudio/<repo-last-path-component>` (the directory name must match the HF repo name, see German model caveat). WhisperKit under `whisperkit/` via `downloadBase`. |
| Type collisions | FluidAudio exports `DiarizationResult`, `Speaker`, `Language`, `WordTiming`; WhisperKit exports `WordTiming`, `TranscriptionSegment`. StenoSpeech never `import`s both frameworks in one file and always module-qualifies StenoCore types where a collision exists. |

## Public API

```swift
// Engines
public enum SpeechEngineID: String, Sendable, CaseIterable {
    case parakeetV3 = "parakeet-v3", parakeetUltra = "parakeet-ultra"
    case parakeetDE = "parakeet-de", whisperKitLargeV3Turbo = "whisperkit-large-v3-turbo"
}
public actor ParakeetEngine: SpeechEngine {
    public init(variant: ParakeetVariant, models: ModelStore)         // .v3, .ultra, .custom(directory: URL, id: String)
}
public actor WhisperKitEngine: SpeechEngine {
    public init(variant: String = "openai_whisper-large-v3-v20240930_turbo", models: ModelStore)
}
public enum SpeechEngineFactory {
    public static func make(_ id: SpeechEngineID, models: ModelStore) throws -> any SpeechEngine
}

// Models
public struct ModelDownloadProgress: Sendable, Equatable {
    public let asset: ModelAsset; public let fractionCompleted: Double; public let phase: String
}
public enum ModelAsset: String, Sendable, CaseIterable {
    case parakeetV3, parakeetUltra, parakeetDE, whisperLargeV3Turbo, offlineDiarizer
    public var approximateBytes: Int64 { get }
    public var sourceRepo: String { get }
}
public actor ModelStore {
    public init(directory: URL)                                       // default: Application Support/Steno/Models
    public func isInstalled(_ asset: ModelAsset) -> Bool
    public func ensure(_ asset: ModelAsset) -> AsyncThrowingStream<ModelDownloadProgress, Error>
    public func remove(_ asset: ModelAsset) throws
    public func directory(for asset: ModelAsset) -> URL
}

// Diarization and speakers
public actor FluidDiarizer: Diarizer {
    public init(models: ModelStore, config: FluidDiarizerConfig = .default)
}
public struct FluidDiarizerConfig: Sendable {
    public var clusteringThreshold: Double = 0.6                      // passed to OfflineDiarizerConfig.clustering.threshold
    public var minSpeakers: Int?; public var maxSpeakers: Int?
}
public struct SampleClipPicker: Sendable {
    public static func pick(for cluster: StenoCore.DiarizationCluster, chunks: [ClusterChunk],
                            targetSeconds: TimeInterval = 10, minimumSeconds: TimeInterval = 3) -> (range: ClosedRange<TimeInterval>, lowConfidence: Bool)
}
public actor CosineSpeakerMemory: SpeakerMemory {
    public init(store: any PersonStore, threshold: Float = 0.60, margin: Float = 0.05, maxSamples: Int = 50)
    public func match(_ embedding: [Float]) async throws -> (Person, Float)?
    public func enroll(_ embedding: [Float], as person: Person) async throws
    public func merge(_ source: Person, into target: Person) async throws -> Person   // added member
    public func rankedCandidates(_ embedding: [Float], limit: Int) async throws -> [(Person, Float)]  // added, for the review sheet
}
public enum Embeddings {
    public static func normalised(_ v: [Float]) -> [Float]
    public static func cosine(_ a: [Float], _ b: [Float]) -> Float     // vDSP, both inputs must be unit length
}

// Segmentation and language
public struct TranscriptSegmenter: Sendable {
    public init(maxSegmentSeconds: TimeInterval = 30, splitGapSeconds: TimeInterval = 0.7)
    public func segments(fromWords words: [TimedWord]) -> [RawSegment]
}
public struct LanguageTagger: Sendable {
    public init(candidates: Set<Locale.Language> = [de, en])
    public func tag(_ segments: [RawSegment]) -> [RawSegment]          // fills RawSegment.language
    public func dominantLanguage(of segments: [RawSegment]) -> Locale.Language?  // duration-weighted
}

// Bake-off
public struct BakeoffReport: Codable, Sendable { public let rows: [BakeoffRow]; public func markdown() -> String }
public struct BakeoffRow: Codable, Sendable {
    public let file: String, engine: SpeechEngineID, audioSeconds: Double, wallSeconds: Double
    public var realtimeFactor: Double { audioSeconds / wallSeconds }
    public let wer: Double?, languageFlips: Int, dominantLanguage: String?, cleanedWER: Double?
}
public enum WordErrorRate {
    public static func compute(reference: String, hypothesis: String, foldUmlauts: Bool = true) -> Double
}
```

## Files

```
Sources/StenoSpeech/
  Engines/SpeechEngineID.swift            ids, factory, supportedLanguages tables
  Engines/ParakeetEngine.swift            AsrManager wrapper, v3/ultra/custom directory
  Engines/WhisperKitEngine.swift          WhisperKit wrapper, language pinning, VAD chunking
  Engines/TimedWord.swift                 engine-neutral word timing; adapters from TokenTiming and WhisperKit words
  Segmentation/TranscriptSegmenter.swift  words -> RawSegments (gap, punctuation, max length)
  Segmentation/LanguageTagger.swift       NLLanguageRecognizer, constrained candidates, dominant language
  Models/ModelAsset.swift                 asset table: repo, files, bytes, licence
  Models/ModelStore.swift                 download with progress, install check, removal, offline flag
  Diarization/FluidDiarizer.swift         OfflineDiarizerManager wrapper, result mapping
  Diarization/ClusterEmbedding.swift      chunk-weighted mean, normalisation
  Diarization/SampleClipPicker.swift      ten-second clip selection
  Speakers/CosineSpeakerMemory.swift      match, enroll, merge, ranked candidates
  Speakers/Embeddings.swift               normalise, cosine (vDSP)
  Bakeoff/BakeoffRunner.swift             folder walk, per-engine run, timing, flips
  Bakeoff/WordErrorRate.swift             normalisation + Levenshtein over words
  Bakeoff/BakeoffReport.swift             Markdown and JSON rendering
Sources/steno/Commands/BakeoffCommand.swift   steno bakeoff <audio-dir> [--engines] [--reference-dir] [--cleanup] [--out]
Sources/steno/Commands/ModelsCommand.swift    steno models list|download|remove <asset>
Tests/StenoSpeechTests/                       one file per type above plus ModelIntegrationTests.swift
Tests/Fixtures/speech/de-short.wav            `say -v Anna` sentence, 16 kHz mono, < 6 s
Tests/Fixtures/speech/en-short.wav            `say -v Samantha` sentence, < 6 s
Tests/Fixtures/speech/denglish.wav            de sentence with two English product names, < 8 s
Tests/Fixtures/speech/two-speakers.wav        two `say` voices alternating, < 10 s
Tests/Fixtures/speech/*.ref.txt               reference transcripts for the WAVs above
```

Package.swift additions (StenoCore workstream owns the file, we send the
diff): `FluidAudio` from `0.17.3`, `argmax-oss-swift` from `1.1.0` product
`WhisperKit`. No other third-party packages; `NaturalLanguage` and
`Accelerate` are system frameworks.

## Verified third-party surface we build on

FluidAudio (`https://github.com/FluidInference/FluidAudio.git`, swift-tools 6.0, macOS 14+):
- `AsrModels.downloadAndLoad(to:configuration:version:encoderPrecision:encoderComputeUnits:progressHandler:)`,
  `AsrModels.download(to:force:version:encoderPrecision:progressHandler:) -> URL`,
  `AsrModels.load(from:configuration:version:encoderPrecision:encoderComputeUnits:progressHandler:)`,
  `AsrModels.loadLocal(from:version:configuration:encoderPrecision:encoderComputeUnits:)`,
  `AsrModels.modelsExist(at:)`, `AsrModels.defaultCacheDirectory(for:)`.
- `AsrModelVersion`: `.v2 .v3 .redux .ultra .tdtCtc110m .tdtJa`. `ParakeetEncoderPrecision`: `.int8` (Encoder.mlmodelc), `.int8V2` (Encoder_v2.mlmodelc), `.int4` (EncoderInt4.mlmodelc).
- `actor AsrManager`, `init(config: ASRConfig = .default, models: AsrModels? = nil)`, `loadModels(_:)`,
  `transcribe(_ samples: [Float], decoderState: inout TdtDecoderState, language: Language? = nil) async throws -> ASRResult`,
  same for `AVAudioPCMBuffer` and `URL`, plus `transcribeDiskBacked(_ url:decoderState:language:)`,
  `transcriptionProgressStream: AsyncThrowingStream<Double, Error>`. `TdtDecoderState()` default init: **unverified**.
  The README's `transcribe(samples)` without `decoderState`: **unverified**.
- `ASRResult { text, confidence, duration, processingTime, tokenTimings: [TokenTiming]?, rtfx }`,
  `TokenTiming { token, tokenId, startTime, endTime, confidence }`. FluidAudio's own `WordTiming` exists;
  the aggregation helper name is **unverified**, so we aggregate on SentencePiece `▁` ourselves.
- `Language` (script filter only): `.german = "de"`, `.english = "en"`, 28 cases, `Script.latin/.cyrillic`.
- `ProgressHandler = @Sendable (DownloadProgress) -> Void`, `DownloadProgress { fractionCompleted: Double, phase: DownloadPhase }`,
  `ModelHub.offlineMode: Bool` (the model card's `DownloadUtils.enforceOffline` is stale). Default directory
  `~/Library/Application Support/FluidAudio/Models/<repo>`.
- `final class OfflineDiarizerManager` (not Sendable, wrapped in our actor), `init(config: OfflineDiarizerConfig = .default)`,
  `prepareModels(directory:configuration:forceRedownload:)`, `initialize(models:)`,
  `process(audio: [Float], progressCallback: (@Sendable (Int, Int) -> Void)?) async throws -> DiarizationResult`, `process(_ url:)`.
- `OfflineDiarizerConfig`: `.segmentation` (10 s window, `stepRatio` 0.2), `.embedding` (`minSegmentDurationSeconds` 1.0, `excludeOverlap`),
  `.clustering` (`threshold` 0.6 euclidean on unit vectors, `warmStartFa` 0.07, `warmStartFb` 0.8, `minSpeakers`, `maxSpeakers`, `numSpeakers`, `constrainedAssignment`),
  `.vbx`, `exposeChunkEmbeddings` (default false).
- `DiarizationResult { segments: [TimedSpeakerSegment], speakerDatabase: [String: [Float]]?, chunkEmbeddings: [ChunkEmbedding]?, timings }`,
  `TimedSpeakerSegment { speakerId "S1"…, embedding (cluster centroid), startTimeSeconds: Float, endTimeSeconds: Float, qualityScore: Float }`,
  `ChunkEmbedding { speakerId, chunkIndex, speakerIndex, startTimeSeconds: Double, endTimeSeconds: Double, embedding256 (L2-normalised), rho128 }`.
- `OfflineDiarizerModels.load(from:configuration:progressHandler:)`; files `Segmentation.mlmodelc`, `FBank.mlmodelc`, `Embedding.mlmodelc`, `PldaRho.mlmodelc`, `plda-parameters.json`.
- `SpeakerUtilities.cosineDistance(_:_:)` public. `SpeakerManager` is streaming-only and documented as unsupported with the offline pipeline; not used.
- `AudioConverter().resampleAudioFile(_ url: URL) -> [Float]`, `.resample(_:from:)` (bake-off decode fallback if StenoCore's decoder is late).

WhisperKit (`https://github.com/argmaxinc/WhisperKit.git`, `Package@swift-6.2.swift` with `swiftLanguageModes: [.v6]`, macOS 13+):
- `WhisperKit.download(variant:downloadBase:useBackgroundSession:from: "argmaxinc/whisperkit-coreml":token:endpoint:progressCallback:) async throws -> URL`,
  `ProgressCallback = @Sendable (Progress) -> Void`. Subdirectory layout under `downloadBase`: **unverified**.
- `WhisperKitConfig` fields `model`, `modelRepo`, `modelFolder`, `downloadBase`, `computeOptions: ModelComputeOptions`
  (`melCompute .cpuAndGPU`, `audioEncoderCompute .cpuAndNeuralEngine`, `textDecoderCompute .cpuAndNeuralEngine`). `prewarm`/`load`/`download` init flags: **unverified**.
- `transcribe(audioArray: [Float], decodeOptions: DecodingOptions?, callback:, segmentCallback:) async throws -> [TranscriptionResult]`,
  `detectLangauge(audioArray:) -> (language: String, langProbs: [String: Float])` (typo is the real API name).
- `DecodingOptions`: `language: String?`, `detectLanguage`, `usePrefillPrompt`, `wordTimestamps`, `chunkingStrategy: .none/.vad`,
  `concurrentWorkerCount`, `noSpeechThreshold` 0.6, `logProbThreshold` -1.0, `compressionRatioThreshold` 2.4, `temperatureFallbackCount` 5, `clipTimestamps`.
- `TranscriptionResult { text, segments: [TranscriptionSegment], language: String, timings }`,
  `TranscriptionSegment { start: Float, end: Float, text, avgLogprob, compressionRatio, noSpeechProb, words: [WordTiming]? }`,
  `WordTiming { word, tokens, start, end, probability }`.

## Models: sizes and sources (measured from the HF trees, 2026-09-25)

| Asset | Repo | Files we download | On disk | Licence |
|---|---|---|---|---|
| Parakeet TDT 0.6B v3, int8 | `FluidInference/parakeet-tdt-0.6b-v3-coreml` | Preprocessor 0.5 MB, Encoder 446 MB, Decoder 24 MB, JointDecisionv3 13 MB, vocab | ~485 MB (int4 encoder 299 MB, int8-v2 595 MB) | CC-BY-4.0 (NVIDIA) |
| Parakeet Ultra, int8 | `FluidInference/parakeet-ultra-coreml` | same layout | ~632 MB | CC-BY-4.0 |
| Parakeet German fine-tune, fp16 | `ValentinWeyer/parakeet-primeline-de-coreml` | Encoder 1187 MB, Decoder, JointDecisionv3, Preprocessor, vocab | ~1.22 GB | CC-BY-4.0 |
| Offline diarizer | `FluidInference/speaker-diarization-coreml` | Segmentation 6 MB, FBank 1.8 MB, Embedding 13.5 MB, PldaRho 0.2 MB, plda-parameters.json | ~22 MB (repo total 129 MB; whether FluidAudio fetches only the offline files: **unverified**) | Apache-2.0 wrapper, pyannote/WeSpeaker upstream |
| Whisper large-v3 turbo | `argmaxinc/whisperkit-coreml/openai_whisper-large-v3-v20240930_turbo` | full variant dir | ~1.64 GB (`_632MB` compressed variant: 646 MB) | MIT (WhisperKit), OpenAI weights |

Default install: Parakeet v3 + diarizer, about 0.5 GB. Everything else on demand.
Published performance (FluidAudio docs, M4 Pro): Parakeet v3 ~190x realtime, 2.5 % WER en, 14.7 % mean over 25 languages;
Ultra 11.7 % mean on FLEURS-24; offline diarization ~122x realtime, ~15 % DER. German fine-tune card: 5.27 % mean WER on
Tuda-De/MLS/CV19 (umlaut-folded) with its own harness; not comparable to the FluidAudio numbers.

German model caveats (from the card, to be reconfirmed in spike B): directory must be named exactly
`parakeet-tdt-0.6b-v3`; load with `version: .v3, encoderPrecision: .int8` even though weights are fp16 (name selector only);
set `ModelHub.offlineMode = true` around the load, otherwise a failed load deletes the folder and re-downloads the official v3.
We keep it under `Models/fluidaudio-de/parakeet-tdt-0.6b-v3/` so it cannot shadow the real v3.

## Language strategy (German, English, Denglish)

1. Parakeet engines: run once, `language: nil`. The model decides per utterance; code switching inside a sentence comes out
   mixed, which is what we want for Denglish. Failure mode: short German utterances decoded as English (the reason the German
   fine-tune exists). Mitigation is downstream: `LanguageTagger` per segment, LLM cleanup pass, bake-off measures flips.
2. WhisperKit: decide the meeting language first. If the pipeline passes `hint`, pin `language` to it. Otherwise run
   `detectLangauge(audioArray:)` on the three most speech-dense 30 s windows (energy-ranked, no VAD model needed), take the
   majority, pin it. Never run Whisper unpinned on Denglish: per-window auto-detect flips whole windows and occasionally translates.
   `wordTimestamps: true`, `chunkingStrategy: .vad`, `task: .transcribe`.
3. `RawSegment.language` is always filled by `LanguageTagger` (`NLLanguageRecognizer` with `languageConstraints = [de, en]`
   and `languageHints` weighted toward the dominant language). Segments under four words inherit the previous segment's language.
4. `Meeting.language` = duration-weighted dominant language; the pipeline owns writing it.
5. `parakeet-de` is German-only: it will garble English passages. It is a bake-off entrant, and at most a per-meeting override,
   never auto-selected.

## Steps

0. **Spike A + B, half a day each.** A: Package resolving FluidAudio 0.17.3 and WhisperKit 1.1.0 together compiles a
   file that imports both, in Swift 6 language mode, on the `macos-15` CI job. B: German model loads via `AsrModels.load(from:)`
   with `ModelHub.offlineMode = true` and transcribes `de-short.wav`. Acceptance: CI green with both imports; B transcript
   contains the fixture's nouns. Go/no-go below.
1. **Module skeleton and `ModelStore`, one day.** `ModelAsset` table, download to `Steno/Models` with progress
   stream, install check by file presence, `remove`, `steno models list|download|remove`. Acceptance:
   `steno models download offlineDiarizer` prints progress to 100 % and `list` shows it installed; unit test uses a fake
   downloader against a temp directory.
2. **`TranscriptSegmenter` and `LanguageTagger`, one day.** Pure functions over `TimedWord`. Acceptance: unit tests with
   synthetic word lists cover gap split, punctuation split, 30 s cap, short-segment inheritance, dominant language.
3. **`ParakeetEngine`, one day.** Wrap `AsrManager`, aggregate `TokenTiming` to words, segment, tag. `supportedLanguages`
   = FluidAudio's 25. `prepare()` = `ModelStore.ensure` then `loadModels`. Acceptance: opt-in integration test transcribes
   `de-short.wav` and `en-short.wav`; segments have monotonic non-overlapping times; RTF under 1/20 on the CI runner.
4. **`WhisperKitEngine`, one day.** Language decision as above, map segments and words. Acceptance: opt-in integration
   test on `denglish.wav` with `hint: de` yields one language, no segment with `noSpeechProb > 0.6` kept; unit test for the
   window-ranking function on synthetic energy profiles.
5. **`FluidDiarizer`, one day.** Wrap `OfflineDiarizerManager`, map to `StenoCore.DiarizationResult`, compute
   normalised cluster embeddings from `chunkEmbeddings`, attach `SampleClipPicker` result. Acceptance: mapping unit test
   builds a `FluidAudio.DiarizationResult` by hand and checks unit-length embeddings, range merging and clip choice;
   opt-in integration test on `two-speakers.wav` yields exactly two clusters.
6. **`CosineSpeakerMemory`, one day.** Match with threshold and margin, enroll running mean, merge, ranked candidates,
   over a fake `PersonStore`. Acceptance: unit tests for near-duplicate match, below-threshold miss, margin rejection,
   running-mean cap, merge weighting and repointing. Calibration note recorded from the two `say` voices plus the
   bake-off meetings (similarity distributions same-speaker vs different-speaker).
7. **Bake-off harness, one day.** `BakeoffRunner` walks `<audio-dir>` for `wav|m4a|mp3|caf`, decodes via StenoCore's
   decoder, runs each requested engine, times wall clock, computes WER against `<name>.ref.txt` when present (lower-case,
   punctuation stripped, optional umlaut folding), counts language flips (adjacent segments with different `language`),
   optionally runs the StenoLLM cleanup and reports `cleanedWER`. Writes `report.md`, `report.json` and per-file
   `<name>.<engine>.json`. Acceptance: unit tests for WER (known pairs), flip count, Markdown rendering; running it on
   `Tests/Fixtures/speech` with `--engines parakeet-v3` under `STENO_MODEL_TESTS=1` produces a table with four rows.
8. **Wiring and settings, half a day.** `SpeechEngineFactory`, settings keys `speech.engine`, `speech.speakerThreshold`,
   `speech.diarizationThreshold`, `speech.modelsDirectory`; PR with CI URL and test count. Acceptance: `steno process`
   (core) runs end to end with `parakeet-v3` on `two-speakers.wav`.
9. **Bake-off on real meetings, manual, one day.** Three real Denglish meetings kept outside the repository, references
   typed by hand for two five-minute excerpts each. Engines: `parakeet-v3`, `parakeet-ultra`, `whisperkit-large-v3-turbo`,
   `parakeet-de`, each with and without cleanup. Result and chosen default written to
   `.plans/2026-xx-xx-stt-bakeoff-result.md`. Acceptance: that plan exists and names the default engine and thresholds.

## Tests

- Unit (no network, no models): `TranscriptSegmenterTests`, `LanguageTaggerTests`, `SampleClipPickerTests`,
  `ClusterEmbeddingTests`, `CosineSpeakerMemoryTests` (fake `PersonStore`), `EmbeddingsTests`, `WordErrorRateTests`,
  `BakeoffReportTests`, `ModelStoreTests` (fake downloader), `FluidDiarizerMappingTests`, `WhisperWindowRankingTests`.
  A `FakeSpeechEngine` and `FakeDiarizer` conforming to the StenoCore protocols live in `Tests/StenoSpeechTests/Fakes/` and
  are offered to the pipeline tests in StenoCore.
- Integration, opt-in with `STENO_MODEL_TESTS=1`: `ModelIntegrationTests` downloads Parakeet v3 and the offline diarizer
  into a temp directory, transcribes the four fixtures with both engines (WhisperKit only if `STENO_MODEL_TESTS_WHISPER=1`,
  1.6 GB), diarizes `two-speakers.wav`, and runs the bake-off over the fixture folder. Skipped, not failed, when the
  variable is unset; the PR report names it as skipped.
- Manual, one human on a Mac: step 9. Additionally, listen to each generated sample clip for the three meetings and confirm
  it contains only the named speaker; confirm that enrolling a speaker in meeting one auto-labels them in meeting two.

## Spikes with go/no-go

- **A. Both dependencies in one Swift 6 package.** Go: builds in language mode 6 with no `@preconcurrency import`
  beyond the two framework imports. No-go: WhisperKit fails to build under Swift 6 or its transitive dependencies clash with
  FluidAudio; then WhisperKit moves behind a separate SwiftPM target with `swiftLanguageModes: [.v5]` for that target only,
  and the plan notes it. Blocks steps 3 to 5.
- **B. German fine-tune loads from a custom directory.** Go: transcribes `de-short.wav` offline. No-go: FluidAudio 0.17
  rejects the layout; `parakeet-de` is dropped from the bake-off and the engine list, nothing else changes. Blocks only
  the `parakeet-de` entrant of step 9.
- **C. Cluster embeddings are usable across recordings.** During step 6: same `say` voice in two fixtures must score
  above 0.6, the two different voices below 0.5; on the bake-off meetings the same person across two meetings must beat
  every other speaker by the margin. Go: thresholds hold. No-go: cosine on WeSpeaker embeddings is not separable across
  microphones; fall back to enrolment-only matching (never auto-assign, always propose top three in the review sheet) and
  record it. Blocks the auto-assignment behaviour of step 8, not its code.

## Needs from other workstreams

- StenoCore: `SpeechEngine`, `Diarizer`, `SpeakerMemory` as in the program. `RawSegment` must carry
  `language: Locale.Language?` and `wordTimings: [WordTiming]?` (program says it does). `StenoCore.DiarizationResult` needs
  a cluster type; proposed `DiarizationCluster { label: String, ranges: [ClosedRange<TimeInterval>], embedding: [Float],
  confidence: Float, sampleClipRange: ClosedRange<TimeInterval>?, lowConfidence: Bool }` (**unverified**, to agree with core).
- StenoCore: a `PersonStore` protocol (**unverified name**) with `allPeople()`, `save(Person)`, `delete(Person)`,
  `repointSpeakers(from:to:)`; `CosineSpeakerMemory` is pure math over it.
- StenoCore: the pipeline's step-1 decoder as a callable type (**unverified name**, e.g. `AudioDecoder.load(_ url:) ->
  AudioBuffer16k`) for the bake-off; and `AudioBuffer16k` exposing `samples: [Float]`.
- StenoCore `steno` CLI: command registration point and the argument parser choice, so `BakeoffCommand` and `ModelsCommand`
  can plug in; settings keys listed in step 8.
- StenoLLM: the cleanup pass as a callable (`LanguageModel` plus its prompt assembly, **unverified name**) for `--cleanup`.
- macOS app: the speaker review sheet calls `rankedCandidates`, `enroll`, `merge`, and plays `Speaker.sampleClipRange`
  from the lane audio; the settings pane exposes engine choice, thresholds and `steno models` actions with progress.

## Requested changes to the program document

1. Allow two optional engine ids beyond the two named: `parakeet-ultra` and `parakeet-de`. Ultra is the same FluidAudio
   API and is FluidInference's current recommendation; it should be a legitimate bake-off winner without a program edit.
2. `SpeakerMemory` gains `merge(_ source: Person, into target: Person) async throws -> Person` and
   `rankedCandidates(_ embedding: [Float], limit: Int) async throws -> [(Person, Float)]`. Scope requires merge; the review
   sheet needs ranked candidates.
3. Add a `PersonStore` protocol to StenoCore (or state that the `SpeakerMemory` implementation lives in StenoCore next to
   GRDB). Today the program places speaker matching in StenoSpeech but gives it no storage seam.
4. Fix the shape of `StenoCore.DiarizationResult` (cluster type above) so StenoSpeech and the pipeline agree before code.
5. Note that `AudioBuffer16k` for a two-hour lane is ~460 MB of Float32; acceptable for v1, but the pipeline should
   process lanes sequentially, not hold both. Optionally add a default-implemented `transcribe(fileURL:hint:)` later so
   engines can use disk-backed paths (`transcribeDiskBacked`, `process(_ url:)`).
