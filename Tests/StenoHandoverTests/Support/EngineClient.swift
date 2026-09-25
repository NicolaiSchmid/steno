import Crypto
import Foundation
import NIOHTTP1
import StenoCore

@testable import StenoHandover

/// The protocol core driven without the listener: `HandoverEngine.handle`
/// with hand-built requests, the seam `HTTPHandler` sits on. Runs in
/// milliseconds and orders concurrent requests exactly, which the loopback
/// clients cannot.
struct EngineClient {
  let engine: HandoverEngine

  init(_ test: TestService) {
    self.engine = test.service.engine
  }

  func hello() async -> HandoverResponse {
    await engine.handle(
      HandoverRequest(route: .hello, principal: .anonymous, headers: [:], body: Data()))
  }

  /// `POST /v1/pair` past the gate, as a request whose secret matched.
  func pair(deviceID: UUID = UUID(), deviceName: String = "Direct iPhone") async throws
    -> HandoverResponse
  {
    await engine.handle(
      HandoverRequest(
        route: .pair, principal: .pairing, headers: [:],
        body: try StenoJSON.encode(Wire.PairRequest(deviceID: deviceID, deviceName: deviceName))))
  }

  /// Opens a window, pairs and returns the paired device's view.
  static func paired(_ test: TestService, deviceName: String = "Direct iPhone") async throws
    -> EngineDevice
  {
    let client = EngineClient(test)
    _ = await client.engine.beginPairing()
    let deviceID = UUID()
    let response = try await client.pair(deviceID: deviceID, deviceName: deviceName)
    guard response.status == .ok, let device = try await test.store.pairedDevice(id: deviceID)
    else {
      throw Phone.PhoneError.pairingFailed(Int(response.status.code))
    }
    return EngineDevice(engine: client.engine, device: device)
  }
}

/// A paired device's recording calls, straight into the engine.
struct EngineDevice {
  let engine: HandoverEngine
  let device: PairedDevice

  func announce(_ metadata: RecordingMetadata) async throws -> HandoverResponse {
    await handle(.announce(metadata.recordingID), body: try StenoJSON.encode(metadata))
  }

  func status(_ recordingID: UUID) async -> HandoverResponse {
    await handle(.status(recordingID))
  }

  func upload(_ recordingID: UUID, chunk index: Int, _ bytes: Data) async -> HandoverResponse {
    await handle(
      .chunk(recordingID, index), body: bytes,
      headers: [Wire.chunkHashHeader: Data(SHA256.hash(data: bytes)).base64EncodedString()])
  }

  func complete(_ recordingID: UUID) async -> HandoverResponse {
    await handle(.complete(recordingID))
  }

  /// Announces and uploads every chunk of `bytes`.
  func uploadAll(_ metadata: RecordingMetadata, _ bytes: Data) async throws {
    let announced = try await announce(metadata)
    guard announced.status == .created || announced.status == .ok else {
      throw Phone.PhoneError.unexpectedStatus(Int(announced.status.code))
    }
    for (index, chunk) in Phone.chunks(of: bytes, size: metadata.chunkSize).enumerated() {
      let response = await upload(metadata.recordingID, chunk: index, chunk)
      guard response.status == .noContent else {
        throw Phone.PhoneError.unexpectedStatus(Int(response.status.code))
      }
    }
  }

  /// Metadata for `bytes` as the phone would declare it; the hash is
  /// swift-crypto's, so a large file costs milliseconds on Linux too.
  func metadata(for bytes: Data, chunkSize: Int) -> RecordingMetadata {
    Phone.metadata(
      for: bytes, deviceName: device.name, chunkSize: chunkSize,
      sha256: Data(SHA256.hash(data: bytes)))
  }

  private func handle(_ route: Route, body: Data = Data(), headers: [String: String] = [:]) async
    -> HandoverResponse
  {
    var httpHeaders = HTTPHeaders()
    for (name, value) in headers { httpHeaders.add(name: name, value: value) }
    return await engine.handle(
      HandoverRequest(route: route, principal: .device(device), headers: httpHeaders, body: body))
  }
}

extension HandoverResponse {
  var code: Int { Int(status.code) }

  func json<T: Decodable>(_ type: T.Type) throws -> T {
    try StenoJSON.decode(type, from: body)
  }
}
