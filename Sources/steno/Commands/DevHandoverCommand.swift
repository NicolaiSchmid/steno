import ArgumentParser
import Foundation
import StenoCore
import StenoHandover

/// `steno dev handover serve [--pair]`: runs the Mac side of the phone
/// handover against an in-memory store and core's `FakeHandoverIntake`, so a
/// phone dev build can pair and upload without touching the real database.
/// The real app wires `HandoverService` to the on-disk store and the pipeline.
struct DevHandover: AsyncParsableCommand {
  static let configuration = CommandConfiguration(
    commandName: "handover",
    abstract: "Developer tools for the phone handover.",
    subcommands: [Serve.self]
  )

  struct Serve: AsyncParsableCommand {
    static let configuration = CommandConfiguration(
      abstract: "Advertise the handover service and accept phone uploads.")

    @Flag(help: "Open a pairing window at once and print the QR payload URL.")
    var pair = false

    @Option(help: "Bonjour service name shown on the phone.")
    var name = HandoverConfiguration.defaultServiceName()

    @Option(help: "Listen on this port; 0 lets the system choose.")
    var port: UInt16 = 0

    @Option(help: "Directory for partial uploads; a temp directory by default.")
    var inbox: String?

    func run() async throws {
      let inboxURL =
        inbox.map { URL(fileURLWithPath: $0, isDirectory: true) }
        ?? FileManager.default.temporaryDirectory
        .appendingPathComponent("steno-handover-\(UUID().uuidString)", isDirectory: true)

      let store = try MeetingStore.inMemory()
      let intake = FakeHandoverIntake()
      let identity = try Self.identity(name: name)
      let service = HandoverService(
        configuration: HandoverConfiguration(
          serviceName: name, advertise: true, inboxDirectory: inboxURL, port: port),
        store: store, intake: intake, identity: identity)

      try await service.start()
      guard let boundPort = await service.port else {
        throw RuntimeFailure(description: "the handover listener reported no port")
      }
      print("Steno handover listening on port \(boundPort)")
      print("Mac id: \(service.macID)")
      print("Fingerprint (base64): \(identity.fingerprint.base64EncodedString())")
      print("Advertising _steno._tcp as \"\(name)\"")

      if pair {
        let payload = await service.beginPairing()
        print("\nPairing window open for 5 minutes. Scan this on the phone:")
        print(payload.urlString)
        Self.printQR(payload.urlString)
      } else {
        print("\nRun with --pair to open a pairing window, or pair from the app.")
      }
      print("\nPress Ctrl-C to stop.")

      // Stay up until interrupted; the actor keeps serving on its own tasks.
      while !Task.isCancelled {
        try? await Task.sleep(for: .seconds(3600))
      }
      await service.stop()
    }

    /// The login-keychain identity in the product; where there is no
    /// keychain (the Linux container) a fresh one, minted in memory and
    /// forgotten on exit, for the plaintext loopback listener.
    static func identity(name: String) throws -> HandoverIdentity {
      #if canImport(Security)
        return try IdentityKeychain.loadOrCreate(commonName: "Steno on \(name)")
      #else
        return HandoverIdentity(
          certificateDER: try ServerIdentity.mint(commonName: "Steno on \(name)").certificateDER)
      #endif
    }

    /// Renders the payload as a QR with `qrencode` when it is on PATH; the
    /// macOS app draws the real QR with `CIQRCodeGenerator`.
    static func printQR(_ text: String) {
      let process = Foundation.Process()
      process.executableURL = URL(fileURLWithPath: "/usr/bin/env")
      process.arguments = ["qrencode", "-t", "ANSIUTF8", "-m", "1", text]
      let hint = "(install qrencode to render a scannable QR in the terminal)"
      do {
        try process.run()
        process.waitUntilExit()
        if process.terminationStatus != 0 { print(hint) }
      } catch {
        print(hint)
      }
    }
  }
}
