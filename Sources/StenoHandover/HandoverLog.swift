import Foundation

#if canImport(os)
  import os
#endif

/// Where the engine puts the detail it must not send: a failing write, hash,
/// promote, intake or store call carries the inbox path (and with it the
/// Mac user's home directory) in its description. The phone and the receipt
/// get a fixed phrase; the detail goes here, on the Mac only.
enum HandoverLog {
  #if canImport(os)
    private static let logger = Logger(subsystem: "app.steno.handover", category: "engine")
  #endif

  static func failure(_ what: String, _ error: any Error) {
    #if canImport(os)
      logger.error(
        "\(what, privacy: .public) failed: \(String(describing: error), privacy: .private)")
    #else
      FileHandle.standardError.write(Data("steno handover: \(what) failed: \(error)\n".utf8))
    #endif
  }
}
