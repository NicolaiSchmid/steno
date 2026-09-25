import ArgumentParser
import Foundation
import StenoCore

/// `steno dev fixtures generate --out DIR`: the one synthetic-audio generator.
struct DevFixtures: AsyncParsableCommand {
  static let configuration = CommandConfiguration(
    commandName: "fixtures",
    abstract: "Generated test fixtures.",
    subcommands: [Generate.self]
  )

  struct Generate: AsyncParsableCommand {
    static let configuration = CommandConfiguration(
      abstract: "Write every generated audio fixture and MANIFEST.sha256 into a directory.")

    @Option(help: "Target directory, for example Tests/Fixtures.")
    var out: String

    func run() async throws {
      let root = URL(fileURLWithPath: out, isDirectory: true)
      let outputs = try FixtureGenerator.generate(into: root)
      try Data(FixtureGenerator.manifest(outputs).utf8)
        .write(to: root.appendingPathComponent("MANIFEST.sha256"), options: .atomic)
      for output in outputs {
        print("\(output.sha256)  \(output.relativePath)")
      }
    }
  }
}
