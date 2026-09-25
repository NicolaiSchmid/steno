import Foundation
import StenoCore

/// Limits on what a phone may announce. Everything here is checked before a
/// partial file is created, so a bad announce costs nothing on disk.
enum MetadataValidation {
  /// 4 GiB: about 140 hours at the phone's 64 kbps preset.
  static let maxByteCount: Int64 = 4 * 1024 * 1024 * 1024
  static let minChunkSize = 64 * 1024
  /// Seven days; the phone records to a file and uploads later.
  static let maxDurationSeconds: Double = 7 * 24 * 3600
  static let maxDeviceNameLength = 128
  /// Formats the intake can place and the pipeline can decode.
  static let acceptedFormats: Set<AudioFormat> = [.m4aAAC, .wav16kInt16]

  /// The first problem with `metadata`, or nil when it is acceptable.
  static func problem(with metadata: RecordingMetadata, configuration: HandoverConfiguration)
    -> String?
  {
    if metadata.byteCount <= 0 || metadata.byteCount > maxByteCount {
      return "byteCount must be between 1 and \(maxByteCount)"
    }
    if metadata.chunkSize < minChunkSize || metadata.chunkSize > configuration.chunkSize {
      return "chunkSize must be between \(minChunkSize) and \(configuration.chunkSize)"
    }
    if metadata.sha256.count != 32 {
      return "sha256 must be 32 bytes"
    }
    if !metadata.durationSeconds.isFinite || metadata.durationSeconds < 0
      || metadata.durationSeconds > maxDurationSeconds
    {
      return "durationSeconds must be between 0 and \(Int(maxDurationSeconds))"
    }
    let name = metadata.deviceName.trimmingCharacters(in: .whitespacesAndNewlines)
    if name.isEmpty || name.count > maxDeviceNameLength {
      return "deviceName must be 1 to \(maxDeviceNameLength) characters"
    }
    if !acceptedFormats.contains(metadata.format) {
      return "format \(metadata.format.rawValue) is not accepted"
    }
    return nil
  }

  /// How many chunks a recording of `byteCount` bytes has at `chunkSize`.
  static func chunkCount(byteCount: Int64, chunkSize: Int) -> Int {
    Int((byteCount + Int64(chunkSize) - 1) / Int64(chunkSize))
  }

  /// The byte length of chunk `index`; the last chunk may be short.
  static func chunkLength(index: Int, byteCount: Int64, chunkSize: Int) -> Int {
    let offset = Int64(index) * Int64(chunkSize)
    return Int(min(Int64(chunkSize), byteCount - offset))
  }
}
