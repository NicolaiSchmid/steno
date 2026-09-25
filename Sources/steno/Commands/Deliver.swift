import ArgumentParser
import Foundation
import StenoAdapters
import StenoCore

/// `steno deliver <meeting-id>`: re-exports a processed meeting to every
/// configured destination through `ProcessingPipeline.redeliver` and the real
/// `DeliveryCoordinator`. Without `--vault` the stored `Settings.obsidian`
/// decides; with it the Obsidian destination is built for this run alone and
/// the stored settings stay untouched. Prints one line per destination.
struct Deliver: AsyncParsableCommand {
  static let configuration = CommandConfiguration(
    abstract: "Deliver a processed meeting to every configured destination.")

  @Argument(help: "The meeting id printed by `steno process`.")
  var meetingID: String

  @Option(help: "Obsidian vault for this run; defaults to the stored Obsidian settings.")
  var vault: String?

  @Option(name: .customLong("people-folder"), help: "Folder for person pages inside --vault.")
  var peopleFolder: String?

  @Flag(name: .customLong("include-audio"), help: "Copy the audio mixdown into --vault.")
  var includeAudio = false

  @Option(name: .customLong("task-tag"), help: "Tag appended to every task line in --vault.")
  var taskTag: String?

  @OptionGroup var database: DatabaseOptions

  func validate() throws {
    guard UUID(uuidString: meetingID) != nil else {
      throw ValidationError("\(meetingID) is not a UUID.")
    }
    if vault == nil, peopleFolder != nil || includeAudio || taskTag != nil {
      throw ValidationError("--people-folder, --include-audio and --task-tag need --vault.")
    }
  }

  func run() async throws {
    let id = UUID(uuidString: meetingID)!
    let opened = try Wiring.open(database)
    let dispatcher: DeliveryCoordinator
    if let vault {
      let obsidian = ObsidianSettings(
        vaultPath: URL(fileURLWithPath: vault, isDirectory: true).standardizedFileURL.path,
        peopleFolder: peopleFolder, includeAudio: includeAudio, taskTag: taskTag)
      dispatcher = DeliveryCoordinator(
        store: opened.store, settings: opened.settings,
        destinations: { _ in [ObsidianFolderDestination(settings: obsidian)] })
    } else {
      guard try await opened.settings.load().obsidian != nil else {
        throw RuntimeFailure(
          description:
            "No destination configured: set the Obsidian vault in Settings or pass --vault.")
      }
      dispatcher = DeliveryCoordinator(store: opened.store, settings: opened.settings)
    }
    let pipeline = ProcessingPipeline(
      dependencies: Wiring.dependencies(
        store: opened.store, settings: opened.settings, dispatcher: dispatcher))
    try await pipeline.redeliver(meetingID: id)

    let deliveries = try await opened.store.deliveries(meetingID: id)
    var failures: [String] = []
    for delivery in deliveries {
      switch delivery.status {
      case .delivered:
        let folder = delivery.receipt.map { "\($0.root)/\($0.folder)" } ?? ""
        print("\(delivery.destinationID)\tdelivered\t\(folder)")
      case .failed(let reason):
        print("\(delivery.destinationID)\tfailed\t\(reason)")
        failures.append("\(delivery.destinationID): \(reason)")
      case .pending:
        print("\(delivery.destinationID)\tpending")
      }
    }
    if !failures.isEmpty {
      throw RuntimeFailure(description: "delivery failed: " + failures.joined(separator: "; "))
    }
  }
}
