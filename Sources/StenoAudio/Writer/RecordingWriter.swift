import Foundation
import StenoCore

/// One processed frame handed to the writer: `frameCount` samples per lane in
/// the session's lane order, plus the raw microphone when kept. A view over
/// the writer thread's buffers, valid for the duration of `write`.
struct LaneFrames {
  var frameCount: Int
  var lanes: [UnsafePointer<Float>]
  var rawMic: UnsafePointer<Float>?

  init(frameCount: Int, lanes: [UnsafePointer<Float>], rawMic: UnsafePointer<Float>? = nil) {
    self.frameCount = frameCount
    self.lanes = lanes
    self.rawMic = rawMic
  }
}

struct RecordingFiles: Sendable, Equatable, Hashable {
  var master: URL
  var sidecars16k: [AudioLane: URL]
  var rawMic: URL?
  var duration: TimeInterval
}

/// What the writer thread and the session need from the file writer.
/// `RecordingWriter` is the production implementation; tests wrap it to
/// inject the I/O failures a full disk produces.
protocol RecordingWriting: AnyObject, Sendable {
  /// The files and the duration written so far; valid before `finish()`
  /// and after a failed one, so the session can still hand out the asset.
  var files: RecordingFiles { get }
  func write(_ frames: LaneFrames) throws
  @discardableResult
  func finish() throws -> RecordingFiles
}

/// Writes the master (`recording.caf`, 48 kHz Float32, one channel per lane)
/// and one 16 kHz Int16 WAV sidecar per lane through `Resampler48kTo16k`,
/// plus `mic.raw.caf` when asked, all into
/// `RecordingLayout(audioFolder:meetingID:)`. Owned by the writer thread;
/// one `write` per 10 ms frame, `finish()` patches sizes and returns the
/// files. All scratch buffers are allocated in `init`.
///
/// The sidecars lag the master by the resampler's group delay: the 192-tap
/// linear-phase FIR delays by 95.5 input samples, so every sidecar sample
/// sits 2.0 ms (32 samples at 16 kHz) after the master sample it belongs to.
/// Segment times taken from a sidecar are 2 ms late against the master and
/// against lanes `AVFoundationAudioCodec` decodes from it; harmless for
/// transcripts, and deliberate, so do not "fix" a 2 ms offset by hand.
final class RecordingWriter: RecordingWriting, @unchecked Sendable {
  let layout: RecordingLayout
  let lanes: [AudioLane]
  private let master: CAFStreamWriter
  private let sidecars: [WAVStreamWriter]
  private let resamplers: [Resampler48kTo16k]
  private let rawMic: CAFStreamWriter?
  private let interleaved: UnsafeMutablePointer<Float>
  private let sidecarScratch: UnsafeMutablePointer<Int16>
  private var isFinished = false

  /// `layout.directory` is created if needed.
  init(layout: RecordingLayout, lanes: [AudioLane], keepRawMic: Bool = false) throws {
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
  func write(_ frames: LaneFrames) throws {
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
      lane += 1
    }
    // The master first: when the disk fills on this frame, the recoverable
    // copy is never shorter than a sidecar.
    try master.write(interleaved: interleaved, frameCount: frameSize)
    lane = 0
    while lane < laneCount {
      resamplers[lane].process(frames.lanes[lane], into: sidecarScratch)
      try sidecars[lane].write(sidecarScratch, count: frameSize / Resampler48kTo16k.factor)
      lane += 1
    }
    if let rawMic, let raw = frames.rawMic {
      try rawMic.write(interleaved: raw, frameCount: frameSize)
    }
  }

  /// The URLs are fixed at `init`; the duration is what the master holds.
  var files: RecordingFiles {
    var sidecarURLs: [AudioLane: URL] = [:]
    for (lane, sidecar) in zip(lanes, sidecars) { sidecarURLs[lane] = sidecar.url }
    return RecordingFiles(
      master: master.url, sidecars16k: sidecarURLs, rawMic: rawMic?.url, duration: master.duration)
  }

  /// Closes every file, the master first. A failure on one file still
  /// closes the others before it is rethrown.
  @discardableResult
  func finish() throws -> RecordingFiles {
    guard !isFinished else { throw CaptureError.writerFailed("finish called twice") }
    isFinished = true
    var firstError: (any Error)?
    func attempt(_ close: () throws -> Void) {
      do { try close() } catch { firstError = firstError ?? error }
    }
    attempt { try master.finish() }
    for sidecar in sidecars { attempt { try sidecar.finish() } }
    attempt { try rawMic?.finish() }
    if let firstError { throw firstError }
    return files
  }
}
