import Foundation
import StenoCore
import Synchronization
import Testing

@testable import StenoAudio

/// The synthetic backend on a bare sink, without a session: the device
/// change fires once per instance, every `start` delivers its own `seconds`,
/// and a restart reports `streamAfterRestart`.
@Suite struct SyntheticCaptureBackendTests {
  /// Reports counted from the backend's producer thread.
  private final class Reports: Sendable {
    private let count = Mutex(0)
    func add() { count.withLock { $0 += 1 } }
    var value: Int { count.withLock { $0 } }
  }

  @Test func aChangeFiresOncePerInstanceAndEveryStartDeliversItsSeconds() async throws {
    let reports = Reports()
    let sink = LaneFrameSink(lanes: [.mixed]) { _ in reports.add() }
    let restarted = CaptureStream(
      sampleRate: StenoAudio.sampleRate, inputLatencyFrames: 480, outputLatencyFrames: 9_600,
      layout: nil)
    let backend = SyntheticCaptureBackend(
      lanes: [.mixed], tone: [.mixed: 440], seconds: 0.2, changeDeviceAfter: 0.1,
      restartsThatFail: 1, streamAfterRestart: restarted)

    #expect(try backend.start(lanes: [.mixed], inputDeviceUID: nil, sink: sink) == .synthetic)
    await backend.waitUntilFinished()
    #expect(reports.value == 1)
    #expect(backend.framesDelivered == 4_800, "clipped to the change")
    backend.stop()
    sink.clear()

    #expect(throws: CaptureError.inputDeviceUnavailable) {
      try backend.start(lanes: [.mixed], inputDeviceUID: nil, sink: sink)
    }
    #expect(backend.starts == 2)

    // The session rearms the latch before a restart; a change that fired
    // again would be counted here.
    sink.rearmDeviceChange()
    #expect(try backend.start(lanes: [.mixed], inputDeviceUID: nil, sink: sink) == restarted)
    await backend.waitUntilFinished()
    #expect(reports.value == 1, "the one change is spent")
    #expect(backend.framesDelivered == 4_800 + 9_600, "the restart delivers its full 0.2 s")
    #expect(backend.starts == 3)
    backend.stop()
  }
}
