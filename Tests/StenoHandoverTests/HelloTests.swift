import Foundation
import StenoCore
import Testing

@testable import StenoHandover

@Suite struct HelloTests {
  @Test func helloAnswersTheMacIDAndProtocolWithoutAuth() async throws {
    let test = try await TestService.start()
    defer { Task { await test.stop() } }
    let client = try await test.client()

    let response = try await client.request("GET", "/v1/hello")
    #expect(response.status == 200)
    #expect(response.headers["content-type"] == "application/json")
    let hello = try response.json(Wire.Hello.self)
    #expect(hello.macID == test.service.macID)
    #expect(hello.protocol == 1)
    #expect(test.metrics.handledRequests == 1)
  }

  @Test func unknownRoutesAre404AndNeverReachTheEngine() async throws {
    let test = try await TestService.start()
    defer { Task { await test.stop() } }
    let client = try await test.client()

    for (method, path) in [("GET", "/v1/nothing"), ("GET", "/v2/hello"), ("POST", "/v1/hello")] {
      let response = try await client.request(method, path)
      #expect(response.status == 404, "\(method) \(path)")
    }
    #expect(test.metrics.handledRequests == 0)
  }

  @Test func statesStreamFollowsStartAndStop() async throws {
    let directory = try Fixtures.temporaryDirectory("handover")
    defer { try? FileManager.default.removeItem(at: directory) }
    let service = HandoverService(
      configuration: HandoverConfiguration(
        serviceName: "Test Mac", advertise: false, inboxDirectory: directory),
      store: try MeetingStore.inMemory(), intake: FakeHandoverIntake(),
      identity: try TestIdentity.load(), clock: ManualClock())
    let states = await service.states
    var iterator = states.makeAsyncIterator()
    #expect(await iterator.next() == .stopped)

    try await service.start()
    let port = try #require(await service.port)
    #expect(await iterator.next() == .listening(port: port))
    #expect(await service.state == .listening(port: port))

    await service.stop()
    #expect(await iterator.next() == .stopped)
    #expect(await service.port == nil)
  }
}
