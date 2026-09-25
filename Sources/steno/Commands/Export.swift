import ArgumentParser
import Foundation
import StenoCore

/// `steno export <meeting-id>`: writes `meeting.json` (the `StenoJSON`
/// encoding of `MeetingExport`) into `--out` and prints its path.
struct Export: AsyncParsableCommand {
  static let configuration = CommandConfiguration(abstract: "Write a meeting as meeting.json.")

  @Argument(help: "The meeting id printed by `steno process`.")
  var meetingID: String

  @Option(help: "Output directory; defaults to the current directory.")
  var out: String = "."

  @OptionGroup var database: DatabaseOptions

  func run() async throws {
    guard let id = UUID(uuidString: meetingID) else {
      throw ValidationError("\(meetingID) is not a UUID.")
    }
    let opened = try Wiring.open(database)
    let export = try await opened.store.export(meetingID: id)
    let directory = URL(fileURLWithPath: out, isDirectory: true)
    try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
    let url = directory.appendingPathComponent("meeting.json")
    try StenoJSON.encode(export).write(to: url, options: .atomic)
    print(url.path)
  }
}
