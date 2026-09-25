import Foundation
import StenoCore

/// How the Mac side of the handover listens and where it keeps partial
/// uploads. Tests use `advertise: false` and a fresh temporary inbox, so
/// nothing leaves 127.0.0.1.
public struct HandoverConfiguration: Sendable {
  /// Bonjour instance name; the phone shows it. Defaults to the computer
  /// name.
  public var serviceName: String
  /// Publish `_steno._tcp` on the local network. `false` binds loopback only.
  public var advertise: Bool
  /// The largest chunk the Mac accepts; the phone declares its own chunk
  /// size per recording and it must not exceed this. 16 MiB in the product.
  public var chunkSize: Int
  /// Where partial uploads and their metadata live until `HandoverIntake`
  /// takes the finished file.
  public var inboxDirectory: URL
  /// How long a pairing QR code stays valid on the injected clock.
  public var pairingWindow: Duration
  /// `0` lets the system choose; the port is published through Bonjour.
  public var port: UInt16

  public static let defaultChunkSize = 16 * 1024 * 1024
  /// Headroom over the chunk size for the request body limit.
  public static let bodyHeadroom = 64 * 1024
  /// Upper bound for the JSON bodies of the small routes.
  public static let jsonBodyLimit = 64 * 1024

  public init(
    serviceName: String = HandoverConfiguration.defaultServiceName(),
    advertise: Bool = true,
    chunkSize: Int = HandoverConfiguration.defaultChunkSize,
    inboxDirectory: URL = HandoverConfiguration.defaultInboxDirectory(),
    pairingWindow: Duration = .seconds(300),
    port: UInt16 = 0
  ) {
    self.serviceName = serviceName
    self.advertise = advertise
    self.chunkSize = chunkSize
    self.inboxDirectory = inboxDirectory
    self.pairingWindow = pairingWindow
    self.port = port
  }

  /// Body limit for chunk uploads: the chunk size plus 64 KiB.
  public var chunkBodyLimit: Int { chunkSize + Self.bodyHeadroom }

  public static func defaultServiceName() -> String {
    #if canImport(Darwin)
      if let name = Host.current().localizedName, !name.isEmpty { return name }
    #endif
    return ProcessInfo.processInfo.hostName
  }

  /// `<Application Support>/Steno/handover-inbox`; `StenoPaths` decides the
  /// root so a `HOME` override in tests applies here too.
  public static func defaultInboxDirectory() -> URL {
    StenoPaths.defaultSupportDirectory.appendingPathComponent("handover-inbox", isDirectory: true)
  }
}
