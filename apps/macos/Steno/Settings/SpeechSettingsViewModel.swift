import Foundation
import StenoCore
import StenoSpeech

/// Speech: the engine and the model assets it needs, with download progress
/// from `ModelStore.ensure`. Changing the engine rebuilds the pipeline.
@MainActor
@Observable
final class SpeechSettingsViewModel {
  enum AssetState: Equatable, Sendable {
    case absent
    case downloading(fraction: Double, phase: String)
    case installed(bytes: Int64?)
    case failed(String)
  }

  private(set) var engineID: SpeechEngineID = .parakeetV3
  private(set) var assetStates: [ModelAsset: AssetState] = [:]
  private(set) var error: String?
  let engines = SpeechEngineID.userSelectable
  private let environment: AppEnvironment
  private var downloads: [ModelAsset: Task<Void, Never>] = [:]

  init(environment: AppEnvironment) {
    self.environment = environment
  }

  /// The assets the selected engine needs: its model and the diarizer.
  var assets: [ModelAsset] { [engineID.asset, .offlineDiarizer] }

  func load() async {
    do {
      let settings = try await environment.settings.load()
      engineID = (try? SpeechEngineID(settingsValue: settings.speechEngineID)) ?? .parakeetV3
    } catch {
      self.error = "Settings could not be loaded: \(error)"
    }
    refreshStates()
  }

  func refreshStates() {
    for asset in ModelAsset.allCases {
      if case .downloading = assetStates[asset] { continue }
      assetStates[asset] =
        environment.models.isInstalled(asset)
        ? .installed(bytes: environment.models.installedSize(of: asset)) : .absent
    }
  }

  func state(of asset: ModelAsset) -> AssetState {
    assetStates[asset] ?? .absent
  }

  func setEngine(_ id: SpeechEngineID) async {
    guard id != engineID else { return }
    engineID = id
    do {
      var settings = try await environment.settings.load()
      settings.speechEngineID = id.rawValue
      try await environment.settings.save(settings)
      try await environment.reloadPipeline()
      error = nil
    } catch {
      self.error = "Engine could not be changed: \(error)"
    }
    refreshStates()
  }

  /// Streams `ensure` progress; the stream ends when the asset is installed.
  func download(_ asset: ModelAsset) {
    guard downloads[asset] == nil else { return }
    assetStates[asset] = .downloading(fraction: 0, phase: "starting")
    let models = environment.models
    downloads[asset] = Task { [weak self] in
      do {
        let stream = await models.ensure(asset)
        for try await progress in stream {
          guard let self else { return }
          self.assetStates[asset] = .downloading(
            fraction: progress.fractionCompleted, phase: progress.phase)
        }
        guard let self else { return }
        self.assetStates[asset] = .installed(bytes: models.installedSize(of: asset))
      } catch {
        self?.assetStates[asset] = .failed(String(describing: error))
      }
      self?.downloads[asset] = nil
    }
  }

  func remove(_ asset: ModelAsset) async {
    do {
      try await environment.models.remove(asset)
      assetStates[asset] = .absent
    } catch {
      self.error = "Model could not be removed: \(error)"
    }
  }

  static func formatBytes(_ bytes: Int64) -> String {
    ByteCountFormatter.string(fromByteCount: bytes, countStyle: .file)
  }
}
