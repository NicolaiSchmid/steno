import Foundation

/// Which Sparkle channels a build reads, from its own version string. The
/// rolling appcast tags pre-release items with `beta`; a build whose
/// marketing version carries a pre-release suffix ("0.9.0-rc.1") reads that
/// lane too, a stable build ("0.9.0") reads the default channel only. No
/// setting: the lane follows what the user installed.
enum UpdateChannels {
  static let preRelease = "beta"

  nonisolated static func allowed(forVersion version: String) -> Set<String> {
    version.contains("-") ? [preRelease] : []
  }
}

/// The running build's version as Settings shows it: "0.9.0 (244)". Falls
/// back to zeros outside an app bundle (the hostless tests).
enum AppVersion {
  static var marketing: String {
    Bundle.main.infoDictionary?["CFBundleShortVersionString"] as? String ?? "0.0.0"
  }

  static var build: String {
    Bundle.main.infoDictionary?["CFBundleVersion"] as? String ?? "0"
  }

  static var display: String { "\(marketing) (\(build))" }
}
