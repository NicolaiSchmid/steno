import ArgumentParser
import Foundation
import StenoAdapters
import StenoCore
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
/// speech, diarization and LLM; delivery runs through the real
/// `DeliveryCoordinator` (or the `dispatcher` a command passes, as `steno
/// deliver --vault` does); `--engine <id>` swaps in the real speech engine,
/// diarizer and cosine speaker memory (the speech PR's one recorded edit
/// here). Later workstreams swap the rest in behind flags in their own
/// command files.
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

  static func dependencies(
    store: MeetingStore, settings: SettingsStore, engine: SpeechEngineID? = nil,
    modelsDirectory: URL? = nil, dispatcher: (any DeliveryDispatcher)? = nil
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
      cleaner: PassthroughCleaner(),
      summarizer: FakeSummarizer(),
      dispatcher: dispatcher ?? DeliveryCoordinator(store: store, settings: settings),
      store: store,
      settings: settings,
      events: MeetingEventBus()
    )
  }
}
