import ArgumentParser

/// `steno dev`: developer tools. Every module adds its own subcommand here
/// (`audio-devices`, `bakeoff`, `models`, `llm`, `handover`, ...).
struct Dev: AsyncParsableCommand {
  static let configuration = CommandConfiguration(
    abstract: "Developer tools.",
    subcommands: [
      DevDB.self, DevFixtures.self, DevModels.self, DevBakeoff.self, DevHandover.self,
    ]
  )
}
