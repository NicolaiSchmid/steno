import Foundation
import StenoCore

public enum CodecError: Error, Sendable, Equatable, CustomStringConvertible {
  case laneNotInAsset(AudioLane)
  case channelMissing(lane: AudioLane, channel: Int, channels: Int)
  case unsupportedPlatform
  case conversionFailed(String)

  public var description: String {
    switch self {
    case .laneNotInAsset(let lane): "the asset has no \(lane.rawValue) lane"
    case .channelMissing(let lane, let channel, let channels):
      "lane \(lane.rawValue) is channel \(channel) but the file has \(channels)"
    case .unsupportedPlatform: "decoding CAF and m4a needs macOS (AVFoundation)"
    case .conversionFailed(let detail): "audio conversion failed: \(detail)"
    }
  }
}

#if canImport(AVFoundation)
  import AVFoundation

  /// The program's `AudioDecoder` for real recordings: `decode(_:lane:)`
  /// returns the lane's 16 kHz sidecar when it is present and complete, else
  /// reads the master (CAF, m4a or WAV) through `AVAudioFile` and resamples
  /// channel n (lane n of the master) to 16 kHz mono with `AVAudioConverter`,
  /// in chunks so a two-hour file never sits in memory at 48 kHz. `mixdown`
  /// averages the lanes to mono and writes AAC 64 kbps `.m4a` through
  /// `AVAudioFile`; `.m4aAAC` inputs are copied.
  public struct AVFoundationAudioCodec: AudioDecoder, Sendable {
    static let chunkFrames: AVAudioFrameCount = 32_768

    public init() {}

    public var mixdownFormat: AudioFormat { .m4aAAC }

    public func decode(_ asset: AudioAsset, lane: AudioLane) async throws -> AudioBuffer16k {
      if let sidecar = asset.sidecars16k[lane],
        let buffer = try? WAVAudioDecoder.read(sidecar), !buffer.samples.isEmpty
      {
        return buffer
      }
      guard let channel = asset.lanes.firstIndex(of: lane) else {
        throw CodecError.laneNotInAsset(lane)
      }
      return try Self.decode(url: asset.url, channel: channel, lane: lane)
    }

    /// Channel `channel` of `url`, resampled to 16 kHz mono.
    public static func decode(url: URL, channel: Int, lane: AudioLane) throws -> AudioBuffer16k {
      let file = try AVAudioFile(
        forReading: url, commonFormat: .pcmFormatFloat32, interleaved: false)
      let source = file.processingFormat
      let channels = Int(source.channelCount)
      guard channel < channels else {
        throw CodecError.channelMissing(lane: lane, channel: channel, channels: channels)
      }
      guard
        let mono = AVAudioFormat(
          commonFormat: .pcmFormatFloat32, sampleRate: source.sampleRate, channels: 1,
          interleaved: false),
        let target = AVAudioFormat(
          commonFormat: .pcmFormatFloat32, sampleRate: AudioBuffer16k.sampleRate, channels: 1,
          interleaved: false),
        let fileBuffer = AVAudioPCMBuffer(pcmFormat: source, frameCapacity: chunkFrames),
        let monoBuffer = AVAudioPCMBuffer(pcmFormat: mono, frameCapacity: chunkFrames)
      else { throw CodecError.conversionFailed("could not allocate conversion buffers") }

      var samples: [Float] = []
      samples.reserveCapacity(
        Int(Double(file.length) * AudioBuffer16k.sampleRate / source.sampleRate) + 16)

      // Reads the next chunk and extracts the channel; nil at the end.
      let reader = ChunkReader(
        file: file, fileBuffer: fileBuffer, monoBuffer: monoBuffer, channel: channel)

      if source.sampleRate == AudioBuffer16k.sampleRate {
        while let chunk = try reader.next() {
          samples.append(
            contentsOf: UnsafeBufferPointer(
              start: chunk.floatChannelData![0], count: Int(chunk.frameLength)))
        }
        return AudioBuffer16k(samples: samples)
      }

      guard let converter = AVAudioConverter(from: mono, to: target),
        let output = AVAudioPCMBuffer(pcmFormat: target, frameCapacity: chunkFrames)
      else { throw CodecError.conversionFailed("no converter from \(source.sampleRate) Hz") }
      converter.sampleRateConverterQuality = AVAudioQuality.max.rawValue

      var readError: (any Error)?
      var ended = false
      let input: AVAudioConverterInputBlock = { _, status in
        if ended {
          status.pointee = .endOfStream
          return nil
        }
        do {
          if let chunk = try reader.next() {
            status.pointee = .haveData
            return chunk
          }
        } catch {
          readError = error
        }
        ended = true
        status.pointee = .endOfStream
        return nil
      }
      while true {
        var error: NSError?
        let status = converter.convert(to: output, error: &error, withInputFrom: input)
        if let readError { throw readError }
        if let error { throw CodecError.conversionFailed(error.localizedDescription) }
        if output.frameLength > 0 {
          samples.append(
            contentsOf: UnsafeBufferPointer(
              start: output.floatChannelData![0], count: Int(output.frameLength)))
        }
        output.frameLength = 0
        if status == .endOfStream || status == .error { break }
        if status == .inputRanDry && ended { break }
      }
      // The converter may emit a few samples of filter tail or swallow a few
      // of priming; trim or pad to the exact length so the 16 kHz lane lasts
      // exactly as long as the master and segment counts stay stable.
      let expected = Int(
        (Double(file.length) * AudioBuffer16k.sampleRate / source.sampleRate).rounded())
      if samples.count > expected {
        samples.removeLast(samples.count - expected)
      } else if samples.count < expected {
        samples.append(contentsOf: repeatElement(0, count: expected - samples.count))
      }
      return AudioBuffer16k(samples: samples)
    }

    public func mixdown(_ asset: AudioAsset, to url: URL) async throws {
      try FileManager.default.createDirectory(
        at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
      if FileManager.default.fileExists(atPath: url.path) {
        try FileManager.default.removeItem(at: url)
      }
      if asset.format == .m4aAAC {
        try FileManager.default.copyItem(at: asset.url, to: url)
        return
      }
      try Self.mixdown(url: asset.url, to: url)
    }

    /// Averages every channel of `source` to mono and writes AAC 64 kbps.
    public static func mixdown(url source: URL, to destination: URL, bitRate: Int = 64_000) throws {
      let file = try AVAudioFile(
        forReading: source, commonFormat: .pcmFormatFloat32, interleaved: false)
      let format = file.processingFormat
      let channels = Int(format.channelCount)
      guard
        let mono = AVAudioFormat(
          commonFormat: .pcmFormatFloat32, sampleRate: format.sampleRate, channels: 1,
          interleaved: false),
        let input = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: chunkFrames),
        let output = AVAudioPCMBuffer(pcmFormat: mono, frameCapacity: chunkFrames)
      else { throw CodecError.conversionFailed("could not allocate mixdown buffers") }
      let settings: [String: Any] = [
        AVFormatIDKey: kAudioFormatMPEG4AAC,
        AVSampleRateKey: format.sampleRate,
        AVNumberOfChannelsKey: 1,
        AVEncoderBitRateKey: bitRate,
      ]
      let writer = try AVAudioFile(
        forWriting: destination, settings: settings, commonFormat: .pcmFormatFloat32,
        interleaved: false)
      let scale = 1 / Float(max(channels, 1))
      while file.framePosition < file.length {
        let remaining = AVAudioFrameCount(file.length - file.framePosition)
        try file.read(into: input, frameCount: min(chunkFrames, remaining))
        let frames = Int(input.frameLength)
        guard frames > 0 else { break }
        let out = output.floatChannelData![0]
        out.update(repeating: 0, count: frames)
        for channel in 0..<channels {
          let source = input.floatChannelData![channel]
          for index in 0..<frames { out[index] += source[index] * scale }
        }
        output.frameLength = AVAudioFrameCount(frames)
        try writer.write(from: output)
      }
    }

    /// Reads `chunkFrames` at a time and copies one channel into a mono buffer.
    private final class ChunkReader {
      let file: AVAudioFile
      let fileBuffer: AVAudioPCMBuffer
      let monoBuffer: AVAudioPCMBuffer
      let channel: Int

      init(
        file: AVAudioFile, fileBuffer: AVAudioPCMBuffer, monoBuffer: AVAudioPCMBuffer, channel: Int
      ) {
        self.file = file
        self.fileBuffer = fileBuffer
        self.monoBuffer = monoBuffer
        self.channel = channel
      }

      func next() throws -> AVAudioPCMBuffer? {
        guard file.framePosition < file.length else { return nil }
        let remaining = AVAudioFrameCount(file.length - file.framePosition)
        try file.read(
          into: fileBuffer, frameCount: min(AVFoundationAudioCodec.chunkFrames, remaining))
        let frames = Int(fileBuffer.frameLength)
        guard frames > 0 else { return nil }
        monoBuffer.floatChannelData![0].update(
          from: fileBuffer.floatChannelData![channel], count: frames)
        monoBuffer.frameLength = AVAudioFrameCount(frames)
        return monoBuffer
      }
    }
  }
#else
  /// Without AVFoundation the codec exists so callers compile; `decode` still
  /// serves sidecars and 16 kHz WAV masters through core's reader, everything
  /// else throws `CodecError.unsupportedPlatform`.
  public struct AVFoundationAudioCodec: AudioDecoder, Sendable {
    public init() {}

    public var mixdownFormat: AudioFormat { .m4aAAC }

    public func decode(_ asset: AudioAsset, lane: AudioLane) async throws -> AudioBuffer16k {
      if let sidecar = asset.sidecars16k[lane] { return try WAVAudioDecoder.read(sidecar) }
      if asset.format == .wav16kInt16 { return try WAVAudioDecoder.read(asset.url) }
      throw CodecError.unsupportedPlatform
    }

    public func mixdown(_ asset: AudioAsset, to url: URL) async throws {
      throw CodecError.unsupportedPlatform
    }
  }
#endif
