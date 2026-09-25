import Foundation
import StenoCore
import Testing

@testable import StenoLLM

@Suite struct RetryTests {
  @Test func policyBacksOffExponentiallyAndHonoursRetryAfterWithinTheCap() {
    let policy = RetryPolicy()
    #expect(policy.maxAttempts == 3)
    #expect(policy.delay(beforeRetry: 1) == .seconds(2))
    #expect(policy.delay(beforeRetry: 2) == .seconds(4))
    #expect(policy.delay(beforeRetry: 3) == .seconds(8))
    #expect(policy.delay(beforeRetry: 10) == .seconds(30))
    #expect(policy.delay(beforeRetry: 1, retryAfter: .seconds(7)) == .seconds(7))
    #expect(policy.delay(beforeRetry: 1, retryAfter: .seconds(600)) == .seconds(30))
    #expect(RetryPolicy(maxAttempts: 0).maxAttempts == 1)
    #expect(RetryPolicy.none.maxAttempts == 1)
  }

  @Test func rateLimitThenSuccessHonoursRetryAfterOnTheClock() async throws {
    let harness = try ClientHarness()
    defer { harness.stop() }
    harness.server.enqueue(contentsOf: [
      Scripts.rateLimited(retryAfterSeconds: 7), Scripts.completion("{}"),
    ])
    let task = Task { try await harness.client.complete(ClientHarness.request()) }

    let retrying = await harness.next {
      if case .retrying = $0 { return true } else { return false }
    }
    #expect(
      retrying
        == .retrying(after: .seconds(7), attempt: 1, reason: .rateLimited(retryAfter: .seconds(7))))
    #expect(await harness.clock.waitForSleepers(1))
    #expect(harness.server.requests.count == 1)

    harness.clock.advance(by: .seconds(6))
    #expect(harness.clock.pendingSleepers == 1, "still asleep one second before Retry-After")
    #expect(harness.server.requests.count == 1)

    harness.clock.advance(by: .seconds(1))
    let response = try await task.value
    #expect(response.text == "{}")
    #expect(harness.server.requests.count == 2)
  }

  @Test func threeServerErrorsThrowHTTP500AfterTwoBackoffs() async throws {
    let harness = try ClientHarness()
    defer { harness.stop() }
    harness.server.enqueue(contentsOf: [
      Scripts.serverError(), Scripts.serverError(), Scripts.serverError(),
    ])
    let driver = harness.driveRetries()
    defer { driver.cancel() }
    let error = await #expect(throws: LLMError.self) {
      try await harness.client.complete(ClientHarness.request())
    }
    #expect(error == .http(status: 500, body: "The server had an error"))
    #expect(harness.server.requests.count == 3)
    let delays = harness.events.compactMap { event -> Duration? in
      if case .retrying(let delay, _, _) = event { return delay }
      return nil
    }
    #expect(delays == [.seconds(2), .seconds(4)])
    #expect(harness.clock.now.offset == .seconds(6))
  }

  @Test func timeoutOnTheClockCancelsTheAttemptAndRetries() async throws {
    let harness = try ClientHarness { $0.requestTimeout = .seconds(30) }
    defer { harness.stop() }
    harness.server.enqueue(contentsOf: [Scripts.hang, Scripts.completion("late")])
    let driver = harness.driveRetries()
    defer { driver.cancel() }
    let task = Task { try await harness.client.complete(ClientHarness.request()) }
    await harness.server.received(atLeast: 1)
    #expect(await harness.clock.waitForSleepers(1), "the timeout sleeper is registered")
    harness.clock.advance(by: .seconds(30))
    let response = try await task.value
    #expect(response.text == "late")
    #expect(harness.server.requests.count == 2)
    #expect(harness.events.contains(.timedOut(attempt: 1)))
    #expect(harness.events.contains(.retrying(after: .seconds(2), attempt: 1, reason: .timeout)))
  }

  @Test func droppedConnectionIsATransportErrorAndRetried() async throws {
    let harness = try ClientHarness()
    defer { harness.stop() }
    harness.server.enqueue(contentsOf: [Scripts.drop, Scripts.completion("again")])
    let driver = harness.driveRetries()
    defer { driver.cancel() }
    let response = try await harness.client.complete(ClientHarness.request())
    #expect(response.text == "again")
    #expect(harness.server.requests.count == 2)
    let reasons = harness.events.compactMap { event -> LLMError? in
      if case .retrying(_, _, let reason) = event { return reason }
      return nil
    }
    #expect(reasons.count == 1)
    if case .transport = reasons.first {
    } else {
      Issue.record("expected a transport error, got \(reasons)")
    }
  }

  @Test func requestTimeoutIsRetriedOn408() async throws {
    let harness = try ClientHarness()
    defer { harness.stop() }
    harness.server.enqueue(contentsOf: [Scripts.serverError(408), Scripts.completion("ok")])
    let driver = harness.driveRetries()
    defer { driver.cancel() }
    let response = try await harness.client.complete(ClientHarness.request())
    #expect(response.text == "ok")
    #expect(harness.server.requests.count == 2)
  }

  @Test func retryPolicyNoneNeverSleeps() async throws {
    let harness = try ClientHarness(retry: .none)
    defer { harness.stop() }
    harness.server.enqueue(Scripts.rateLimited())
    let error = await #expect(throws: LLMError.self) {
      try await harness.client.complete(ClientHarness.request())
    }
    #expect(error == .rateLimited(retryAfter: nil))
    #expect(harness.server.requests.count == 1)
    #expect(harness.clock.pendingSleepers == 0)
  }
}
