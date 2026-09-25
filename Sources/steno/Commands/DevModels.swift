import ArgumentParser
import Foundation
import StenoCore
import StenoSpeech

extension ModelAsset: ExpressibleByArgument {
  public static var allValueStrings: [String] { allCases.map(\.rawValue) }
}

/// `steno dev models list|download|remove <asset>`: the model store from
/// the command line. `--models-dir` overrides `Settings.modelsDirectory`
/// and the default location for one invocation.
struct DevModels: AsyncParsableCommand {
  static let configuration = CommandConfiguration(
    commandName: "models",
    abstract: "List, download or remove speech and diarization models.",
    subcommands: [List.self, Download.self, Remove.self]
  )

  struct Options: ParsableArguments {
    @Option(
      name: .customLong("models-dir"),
      help:
        "Models directory; defaults to the settings' directory or Application Support/Steno/Models."
    )
    var modelsDirectory: String?

    @OptionGroup var database: DatabaseOptions

    func store() async throws -> ModelStore {
      if let modelsDirectory {
        return ModelStore(directory: URL(fileURLWithPath: modelsDirectory, isDirectory: true))
      }
      let opened = try Wiring.open(database)
      let settings = try await opened.settings.load()
      return ModelStore(directory: settings.modelsDirectory)
    }
  }

  struct List: AsyncParsableCommand {
    static let configuration = CommandConfiguration(abstract: "Show every asset and its state.")

    @OptionGroup var options: Options

    func run() async throws {
      let store = try await options.store()
      print("models: \(store.directory.path)")
      for asset in ModelAsset.allCases {
        let state: String
        if let size = store.installedSize(of: asset) {
          state = "installed (\(Self.megabytes(size)) MB)"
        } else {
          state = "not installed (~\(Self.megabytes(asset.approximateBytes)) MB)"
        }
        print("\(asset.rawValue.padding(toLength: 20, withPad: " ", startingAt: 0)) \(state)")
        print("  \(asset.displayName); \(asset.sourceRepo); \(asset.licence)")
      }
    }

    static func megabytes(_ bytes: Int64) -> Int64 { bytes / 1_000_000 }
  }

  struct Download: AsyncParsableCommand {
    static let configuration = CommandConfiguration(abstract: "Download an asset, with progress.")

    @Argument(help: "One of \(ModelAsset.allValueStrings.joined(separator: ", ")).")
    var asset: ModelAsset

    @OptionGroup var options: Options

    func run() async throws {
      let store = try await options.store()
      var lastPercent = -1
      for try await progress in await store.ensure(asset) {
        let percent = Int((progress.fractionCompleted * 100).rounded(.down))
        if percent != lastPercent {
          print("\(asset.rawValue): \(percent) % \(progress.phase)")
          lastPercent = percent
        }
      }
      print("\(asset.rawValue): installed at \(store.directory(for: asset).path)")
    }
  }

  struct Remove: AsyncParsableCommand {
    static let configuration = CommandConfiguration(abstract: "Delete an asset's files.")

    @Argument(help: "One of \(ModelAsset.allValueStrings.joined(separator: ", ")).")
    var asset: ModelAsset

    @OptionGroup var options: Options

    func run() async throws {
      let store = try await options.store()
      try await store.remove(asset)
      print("\(asset.rawValue): removed")
    }
  }
}
