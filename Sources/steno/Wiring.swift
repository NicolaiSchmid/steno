import ArgumentParser
import Foundation
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
/// speech, diarization, LLM and delivery; later workstreams swap real
/// implementations in behind flags in their own command files, with one
/// recorded exception: the speech PR adds `--engine <id>` here so
/// `steno process` can run the real engines.
enum Wiring {
  static func open(_ options: DatabaseOptions) throws -> (
    store: MeetingStore, settings: SettingsStore
  ) {
    let store = try MeetingStore.onDisk(at: try options.url())
    return (store, SettingsStore(writer: store.writer))
  }

  /// `llm` replaces the fake cleaner and summarizer when the settings name
  /// an endpoint; see `llmComponents(settings:)`.
  static func dependencies(
    store: MeetingStore, settings: SettingsStore, llm: LLMComponents? = nil
  ) -> PipelineDependencies {
    PipelineDependencies(
      decoder: WAVAudioDecoder(),
      speechEngine: FakeSpeechEngine(),
      diarizer: FakeDiarizer(),
      speakerMemory: InMemorySpeakerMemory(),
      cleaner: llm?.cleaner ?? PassthroughCleaner(),
      summarizer: llm?.summarizer ?? FakeSummarizer(),
      dispatcher: FakeDeliveryDispatcher(store: store, destinations: []),
      store: store,
      settings: settings,
      events: MeetingEventBus()
    )
  }

  /// The real cleaner and summarizer on an `OpenAICompatibleClient`, or nil
  /// when `Settings.llmBaseURL` or `llmModel` is unset (the fakes stay). The
  /// key comes from `secretStore()`: `STENO_LLM_API_KEY` or the 0600
  /// secrets file in the support directory.
  static func llmComponents(settings: Settings) async throws -> LLMComponents? {
    guard LLMEndpoint.isConfigured(settings) else { return nil }
    let endpoint = try LLMEndpoint(settings: settings)
    let apiKey = try await secretStore().secret(for: .llmAPIKey)
    return LLMComponents(endpoint: endpoint, apiKey: apiKey)
  }

  static func secretStore() throws -> FileSecretStore {
    FileSecretStore(
      url: try StenoPaths.default().supportDirectory.appendingPathComponent("secrets.json"))
  }
}

/// One client shared by both passes, so a structured output mode learned
/// during cleanup carries over to the summary.
struct LLMComponents {
  let client: OpenAICompatibleClient
  let cleaner: LLMTranscriptCleaner
  let summarizer: LLMMeetingSummarizer

  init(endpoint: LLMEndpoint, apiKey: String?) {
    client = OpenAICompatibleClient(endpoint: endpoint, apiKey: apiKey)
    cleaner = LLMTranscriptCleaner(model: client, endpoint: endpoint)
    summarizer = LLMMeetingSummarizer(model: client, endpoint: endpoint)
  }
}
