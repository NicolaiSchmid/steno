import Foundation
import StenoCore
import Synchronization
import Testing

@testable import StenoLLM

#if canImport(FoundationNetworking)
  import FoundationNetworking
#endif

/// Edge cases of the client contract the plan promises: an empty key is no
/// key, every 5xx and only 408 among the 4xx retries, the wall-clock
/// backstop sits well past the clock timeout and a URLSession timeout maps
/// to `LLMError.timeout`, `probe()` fails on 401 and on an unreachable
/// server but records a downgraded mode, settings clamp the context.
@Suite struct ClientContractTests {
  @Test func anEmptyKeyIsNoKey() async throws {
    let harness = try ClientHarness(apiKey: "")
    defer { harness.stop() }
    harness.server.enqueue(Scripts.completion("ok"))
    _ = try await harness.client.complete(ClientHarness.request(format: .text))
    let recorded = try #require(harness.server.requests.first)
    #expect(recorded.authorization == nil)
    #expect(recorded.headers["user-agent"] == "steno/\(StenoCore.version)")
    #expect(recorded.headers["accept"] == "application/json")
  }

  @Test func retryabilityMatrix() {
    for status in [500, 502, 503, 504, 599, 408] {
      #expect(LLMError.http(status: status, body: "").isRetryable, "\(status)")
    }
    for status in [400, 401, 403, 404, 413, 422, 429, 499, 200] {
      #expect(!LLMError.http(status: status, body: "").isRetryable, "\(status)")
    }
    #expect(LLMError.rateLimited(retryAfter: nil).isRetryable)
    #expect(LLMError.transport("x").isRetryable)
    #expect(LLMError.timeout.isRetryable)
    for error in [LLMError.invalidJSON("x"), .truncated, .refused("x")] {
      #expect(!error.isRetryable, "\(error)")
      #expect(error.isAnswerProblem, "\(error)")
    }
    #expect(!LLMError.transcriptTooLong(estimatedTokens: 1, budget: 1).isRetryable)
    #expect(!LLMError.http(status: 500, body: "").isAnswerProblem)
    #expect(!LLMError.timeout.isAnswerProblem)
  }

  @Test func wallClockBackstopIsTwiceTheTimeoutAndAtLeastThirtySeconds() {
    #expect(OpenAICompatibleClient.wallClockBackstop(for: .seconds(240)) == 480)
    #expect(OpenAICompatibleClient.wallClockBackstop(for: .seconds(30)) == 60)
    #expect(OpenAICompatibleClient.wallClockBackstop(for: .seconds(5)) == 30)
    #expect(OpenAICompatibleClient.wallClockBackstop(for: .milliseconds(1_500)) == 30)
    #expect(OpenAICompatibleClient.wallClockBackstop(for: .zero) == 30)
  }

  /// A `URLProtocol` that fails the first transfer with `URLError.timedOut`
  /// (what the wall-clock backstop produces) and serves a completion on the
  /// next one. Matched by host so no other session is affected.
  final class TimedOutOnceProtocol: URLProtocol, @unchecked Sendable {
    static let attempts = Mutex(0)
    static let host = "backstop.invalid"

    override class func canInit(with request: URLRequest) -> Bool {
      request.url?.host == host
    }

    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }

    override func startLoading() {
      let attempt = Self.attempts.withLock { count -> Int in
        count += 1
        return count
      }
      guard let client, let url = request.url else { return }
      if attempt == 1 {
        client.urlProtocol(self, didFailWithError: URLError(.timedOut))
        return
      }
      guard
        let response = HTTPURLResponse(
          url: url, statusCode: 200, httpVersion: "HTTP/1.1",
          headerFields: ["Content-Type": "application/json"])
      else { return }
      client.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
      client.urlProtocol(self, didLoad: Scripts.completion("after the backstop").body)
      client.urlProtocolDidFinishLoading(self)
    }

    override func stopLoading() {}
  }

  @Test func aURLSessionTimeoutIsAnLLMTimeoutAndIsRetried() async throws {
    TimedOutOnceProtocol.attempts.withLock { $0 = 0 }
    let configuration = URLSessionConfiguration.ephemeral
    configuration.protocolClasses = [TimedOutOnceProtocol.self]
    let session = URLSession(configuration: configuration)
    defer { session.invalidateAndCancel() }
    let clock = ManualClock()
    let log = EventLog()
    let baseURL = try #require(URL(string: "http://\(TimedOutOnceProtocol.host)/v1"))
    let client = OpenAICompatibleClient(
      endpoint: LLMEndpoint(baseURL: baseURL, model: "m"),
      apiKey: nil, session: session, clock: clock, observer: { log.append($0) })
    let task = Task { try await client.complete(ClientHarness.request(format: .text)) }
    // The attempt's own timeout sleeper is on the clock too, so wait for the
    // backoff announcement, then for its sleeper.
    let retrying = LLMClientEvent.retrying(after: .seconds(2), attempt: 1, reason: .timeout)
    for _ in 0..<ClientHarness.sleeperAttempts where !log.entries.contains(retrying) {
      await Task.yield()
    }
    #expect(log.entries.contains(retrying))
    #expect(log.entries.contains(.timedOut(attempt: 1)), "reported like the clock timeout")
    #expect(await clock.waitForSleepers(1, attempts: ClientHarness.sleeperAttempts))
    clock.advance(by: .seconds(2))
    let response = try await task.value
    #expect(response.text == "after the backstop")
    #expect(TimedOutOnceProtocol.attempts.withLock { $0 } == 2)
  }

  @Test func probeThrowsOn401() async throws {
    let harness = try ClientHarness(retry: .none)
    defer { harness.stop() }
    harness.server.respond { request in
      request.method == "GET" ? Scripts.models(["stub-model"]) : Scripts.unauthorized()
    }
    let error = await #expect(throws: LLMError.self) { try await harness.client.probe() }
    #expect(error == .http(status: 401, body: "Incorrect API key provided"))
    #expect(harness.server.requests.count == 2, "the model list answered, the completion did not")
  }

  @Test func probeThrowsATransportErrorWhenNothingListens() async throws {
    // Port 1 (tcpmux) is closed on every runner: the connection is refused.
    let baseURL = try #require(URL(string: "http://127.0.0.1:1/v1"))
    let client = OpenAICompatibleClient(
      endpoint: LLMEndpoint(baseURL: baseURL, model: "m"), apiKey: nil, retry: .none,
      clock: ManualClock())
    let error = await #expect(throws: LLMError.self) { try await client.probe() }
    guard case .transport = error else {
      Issue.record("expected a transport error, got \(String(describing: error))")
      return
    }
  }

  @Test func probeRecordsTheDowngradedModeAndAModelThatIsNotListed() async throws {
    let harness = try ClientHarness()
    defer { harness.stop() }
    harness.server.respond(
      with: Scripts.server(
        models: ["other"], rejecting: ["json_schema"],
        completion: Scripts.completion("{\"ok\":true}")))
    let probe = try await harness.client.probe()
    #expect(probe.reachable)
    #expect(probe.modelListed == false)
    #expect(probe.resolvedMode == .jsonObject)
    #expect(harness.server.requests.count == 3, "GET, rejected json_schema, json_object")
    #expect(harness.clock.pendingSleepers == 0)
  }

  @Test func settingsClampTheContextToAtLeast1024Tokens() throws {
    var settings = Settings()
    settings.llmBaseURL = URL(string: "http://127.0.0.1:1234/v1")
    settings.llmModel = "m"
    settings.llmContextTokens = 10
    #expect(try #require(LLMEndpoint(settings: settings)).contextTokens == 1_024)
  }

  @Test func aBodyWithoutUsageStillCountsOneRequest() async throws {
    let harness = try ClientHarness()
    defer { harness.stop() }
    harness.server.enqueue(Scripts.completion("{}", usage: nil))
    let response = try await harness.client.complete(ClientHarness.request())
    #expect(response.usage == LLMUsage(promptTokens: 0, completionTokens: 0, requests: 1))
    #expect(response.countedUsage.requests == 1)
  }
}

/// Retry paths beyond the happy ones: cancelling during the backoff sends
/// nothing more, a 429 without `Retry-After` backs off exponentially, three
/// clock timeouts exhaust the policy with every wait on the clock, a
/// `Retry-After` past the cap is clamped by the client too, and a mode
/// downgrade never consumes an attempt.
@Suite struct RetryEdgeTests {
  @Test func cancellationDuringTheBackoffThrowsAndSendsNothingMore() async throws {
    let harness = try ClientHarness()
    defer { harness.stop() }
    harness.server.enqueue(Scripts.serverError(), Scripts.completion("never"))
    let task = Task { try await harness.client.complete(ClientHarness.request()) }
    let retrying = await harness.next {
      if case .retrying = $0 { return true } else { return false }
    }
    #expect(retrying != nil)
    #expect(await harness.clock.waitForSleepers(1, attempts: ClientHarness.sleeperAttempts))
    task.cancel()
    await #expect(throws: CancellationError.self) { try await task.value }
    #expect(harness.server.requests.count == 1)
    #expect(harness.clock.pendingSleepers == 0, "the sleeper was removed on cancel")
  }

  @Test func aRateLimitWithoutRetryAfterUsesTheBackoff() async throws {
    let harness = try ClientHarness()
    defer { harness.stop() }
    harness.server.enqueue(Scripts.rateLimited(), Scripts.rateLimited(), Scripts.completion("ok"))
    let driver = harness.driveRetries()
    defer { driver.cancel() }
    let response = try await harness.client.complete(ClientHarness.request())
    #expect(response.text == "ok")
    let delays = harness.events.compactMap { event -> Duration? in
      if case .retrying(let delay, _, .rateLimited(retryAfter: nil)) = event { return delay }
      return nil
    }
    #expect(delays == [.seconds(2), .seconds(4)])
    #expect(harness.server.requests.count == 3)
  }

  @Test func aRetryAfterPastTheCapIsClampedByTheClient() async throws {
    let harness = try ClientHarness(retry: RetryPolicy(maxAttempts: 2, maxDelay: .seconds(5)))
    defer { harness.stop() }
    harness.server.enqueue(Scripts.rateLimited(retryAfterSeconds: 600), Scripts.completion("ok"))
    let driver = harness.driveRetries()
    defer { driver.cancel() }
    _ = try await harness.client.complete(ClientHarness.request())
    #expect(
      harness.events.contains(
        .retrying(
          after: .seconds(5), attempt: 1, reason: .rateLimited(retryAfter: .seconds(600)))))
    #expect(harness.clock.now.offset == .seconds(5))
  }

  @Test func threeClockTimeoutsExhaustThePolicy() async throws {
    let harness = try ClientHarness { $0.requestTimeout = .seconds(10) }
    defer { harness.stop() }
    harness.server.enqueue(.hang, .hang, .hang)
    let driver = harness.driveRetries()
    defer { driver.cancel() }
    let task = Task { try await harness.client.complete(ClientHarness.request()) }
    for attempt in 1...3 {
      await harness.server.received(atLeast: attempt)
      #expect(
        await harness.clock.waitForSleepers(1, attempts: ClientHarness.sleeperAttempts),
        "attempt \(attempt) registers its timeout")
      harness.clock.advance(by: .seconds(10))
    }
    let error = await #expect(throws: LLMError.self) { try await task.value }
    #expect(error == .timeout)
    #expect(harness.server.requests.count == 3)
    let timedOut = harness.events.filter { if case .timedOut = $0 { true } else { false } }
    #expect(timedOut.count == 3)
    #expect(harness.clock.now.offset == .seconds(10 + 2 + 10 + 4 + 10))
  }

  @Test func aModeDowngradeDoesNotConsumeAnAttempt() async throws {
    let harness = try ClientHarness(retry: RetryPolicy(maxAttempts: 2))
    defer { harness.stop() }
    harness.server.enqueue(
      Scripts.rejectsResponseFormat(), Scripts.serverError(), Scripts.completion("{}"))
    let driver = harness.driveRetries()
    defer { driver.cancel() }
    let response = try await harness.client.complete(ModeFallbackTests.schemaRequest)
    #expect(response.text == "{}")
    #expect(harness.server.requests.count == 3)
    #expect(
      harness.server.requests.map { $0.chat?.responseFormat?.type } == [
        "json_schema", "json_object", "json_object",
      ])
    #expect(await harness.client.resolvedMode == .jsonObject)
  }
}

/// The two ends of the fallback chain: a plain text request never
/// downgrades however the server words its 400, a 400 at `promptOnly` has
/// nowhere left to go and is an HTTP error, and a `jsonObject` request
/// enters the chain at its own step.
@Suite struct ModeFallbackEdgeTests {
  @Test func aTextRequestNeverDowngrades() async throws {
    let harness = try ClientHarness()
    defer { harness.stop() }
    harness.server.enqueue(Scripts.rejectsResponseFormat())
    let error = await #expect(throws: LLMError.self) {
      try await harness.client.complete(ClientHarness.request(format: .text))
    }
    #expect(
      error
        == .http(
          status: 400,
          body: "'response_format' of type 'json_schema' is not supported with this model"))
    #expect(await harness.client.resolvedMode == .jsonSchema)
    #expect(harness.server.requests.count == 1)
    #expect(!harness.events.contains(.modeDowngraded(to: .jsonObject)))
  }

  @Test func a400AtPromptOnlyIsAnHTTPErrorAndNeverRetried() async throws {
    let harness = try ClientHarness { $0.structuredOutputMode = .promptOnly }
    defer { harness.stop() }
    harness.server.enqueue(Scripts.rejectsResponseFormat())
    let error = await #expect(throws: LLMError.self) {
      try await harness.client.complete(ModeFallbackTests.schemaRequest)
    }
    guard case .http(let status, _) = error else {
      Issue.record("expected http, got \(String(describing: error))")
      return
    }
    #expect(status == 400)
    #expect(harness.server.requests.count == 1)
    #expect(harness.server.requests.first?.chat?.responseFormat == nil)
    #expect(harness.clock.pendingSleepers == 0)
  }

  @Test func aJSONObjectRequestWalksTheWholeChainAndEndsAtPromptOnly() async throws {
    let harness = try ClientHarness()
    defer { harness.stop() }
    harness.server.respond(
      with: Scripts.server(rejecting: ["json_object"], completion: Scripts.completion("{}")))
    _ = try await harness.client.complete(ClientHarness.request(format: .jsonObject))
    // The chain is walked mode by mode, so the jsonSchema and jsonObject
    // modes both send json_object for this request: one repeated wire
    // format before promptOnly. Steno's builders never send jsonObject.
    #expect(
      harness.server.requests.map { $0.chat?.responseFormat?.type } == [
        "json_object", "json_object", nil,
      ])
    #expect(await harness.client.resolvedMode == .promptOnly)
    let downgrades = harness.events.filter { if case .modeDowngraded = $0 { true } else { false } }
    #expect(downgrades.count == 2)
    _ = try await harness.client.complete(ModeFallbackTests.schemaRequest)
    #expect(harness.server.requests.count == 4)
    #expect(harness.server.requests.last?.chat?.responseFormat == nil)
    #expect(harness.clock.pendingSleepers == 0)
  }
}
