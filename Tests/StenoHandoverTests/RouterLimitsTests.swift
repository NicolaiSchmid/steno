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
    let test = try await TestService.start(chunkSize: Self.chunkSize)
    defer { Task { await test.stop() } }
    let raw = try await test.rawClient()
    let limit = Self.chunkSize + HandoverConfiguration.bodyHeadroom

    let request = RawClient.request(
      "PUT", "/v1/recordings/\(UUID().uuidString)/chunks/0",
      headers: [
        ("Authorization", "Bearer nobody"), ("Content-Type", "application/octet-stream"),
        ("Content-Length", String(limit + 1)),
      ],
      body: Data(repeating: 0x41, count: 64 * 1024))
    let exchange = try await raw.exchange(request)

    #expect(exchange.status == 413)
    #expect(exchange.headers["connection"] == "close")
    #expect(exchange.closedByServer, "the server closes after a 413")
    #expect(test.metrics.handledRequests == 0)
    #expect(test.metrics.statuses == [413])
  }

  @Test func streamedBodyOverTheLimitIs413AndCloses() async throws {
    let test = try await TestService.start(chunkSize: Self.chunkSize)
    defer { Task { await test.stop() } }
    let raw = try await test.rawClient()
    // No Content-Length: the counting handler must catch it as it arrives.
    // `/v1/hello` needs no auth, so the counter is the only thing in the way.
    let oversized = HandoverConfiguration.jsonBodyLimit + 4096
    var body = Data()
    body.append(Data("\(String(oversized, radix: 16))\r\n".utf8))
    body.append(Data(repeating: 0x42, count: oversized))
    body.append(Data("\r\n0\r\n\r\n".utf8))
    let request = RawClient.request(
      "GET", "/v1/hello", headers: [("Transfer-Encoding", "chunked")], body: body)
    let exchange = try await raw.exchange(request)

    #expect(exchange.status == 413)
    #expect(exchange.closedByServer)
    #expect(test.metrics.handledRequests == 0)
  }

  @Test func unauthenticatedPutIsAnswered401BeforeItsBodyIsRead() async throws {
    let test = try await TestService.start(chunkSize: Self.chunkSize)
    defer { Task { await test.stop() } }
    let raw = try await test.rawClient()
    let declared = Self.chunkSize
    let sent = 16 * 1024

    // Head plus the first 16 KiB of a 256 KiB body; the rest never comes.
    let request = RawClient.request(
      "PUT", "/v1/recordings/\(UUID().uuidString)/chunks/0",
      headers: [
        ("Authorization", "Bearer \(DeviceTokens.mint())"),
        ("Content-Type", "application/octet-stream"),
        ("Content-Length", String(declared)),
      ],
      body: Data(repeating: 0x43, count: sent))
    let exchange = try await raw.exchange(request, closeGrace: .milliseconds(100))

    #expect(exchange.status == 401)
    #expect(exchange.headers["connection"] == "close")
    let problem = try StenoJSON.decode(Wire.Problem.self, from: exchange.body)
    #expect(problem.error.contains("token"))
    let metrics = test.metrics
    #expect(metrics.handledRequests == 0, "the engine never saw the request")
    #expect(metrics.discardedBodyBytes <= sent, "only bytes already on the wire were consumed")
    #expect(metrics.statuses == [401])
  }

  @Test func missingAndMalformedAuthorizationAre401() async throws {
    let test = try await TestService.start()
    defer { Task { await test.stop() } }
    let client = try await test.client()

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
