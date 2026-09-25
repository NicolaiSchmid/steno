import Foundation
import StenoCore
import Testing

@testable import StenoHandover

/// Every bound `MetadataValidation` enforces before a partial file exists,
/// at the edge on both sides. The route test (`ChunkUploadTests`) proves a
/// rejected announce is 400 with nothing on disk; this pins the limits.
@Suite struct MetadataValidationTests {
  static let configuration = HandoverConfiguration(
    serviceName: "Test Mac", advertise: false, chunkSize: 1024 * 1024,
    inboxDirectory: URL(fileURLWithPath: "/nonexistent/inbox"), pairingWindow: .seconds(300))

  static let valid = RecordingMetadata(
    recordingID: UUID(), startedAt: Date(timeIntervalSince1970: 1_790_000_000),
    durationSeconds: 61.5, byteCount: 3_000_000, sha256: Data(repeating: 1, count: 32),
    chunkSize: 1024 * 1024, format: .m4aAAC, deviceName: "iPhone")

  static func problem(_ change: (inout RecordingMetadata) -> Void) -> String? {
    var metadata = valid
    change(&metadata)
    return MetadataValidation.problem(with: metadata, configuration: configuration)
  }

  @Test func theSampleIsAccepted() {
    #expect(Self.problem { _ in } == nil)
  }

  @Test func byteCountIsOneToFourGiB() {
    #expect(Self.problem { $0.byteCount = 0 }?.contains("byteCount") == true)
    #expect(Self.problem { $0.byteCount = -1 }?.contains("byteCount") == true)
    #expect(Self.problem { $0.byteCount = 1 } == nil)
    #expect(Self.problem { $0.byteCount = MetadataValidation.maxByteCount } == nil)
    #expect(
      Self.problem { $0.byteCount = MetadataValidation.maxByteCount + 1 }?.contains("byteCount")
        == true)
  }

  @Test func chunkSizeIs64KiBToTheMacsLimit() {
    #expect(
      Self.problem { $0.chunkSize = MetadataValidation.minChunkSize - 1 }?.contains("chunkSize")
        == true)
    #expect(Self.problem { $0.chunkSize = MetadataValidation.minChunkSize } == nil)
    #expect(Self.problem { $0.chunkSize = Self.configuration.chunkSize } == nil)
    #expect(
      Self.problem { $0.chunkSize = Self.configuration.chunkSize + 1 }?.contains("chunkSize")
        == true)
    #expect(Self.problem { $0.chunkSize = 0 }?.contains("chunkSize") == true)
  }

  @Test func sha256IsExactly32Bytes() {
    #expect(Self.problem { $0.sha256 = Data(repeating: 1, count: 31) }?.contains("sha256") == true)
    #expect(Self.problem { $0.sha256 = Data(repeating: 1, count: 33) }?.contains("sha256") == true)
    #expect(Self.problem { $0.sha256 = Data() }?.contains("sha256") == true)
  }

  @Test func durationIsZeroToSevenDaysAndFinite() {
    #expect(Self.problem { $0.durationSeconds = 0 } == nil)
    #expect(Self.problem { $0.durationSeconds = MetadataValidation.maxDurationSeconds } == nil)
    #expect(Self.problem { $0.durationSeconds = -0.5 }?.contains("durationSeconds") == true)
    #expect(
      Self.problem { $0.durationSeconds = MetadataValidation.maxDurationSeconds + 1 }?
        .contains("durationSeconds") == true)
    #expect(Self.problem { $0.durationSeconds = .nan }?.contains("durationSeconds") == true)
    #expect(Self.problem { $0.durationSeconds = .infinity }?.contains("durationSeconds") == true)
  }

  @Test func deviceNameIsOneTo128CharactersAfterTrimming() {
    #expect(Self.problem { $0.deviceName = "" }?.contains("deviceName") == true)
    #expect(Self.problem { $0.deviceName = " \n\t" }?.contains("deviceName") == true)
    #expect(Self.problem { $0.deviceName = String(repeating: "x", count: 128) } == nil)
    #expect(
      Self.problem { $0.deviceName = String(repeating: "x", count: 129) }?.contains("deviceName")
        == true)
    #expect(Self.problem { $0.deviceName = "  Nicolai's iPhone  " } == nil)
  }

  @Test func onlyPhoneAndFixtureFormatsAreAccepted() {
    #expect(Self.problem { $0.format = .m4aAAC } == nil)
    #expect(Self.problem { $0.format = .wav16kInt16 } == nil)
    #expect(Self.problem { $0.format = .caf48kFloat32 }?.contains("format") == true)
  }

  @Test func theFirstProblemWinsAndTheDiskIsNeverTouched() {
    // Two problems: byteCount reports first, the order the phone can rely on
    // in logs. The inbox directory does not exist, so nothing was created.
    let problem = Self.problem {
      $0.byteCount = 0
      $0.format = .caf48kFloat32
    }
    #expect(problem?.contains("byteCount") == true)
    #expect(!FileManager.default.fileExists(atPath: Self.configuration.inboxDirectory.path))
  }
}
