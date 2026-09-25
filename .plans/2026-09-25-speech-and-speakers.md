# Speech and speakers: StenoSpeech

Status: workstream plan, 2026-09-25, reconciled and then revised the same day after the three reviews (program
review application log). Binding program: [`2026-09-25-v1-program.md`](2026-09-25-v1-program.md). Scope authority:
[`2026-09-24-initial-scope.md`](2026-09-24-initial-scope.md). Owns `Sources/StenoSpeech`, `Tests/StenoSpeechTests`,
the `steno dev bakeoff` and `steno dev models` tools, and one line in core's `Wiring.swift` (the `--engine` flag).

API names below were checked against FluidAudio `v0.17.3` (released 2026-09-24; 0.17.4 followed on 2026-09-25) and
WhisperKit `v1.1.0` (released 2026-08-06; repository now named `argmax-oss-swift`, old URL redirects). Anything marked
**unverified** was seen only in a README, model card or doc page, or not seen at all, and must be confirmed in step 0.

## Goal

Turn a 16 kHz mono lane into timed, language-tagged `RawSegment`s with two interchangeable on-device engines, turn
the "them" lane (or the whole mix) into speaker clusters with L2-normalised 256-dim embeddings and a ten-second
sample clip range each, rank those clusters against remembered people, and give the project a repeatable bake-off so
the default engine is chosen from measurements on real German/English/Denglish meetings, not from a README.

## Non-goals

- Live or streaming transcription and streaming diarization (`SlidingWindowAsrManager`, `DiarizerManager`,
  `LSEENDDiarizer`, Sortformer are not wrapped).
- Any model conversion or fine-tuning; we consume published CoreML repos.
- Speaker name inference from context (StenoLLM), speaker review UI (app).
- Custom vocabulary boosting (FluidAudio CTC rescoring); revisit after v1.
- Apple `SpeechAnalyzer`, whisper.cpp, cloud STT.
- Audio decode and resampling to 16 kHz (StenoCore pipeline, `AudioDecoder`).
- Persisting `Person` rows, confirming speakers, or merging people or clusters (`MeetingStore.confirm`,
  `mergePersons`, `mergeSpeakers` in StenoCore); we only compute, rank and average.
- Fakes of core protocols: `FakeSpeechEngine` and `FakeDiarizer` come from `StenoCore/Testing/`. This module's own
  `Testing/` holds only `FakeModelDownloader`.

## Decisions

| Topic | Decision |
|---|---|
| Default engine candidate | Parakeet TDT v3 via FluidAudio (`parakeet-v3`). Final default set by the bake-off (step 9) and recorded in a follow-up plan. |
| Second engine | WhisperKit `openai_whisper-large-v3-v20240930_turbo` (`whisperkit-large-v3-turbo`). |
| Optional engines | `parakeet-ultra` (same FluidAudio API, `AsrModelVersion.ultra`); `parakeet-de` (German fine-tune, custom directory, German-only). Bake-off entrants sharing the `ParakeetEngine` implementation; not user-selectable in the app until the bake-off result plan promotes one. |
| Actor conformance | `ParakeetEngine` and `WhisperKitEngine` are actors; `id` and `supportedLanguages` are `nonisolated public let`, because Swift 6 rejects an actor-isolated property satisfying a nonisolated protocol requirement. |
| Language handling | No engine can be forced per segment. Parakeet has no language control at all (its `Language` parameter only filters Latin vs Cyrillic script; `de` and `en` are both Latin). Whisper can be pinned per call, not per segment. So: engines run unpinned (Parakeet) or pinned to the `hint` (Whisper); every `RawSegment` gets `language` from `NLLanguageRecognizer` constrained to `{de, en}`; the core pipeline elects `Meeting.language` from the tagged segments and passes the first lane's result as the second lane's `hint`; the LLM cleanup pass fixes the rest. |
| Diarizer | FluidAudio `OfflineDiarizerManager`, pyannote community-1 offline pipeline, community defaults, `exposeChunkEmbeddings = true`. |
| Cluster embedding | Mean of the cluster's `ChunkEmbedding.embedding256` values weighted by chunk duration, then L2-normalised. Not `speakerDatabase`/`TimedSpeakerSegment.embedding`: those are VBx centroids, an un-normalised mean of unit vectors, so cosine against stored people would be biased by cluster purity. |
| Speaker match | Cosine similarity on unit vectors. `CosineSpeakerMemory` implements `candidates(for:limit:)` and `enroll`; the threshold and margin logic is the program's provided `SpeakerMemory.match(_:threshold:margin:)`. Threshold `0.60` is `Settings.speakerMatchThreshold`; the margin `0.05` over the runner-up is the program constant. Calibrated in step 6. |
| Enrolment | Running mean: `e' = normalise((e * n + x) / (n + 1))`, `n` capped at 50 so a voice can drift. |
| Merges | Not here. Person merge is `MeetingStore.mergePersons(keep:remove:)` (weighted mean in the store); in-meeting cluster merge is `MeetingStore.mergeSpeakers`. |
| Sample clip | `FluidDiarizer` fills `SpeakerCluster.sampleClipRange`: longest contiguous single-speaker segment of the cluster, capped to 10 s centred on the highest-`qualityScore` chunk. `SpeakerCluster.clusterConfidence` is the mean chunk quality, halved when the longest segment is under 3 s. Core copies both onto `Speaker` and writes the clip file. |
| Model location | `~/Library/Application Support/Steno/Models/`. FluidAudio repos as `fluidaudio/<repo-last-path-component>` (the directory name must match the HF repo name, see German model caveat). WhisperKit under `whisperkit/` via `downloadBase`. Downloads go through `ModelDownloading` so `ModelStore` is unit-tested without network. |
| Type collisions | FluidAudio exports `DiarizationResult`, `Speaker`, `Language`, `WordTiming`; WhisperKit exports `WordTiming`, `TranscriptionSegment`. StenoSpeech never `import`s both frameworks in one file and always module-qualifies StenoCore types where a collision exists. FluidAudio's result is mapped first into StenoSpeech's own `ClusterChunk` values, and every mapping test starts from `ClusterChunk`, because FluidAudio's memberwise initialisers may be internal. |

## Public API

```swift
// Engines
public enum SpeechEngineID: String, Sendable, CaseIterable {
    case parakeetV3 = "parakeet-v3", parakeetUltra = "parakeet-ultra"
    case parakeetDE = "parakeet-de", whisperKitLargeV3Turbo = "whisperkit-large-v3-turbo"
}
public actor ParakeetEngine: SpeechEngine {
    public nonisolated let id: String; public nonisolated let supportedLanguages: Set<Locale.Language>
    public init(variant: ParakeetVariant, models: ModelStore)         // .v3, .ultra, .custom(directory: URL, id: String)
}
public actor WhisperKitEngine: SpeechEngine {
    public nonisolated let id: String; public nonisolated let supportedLanguages: Set<Locale.Language>
    public init(variant: String = "openai_whisper-large-v3-v20240930_turbo", models: ModelStore)
}
public func makeSpeechEngine(_ id: SpeechEngineID, models: ModelStore) throws -> any SpeechEngine   // the app injects this as a closure

// Models
public struct ModelDownloadProgress: Sendable, Equatable { public let asset: ModelAsset; public let fractionCompleted: Double; public let phase: String }
public enum ModelAsset: String, Sendable, CaseIterable {
    case parakeetV3, parakeetUltra, parakeetDE, whisperLargeV3Turbo, offlineDiarizer
    public var approximateBytes: Int64 { get }
    public var sourceRepo: String { get }
}
public protocol ModelDownloading: Sendable {                                   // seam: FluidAudio / WhisperKit downloaders behind one call
    func download(_ asset: ModelAsset, into directory: URL, progress: @Sendable (Double, String) -> Void) async throws
}
public actor ModelStore {
    public init(directory: URL, downloader: any ModelDownloading = LiveModelDownloader())   // default directory: Application Support/Steno/Models
    public func isInstalled(_ asset: ModelAsset) -> Bool
    public func ensure(_ asset: ModelAsset) -> AsyncThrowingStream<ModelDownloadProgress, Error>
    public func remove(_ asset: ModelAsset) throws
    public func directory(for asset: ModelAsset) -> URL
}

// Diarization and speakers
public actor FluidDiarizer: Diarizer { public init(models: ModelStore, config: FluidDiarizerConfig = .default) }
public struct FluidDiarizerConfig: Sendable {
    public var clusteringThreshold: Double = 0.6                      // passed to OfflineDiarizerConfig.clustering.threshold
    public var minSpeakers: Int?; public var maxSpeakers: Int?
}
struct ClusterChunk: Sendable, Equatable { let speakerLabel: String; let start, end: TimeInterval; let embedding: [Float]; let quality: Float }   // internal mapping unit
struct SampleClipPicker: Sendable {                                   // internal, used by FluidDiarizer
    static func pick(ranges: [ClosedRange<TimeInterval>], chunks: [ClusterChunk],
                     targetSeconds: TimeInterval = 10, minimumSeconds: TimeInterval = 3) -> (range: ClosedRange<TimeInterval>, clusterConfidence: Float)
}
public actor CosineSpeakerMemory: SpeakerMemory {                     // candidates(for:limit:) and enroll(_:as:); pure math over MeetingStore
    public init(store: MeetingStore, maxSamples: Int = 50)
}
public enum Embeddings {
    public static func normalised(_ v: [Float]) -> [Float]
    public static func cosine(_ a: [Float], _ b: [Float]) -> Float     // vDSP, both inputs must be unit length
}

// Segmentation and language
public struct TokenAggregator: Sendable { public func words(from tokens: [TimedToken]) -> [TimedWord] }   // SentencePiece ▁ boundaries, punctuation glued, empty tokens dropped
public struct TranscriptSegmenter: Sendable {
    public init(maxSegmentSeconds: TimeInterval = 30, splitGapSeconds: TimeInterval = 0.7)
    public func segments(fromWords words: [TimedWord]) -> [RawSegment]
}
public struct LanguageTagger: Sendable {
    public init(candidates: Set<Locale.Language> = [de, en])
    public func tag(_ segments: [RawSegment]) -> [RawSegment]          // fills RawSegment.language
    public func dominantLanguage(of segments: [RawSegment]) -> Locale.Language?  // duration-weighted; used by WhisperKitEngine and the bake-off
}

// Bake-off
public struct BakeoffReport: Codable, Sendable { public let rows: [BakeoffRow]; public func markdown() -> String }
public struct BakeoffRow: Codable, Sendable {
    public let file: String, engine: SpeechEngineID, audioSeconds: Double, wallSeconds: Double
    public var realtimeFactor: Double { audioSeconds / wallSeconds }
    public let wer: Double?, languageFlips: Int, dominantLanguage: String?, cleanedWER: Double?
}
public enum WordErrorRate { public static func compute(reference: String, hypothesis: String, foldUmlauts: Bool = true) -> Double }
// Testing/: FakeModelDownloader (writes marker files, scripted progress and failures)
```

## Files

```
Sources/StenoSpeech/
  Engines/SpeechEngineID.swift            ids, makeSpeechEngine, supportedLanguages tables
  Engines/ParakeetEngine.swift            AsrManager wrapper, v3/ultra/custom directory
  Engines/WhisperKitEngine.swift          WhisperKit wrapper, language pinning, VAD chunking
  Engines/TimedWord.swift                 TimedToken, TimedWord; adapters from TokenTiming and WhisperKit words
  Segmentation/TokenAggregator.swift      tokens -> words on ▁ boundaries
  Segmentation/TranscriptSegmenter.swift  words -> RawSegments (gap, punctuation, max length)
  Segmentation/LanguageTagger.swift       NLLanguageRecognizer, constrained candidates, dominant language
  Models/ModelAsset.swift                 asset table: repo, files, bytes, licence
  Models/ModelDownloading.swift           protocol; LiveModelDownloader over FluidAudio and WhisperKit download APIs
  Models/ModelStore.swift                 install check, progress stream, removal, offline flag
  Diarization/FluidDiarizer.swift         OfflineDiarizerManager wrapper, FluidAudio result -> [ClusterChunk] -> StenoCore.DiarizationResult
  Diarization/ClusterEmbedding.swift      chunk-weighted mean, normalisation
  Diarization/SampleClipPicker.swift      ten-second clip selection and clusterConfidence (internal)
  Speakers/CosineSpeakerMemory.swift      candidates, enroll
  Speakers/Embeddings.swift               normalise, cosine (vDSP)
  Bakeoff/BakeoffRunner.swift             folder walk, per-engine run, timing, flips
  Bakeoff/WordErrorRate.swift             normalisation + Levenshtein over words
  Bakeoff/BakeoffReport.swift             Markdown and JSON rendering
  Testing/FakeModelDownloader.swift
Sources/steno/Commands/DevBakeoffCommand.swift   steno dev bakeoff <audio-dir> [--engines] [--reference-dir] [--cleanup] [--out]
Sources/steno/Commands/DevModelsCommand.swift    steno dev models list|download|remove <asset>
Sources/steno/Wiring.swift                       (core-owned) gains `--engine <id>`; the one cross-ownership edit, recorded in both plans
Tests/StenoSpeechTests/                          one file per type above plus TokenAggregationTests.swift and ModelIntegrationTests.swift
Tests/Fixtures/speech/de-short.wav, de-short-2.wav   `say -v Anna`, two different sentences, 16 kHz mono, < 6 s each (spike C needs the pair)
Tests/Fixtures/speech/en-short.wav               `say -v Samantha` sentence, < 6 s
Tests/Fixtures/speech/denglish.wav               de sentence with two English product names, < 8 s
Tests/Fixtures/speech/two-speakers.wav           two `say` voices alternating, < 10 s
Tests/Fixtures/speech/*.ref.txt                  reference transcripts for the WAVs above
```

`say` output changes with macOS releases and voices, so these fixtures are generated once, committed with their
`.ref.txt`, listed in `Tests/Fixtures/MANIFEST.sha256` and never regenerated; `Tests/Fixtures/README.md` says so.

Package.swift additions in this workstream's PR (program rule): `FluidAudio` from `0.17.3`, `argmax-oss-swift` from
`1.1.0` product `WhisperKit`. No other third-party packages; `NaturalLanguage` and `Accelerate` are system frameworks.
FluidAudio ships one binary target (`NemoTextProcessing` xcframework) plus C targets and `cxxLanguageStandard:
.cxx17`; the macOS plan's archive signs and notarises the binary. WhisperKit publishes a tools-5.10 manifest
(`swiftLanguageVersions: [.v5]`) and a `Package@swift-6.2.swift` (`swiftLanguageModes: [.v6]`); on the runner's Xcode
16.4 SwiftPM selects the 5.10 manifest, and a dependency's language mode does not constrain Steno's Swift 6 mode
(verified: both manifests, correctness review).

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
  the aggregation helper name is **unverified**, so `TokenAggregator` aggregates on SentencePiece `▁` ourselves.
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
- `AudioConverter().resampleAudioFile(_ url: URL) -> [Float]`, `.resample(_:from:)` (bake-off decode fallback if StenoAudio's `AVFoundationAudioCodec` is late).

WhisperKit (`https://github.com/argmaxinc/WhisperKit.git`, tools 5.10 plus `Package@swift-6.2.swift`, macOS 13+):
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
4. `Meeting.language` is elected by the core pipeline (stage 1) from `RawSegment.language` by summed duration; this module only tags.
5. `parakeet-de` is German-only: it will garble English passages. It is a bake-off entrant, and at most a per-meeting override,
   never auto-selected.

## Steps

0. **Spike A + B, half a day each.** A: Package resolving FluidAudio 0.17.3 and WhisperKit 1.1.0 together compiles a file that
   imports both, in Swift 6 language mode, on the `macos-15` CI job. B: German model loads via `AsrModels.load(from:)` with
   `ModelHub.offlineMode = true` and transcribes `de-short.wav`. Acceptance A `[ci]`: CI green with both imports. Acceptance B
   `[opt-in: STENO_MODEL_TESTS]`: transcript contains the fixture's nouns. Go/no-go below.
1. **Module skeleton, `ModelDownloading`, `ModelStore`, one day.** `ModelAsset` table, download to `Steno/Models` with progress
   stream, install check by file presence, `remove`, `steno dev models list|download|remove`. Acceptance `[ci]`: `ModelStoreTests`
   with `FakeModelDownloader` against a temp directory cover ensure-once, progress forwarding, failure surfacing and removal.
   `[manual]`: `steno dev models download offlineDiarizer` prints progress to 100 % and `list` shows it installed.
2. **`TokenAggregator`, `TranscriptSegmenter`, `LanguageTagger`, one day.** Pure functions. Acceptance `[ci]`:
   `TokenAggregationTests` (a word over three tokens, a punctuation token, a leading `▁` only, an empty token); segmenter tests with
   synthetic word lists cover gap split, punctuation split, 30 s cap, short-segment inheritance, dominant language.
3. **`ParakeetEngine`, one day.** Wrap `AsrManager`, aggregate tokens, segment, tag. `supportedLanguages` = FluidAudio's 25.
   `prepare()` = `ModelStore.ensure` then `loadModels`. Acceptance `[opt-in: STENO_MODEL_TESTS]`: integration test transcribes
   `de-short.wav` and `en-short.wav`; segments have monotonic non-overlapping times; RTF is reported in the test log, not asserted.
4. **`WhisperKitEngine`, one day.** Language decision as above, map segments and words. Acceptance `[opt-in: STENO_MODEL_TESTS]`
   (plus `STENO_MODEL_TESTS_WHISPER=1`): `denglish.wav` with `hint: de` yields one language, no segment with `noSpeechProb > 0.6`
   kept. `[ci]`: `WhisperWindowRankingTests` on synthetic energy profiles.
5. **`FluidDiarizer`, one day.** Wrap `OfflineDiarizerManager`, map to `[ClusterChunk]` then `StenoCore.DiarizationResult`,
   compute normalised cluster embeddings from `chunkEmbeddings`, fill `sampleClipRange` and `clusterConfidence` through
   `SampleClipPicker`. Acceptance `[ci]`: `FluidDiarizerMappingTests` build `[ClusterChunk]` by hand and check unit-length
   embeddings, range merging, clip choice and the under-3 s penalty. `[opt-in: STENO_MODEL_TESTS]`: `two-speakers.wav` yields
   exactly two clusters, each with a clip range inside its own ranges.
6. **`CosineSpeakerMemory`, one day.** `candidates(for:limit:)` ranked by cosine, `enroll` running mean, over
   `MeetingStore.inMemory()`. Acceptance `[ci]`: unit tests for ranking order, the program's provided `match` (near-duplicate
   hit, below-threshold miss, margin rejection), running-mean cap. Calibration note `[manual]` recorded from the two `say`
   voices plus the bake-off meetings (similarity distributions same-speaker vs different-speaker).
7. **Bake-off harness, one day.** `BakeoffRunner` walks `<audio-dir>` for `wav|m4a|mp3|caf`, decodes through an injected
   `AudioDecoder` (StenoAudio's codec in the CLI, core's WAV decoder in tests), runs each requested engine, times wall clock,
   computes WER against `<name>.ref.txt` when present (lower-case, punctuation stripped, optional umlaut folding), counts
   language flips (adjacent segments with different `language`), optionally runs an injected `TranscriptCleaner` and reports
   `cleanedWER`. Writes `report.md`, `report.json` and per-file `<name>.<engine>.json`. Acceptance `[ci]`: unit tests for WER
   (known pairs), flip count, Markdown rendering. `[opt-in: STENO_MODEL_TESTS]`: the fixture folder with `--engines parakeet-v3`
   produces a table with five rows.
8. **Wiring, settings, end-to-end, half a day.** `makeSpeechEngine` reads `Settings.speechEngineID`; `ModelStore` takes
   `Settings.modelsDirectory` (nil = default); core's `Wiring.swift` gains `--engine <id>` (ownership exception, one line, both
   plans record it); `Tests/StenoEndToEndTests` swaps `InMemorySpeakerMemory` for `CosineSpeakerMemory` over the test store
   (engines and diarizer stay fakes: no models on CI). Acceptance `[ci]`: the end-to-end test passes with a suggested match for
   a pre-enrolled person. `[opt-in: STENO_MODEL_TESTS]`: `steno process two-speakers.wav --engine parakeet-v3` runs end to end.
9. **Bake-off on real meetings, `[manual]`, one day.** Three real Denglish meetings kept outside the repository, references typed
   by hand for two five-minute excerpts each. Engines: `parakeet-v3`, `parakeet-ultra`, `whisperkit-large-v3-turbo`, `parakeet-de`,
   each with and without cleanup. Result and chosen default written to `.plans/2026-xx-xx-stt-bakeoff-result.md`. Acceptance:
   that plan exists and names the default engine and threshold.

## Tests

- Unit `[ci]` (no network, no models): `TokenAggregationTests`, `TranscriptSegmenterTests`, `LanguageTaggerTests`,
  `SampleClipPickerTests`, `ClusterEmbeddingTests`, `CosineSpeakerMemoryTests` (in-memory `MeetingStore`), `EmbeddingsTests`,
  `WordErrorRateTests`, `BakeoffReportTests`, `ModelStoreTests` (`FakeModelDownloader`), `FluidDiarizerMappingTests`,
  `WhisperWindowRankingTests`. Suites touching `ModelHub.offlineMode` or the environment are `.serialized`.
- Integration `[opt-in: STENO_MODEL_TESTS]`: `ModelIntegrationTests` downloads Parakeet v3 and the offline diarizer into a temp
  directory, transcribes the fixtures with both engines (WhisperKit only if `STENO_MODEL_TESTS_WHISPER=1`, 1.6 GB), diarizes
  `two-speakers.wav`, and runs the bake-off over the fixture folder. Skipped, not failed, when the variable is unset; the skip
  message names the variable.
- Manual `[manual]`, one human on a Mac: step 9. Additionally, listen to each generated sample clip for the three meetings and
  confirm it contains only the named speaker; confirm that confirming a speaker in meeting one suggests them in meeting two.

Reviewer trap: a `Package.resolved` bump of a model package without a sentence in the PR; a skip message that does not name
`STENO_MODEL_TESTS`; an RTF assertion (report only).

## Spikes with go/no-go

- **A. Both dependencies in one Swift 6 package.** Go: builds in language mode 6 with no `@preconcurrency import` beyond the
  two framework imports. The correctness review verified that "WhisperKit fails to build under Swift 6" cannot be the reason
  (SwiftPM picks its 5.10 manifest on Xcode 16.4); the remaining risk is a transitive clash with FluidAudio. No-go: WhisperKit
  moves behind a separate SwiftPM target for isolation, and the plan notes it. Blocks steps 3 to 5.
- **B. German fine-tune loads from a custom directory** (`[opt-in: STENO_MODEL_TESTS]`, 1.2 GB download). Go: transcribes
  `de-short.wav` offline. No-go: FluidAudio 0.17 rejects the layout; `parakeet-de` is dropped from the bake-off and the engine
  list, nothing else changes. Blocks only the `parakeet-de` entrant of step 9.
- **C. Cluster embeddings are usable across recordings.** During step 6: `de-short.wav` against `de-short-2.wav` (same voice)
  must score above 0.6, the two different voices below 0.5; on the bake-off meetings the same person across two meetings must
  beat every other speaker by the margin. Go: thresholds hold. No-go: cosine on WeSpeaker embeddings is not separable across
  microphones; fall back to never suggesting (always `.unknown`, top three shown in the review sheet) and record it. Blocks the
  auto-suggest behaviour of step 8, not its code.

## Needs from other workstreams

- StenoCore: `SpeechEngine`, `Diarizer`, `SpeakerMemory` with the provided `match`, `SpeakerMatch`, `SpeakerCluster` (label,
  ranges, embedding, clusterConfidence, sampleClipRange), `RawSegment.language` and `wordTimings`, `AudioBuffer16k.samples`,
  `AudioDecoder`, `MeetingStore` (persons, save), `Settings.speechEngineID` / `speakerMatchThreshold` / `modelsDirectory`, the
  `steno` root command and `dev` group, `Wiring.swift` accepting the `--engine` edit, `Tests/StenoEndToEndTests`.
- StenoAudio: `AVFoundationAudioCodec` for the bake-off's `m4a|mp3|caf` inputs.
- StenoLLM: `LLMTranscriptCleaner` for `--cleanup`.
- macOS app: the speaker review sheet calls `candidates(for:limit:)`, `MeetingStore.confirm` and `mergePersons`, and plays
  `Speaker.sampleClipURL`; the settings pane exposes engine choice, threshold and `ModelStore` actions with progress; the
  archive signs FluidAudio's binary target.

## Deferred

- Disk-backed `transcribe(fileURL:hint:)` on `SpeechEngine`; the pipeline processes lanes sequentially instead.
- `parakeet-ultra` and `parakeet-de` as user-selectable engines; bake-off entrants only until the result plan says otherwise.
- Diarization clustering threshold as a user setting; it stays a `FluidDiarizerConfig` constant.
- Custom vocabulary boosting (FluidAudio CTC rescoring).

## Deviations (implementation)

Recorded by the speech workstream while building steps 0 to 8 (PR #8, 2026-09-25).

- **FluidAudio pinned to the minor.** `Package.swift` says `.upToNextMinor(from: "0.17.4")`, not `from: "0.17.3"`:
  the API was verified against 0.17.4 (`AsrModels.loadLocal(from:)`, `ModelRegistry.repoOverrides`, `Repo.folderName`
  all exist there) and FluidAudio has renamed types inside a major before. WhisperKit stays `from: "1.1.0"` under its
  canonical repository name `argmax-oss-swift`.
- **Model-side language values are `LanguageTag`** (`LanguageTagger.candidates`, `dominantLanguage`, `tag(_:hint:)`);
  `Locale.Language` appears only on the engines' `supportedLanguages` and `hint`, converted with `LanguageTag(_:)`.
  Follows the PR #3 contract change.
- **Module qualification.** `StenoCore.WordTiming` and `StenoCore.DiarizationResult` do not compile: `StenoCore` is
  also the name of core's version enum, so the qualifier resolves to the enum. Files that import FluidAudio or
  WhisperKit reach the core types through the internal aliases `CoreWordTiming` and `CoreDiarizationResult`,
  declared in files that import only StenoCore.
- **Directory names.** FluidAudio derives its folder from the repository name minus `-coreml`, so the assets live at
  `Models/fluidaudio/parakeet-tdt-0.6b-v3`, `Models/fluidaudio/parakeet-ultra`, `Models/fluidaudio/speaker-diarization`
  and (own parent, cannot shadow v3) `Models/fluidaudio-de/parakeet-tdt-0.6b-v3`; WhisperKit under
  `Models/whisperkit/models/argmaxinc/whisperkit-coreml/<variant>` with `downloadBase = Models/whisperkit`. The
  tokenizer is fetched right after the weights (`ModelUtilities.loadTokenizer`) so an installed asset is usable
  offline.
- **German fine-tune download.** Every FluidAudio download entry point takes a `Repo` case and the override table is
  process-wide and prefix-matched, so `parakeetDE` cannot be a plain table entry: it is fetched by redirecting the
  v3 repository through `ModelRegistry.repoOverrides` for the duration of that one download. `LiveModelDownloader`
  runs all downloads strictly in sequence (a `Task` chain; an actor alone is re-entrant across the awaited download)
  so a concurrent v3 download never sees the redirect. `ParakeetEngine` ensures the asset like the other two
  Parakeet ids and loads with `loadLocal(from:)`, which never re-downloads, so the model-card `offlineMode` caveat
  does not apply. Spike B stays `[opt-in]` and is not yet run (needs a Mac and 1.2 GB).
- **Chunk quality.** `ChunkEmbedding` carries no quality; each chunk takes the `qualityScore` of the turn it overlaps
  most (1 when none), and `clusterConfidence` is the duration-weighted mean of those.
- **Fixtures.** No Mac was reachable (Forge down), so the `say` fixtures are generated on the `macos-15` runner by a
  temporary workflow (`.github/workflows/speech-fixtures.yml`, removed before the PR is ready), downloaded and
  committed once. Their hashes live in `Tests/Fixtures/speech/MANIFEST.sha256` (checked by `SpeechFixtureTests`),
  not in the root manifest: `FixtureManifestTests` compares the root file with `FixtureGenerator`'s output
  verbatim, so foreign lines there would fail core's test.
- **`--engine` touches two core files, not one.** `Wiring.swift` gains `SpeechOptions` and the `engine:` and
  `modelsDirectory:` parameters of `dependencies`; `Process.swift` needs one `@OptionGroup` line and passes both
  through. Without `--engine` the fakes run as before, so `stenoTests` are unchanged.
- **Bake-off CLI input is WAV only** until StenoAudio's codec exists (`BakeoffRunner` takes any `AudioDecoder`; the
  CLI passes `WAVAudioDecoder`). `--cleanup` is not a flag yet; the runner's `cleaner` seam is there for StenoLLM.
- **Linux builds.** Every file that imports FluidAudio or WhisperKit is wrapped in `#if canImport(...)`, and
  `LanguageTagger` falls back from NaturalLanguage to function words, so the pure logic builds and runs in the
  Linux container (FluidAudio 0.17.4 itself does not compile on Linux, so the local loop strips the two framework
  packages from a scratch copy of `Package.swift`). `makeSpeechEngine` and `makeDiarizer` throw
  `SpeechEngineError.unavailable` there.
- **`FakeModelDownloader.failure`** is thrown by the first download only, so one test shows that a failed download
  is retried.
- **Simplify pass (after the PR review).** Shapes the contract above named that the code no longer has, each
  replaced by something the module or core already had: `ParakeetVariant` (the three Parakeet ids are
  `ParakeetEngine(id:models:)`; the German fine-tune is an ordinary asset), `WhisperKitEngine(variant:)` (the variant
  is the asset directory's last component, so `ModelAsset.whisperVariant` went too), `Embeddings` (core's
  `Embedding` has `normalized()`, `cosineSimilarity(to:)` and `magnitude`; the cluster mean is one loop in
  `ClusterEmbedding`), `TimedToken` (tokens are `TimedWord`s that `TokenAggregator` joins), `ParakeetLanguages` and
  `WhisperLanguages` (private tables on `SpeechEngineID`), the `StenoSpeech` namespace enum and
  `SpeechEngineError.emptyResult` (unused), the per-framework `FluidAudioDownloads` and `WhisperKitDownloads` types
  (one `LiveModelDownloader`), and the `DiarizationMapping` fallback that re-implemented the picker's weighting for
  clusters without chunks (turns stand in as chunks without an embedding).
- **`makeDiarizer(models:config:)`** added beside `makeSpeechEngine` so the app and the CLI never name `FluidDiarizer`.
- **Two `@unchecked Sendable` boxes.** `WhisperKit` and `OfflineDiarizerManager` are non-Sendable classes whose
  work is async: Swift 6 lets neither a fresh instance be returned into an actor (`WhisperKit(config)`) nor an
  actor-owned instance be passed to a nonisolated async method (`process(audio:)`). `WhisperKitBox` and
  `OfflineDiarizerBox` own one instance each, are created and called only by their actor (`WhisperKitEngine`,
  `FluidDiarizer`), and are the only places the module marks anything `@unchecked Sendable`. `AsrManager` is an
  actor in FluidAudio and needs no box.
- **`Diarizer` clash.** FluidAudio also exports `Diarizer`; `FluidDiarizer` conforms through the public alias
  `CoreDiarizer`, and `CoreDiarizationResult` is public too because both appear in its public signatures.
