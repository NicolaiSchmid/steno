import Foundation
import StenoCore

/// Reads any PCM RIFF/WAVE file (8/16/24/32-bit integer or 32-bit float, any
/// rate, any channel count) into de-interleaved channels. For `steno dev
/// aec-bench` and tests on 48 kHz material; core's `WAVAudioDecoder` stays
/// the strict 16 kHz mono reader the pipeline uses.
public struct WAVFile: Sendable, Equatable {
  public var sampleRate: Double
  public var channels: [[Float]]

  public var frameCount: Int { channels.first?.count ?? 0 }
  public var duration: TimeInterval { Double(frameCount) / sampleRate }

  public static func read(_ url: URL) throws -> WAVFile {
    try read(Data(contentsOf: url))
  }

  public static func read(_ data: Data) throws -> WAVFile {
    guard data.count >= 12, tag(data, 0) == "RIFF", tag(data, 8) == "WAVE" else {
      throw WAVDecodeError.malformed("missing RIFF/WAVE tags")
    }
    var offset = 12
    var format: (tag: UInt16, channels: Int, rate: Int, bits: Int)?
    var samples: Range<Int>?
    while offset + 8 <= data.count {
      let id = tag(data, offset)
      let size = Int(uint32(data, offset + 4))
      let body = offset + 8
      guard body + size <= data.count else {
        throw WAVDecodeError.malformed("chunk \(id) runs past the end of the file")
      }
      switch id {
      case "fmt ":
        guard size >= 16 else { throw WAVDecodeError.malformed("fmt chunk too short") }
        var formatTag = uint16(data, body)
        if formatTag == 0xFFFE, size >= 26 { formatTag = uint16(data, body + 24) }
        format = (
          formatTag, Int(uint16(data, body + 2)), Int(uint32(data, body + 4)),
          Int(uint16(data, body + 14))
        )
      case "data":
        samples = body..<(body + size)
      default:
        break
      }
      offset = body + size + (size % 2)
    }
    guard let format else { throw WAVDecodeError.malformed("no fmt chunk") }
    guard let samples else { throw WAVDecodeError.malformed("no data chunk") }
    let isFloat: Bool
    switch format.tag {
    case 1: isFloat = false
    case 3: isFloat = true
    default: throw WAVDecodeError.unsupportedFormat("audio format tag \(format.tag)")
    }
    guard [8, 16, 24, 32].contains(format.bits), !isFloat || format.bits == 32 else {
      throw WAVDecodeError.unsupportedFormat("\(format.bits)-bit \(isFloat ? "float" : "integer")")
    }
    let channelCount = max(1, format.channels)
    let bytesPerSample = format.bits / 8
    let bytesPerFrame = bytesPerSample * channelCount
    let frames = samples.count / bytesPerFrame
    var channels = [[Float]](repeating: [Float](repeating: 0, count: frames), count: channelCount)
    data.withUnsafeBytes { raw in
      for frame in 0..<frames {
        for channel in 0..<channelCount {
          let at = samples.lowerBound + frame * bytesPerFrame + channel * bytesPerSample
          let value: Float
          switch (isFloat, format.bits) {
          case (true, _):
            value = Float(
              bitPattern: UInt32(
                littleEndian: raw.loadUnaligned(fromByteOffset: at, as: UInt32.self)))
          case (false, 8):
            value = (Float(raw[at]) - 128) / 128
          case (false, 16):
            value =
              Float(
                Int16(
                  bitPattern: UInt16(
                    littleEndian: raw.loadUnaligned(fromByteOffset: at, as: UInt16.self)))) / 32768
          case (false, 24):
            let raw24 = Int32(raw[at]) | Int32(raw[at + 1]) << 8 | Int32(raw[at + 2]) << 16
            value = Float(raw24 >= 0x80_0000 ? raw24 - 0x100_0000 : raw24) / 8_388_608
          default:
            value =
              Float(
                Int32(
                  bitPattern: UInt32(
                    littleEndian: raw.loadUnaligned(fromByteOffset: at, as: UInt32.self))))
              / 2_147_483_648
          }
          channels[channel][frame] = value
        }
      }
    }
    return WAVFile(sampleRate: Double(format.rate), channels: channels)
  }

  private static func tag(_ data: Data, _ offset: Int) -> String {
    String(
      decoding: data.subdata(in: (data.startIndex + offset)..<(data.startIndex + offset + 4)),
      as: UTF8.self)
  }

  private static func uint16(_ data: Data, _ offset: Int) -> UInt16 {
    UInt16(data[data.startIndex + offset]) | UInt16(data[data.startIndex + offset + 1]) << 8
  }

  private static func uint32(_ data: Data, _ offset: Int) -> UInt32 {
    UInt32(uint16(data, offset)) | UInt32(uint16(data, offset + 2)) << 16
  }
}

/// Reads a 48 kHz lane from a CAF or WAV file for the bench tools:
/// `path` or `path:channel`.
public enum LaneFileReader {
  public static func read(_ argument: String) throws -> (samples: [Float], sampleRate: Double) {
    var path = argument
    var channel = 0
    if let colon = argument.lastIndex(of: ":"),
      let index = Int(argument[argument.index(after: colon)...])
    {
      path = String(argument[..<colon])
      channel = index
    }
    let url = URL(fileURLWithPath: path)
    let channels: [[Float]]
    let rate: Double
    if url.pathExtension.lowercased() == "caf" {
      let file = try CAFFile.read(url)
      channels = file.channels
      rate = file.sampleRate
    } else {
      let file = try WAVFile.read(url)
      channels = file.channels
      rate = file.sampleRate
    }
    guard channel < channels.count else {
      throw CodecError.channelMissing(lane: .mixed, channel: channel, channels: channels.count)
    }
    return (channels[channel], rate)
  }
}
