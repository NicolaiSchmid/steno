import ArgumentParser
import StenoCore

/// The `steno` command line tool. Product commands are `record`, `process`,
/// `export` and `deliver`; every developer tool sits under `steno dev`. Each
/// module registers its subcommands in `Sources/steno/Commands/`.
@main
struct Steno: AsyncParsableCommand {
  static let configuration = CommandConfiguration(
    commandName: "steno",
    abstract: "Bot-free meeting recorder for the Mac.",
    version: StenoCore.version,
    subcommands: []
  )
}
