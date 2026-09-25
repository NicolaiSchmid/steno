import ArgumentParser
import Foundation
import StenoAdapters
import StenoCore

/// `steno deliver <meeting-id>`: re-exports a processed meeting to every
/// configured destination through `ProcessingPipeline.redeliver` and the real
/// `DeliveryCoordinator`. Without `--vault` the stored `Settings.obsidian`
/// decides; with it the Obsidian destination is built for this run alone
/// under its own destination id (`obsidian-folder@<vault>`), so neither the
/// stored settings nor the stored destination's receipt are touched and a
/// second run into the same vault is a proper re-export. Prints one line per
/// destination of this run.
struct Deliver: AsyncParsableCommand {
  static let configuration = CommandConfiguration(
    abstract: "Deliver a processed meeting to every configured destination.")

  @Argument(help: "The meeting id printed by `steno process`.", transform: Deliver.uuid)
  var meetingID: UUID

  @Option(help: "Obsidian vault for this run; defaults to the stored Obsidian settings.")
  var vault: String?

  @Option(name: .customLong("people-folder"), help: "Folder for person pages inside --vault.")
  var peopleFolder: String?

  @Flag(name: .customLong("include-audio"), help: "Copy the audio mixdown into --vault.")
  var includeAudio = false

  @Option(name: .customLong("task-tag"), help: "Tag appended to every task line in --vault.")
  var taskTag: String?

  @OptionGroup var database: DatabaseOptions

  static func uuid(_ argument: String) throws -> UUID {
    guard let id = UUID(uuidString: argument) else {
      throw ValidationError("\(argument) is not a UUID.")
    }
    return id
  }

  func validate() throws {
    if vault == nil, peopleFolder != nil || includeAudio || taskTag != nil {
      throw ValidationError("--people-folder, --include-audio and --task-tag need --vault.")
    }
  }

  func run() async throws {
    let opened = try Wiring.open(database)
    let targets: [any Destination]
    if let vault {
      let path = URL(fileURLWithPath: vault, isDirectory: true).standardizedFileURL.path
      let obsidian = ObsidianSettings(
        vaultPath: path, peopleFolder: peopleFolder, includeAudio: includeAudio, taskTag: taskTag)
      targets = [
        ObsidianFolderDestination(
          settings: obsidian, id: "\(ObsidianFolderDestination.destinationID)@\(path)")
      ]
    } else {
      targets = destinations(for: try await opened.settings.load())
      guard !targets.isEmpty else {
        throw RuntimeFailure(
          description:
            "No destination configured: set the Obsidian vault in Settings or pass --vault.")
      }
    }
    let dispatcher = DeliveryCoordinator(
      store: opened.store, settings: opened.settings, destinations: { _ in targets })
    let pipeline = ProcessingPipeline(
      dependencies: Wiring.dependencies(
        store: opened.store, settings: opened.settings, dispatcher: dispatcher))
    try await pipeline.redeliver(meetingID: meetingID)

    let ids = Set(targets.map(\.id))
    let deliveries = try await opened.store.deliveries(meetingID: meetingID)
      .filter { ids.contains($0.destinationID) }
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
