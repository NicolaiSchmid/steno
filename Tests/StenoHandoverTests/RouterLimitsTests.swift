import Foundation
import StenoCore
import Testing

@testable import StenoHandover

/// The two limits the plan names: bodies over chunk size plus 64 KiB are
/// answered 413 and the connection closes; unauthenticated requests are
/// answered 401 before their body is read.
@Suite struct RouterLimitsTests {
  static let chunkSize = 256 * 1024

  @Test func declaredBodyOverTheChunkLimitIs413AndCloses() async throws {
    try await TestService.run(chunkSize: Self.chunkSize) { test in
      let raw = try test.rawClient()
      let limit = Self.chunkSize + HandoverConfiguration.bodyHeadroom

      let exchange = try await raw.exchange(
        .PUT, "/v1/recordings/\(UUID().uuidString)/chunks/0",
        headers: [
          ("Authorization", "Bearer nobody"), ("Content-Type", "application/octet-stream"),
          ("Content-Length", String(limit + 1)),
        ],
        body: Data(repeating: 0x41, count: 64 * 1024))

      #expect(exchange.status == 413)
      #expect(exchange.headers.first(name: "connection") == "close")
      #expect(exchange.closedByServer, "the server closes after a 413")
      #expect(test.metrics.handledRequests == 0)
      #expect(test.metrics.statuses == [413])
    }
  }

  @Test func streamedBodyOverTheLimitIs413AndCloses() async throws {
    try await TestService.run(chunkSize: Self.chunkSize) { test in
      let raw = try test.rawClient()
      // No Content-Length: the counting handler must catch it as it arrives.
      // `/v1/hello` needs no auth, so the counter is the only thing in the way.
      let oversized = HandoverConfiguration.jsonBodyLimit + 4096
      let exchange = try await raw.exchange(
        .GET, "/v1/hello", headers: [("Transfer-Encoding", "chunked")],
        body: Data(repeating: 0x42, count: oversized))

      #expect(exchange.status == 413)
      #expect(exchange.closedByServer)
      #expect(test.metrics.handledRequests == 0)
    }
  }

  @Test func unauthenticatedPutIsAnswered401BeforeItsBodyIsRead() async throws {
    try await TestService.run(chunkSize: Self.chunkSize) { test in
      let raw = try test.rawClient()
      let declared = Self.chunkSize
      let sent = 16 * 1024

      // Head plus the first 16 KiB of a 256 KiB body; the rest never comes.
      let exchange = try await raw.exchange(
        .PUT, "/v1/recordings/\(UUID().uuidString)/chunks/0",
        headers: [
          ("Authorization", "Bearer \(DeviceTokens.mint())"),
          ("Content-Type", "application/octet-stream"),
          ("Content-Length", String(declared)),
        ],
        body: Data(repeating: 0x43, count: sent), closeGrace: .milliseconds(100))

      #expect(exchange.status == 401)
      #expect(exchange.headers.first(name: "connection") == "close")
      let problem = try StenoJSON.decode(Wire.Problem.self, from: exchange.body)
      #expect(problem.error.contains("token"))
      let metrics = test.metrics
      #expect(metrics.handledRequests == 0, "the engine never saw the request")
      #expect(metrics.discardedBodyBytes <= sent, "only bytes already on the wire were consumed")
      #expect(metrics.statuses == [401])
    }
  }

  @Test func badPairingSecretIsAnswered403BeforeItsBodyIsRead() async throws {
    try await TestService.run(chunkSize: Self.chunkSize) { test in
      _ = await test.service.beginPairing()
      let raw = try test.rawClient()
      let declared = HandoverConfiguration.jsonBodyLimit - 1
      let sent = 4 * 1024

      let exchange = try await raw.exchange(
        .POST, "/v1/pair",
        headers: [
          ("Authorization", "Pairing \(Data(repeating: 0x55, count: 32).base64EncodedString())"),
          ("Content-Type", "application/json"),
          ("Content-Length", String(declared)),
        ],
        body: Data(repeating: 0x7B, count: sent), closeGrace: .milliseconds(100))

      #expect(exchange.status == 403)
      #expect(exchange.headers.first(name: "connection") == "close")
      #expect(
        try StenoJSON.decode(Wire.Problem.self, from: exchange.body).error.contains("pairing"))
      let metrics = test.metrics
      #expect(metrics.handledRequests == 0, "the engine never saw the request")
      #expect(metrics.discardedBodyBytes <= sent)
      #expect(metrics.statuses == [403])
      #expect(await test.service.engine.pairingIsOpen, "a wrong secret does not burn the window")
    }
  }

  @Test func missingAndMalformedAuthorizationAre401() async throws {
    try await TestService.run { test in
      let client = try test.client()

      let missing = try await client.request("GET", "/v1/recordings/\(UUID().uuidString)")
      #expect(missing.status == 401)
      let basic = try await client.request(
        "GET", "/v1/recordings/\(UUID().uuidString)", headers: ["Authorization": "Basic abc"])
      #expect(basic.status == 401)
      let empty = try await client.request(
        "GET", "/v1/recordings/\(UUID().uuidString)", headers: ["Authorization": "Bearer "])
      #expect(empty.status == 401)
      #expect(test.metrics.handledRequests == 0)
    }
  }

  @Test func routeMatchingIsStrict() {
    let id = UUID()
    #expect(Route.match(method: .GET, uri: "/v1/hello") == .hello)
    #expect(Route.match(method: .GET, uri: "/v1/hello?x=1") == .hello)
    #expect(Route.match(method: .POST, uri: "/v1/pair") == .pair)
    #expect(Route.match(method: .DELETE, uri: "/v1/pairing") == .unpair)
    #expect(Route.match(method: .PUT, uri: "/v1/recordings/\(id.uuidString)") == .announce(id))
    #expect(
      Route.match(method: .PUT, uri: "/v1/recordings/\(id.uuidString.lowercased())")
        == .announce(id))
    #expect(Route.match(method: .GET, uri: "/v1/recordings/\(id.uuidString)") == .status(id))
    #expect(
      Route.match(method: .PUT, uri: "/v1/recordings/\(id.uuidString)/chunks/7") == .chunk(id, 7))
    #expect(
      Route.match(method: .POST, uri: "/v1/recordings/\(id.uuidString)/complete") == .complete(id))
    #expect(Route.match(method: .PUT, uri: "/v1/recordings/not-a-uuid") == nil)
    #expect(Route.match(method: .PUT, uri: "/v1/recordings/\(id.uuidString)/chunks/-1") == nil)
    #expect(Route.match(method: .PUT, uri: "/v1/recordings/\(id.uuidString)/chunks/07") == nil)
    #expect(Route.match(method: .PUT, uri: "/v1/recordings/\(id.uuidString)/chunks/x") == nil)
    #expect(Route.match(method: .GET, uri: "/v1/recordings/\(id.uuidString)/chunks/1") == nil)
    #expect(Route.match(method: .GET, uri: "/") == nil)
  }
}
