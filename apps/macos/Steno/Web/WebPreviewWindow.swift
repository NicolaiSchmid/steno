#if DEBUG
  import StenoBridge
  import StenoCore
  import SwiftUI

  /// Debug only: a web window at `#/shell` over `PreviewBridgeHost`, opened
  /// from Debug > Open Web Preview. Proves the scheme handler, the bridge and
  /// the transparent web view on macOS 15 and 26 before WP2 gives the main
  /// window a real host (plan, WP1).
  struct WebPreviewWindow: Scene {
    static let id = "web-preview"

    var body: some Scene {
      Window("Web Preview", id: Self.id) {
        WebPreviewContent()
      }
      .windowStyle(.hiddenTitleBar)
      .defaultSize(width: 1120, height: 720)
    }
  }

  private struct WebPreviewContent: View {
    @State private var host = PreviewBridgeHost()

    var body: some View {
      WebWindowView(route: "#/shell", host: host)
    }
  }

  /// Answers `page.ready` and `page.layout` with nothing and every other
  /// method with `unknownMethod`; on `page.ready` it publishes the
  /// `BridgeSamples` snapshots, so the shell renders the fixture data the web
  /// tests use.
  @MainActor
  final class PreviewBridgeHost: BridgeHost {
    private weak var events: (any BridgeEventSink)?
    private(set) var requests: [BridgeRequest] = []

    nonisolated init() {}

    func attach(_ events: any BridgeEventSink) {
      self.events = events
    }

    func handle(_ request: BridgeRequest) async throws -> JSONValue? {
      requests.append(request)
      switch request.method {
      case .pageReady:
        for event in try Self.sampleEvents() {
          events?.emit(event)
        }
        return nil
      case .pageLayout:
        return nil
      default:
        throw BridgeError(
          code: .unknownMethod,
          message: "The preview host does not answer \(request.method.rawValue).")
      }
    }

    /// The four topics the shell reads, from the recorded samples.
    static func sampleEvents() throws -> [BridgeEvent] {
      [
        try BridgeEvent(topic: .app, snapshot: BridgeSamples.app),
        try BridgeEvent(topic: .recording, snapshot: BridgeSamples.recordingIdle),
        try BridgeEvent(topic: .meetingsList, snapshot: BridgeSamples.meetingsList),
        try BridgeEvent(topic: .meetingDetail, snapshot: BridgeSamples.meetingDetail),
      ]
    }
  }
#endif
