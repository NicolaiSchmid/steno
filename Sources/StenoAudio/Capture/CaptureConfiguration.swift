import Foundation
import StenoCore

/// Call: two lanes, `.mic` ("me") and `.system` ("them") with echo
/// cancellation. In person: one `.mixed` room lane from the microphone, no
/// tap, no echo cancellation.
public enum CaptureMode: String, Sendable, Equatable, Hashable, CaseIterable {
  case call
  case inPerson

  /// The lanes a session in this mode records, in master channel order.
  public var lanes: [AudioLane] {
    switch self {
    case .call: [.mic, .system]
    case .inPerson: [.mixed]
    }
  }
}

public struct CaptureConfiguration: Sendable, Equatable {
  public var mode: CaptureMode
  /// nil: the default input device.
  public var inputDeviceUID: String?
  /// Default true in `.call`; ignored in `.inPerson`.
  public var echoCancellation: Bool
  /// Debug: writes `mic.raw.caf` (the microphone before echo cancellation)
  /// next to the master.
  public var keepRawMicLane: Bool
  /// The audio folder; the per-meeting folder
  /// (`RecordingLayout(audioFolder:meetingID:)`) is created inside it.
  public var outputDirectory: URL
  /// Developer tools only: record these lanes instead of the mode's
  /// (`[.system]` for the Continuity spike). The app never sets it.
  public var laneOverride: [AudioLane]?

  public init(
    mode: CaptureMode,
    inputDeviceUID: String? = nil,
    echoCancellation: Bool = true,
    keepRawMicLane: Bool = false,
    outputDirectory: URL,
    laneOverride: [AudioLane]? = nil
  ) {
    self.mode = mode
    self.inputDeviceUID = inputDeviceUID
    self.echoCancellation = echoCancellation
    self.keepRawMicLane = keepRawMicLane
    self.outputDirectory = outputDirectory
    self.laneOverride = laneOverride
  }

  /// The lanes a session records, in master channel order.
  public var lanes: [AudioLane] { laneOverride ?? mode.lanes }

  /// Echo cancellation runs only with both a mic and a system lane.
  public var usesEchoCancellation: Bool {
    echoCancellation && lanes.contains(.mic) && lanes.contains(.system)
  }
}

public enum CaptureError: Error, Sendable, Equatable, Hashable, CustomStringConvertible {
  /// A Core Audio call failed: which one, and its `OSStatus` (rendered as
  /// the four-character code when it is one). Creating the tap or the
  /// aggregate, adding or starting the IOProc.
  case coreAudio(operation: String, status: Int32)
  case inputDeviceUnavailable
  case outputDeviceUnavailable
  /// The aggregate's input streams did not match the expected lanes.
  case unexpectedStreamLayout(String)
  /// The aggregate would not run at `StenoAudio.sampleRate` (the output
  /// device is fixed at another rate); the user changes it in Audio MIDI
  /// Setup or picks another output.
  case sampleRateMismatch(actual: Double)
  /// The tap never rose above `LaneLevel.silentPeakLinear` during the whole
  /// session.
  case systemAudioSilent
  case deviceLost
  case writerFailed(String)
  /// A backend error that is none of the above (its description).
  case backendFailed(String)
  /// `start` while not idle, `stop` while not recording.
  case invalidState(String)

  public var description: String {
    switch self {
    case .coreAudio(let operation, let status): "\(operation) failed: \(fourCharCode(status))"
    case .inputDeviceUnavailable: "the input device is not available"
    case .outputDeviceUnavailable: "the output device is not available"
    case .unexpectedStreamLayout(let detail): "unexpected input stream layout: \(detail)"
    case .sampleRateMismatch(let actual):
      "the audio devices run at \(Int(actual)) Hz, not \(Int(StenoAudio.sampleRate)) Hz"
    case .systemAudioSilent: "the system lane stayed silent"
    case .deviceLost: "an audio device disappeared"
    case .writerFailed(let detail): "writing the recording failed: \(detail)"
    case .backendFailed(let detail): "capture backend failed: \(detail)"
    case .invalidState(let detail): detail
    }
  }
}

/// Renders an `OSStatus` as its four-character code when it is one
/// (`'!obj'`, `'who?'`), else as the number.
func fourCharCode(_ status: Int32) -> String {
  let value = UInt32(bitPattern: status)
  let bytes = [
    UInt8((value >> 24) & 0xff), UInt8((value >> 16) & 0xff), UInt8((value >> 8) & 0xff),
    UInt8(value & 0xff),
  ]
  guard bytes.allSatisfy({ $0 >= 0x20 && $0 < 0x7f }) else { return String(status) }
  return "'" + String(decoding: bytes, as: UTF8.self) + "'"
}

/// What `stop()` returns: the finished master with its sidecars (retention
/// `.keepForever` until the caller sets it from `Settings`) and the
/// session's statistics.
public struct CaptureResult: Sendable, Equatable, Hashable {
  public var asset: AudioAsset
  public var statistics: CaptureStatistics

  public init(asset: AudioAsset, statistics: CaptureStatistics) {
    self.asset = asset
    self.statistics = statistics
  }
}

public enum CaptureState: Sendable, Equatable, Hashable {
  case idle
  case starting
  case recording(startedAt: Date)
  case stopping
  /// `recording` is nil when the start produced nothing, and the finalised
  /// partial recording when a device disappeared or the writer failed
  /// mid-meeting; `stop()` returns the same value or throws when it is nil.
  case failed(CaptureError, recording: CaptureResult?)

  /// The failure, when in `.failed`.
  public var failure: CaptureError? {
    if case .failed(let error, _) = self { return error }
    return nil
  }
}

/// RMS and peak of one lane over the last metering window, in dBFS.
public struct LaneLevel: Sendable, Equatable, Hashable {
  public var rms: Float
  public var peak: Float

  public init(rms: Float, peak: Float) {
    self.rms = rms
    self.peak = peak
  }

  /// Digital silence: the floor every meter reports for zeros.
  public static let silence = LaneLevel(rms: -160, peak: -160)

  /// -80 dBFS, linear: a lane whose peak never exceeds it is "silent" for
  /// `CaptureStatistics.systemLaneSilent` and the permission probe.
  public static let silentPeakLinear: Float = 1e-4
}

/// Published at 10 Hz; `system` is nil in `.inPerson`.
public struct LaneLevels: Sendable, Equatable, Hashable {
  public var mic: LaneLevel
  public var system: LaneLevel?

  public init(mic: LaneLevel, system: LaneLevel?) {
    self.mic = mic
    self.system = system
  }
}

public struct CaptureStatistics: Sendable, Equatable, Hashable {
  /// Seconds of audio written to the master.
  public var duration: TimeInterval
  /// Frames lost per lane to ring overruns or a stalled writer; should be
  /// empty.
  public var droppedFrames: [AudioLane: Int]
  /// True when the tap never exceeded `LaneLevel.silentPeakLinear` (-80 dBFS).
  public var systemLaneSilent: Bool
  /// True when a device disappeared and the session finalised the recording
  /// early (v1 stops on the first loss; rebuilding mid-meeting is v1.1).
  public var endedOnDeviceLoss: Bool

  public init(
    duration: TimeInterval, droppedFrames: [AudioLane: Int], systemLaneSilent: Bool,
    endedOnDeviceLoss: Bool
  ) {
    self.duration = duration
    self.droppedFrames = droppedFrames
    self.systemLaneSilent = systemLaneSilent
    self.endedOnDeviceLoss = endedOnDeviceLoss
  }
}
