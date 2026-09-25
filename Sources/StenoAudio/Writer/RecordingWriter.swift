import Foundation
import StenoCore

/// One processed frame handed to the writer: `frameCount` samples per lane in
/// the session's lane order, plus the raw microphone when kept. A view over
/// the writer thread's buffers, valid for the duration of `write`.
public struct LaneFrames {
  public var frameCount: Int
  public var lanes: [UnsafePointer<Float>]
  public var rawMic: UnsafePointer<Float>?

  public init(frameCount: Int, lanes: [UnsafePointer<Float>], rawMic: UnsafePointer<Float>? = nil) {
    self.frameCount = frameCount
    self.lanes = lanes
    self.rawMic = rawMic
  }
}

public struct RecordingFiles: Sendable, Equatable, Hashable {
  public var master: URL
  public var sidecars16k: [AudioLane: URL]
  public var rawMic: URL?
  public var duration: TimeInterval

  public init(master: URL, sidecars16k: [AudioLane: URL], rawMic: URL?, duration: TimeInterval) {
    self.master = master
    self.sidecars16k = sidecars16k
    self.rawMic = rawMic
    self.duration = duration
  }
}

/// Writes the master (`recording.caf`, 48 kHz Float32, one channel per lane)
/// and one 16 kHz Int16 WAV sidecar per lane through `Resampler48kTo16k`,
/// plus `mic.raw.caf` when asked, all into
/// `RecordingLayout(audioFolder:meetingID:)`. Owned by the writer thread;
/// one `write` per 10 ms frame, `finish()` patches sizes and returns the
/// files. All scratch buffers are allocated in `init`.
public final class RecordingWriter: @unchecked Sendable {
  public let layout: RecordingLayout
  public let lanes: [AudioLane]
  private let master: CAFStreamWriter
  private let sidecars: [WAVStreamWriter]
  private let resamplers: [Resampler48kTo16k]
  private let rawMic: CAFStreamWriter?
  private let interleaved: UnsafeMutablePointer<Float>
  private let sidecarScratch: UnsafeMutablePointer<Int16>
  private var isFinished = false

  /// `layout.directory` is created if needed.
  public init(layout: RecordingLayout, lanes: [AudioLane], keepRawMic: Bool = false) throws {
    precondition(!lanes.isEmpty)
    let frameSize = StenoAudio.frameSize
    self.layout = layout
    self.lanes = lanes
    try layout.createDirectories()
    master = try CAFStreamWriter(
      url: layout.master(.caf48kFloat32), sampleRate: StenoAudio.sampleRate, channels: lanes.count)
    sidecars = try lanes.map { try WAVStreamWriter(url: layout.sidecar($0)) }
    resamplers = lanes.map { _ in Resampler48kTo16k(frameSize: frameSize) }
    rawMic =
      keepRawMic && lanes.contains(.mic)
      ? try CAFStreamWriter(
        url: layout.directory.appendingPathComponent("mic.raw.caf"),
        sampleRate: StenoAudio.sampleRate, channels: 1) : nil
    interleaved = .allocate(capacity: frameSize * lanes.count)
    interleaved.initialize(repeating: 0, count: frameSize * lanes.count)
    sidecarScratch = .allocate(capacity: frameSize / Resampler48kTo16k.factor)
    sidecarScratch.initialize(repeating: 0, count: frameSize / Resampler48kTo16k.factor)
  }

  deinit {
    interleaved.deallocate()
    sidecarScratch.deallocate()
  }

  /// `frames.frameCount` must equal `StenoAudio.frameSize`.
  public func write(_ frames: LaneFrames) throws {
    let frameSize = StenoAudio.frameSize
    guard !isFinished else { throw CaptureError.writerFailed("write after finish") }
    guard frames.frameCount == frameSize, frames.lanes.count == lanes.count else {
      throw CaptureError.writerFailed(
        "expected \(frameSize) frames on \(lanes.count) lanes, got \(frames.frameCount) on \(frames.lanes.count)"
      )
    }
    let laneCount = lanes.count
    var lane = 0
    while lane < laneCount {
      let source = frames.lanes[lane]
      var index = 0
      while index < frameSize {
        interleaved[index * laneCount + lane] = source[index]
        index += 1
      }
      resamplers[lane].process(source, into: sidecarScratch)
      try sidecars[lane].write(sidecarScratch, count: frameSize / Resampler48kTo16k.factor)
      lane += 1
    }
    try master.write(interleaved: interleaved, frameCount: frameSize)
    if let rawMic, let raw = frames.rawMic {
      try rawMic.write(interleaved: raw, frameCount: frameSize)
    }
  }

  public func finish() throws -> RecordingFiles {
    guard !isFinished else { throw CaptureError.writerFailed("finish called twice") }
    isFinished = true
    try master.finish()
    for sidecar in sidecars { try sidecar.finish() }
    try rawMic?.finish()
    var sidecarURLs: [AudioLane: URL] = [:]
    for (lane, sidecar) in zip(lanes, sidecars) { sidecarURLs[lane] = sidecar.url }
    return RecordingFiles(
      master: master.url, sidecars16k: sidecarURLs, rawMic: rawMic?.url, duration: master.duration)
  }
}
