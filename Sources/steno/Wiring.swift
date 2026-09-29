import ArgumentParser
import Foundation
import StenoAdapters
import StenoCore
import StenoLLM
import StenoSpeech

/// `--db PATH`, shared by every command that opens the database. Defaults to
/// `StenoPaths.default().databaseURL`, which follows `HOME`.
struct DatabaseOptions: ParsableArguments {
  @Option(name: .customLong("db"), help: "Path of the SQLite database.")
  var databasePath: String?

  func url() throws -> URL {
    if let databasePath {
      return URL(fileURLWithPath: databasePath)
    }
    return try StenoPaths.default().databaseURL
  }
}

/// `--engine <id>`: the speech workstream's one edit here. Without it the
/// pipeline runs the fakes; with it the named StenoSpeech engine, the
/// FluidAudio diarizer and cosine speaker memory over the store, models
/// downloading on first use.
struct SpeechOptions: ParsableArguments {
  @Option(
    help: "Real speech engine: \(SpeechEngineID.allCases.map(\.rawValue).joined(separator: ", ")).")
  var engine: SpeechEngineID?
}

/// Builds the stores and the `PipelineDependencies`. Core wires fakes for
/// speech and diarization; the LLM passes come from
/// `llmComponents(settings:)` and are nil without an endpoint. Delivery
/// runs through the real `DeliveryCoordinator` (or the `dispatcher` a
/// command passes, as `steno deliver --vault` does); `--engine <id>` swaps
/// in the real speech engine, diarizer and cosine speaker memory (the speech
/// PR's one recorded edit here). Later workstreams swap the rest in behind
/// flags in their own command files.
enum Wiring {
  /// The `transform:` of every `<meeting-id>` argument.
  static func uuid(_ argument: String) throws -> UUID {
    guard let id = UUID(uuidString: argument) else {
      throw ValidationError("\(argument) is not a UUID.")
    }
    return id
  }

  static func open(_ options: DatabaseOptions) throws -> (
    store: MeetingStore, settings: SettingsStore
  ) {
    let store = try MeetingStore.onDisk(at: try options.url())
    return (store, SettingsStore(writer: store.writer))
  }

  /// `llm` is `llmComponents(settings:)`'s result; nil skips both passes.
  /// `events` is the bus the pipeline posts progress on; `steno process`
  /// subscribes to it before it enqueues.
  static func dependencies(
    store: MeetingStore, settings: SettingsStore, engine: SpeechEngineID? = nil,
    modelsDirectory: URL? = nil, dispatcher: (any DeliveryDispatcher)? = nil,
    llm: LLMPasses? = nil, events: MeetingEventBus
  ) throws -> PipelineDependencies {
    let models = ModelStore(directory: modelsDirectory)
    let speechEngine: any SpeechEngine =
      try engine.map { try makeSpeechEngine($0, models: models) } ?? FakeSpeechEngine()
    let diarizer: any Diarizer =
      try engine == nil ? FakeDiarizer() : makeDiarizer(models: models)
    let speakerMemory: any SpeakerMemory =
      engine == nil ? InMemorySpeakerMemory() : CosineSpeakerMemory(store: store)
    return PipelineDependencies(
      decoder: WAVAudioDecoder(),
      speechEngine: speechEngine,
      diarizer: diarizer,
      speakerMemory: speakerMemory,
      cleaner: llm?.cleaner,
      summarizer: llm?.summarizer,
      dispatcher: dispatcher ?? DeliveryCoordinator(store: store, settings: settings),
      store: store,
      settings: settings,
      events: events
    )
  }

  typealias LLMPasses = (cleaner: LLMTranscriptCleaner, summarizer: LLMMeetingSummarizer)

  /// The real cleaner and summarizer on one shared client (so a structured
  /// output mode learned during cleanup carries over to the summary), or nil
  /// when the settings describe no endpoint and the pipeline skips both
  /// passes. The endpoint provider's key comes from `secretStore()`:
  /// `STENO_LLM_API_KEY` or the 0600 secrets file in the support directory.
  /// The Codex provider reads the Codex CLI's own sign-in (`CODEX_HOME`).
  static func llmComponents(settings: Settings) async throws -> LLMPasses? {
    guard let endpoint = LLMEndpoint(settings: settings) else { return nil }
    let client = try await llmClient(endpoint: endpoint, observer: nil)
    return (
      LLMTranscriptCleaner(model: client, endpoint: endpoint),
      LLMMeetingSummarizer(model: client, endpoint: endpoint)
    )
  }

  /// The client for `endpoint`: the Codex backend gets the credential
  /// store, everything else the API key.
  static func llmClient(endpoint: LLMEndpoint, observer: (@Sendable (LLMClientEvent) -> Void)?)
    async throws -> any LLMClient
  {
    if endpoint.isCodexBackend {
      return CodexResponsesClient(
        endpoint: endpoint, credentials: CodexCredentialStore(), observer: observer)
    }
    return OpenAICompatibleClient(
      endpoint: endpoint, apiKey: try await secretStore().secret(for: .llmAPIKey),
      observer: observer)
  }

  static func secretStore() throws -> FileSecretStore {
    FileSecretStore(
      url: try StenoPaths.default().supportDirectory.appendingPathComponent("secrets.json"))
  }
}
