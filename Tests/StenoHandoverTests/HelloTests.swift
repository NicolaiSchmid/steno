import Foundation
import StenoCore
import Testing

@testable import StenoHandover

@Suite struct HelloTests {
  @Test func helloAnswersTheMacIDAndProtocolWithoutAuth() async throws {
    try await TestService.run { test in
      let client = try test.client()

      let response = try await client.request("GET", "/v1/hello")
      #expect(response.status == 200)
      #expect(response.headers["content-type"] == "application/json")
      let hello = try response.json(Wire.Hello.self)
      #expect(hello.macID == test.service.identity.macID)
      #expect(hello.protocol == 1)
      #expect(test.metrics.handledRequests == 1)
    }
  }

  @Test func unknownRoutesAre404AndNeverReachTheEngine() async throws {
    try await TestService.run { test in
      let client = try test.client()

      for (method, path) in [("GET", "/v1/nothing"), ("GET", "/v2/hello"), ("POST", "/v1/hello")] {
        let response = try await client.request(method, path)
        #expect(response.status == 404, "\(method) \(path)")
      }
      #expect(test.metrics.handledRequests == 0)
    }
  }

  @Test func statesStreamFollowsStartAndStop() async throws {
    try await TestService.run(start: false) { test in
      let service = test.service
      var iterator = service.states.makeAsyncIterator()
      #expect(await iterator.next() == .stopped)
      #expect(service.state == .stopped)

      try await service.start()
      let listening = await iterator.next()
      guard case .listening(let port)? = listening, port != 0 else {
        Issue.record("expected a listening state with a port, got \(String(describing: listening))")
        return
      }
      #expect(service.state == .listening(port: port), "`state` reads without an await")

      await service.stop()
      #expect(await iterator.next() == .stopped)
      #expect(service.state == .stopped)
    }
  }
}
