import Foundation
import StenoCore
import StenoSpeech

/// Transcription: the engine and the components it needs on this Mac, with
/// download progress from `ModelStore.ensure`. Changing the engine rebuilds
/// the pipeline. Licences and repositories are not shown here; the
/// Acknowledgements sheet lists them.
@MainActor
@Observable
final class SpeechSettingsViewModel: SettingsSectionModel {
  enum AssetState: Equatable, Sendable {
    case absent
    case downloading(fraction: Double, phase: String)
    case installed(bytes: Int64?)
    case failed(String)
  }

  private(set) var engineID: SpeechEngineID = .parakeetV3
  private(set) var assetStates: [ModelAsset: AssetState] = [:]
  var error: String?
  var errorDetails: String?
  let engines = SpeechEngineID.userSelectable
  private let environment: AppEnvironment
  private var downloads: [ModelAsset: Task<Void, Never>] = [:]

  init(environment: AppEnvironment) {
    self.environment = environment
  }

  /// The assets the selected engine needs: its model and the diarizer.
  var assets: [ModelAsset] { [engineID.asset, .offlineDiarizer] }

  /// The picker is only worth a row when there is a choice.
  var showsEnginePicker: Bool { engines.count > 1 }

  var allInstalled: Bool {
    assets.allSatisfy {
      if case .installed = state(of: $0) { return true }
      return false
    }
  }

  func load() async {
    do {
      let settings = try await environment.settings.load()
      engineID = (try? SpeechEngineID(settingsValue: settings.speechEngineID)) ?? .parakeetV3
    } catch {
      fail("Settings could not be loaded.", error)
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

  // MARK: Copy

  /// What the component does, not what the model is called.
  nonisolated static func componentTitle(_ asset: ModelAsset) -> String {
    asset == .offlineDiarizer ? "Speaker recognition" : "Speech recognition"
  }

  /// "Parakeet · fast · 25 languages".
  nonisolated static func engineTitle(_ id: SpeechEngineID) -> String {
    let name: String
    let speed: String?
    switch id {
    case .parakeetV3:
      name = "Parakeet"
      speed = "fast"
    case .parakeetUltra:
      name = "Parakeet Ultra"
      speed = nil
    case .parakeetDE:
      name = "Parakeet (German)"
      speed = nil
    case .whisperKitLargeV3Turbo:
      name = "Whisper"
      speed = "slower"
    }
    let languages = id.supportedLanguages.count
    let languageText = languages == 1 ? "1 language" : "\(languages) languages"
    return [name, speed, languageText].compactMap { $0 }.joined(separator: " · ")
  }

  /// "Installed · 485 MB", "Downloading… 40%", "Not downloaded · 485 MB".
  func statusText(of asset: ModelAsset) -> String {
    let size = ByteCountFormatter.fileSize
    switch state(of: asset) {
    case .absent, .failed:
      return "Not downloaded · \(size(asset.approximateBytes))"
    case .downloading(let fraction, _):
      return fraction > 0 ? "Downloading… \(Int((fraction * 100).rounded()))%" : "Downloading…"
    case .installed(let bytes):
      return "Installed · \(size(bytes ?? asset.approximateBytes))"
    }
  }

  // MARK: Actions

  func setEngine(_ id: SpeechEngineID) async {
    guard id != engineID else { return }
    engineID = id
    do {
      try await environment.updateSettings { $0.speechEngineID = id.rawValue }
      try await environment.reloadPipeline()
      clearError()
    } catch {
      fail("The language model could not be changed.", error)
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
      fail("The download could not be removed.", error)
    }
  }
}
