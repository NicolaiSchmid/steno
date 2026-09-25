import ArgumentParser
import Foundation
import StenoCore

/// The `steno` command line tool. Product commands are `record`, `process`,
/// `export` and `deliver`; every developer tool sits under `steno dev`. Each
/// module registers its subcommands in `Sources/steno/Commands/`.
///
/// Exit codes: 0 success, 1 usage (bad arguments, unknown template), 2 a
/// runtime failure (database, files, a failed pipeline run).
@main
struct Steno: AsyncParsableCommand {
  static let configuration = CommandConfiguration(
    commandName: "steno",
    abstract: "Bot-free meeting recorder for the Mac.",
    version: StenoCore.version,
    subcommands: [Process.self, Export.self, Dev.self]
  )

  static func main() async {
    do {
      var command = try parseAsRoot()
      if var asyncCommand = command as? AsyncParsableCommand {
        try await asyncCommand.run()
      } else {
        try command.run()
      }
    } catch {
      let code = exitCode(for: error)
      if code.isSuccess {
        // --help and --version print through ArgumentParser and exit 0.
        exit(withError: error)
      }
      let message = fullMessage(for: error)
      FileHandle.standardError.write(Data((message + "\n").utf8))
      Foundation.exit(code == .validationFailure ? 1 : 2)
    }
  }
}

/// A runtime failure reported to the user; exits 2.
struct RuntimeFailure: Error, CustomStringConvertible {
  var description: String
}
