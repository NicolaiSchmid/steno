import Foundation
import Sparkle

/// Sparkle 2 behind `UpdaterControlling`. `SPUStandardUpdaterController`
/// reads `SUFeedURL`, `SUPublicEDKey` and `SUEnableAutomaticChecks` from
/// Info.plist and drives the standard update UI. Not compiled into the
/// hostless unit tests (they use `FakeUpdater`), so the tests never load the
/// framework.
@MainActor
final class UpdaterController: NSObject, UpdaterControlling, SPUUpdaterDelegate {
  /// Debug builds serve this `UserDefaults` value instead of `SUFeedURL`
  /// (`defaults write uno.schmid.steno.mac STENO_FEED_URL <url>`) for the
  /// local appcast spike.
  nonisolated static let feedOverrideKey = "STENO_FEED_URL"

  private var controller: SPUStandardUpdaterController?

  override init() {
    super.init()
    controller = SPUStandardUpdaterController(
      startingUpdater: true, updaterDelegate: self, userDriverDelegate: nil)
  }

  var canCheckForUpdates: Bool { controller?.updater.canCheckForUpdates ?? false }

  var automaticallyChecksForUpdates: Bool {
    get { controller?.updater.automaticallyChecksForUpdates ?? false }
    set { controller?.updater.automaticallyChecksForUpdates = newValue }
  }

  var lastUpdateCheckDate: Date? { controller?.updater.lastUpdateCheckDate }

  func checkForUpdates() {
    controller?.checkForUpdates(nil)
  }

  // MARK: SPUUpdaterDelegate

  nonisolated func feedURLString(for updater: SPUUpdater) -> String? {
    #if DEBUG
      return UserDefaults.standard.string(forKey: UpdaterController.feedOverrideKey)
    #else
      return nil
    #endif
  }
}
