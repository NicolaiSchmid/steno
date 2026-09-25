import ArgumentParser
import Foundation
import StenoAdapters
import StenoCore

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
  static func open(_ options: DatabaseOptions) throws -> (
    store: MeetingStore, settings: SettingsStore
  ) {
    let store = try MeetingStore.onDisk(at: try options.url())
    return (store, SettingsStore(writer: store.writer))
  }

  static func dependencies(
    store: MeetingStore, settings: SettingsStore, dispatcher: (any DeliveryDispatcher)? = nil
  ) -> PipelineDependencies {
    PipelineDependencies(
      decoder: WAVAudioDecoder(),
      speechEngine: FakeSpeechEngine(),
      diarizer: FakeDiarizer(),
      speakerMemory: InMemorySpeakerMemory(),
      cleaner: PassthroughCleaner(),
      summarizer: FakeSummarizer(),
      dispatcher: dispatcher ?? DeliveryCoordinator(store: store, settings: settings),
      store: store,
      settings: settings,
      events: MeetingEventBus()
    )
  }
}
