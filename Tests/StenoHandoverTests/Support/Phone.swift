import Crypto
import Foundation
import StenoCore

@testable import StenoHandover

/// A paired phone in the tests: the bearer from `/v1/pair` and the recording
/// calls the coordinator makes, over `LoopbackClient`.
struct Phone {
  let client: LoopbackClient
  let token: String
  let deviceID: UUID
  let deviceName: String

  /// Pairs a fresh device against the running service.
  static func pair(
    _ test: TestService, deviceID: UUID = UUID(), deviceName: String = "Test iPhone"
  ) async throws -> Phone {
    let client = try await test.client()
    let payload = await test.service.beginPairing()
    let response = try await client.json(
      "POST", "/v1/pair", headers: LoopbackClient.pairing(payload.secret),
      body: Wire.PairRequest(deviceID: deviceID, deviceName: deviceName))
    guard response.status == 200 else {
      throw PhoneError.pairingFailed(response.status)
    }
    let pair = try response.json(Wire.PairResponse.self)
    return Phone(client: client, token: pair.token, deviceID: deviceID, deviceName: deviceName)
  }

  var bearer: [String: String] { LoopbackClient.bearer(token) }

  func announce(_ metadata: RecordingMetadata) async throws -> LoopbackClient.Response {
    try await client.json(
      "PUT", "/v1/recordings/\(metadata.recordingID.uuidString)", headers: bearer, body: metadata)
  }

  func status(_ recordingID: UUID) async throws -> LoopbackClient.Response {
    try await client.request("GET", "/v1/recordings/\(recordingID.uuidString)", headers: bearer)
  }

  /// Uploads one chunk with the hash header `UploadSession.swift` sets; a
  /// different `declaredHash` simulates a corrupt body.
  func upload(
    _ recordingID: UUID, chunk index: Int, _ bytes: Data, declaredHash: Data? = nil,
    hashHeader: Bool = true
  ) async throws -> LoopbackClient.Response {
    var headers = bearer
    headers["Content-Type"] = "application/octet-stream"
    if hashHeader {
      headers[Wire.chunkHashHeader] = (declaredHash ?? Data(SHA256.hash(data: bytes)))
        .base64EncodedString()
    }
    return try await client.request(
      "PUT", "/v1/recordings/\(recordingID.uuidString)/chunks/\(index)", headers: headers,
      body: bytes)
  }

  func complete(_ recordingID: UUID) async throws -> LoopbackClient.Response {
    try await client.request(
      "POST", "/v1/recordings/\(recordingID.uuidString)/complete", headers: bearer)
  }

  /// Announces and uploads every chunk of `bytes`.
  func uploadAll(_ metadata: RecordingMetadata, _ bytes: Data) async throws {
    let announced = try await announce(metadata)
    guard announced.status == 201 || announced.status == 200 else {
      throw PhoneError.unexpectedStatus(announced.status)
    }
    for (index, chunk) in Phone.chunks(of: bytes, size: metadata.chunkSize).enumerated() {
      let response = try await upload(metadata.recordingID, chunk: index, chunk)
      guard response.status == 204 else { throw PhoneError.unexpectedStatus(response.status) }
    }
  }

  static func chunks(of bytes: Data, size: Int) -> [Data] {
    stride(from: 0, to: bytes.count, by: size).map { offset in
      bytes.subdata(in: offset..<min(offset + size, bytes.count))
    }
  }

  /// Metadata for `bytes` as the phone would declare it.
  func metadata(
    for bytes: Data, recordingID: UUID = UUID(), chunkSize: Int = 1024 * 1024,
    format: AudioFormat = .m4aAAC, sha256: Data? = nil,
    startedAt: Date = Date(timeIntervalSince1970: 1_789_990_000)
  ) -> RecordingMetadata {
    RecordingMetadata(
      recordingID: recordingID, startedAt: startedAt, durationSeconds: 61.5,
      byteCount: Int64(bytes.count), sha256: sha256 ?? Data(SHA256.hash(data: bytes)),
      chunkSize: chunkSize, format: format, deviceName: deviceName)
  }

  /// Deterministic pseudo-random bytes (SplitMix64), so a failing test can
  /// name what it sent.
  static func seededBytes(count: Int, seed: UInt64) -> Data {
    var state = seed
    var data = Data(capacity: count)
    while data.count < count {
      state &+= 0x9e37_79b9_7f4a_7c15
      var z = state
      z = (z ^ (z >> 30)) &* 0xbf58_476d_1ce4_e5b9
      z = (z ^ (z >> 27)) &* 0x94d0_49bb_1331_11eb
      z ^= z >> 31
      withUnsafeBytes(of: z.littleEndian) { data.append(contentsOf: $0) }
    }
    return data.prefix(count)
  }

  enum PhoneError: Error {
    case pairingFailed(Int)
    case unexpectedStatus(Int)
  }
}
