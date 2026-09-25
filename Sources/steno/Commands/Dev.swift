import ArgumentParser

/// `steno dev`: developer tools. Every module adds its own subcommand here
/// (`audio-devices`, `bakeoff`, `models`, `llm`, `handover`, ...).
struct Dev: AsyncParsableCommand {
  static let configuration = CommandConfiguration(
    abstract: "Developer tools.",
    subcommands: [
      DevDB.self, DevFixtures.self, DevAudioDevices.self, DevAECBench.self, DevCaptureSpike.self,
      DevModels.self, DevBakeoff.self, DevLLM.self, DevHandover.self,
    ]
  )
}
