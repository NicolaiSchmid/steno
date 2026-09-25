import Foundation
import StenoCore

/// The HAL seam under `CaptureSession`: a backend delivers frames for every
/// lane into the `LaneFrameSink` from its own real-time context and reports
/// device loss. `LiveCaptureBackend` is the tap + aggregate + IOProc;
/// `SyntheticCaptureBackend` (Testing/) generates deterministic tones.
public protocol CaptureBackend: Sendable {
  /// Starts delivering `lanes` (in this order) at `StenoAudio.sampleRate`
  /// and describes the stream it opened. `inputDeviceUID` nil selects the
  /// default input device. Throws a `CaptureError` when a device or the tap
  /// cannot be set up.
  func start(lanes: [AudioLane], inputDeviceUID: String?, sink: LaneFrameSink) throws
    -> CaptureStream
  /// Stops delivering; idempotent. No frame arrives after it returns.
  func stop()
}

/// What one started capture delivers, as the backend found it: the rate the
/// device runs at, the device latencies the far-end delay is built from, and
/// where each lane sits in the HAL's buffers. `CaptureSession.stream` keeps
/// it while recording; `steno dev capture-spike` prints it.
public struct CaptureStream: Sendable, Equatable {
  /// The confirmed rate; `StenoAudio.sampleRate` for every backend that
  /// started (the live one fails otherwise).
  public var sampleRate: Double
  /// Latency plus safety offset of the microphone's input path, in frames.
  public var inputLatencyFrames: Int
  /// Latency plus safety offset of the loudspeaker's output path, in frames:
  /// the tap sees a sample this long before the room hears it.
  public var outputLatencyFrames: Int
  /// nil for a backend without HAL buffers (synthetic).
  public var layout: StreamLayout?

  public init(
    sampleRate: Double, inputLatencyFrames: Int, outputLatencyFrames: Int, layout: StreamLayout?
  ) {
    self.sampleRate = sampleRate
    self.inputLatencyFrames = inputLatencyFrames
    self.outputLatencyFrames = outputLatencyFrames
    self.layout = layout
  }

  /// 48 kHz, no latency, no HAL layout.
  public static let synthetic = CaptureStream(
    sampleRate: StenoAudio.sampleRate, inputLatencyFrames: 0, outputLatencyFrames: 0, layout: nil)
}
