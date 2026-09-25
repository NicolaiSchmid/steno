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
  case tapCreationFailed(Int32)
  case aggregateCreationFailed(Int32)
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
    case .tapCreationFailed(let status): "creating the process tap failed (\(status))"
    case .aggregateCreationFailed(let status): "creating the aggregate device failed (\(status))"
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

public enum CaptureState: Sendable, Equatable, Hashable {
  case idle
  case starting
  case recording(startedAt: Date)
  case stopping
  case failed(CaptureError)
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
  public var deviceChanges: Int

  public init(
    duration: TimeInterval, droppedFrames: [AudioLane: Int], systemLaneSilent: Bool,
    deviceChanges: Int
  ) {
    self.duration = duration
    self.droppedFrames = droppedFrames
    self.systemLaneSilent = systemLaneSilent
    self.deviceChanges = deviceChanges
  }
}
