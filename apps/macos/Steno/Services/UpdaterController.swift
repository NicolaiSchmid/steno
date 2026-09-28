import Foundation
import Sparkle

/// Sparkle 2 behind `UpdaterControlling`. `SPUStandardUpdaterController`
/// reads `SUFeedURL`, `SUPublicEDKey` and `SUEnableAutomaticChecks` from
/// Info.plist and drives the standard update UI. The delegate adds the
/// pre-release lane (`UpdateChannels`) and records the last check's outcome
/// for the General section. Not compiled into the hostless unit tests (they
/// use `FakeUpdater`), so the tests never load the framework.
@MainActor
@Observable
final class UpdaterController: NSObject, UpdaterControlling, SPUUpdaterDelegate {
  /// Debug builds serve this `UserDefaults` value instead of `SUFeedURL`
  /// (`defaults write uno.schmid.steno.mac STENO_FEED_URL <url>`) for the
  /// local appcast spike.
  nonisolated static let feedOverrideKey = "STENO_FEED_URL"

  private static let sparkleErrorDomain = "SUSparkleErrorDomain"
  private static let noUpdateErrorCode = 1001

  @ObservationIgnored private var controller: SPUStandardUpdaterController?
  private(set) var lastOutcome: UpdateCheckOutcome = .notChecked

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

  var automaticallyDownloadsUpdates: Bool {
    get { controller?.updater.automaticallyDownloadsUpdates ?? false }
    set { controller?.updater.automaticallyDownloadsUpdates = newValue }
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

  nonisolated func allowedChannels(for updater: SPUUpdater) -> Set<String> {
    UpdateChannels.allowed(forVersion: AppVersion.marketing)
  }

  nonisolated func updater(_ updater: SPUUpdater, didFindValidUpdate item: SUAppcastItem) {
    let version = item.displayVersionString
    MainActor.assumeIsolated { lastOutcome = .available(version) }
  }

  nonisolated func updaterDidNotFindUpdate(_ updater: SPUUpdater, error: any Error) {
    MainActor.assumeIsolated { lastOutcome = .upToDate }
  }

  nonisolated func updater(_ updater: SPUUpdater, didAbortWithError error: any Error) {
    let nsError = error as NSError
    let isNoUpdate =
      nsError.domain == Self.sparkleErrorDomain && nsError.code == Self.noUpdateErrorCode
    let message = nsError.localizedDescription
    MainActor.assumeIsolated { lastOutcome = isNoUpdate ? .upToDate : .failed(message) }
  }
}
