import ArgumentParser
import Foundation
import StenoAdapters
import StenoCore
import StenoLLM

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

/// Builds the stores and the `PipelineDependencies`. Core wires fakes for
/// speech, diarization and LLM; delivery runs through the real
/// `DeliveryCoordinator` (or the `dispatcher` a command passes, as `steno
/// deliver --vault` does). Later workstreams swap real implementations in
/// behind flags in their own command files, with one recorded exception: the
/// speech PR adds `--engine <id>` here so `steno process` can run the real
/// engines.
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

  /// `llm` replaces the fake cleaner and summarizer when the settings name
  /// an endpoint; see `llmComponents(settings:)`.
  static func dependencies(
    store: MeetingStore, settings: SettingsStore, dispatcher: (any DeliveryDispatcher)? = nil,
    llm: LLMPasses? = nil
  ) -> PipelineDependencies {
    PipelineDependencies(
      decoder: WAVAudioDecoder(),
      speechEngine: FakeSpeechEngine(),
      diarizer: FakeDiarizer(),
      speakerMemory: InMemorySpeakerMemory(),
      cleaner: llm?.cleaner ?? PassthroughCleaner(),
      summarizer: llm?.summarizer ?? FakeSummarizer(),
      dispatcher: dispatcher ?? DeliveryCoordinator(store: store, settings: settings),
      store: store,
      settings: settings,
      events: MeetingEventBus()
    )
  }

  typealias LLMPasses = (cleaner: LLMTranscriptCleaner, summarizer: LLMMeetingSummarizer)

  /// The real cleaner and summarizer on one shared `OpenAICompatibleClient`
  /// (so a structured output mode learned during cleanup carries over to the
  /// summary), or nil when `Settings.llmBaseURL` or `llmModel` is unset and
  /// the fakes stay. The key comes from `secretStore()`: `STENO_LLM_API_KEY`
  /// or the 0600 secrets file in the support directory.
  static func llmComponents(settings: Settings) async throws -> LLMPasses? {
    guard let endpoint = LLMEndpoint(settings: settings) else { return nil }
    let client = OpenAICompatibleClient(
      endpoint: endpoint, apiKey: try await secretStore().secret(for: .llmAPIKey))
    return (
      LLMTranscriptCleaner(model: client, endpoint: endpoint),
      LLMMeetingSummarizer(model: client, endpoint: endpoint)
    )
  }

  static func secretStore() throws -> FileSecretStore {
    FileSecretStore(
      url: try StenoPaths.default().supportDirectory.appendingPathComponent("secrets.json"))
  }
}
