import Foundation
import StenoCore

/// The HAL seam under `CaptureSession`: a backend delivers frames for every
/// lane into the `LaneFrameSink` from its own real-time context and reports
/// device changes through the sink. `LiveCaptureBackend` is the tap +
/// aggregate + IOProc; `SyntheticCaptureBackend` (Testing/) generates
/// deterministic tones. The session orchestrates a rebuild after a change by
/// calling `stop()` and `start` again on the same backend and the same sink,
/// so a backend must be restartable.
public protocol CaptureBackend: Sendable {
  /// Starts delivering `lanes` (in this order) and describes the stream it
  /// opened, at `StenoAudio.sampleRate` or, where the device will not run at
  /// it, at the device's own rate, which the processing thread converts.
  /// `inputDeviceUID` nil selects the default input device. Throws a
  /// `CaptureError` when a device or the tap cannot be set up.
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
  /// The confirmed rate the lanes arrive at: `StenoAudio.sampleRate`, except
  /// when the clock master will not run at it (a Bluetooth headset in the
  /// hands-free profile runs at 24, 16 or 8 kHz).
  public var sampleRate: Double
  /// Latency plus safety offset of the microphone's input path, in frames at
  /// `sampleRate` (a microphone on its own clock has its latency rescaled
  /// from its own rate).
  public var inputLatencyFrames: Int
  /// Latency plus safety offset of the loudspeaker's output path, in frames
  /// at `sampleRate`: the tap sees a sample this long before the room hears
  /// it.
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

  /// `samples` counted at the stream's rate as samples at
  /// `StenoAudio.sampleRate`, the rate the processing thread converts to,
  /// rounded down (`rescaled`).
  public func atOutputRate(_ samples: Int) -> Int {
    Self.rescaled(samples, from: sampleRate, to: StenoAudio.sampleRate)
  }

  /// `frames` counted at `from` hertz as frames at `to` hertz, rounded down:
  /// an over-delayed far end is the one error the echo canceller cannot
  /// recover from. Unchanged when the rates are equal or either is not
  /// positive (a rate that could not be read).
  public static func rescaled(_ frames: Int, from: Double, to: Double) -> Int {
    guard from != to, from > 0, to > 0 else { return frames }
    return Int((Double(frames) * to / from).rounded(.down))
  }
}
