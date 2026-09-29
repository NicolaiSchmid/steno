import Foundation
import StenoCore
import Synchronization
import Testing

@testable import StenoLLM

/// A `CodexHome` (auth file plus token endpoint), a second stub server as
/// the Codex backend, a `ManualClock` and the client wired to them.
final class CodexHarness: Sendable {
  let home: CodexHome
  let backend: StubChatServer
  let clock: ManualClock
  let client: CodexResponsesClient
  let endpoint: LLMEndpoint
  private let recorder = EventRecorder()

  init(
    retry: RetryPolicy = .default, model: String = "gpt-stub",
    configure: (inout LLMEndpoint) -> Void = { _ in }
  ) throws {
    home = try CodexHome()
    try home.write()
    backend = try StubChatServer()
    var endpoint = LLMEndpoint.codex(model: model, contextTokens: 200_000)
    endpoint.baseURL = backend.baseURL
    configure(&endpoint)
    self.endpoint = endpoint
    clock = ManualClock()
    client = CodexResponsesClient(
      endpoint: endpoint, credentials: home.store(), retry: retry, clock: clock,
      observer: recorder.observer)
  }

  var events: [LLMClientEvent] { recorder.events }

  func stop() {
    recorder.finish()
    backend.stop()
    home.stop()
  }

  func driveRetries() -> Task<Void, Never> {
    recorder.driveRetries(clock: clock)
  }
}

@Suite struct CodexClientTests {
  @Test func sendsTheSignInAndAResponsesBodyAndParsesTheStream() async throws {
    let harness = try CodexHarness()
    defer { harness.stop() }
    harness.backend.enqueue(
      Scripts.responsesStream(
        "{\"hi\":true}", usage: LLMUsage(promptTokens: 12, completionTokens: 4, requests: 1),
        model: "gpt-served"))

    let request = ClientHarness.request(
      purpose: "cleanup",
      format: .jsonSchema(name: "reply", schema: ["type": "object"], strict: true))
    let response = try await harness.client.complete(request)
    #expect(response.text == "{\"hi\":true}")
    #expect(response.finishReason == .stop)
    #expect(response.usage == LLMUsage(promptTokens: 12, completionTokens: 4, requests: 1))
    #expect(response.model == "gpt-served")

    let recorded = try #require(harness.backend.requests.first)
    #expect(recorded.path == "/v1/responses")
    #expect(recorded.authorization == "Bearer \(CodexHome.accessToken(expiresIn: 3_600))")
    #expect(recorded.headers["chatgpt-account-id"] == "acct_stored")
    #expect(recorded.headers["originator"] == "steno")
    #expect(recorded.headers["user-agent"] == "steno/\(StenoCore.version)")
    #expect(recorded.headers["accept"] == "text/event-stream")
    #expect(recorded.headers["session-id"]?.isEmpty == false)
    #expect(recorded.purpose == "cleanup")
    #expect(recorded.chat == nil, "not a chat completion")
    let body = try #require(recorded.responses)
    #expect(body.model == "gpt-stub")
    #expect(body.instructions == "You are a test.")
    #expect(body.input == [ResponsesInputItem(LLMMessage(role: .user, content: "Say hi as JSON."))])
    #expect(body.input.first?.content.first?.type == "input_text")
    #expect(body.stream == true)
    #expect(body.store == false)
    #expect(body.reasoning?.effort == "low")
    #expect(body.text?.format.type == "json_schema")
    #expect(body.text?.format.name == "reply")
    #expect(body.text?.format.strict == true)
    // Neither field the backend rejects appears, under any spelling.
    let raw = String(decoding: recorded.body, as: UTF8.self)
    #expect(!raw.contains("max_output_tokens"))
    #expect(!raw.contains("max_tokens"))
    #expect(!raw.contains("temperature"))
  }

  @Test func summaryPurposesUseMediumEffortAndAssistantTurnsAreOutputText() async throws {
    #expect(CodexResponsesClient.reasoningEffort(for: "summary") == "medium")
    #expect(CodexResponsesClient.reasoningEffort(for: "summary-map") == "medium")
    #expect(CodexResponsesClient.reasoningEffort(for: "cleanup") == "low")
    #expect(CodexResponsesClient.reasoningEffort(for: "probe") == "low")
    let body = CodexResponsesClient.requestBody(
      for: LLMRequest(
        messages: [
          LLMMessage(role: .system, content: "A"), LLMMessage(role: .system, content: "B"),
          LLMMessage(role: .user, content: "Q"), LLMMessage(role: .assistant, content: "bad json"),
          LLMMessage(role: .user, content: "fix it"),
        ], responseFormat: .jsonObject, purpose: "summary"),
      model: "m", mode: .jsonSchema)
    #expect(body.instructions == "A\n\nB")
    #expect(body.input.map(\.role) == ["user", "assistant", "user"])
    #expect(body.input[1].content.first?.type == "output_text")
    #expect(body.text?.format == .jsonObject)
    let plain = CodexResponsesClient.requestBody(
      for: LLMRequest(messages: [LLMMessage(role: .user, content: "Q")], purpose: "x"),
      model: "m", mode: .jsonSchema)
    #expect(plain.instructions == nil)
    #expect(plain.text == nil)
  }

  @Test func aStaleTokenIsRefreshedBeforeTheFirstRequest() async throws {
    let harness = try CodexHarness()
    defer { harness.stop() }
    try harness.home.write(access: CodexHome.accessToken(expiresIn: 60))
    let fresh = CodexHome.accessToken(expiresIn: 3_600)
    harness.home.server.enqueue(Scripts.tokenRefresh(access: fresh, refresh: "rt_2"))
    harness.backend.enqueue(Scripts.responsesStream("ok"))
    _ = try await harness.client.complete(ClientHarness.request(format: .text))
    #expect(harness.backend.requests.first?.authorization == "Bearer \(fresh)")
    #expect(harness.home.server.requests.count == 1)
  }

  @Test func unauthorizedRefreshesTheSignInOnceThenStands() async throws {
    let harness = try CodexHarness()
    defer { harness.stop() }
    let fresh = CodexHome.accessToken(expiresIn: 3_600, plan: "pro")
    harness.home.server.enqueue(Scripts.tokenRefresh(access: fresh, refresh: "rt_2"))
    harness.backend.enqueue(Scripts.unauthorized(), Scripts.responsesStream("ok"))
    let response = try await harness.client.complete(ClientHarness.request(format: .text))
    #expect(response.text == "ok")
    #expect(
      harness.backend.requests.map(\.authorization) == [
        "Bearer \(CodexHome.accessToken(expiresIn: 3_600))", "Bearer \(fresh)",
      ])

    // A second 401 after the refresh is the answer, not a loop.
    let second = try CodexHarness()
    defer { second.stop() }
    second.home.server.enqueue(Scripts.tokenRefresh(access: fresh, refresh: "rt_2"))
    second.backend.enqueue(Scripts.unauthorized(), Scripts.unauthorized())
    let error = await #expect(throws: LLMError.self) {
      try await second.client.complete(ClientHarness.request(format: .text))
    }
    guard case .http(401, _)? = error else {
      Issue.record("expected 401, got \(String(describing: error))")
      return
    }
    #expect(second.backend.requests.count == 2)
    #expect(
      second.events.filter { if case .retrying = $0 { return true } else { return false } }.isEmpty)
  }

  @Test func noSignInFailsBeforeAnyRequest() async throws {
    let harness = try CodexHarness()
    defer { harness.stop() }
    try FileManager.default.removeItem(at: harness.home.file)
    await #expect(throws: CodexCredentialError.notSignedIn) {
      try await harness.client.complete(ClientHarness.request())
    }
    #expect(harness.backend.requests.isEmpty)
  }

  @Test func planLimitIsNotRetriedAndNamesThePlan() async throws {
    let harness = try CodexHarness()
    defer { harness.stop() }
    harness.backend.enqueue(Scripts.codexUsageLimit())
    let error = await #expect(throws: LLMError.self) {
      try await harness.client.complete(ClientHarness.request())
    }
    guard case .http(429, let body)? = error else {
      Issue.record("expected a plan-limit http error, got \(String(describing: error))")
      return
    }
    #expect(body.hasPrefix("ChatGPT plan limit reached"))
    #expect(error?.isRetryable == false)
    #expect(harness.backend.requests.count == 1)
    #expect(harness.clock.pendingSleepers == 0, "the Retry-After of an hour is not waited on")
    #expect(
      harness.events.filter { if case .retrying = $0 { return true } else { return false } }.isEmpty
    )
    // The other spelling of the plan limit, on a 402-style status, is the same.
    harness.backend.enqueue(
      .json(
        ChatErrorEnvelope(
          error: .init(message: "Not included in your plan.", type: "usage_not_included")),
        status: 403))
    let notIncluded = await #expect(throws: LLMError.self) {
      try await harness.client.complete(ClientHarness.request())
    }
    #expect(
      notIncluded
        == .http(status: 403, body: "ChatGPT plan limit reached: Not included in your plan."))
  }

  @Test func anOrdinaryRateLimitBacksOff() async throws {
    let harness = try CodexHarness()
    defer { harness.stop() }
    harness.backend.enqueue(
      Scripts.rateLimited(retryAfterSeconds: 3), Scripts.responsesStream("ok"))
    let driver = harness.driveRetries()
    defer { driver.cancel() }
    let response = try await harness.client.complete(ClientHarness.request(format: .text))
    #expect(response.text == "ok")
    #expect(harness.backend.requests.count == 2)
    #expect(
      harness.events.contains(
        .retrying(after: .seconds(3), attempt: 1, reason: .rateLimited(retryAfter: .seconds(3)))))
  }

  @Test func aRejectedSchemaDowngradesTheModeAndUnsupportedParametersSurface() async throws {
    let harness = try CodexHarness()
    defer { harness.stop() }
    harness.backend.respond { request in
      if request.responses?.text?.format.type == "json_schema" {
        return Scripts.badRequest("Invalid value for text.format: json_schema is not supported")
      }
      return Scripts.responsesStream("{}")
    }
    let request = ClientHarness.request(
      format: .jsonSchema(name: "r", schema: ["type": "object"], strict: true))
    _ = try await harness.client.complete(request)
    #expect(await harness.client.resolvedMode == .jsonObject)
    #expect(
      harness.backend.requests.map { $0.responses?.text?.format.type } == [
        "json_schema", "json_object",
      ])

    let second = try CodexHarness()
    defer { second.stop() }
    second.backend.enqueue(Scripts.codexUnsupportedParameter("max_output_tokens"))
    let error = await #expect(throws: LLMError.self) {
      try await second.client.complete(ClientHarness.request(format: .text))
    }
    #expect(error == .http(status: 400, body: "Unsupported parameter: max_output_tokens"))
  }

  @Test func secretsNeverAppearInErrors() async throws {
    let harness = try CodexHarness()
    defer { harness.stop() }
    let token = CodexHome.accessToken(expiresIn: 3_600)
    harness.backend.enqueue(
      Scripts.badRequest("bad token \(token) for account acct_stored with refresh rt_original"))
    let error = await #expect(throws: LLMError.self) {
      try await harness.client.complete(ClientHarness.request(format: .text))
    }
    let text = String(describing: error!)
    #expect(!text.contains(token))
    #expect(!text.contains("acct_stored"))
    #expect(!text.contains("rt_original"))
    #expect(text.contains("[redacted]"))
  }

  @Test func listModelsProbeAndTimeout() async throws {
    let harness = try CodexHarness(retry: .none, model: "gpt-b")
    defer { harness.stop() }
    harness.backend.respond { request in
      if request.method == "GET", request.path.hasPrefix("/v1/models") {
        return Scripts.codexModels([
          CodexModel(slug: "gpt-a", displayName: "A", contextWindow: 272_000),
          CodexModel(slug: "gpt-b", displayName: "B", visibility: "hide"),
        ])
      }
      return Scripts.responsesStream("{\"ok\":true}")
    }
    let models = try await harness.client.listModels()
    #expect(models.map(\.slug) == ["gpt-a", "gpt-b"])
    #expect(models.filter(\.isListed).map(\.slug) == ["gpt-a"])
    let listed = try #require(harness.backend.requests.first)
    #expect(listed.path == "/v1/models?client_version=99.0.0")
    #expect(listed.purpose == "models")
    #expect(listed.headers["accept"] == "application/json")
    #expect(listed.headers["originator"] == "steno")
    #expect(listed.headers["chatgpt-account-id"] == "acct_stored")
    #expect(listed.authorization == "Bearer \(CodexHome.accessToken(expiresIn: 3_600))")
    let probe = try await harness.client.probe()
    #expect(probe.modelListed == true)
    #expect(probe.resolvedMode == .jsonSchema)
    #expect(probe.accountLine == "nicolai@example.com (Plus)")
    #expect(harness.backend.requests.last?.purpose == "probe")

    let hanging = try CodexHarness(retry: .none) { $0.requestTimeout = .seconds(5) }
    defer { hanging.stop() }
    hanging.backend.enqueue(.hang)
    let task = Task { try await hanging.client.complete(ClientHarness.request(format: .text)) }
    _ = await hanging.clock.waitForSleepers(1, attempts: ClientHarness.sleeperAttempts)
    hanging.clock.advance(by: .seconds(5))
    let error = await #expect(throws: LLMError.self) { try await task.value }
    #expect(error == .timeout)
  }

  @Test func endpointFromSettingsNeedsConfirmationAndAModel() throws {
    var settings = Settings()
    settings.llmProvider = .codex
    #expect(LLMEndpoint(settings: settings) == nil)
    settings.codexModel = "gpt-5.6-terra"
    #expect(LLMEndpoint(settings: settings) == nil, "unconfirmed: never configured")
    settings.codexConfirmedAt = Date()
    let endpoint = try #require(LLMEndpoint(settings: settings))
    #expect(endpoint.isCodexBackend)
    #expect(endpoint.model == "gpt-5.6-terra")
    #expect(endpoint.contextTokens == Settings.defaultCodexContextTokens)
    #expect(endpoint.maxOutputTokens == 16_000)
    #expect(
      endpoint.responsesURL.absoluteString == "https://chatgpt.com/backend-api/codex/responses")
    #expect(endpoint.modelsURL.absoluteString == "https://chatgpt.com/backend-api/codex/models")
    // The endpoint provider's fields do not leak into the Codex endpoint and
    // switching back finds them untouched.
    settings.llmBaseURL = URL(string: "http://127.0.0.1:1234/v1")
    settings.llmModel = "local"
    #expect(LLMEndpoint(settings: settings)?.model == "gpt-5.6-terra")
    settings.llmProvider = .endpoint
    #expect(LLMEndpoint(settings: settings)?.model == "local")
    #expect(LLMEndpoint(settings: settings)?.isCodexBackend == false)
  }

  @Test func cancellationMidRequestRethrowsAfterExactlyOneRequest() async throws {
    let harness = try CodexHarness()
    defer { harness.stop() }
    harness.backend.enqueue(.hang)
    let task = Task { try await harness.client.complete(ClientHarness.request()) }
    await harness.backend.received(atLeast: 1)
    task.cancel()
    await #expect(throws: CancellationError.self) { try await task.value }
    #expect(harness.backend.requests.count == 1)
    #expect(harness.home.server.requests.isEmpty, "a cancelled call refreshes nothing")
    #expect(harness.clock.pendingSleepers == 0)
  }

  /// 5xx and 408 back off on the clock with the policy's doubling delays
  /// and the same sign-in each time; `.none` throws the first failure;
  /// the default policy gives up after three attempts.
  @Test func serverErrorsRetryWithBackoffUnlessThePolicySaysOtherwise() async throws {
    let harness = try CodexHarness()
    defer { harness.stop() }
    harness.backend.enqueue(
      Scripts.serverError(503), Scripts.serverError(408), Scripts.responsesStream("ok"))
    let driver = harness.driveRetries()
    defer { driver.cancel() }
    let response = try await harness.client.complete(ClientHarness.request(format: .text))
    #expect(response.text == "ok")
    #expect(harness.backend.requests.count == 3)
    let failed = "The server had an error"
    #expect(
      harness.events.contains(
        .retrying(after: .seconds(2), attempt: 1, reason: .http(status: 503, body: failed))))
    #expect(
      harness.events.contains(
        .retrying(after: .seconds(4), attempt: 2, reason: .http(status: 408, body: failed))))
    #expect(Set(harness.backend.requests.compactMap(\.authorization)).count == 1)
    #expect(harness.home.server.requests.isEmpty, "a server error is not a sign-in problem")

    let once = try CodexHarness(retry: .none)
    defer { once.stop() }
    once.backend.enqueue(Scripts.serverError(502))
    let error = await #expect(throws: LLMError.self) {
      try await once.client.complete(ClientHarness.request(format: .text))
    }
    #expect(error == .http(status: 502, body: failed))
    #expect(once.backend.requests.count == 1)
    #expect(once.clock.pendingSleepers == 0)

    let exhausted = try CodexHarness()
    defer { exhausted.stop() }
    exhausted.backend.enqueue(
      Scripts.serverError(500), Scripts.serverError(500), Scripts.serverError(500),
      Scripts.responsesStream("never reached"))
    let exhaustedDriver = exhausted.driveRetries()
    defer { exhaustedDriver.cancel() }
    let last = await #expect(throws: LLMError.self) {
      try await exhausted.client.complete(ClientHarness.request(format: .text))
    }
    #expect(last == .http(status: 500, body: failed))
    #expect(exhausted.backend.requests.count == 3)
  }

  /// Only a 400 that names the structured output request downgrades the
  /// mode: a 400 about anything else is the answer; a format complaint on a
  /// plain-text request has nothing to downgrade; `.promptOnly` is the floor.
  @Test func aFourHundredThatDoesNotNameTheFormatIsNotDowngraded() async throws {
    let harness = try CodexHarness()
    defer { harness.stop() }
    harness.backend.enqueue(Scripts.badRequest("The model `gpt-stub` does not exist"))
    let schema = ClientHarness.request(
      format: .jsonSchema(name: "r", schema: ["type": "object"], strict: true))
    let unknownModel = await #expect(throws: LLMError.self) {
      try await harness.client.complete(schema)
    }
    #expect(unknownModel == .http(status: 400, body: "The model `gpt-stub` does not exist"))
    #expect(await harness.client.resolvedMode == .jsonSchema)
    #expect(harness.backend.requests.count == 1)

    harness.backend.enqueue(Scripts.badRequest("Invalid value for text.format"))
    let plain = await #expect(throws: LLMError.self) {
      try await harness.client.complete(ClientHarness.request(format: .text))
    }
    #expect(plain == .http(status: 400, body: "Invalid value for text.format"))
    #expect(await harness.client.resolvedMode == .jsonSchema)
    #expect(harness.backend.requests.count == 2)
    #expect(harness.clock.pendingSleepers == 0)

    let floor = try CodexHarness { $0.structuredOutputMode = .promptOnly }
    defer { floor.stop() }
    floor.backend.enqueue(Scripts.badRequest("text.format is not supported"))
    let last = await #expect(throws: LLMError.self) { try await floor.client.complete(schema) }
    #expect(last == .http(status: 400, body: "text.format is not supported"))
    #expect(floor.backend.requests.count == 1)
    #expect(floor.backend.requests.first?.responses?.text == nil)
    #expect(await floor.client.resolvedMode == .promptOnly)
  }

  /// The downgrade runs the whole ladder within one call and the learned
  /// mode sticks for the next request, whatever format it asks for.
  @Test func theDowngradeRunsToPromptOnlyAndSticks() async throws {
    let harness = try CodexHarness()
    defer { harness.stop() }
    harness.backend.respond { request in
      request.responses?.text != nil
        ? Scripts.badRequest("text.format is not supported by this model")
        : Scripts.responsesStream("{}")
    }
    _ = try await harness.client.complete(
      ClientHarness.request(
        format: .jsonSchema(name: "r", schema: ["type": "object"], strict: true)))
    #expect(
      harness.backend.requests.map { $0.responses?.text?.format.type } == [
        "json_schema", "json_object", nil,
      ])
    #expect(await harness.client.resolvedMode == .promptOnly)
    #expect(harness.events.contains(.modeDowngraded(to: .jsonObject)))
    #expect(harness.events.contains(.modeDowngraded(to: .promptOnly)))
    #expect(harness.clock.pendingSleepers == 0, "a downgrade is a resend, not a retry")

    _ = try await harness.client.complete(ClientHarness.request(format: .jsonObject))
    #expect(harness.backend.requests.count == 4)
    #expect(harness.backend.requests.last?.responses?.text == nil)
  }

  /// `text.format` follows the request and the resolved mode: `json_object`
  /// requests never carry a schema, a schema request under `.jsonObject`
  /// mode is sent as `json_object`, and `.promptOnly` sends no `text`.
  @Test func textFormatFollowsTheRequestAndTheMode() async throws {
    typealias Client = CodexResponsesClient
    let schema = LLMResponseFormat.jsonSchema(name: "r", schema: ["type": "object"], strict: false)
    #expect(
      Client.textFormat(for: schema, mode: .jsonSchema)
        == .jsonSchema(name: "r", schema: ["type": "object"], strict: false))
    #expect(Client.textFormat(for: schema, mode: .jsonObject) == .jsonObject)
    #expect(Client.textFormat(for: schema, mode: .promptOnly) == nil)
    #expect(Client.textFormat(for: .jsonObject, mode: .jsonSchema) == .jsonObject)
    #expect(Client.textFormat(for: .jsonObject, mode: .jsonObject) == .jsonObject)
    #expect(Client.textFormat(for: .jsonObject, mode: .promptOnly) == nil)
    for mode in StructuredOutputMode.allCases {
      #expect(Client.textFormat(for: .text, mode: mode) == nil, "\(mode)")
    }

    let harness = try CodexHarness()
    defer { harness.stop() }
    harness.backend.enqueue(Scripts.responsesStream("{}"))
    _ = try await harness.client.complete(ClientHarness.request(format: .jsonObject))
    let recorded = try #require(harness.backend.requests.first)
    #expect(recorded.responses?.text?.format == .jsonObject)
    let raw = String(decoding: recorded.body, as: UTF8.self)
    #expect(!raw.contains("schema"))
    #expect(!raw.contains("strict"))
    #expect(recorded.responses?.reasoning?.effort == "medium")
  }

  /// A model list that answers does not make the backend usable: the probe
  /// completion's own failure is thrown, whether a status, a failed stream
  /// or a missing sign-in. And a model list that fails is tolerated when the
  /// completion works.
  @Test func probeThrowsWhenTheCompletionFailsAndToleratesAFailedModelList() async throws {
    let harness = try CodexHarness(retry: .none)
    defer { harness.stop() }
    let models = Scripts.codexModels([CodexModel(slug: "gpt-stub", displayName: "Stub")])
    harness.backend.respond { request in
      request.method == "GET"
        ? models
        : .json(
          ChatErrorEnvelope(error: .init(message: "model `gpt-stub` does not exist")),
          status: 404)
    }
    let notFound = await #expect(throws: LLMError.self) { try await harness.client.probe() }
    #expect(notFound == .http(status: 404, body: "model `gpt-stub` does not exist"))
    #expect(harness.backend.requests.count == 2)

    harness.backend.respond { request in
      request.method == "GET" ? models : Scripts.responsesFailed("boom")
    }
    let failed = await #expect(throws: LLMError.self) { try await harness.client.probe() }
    #expect(failed == .transport("boom"))
    #expect(harness.backend.requests.count == 4)

    harness.backend.respond { request in
      request.method == "GET" ? Scripts.unauthorized() : Scripts.responsesStream("{\"ok\":true}")
    }
    let probe = try await harness.client.probe()
    #expect(probe.modelListed == nil)
    #expect(probe.accountLine == "nicolai@example.com (Plus)")
    #expect(harness.backend.requests.count == 6)

    harness.backend.respond { request in
      request.method == "GET" ? Scripts.rawCompletion("<html>") : Scripts.responsesStream("{}")
    }
    let undecodable = await #expect(throws: LLMError.self) {
      try await harness.client.listModels()
    }
    #expect(undecodable == .transport("undecodable model list: <html>"))

    try FileManager.default.removeItem(at: harness.home.file)
    await #expect(throws: CodexCredentialError.notSignedIn) { try await harness.client.probe() }
    #expect(harness.backend.requests.count == 7, "no request without a sign-in")
    #expect(harness.clock.pendingSleepers == 0)
  }

  /// A stream that ends in an `error` event and a connection the backend
  /// closes early are transport errors, and neither names a secret.
  @Test func streamErrorsAndDroppedConnectionsAreRedactedTransportErrors() async throws {
    let harness = try CodexHarness(retry: .none)
    defer { harness.stop() }
    let token = CodexHome.accessToken(expiresIn: 3_600)
    harness.backend.enqueue(
      Scripts.responsesErrorEvent("upstream rejected \(token) for acct_stored"))
    let error = await #expect(throws: LLMError.self) {
      try await harness.client.complete(ClientHarness.request(format: .text))
    }
    #expect(error == .transport("upstream rejected [redacted] for [redacted]"))
    #expect(error?.isRetryable == true)

    harness.backend.enqueue(.drop)
    let dropped = await #expect(throws: LLMError.self) {
      try await harness.client.complete(ClientHarness.request(format: .text))
    }
    guard case .transport(let message)? = dropped else {
      Issue.record("expected a transport error, got \(String(describing: dropped))")
      return
    }
    #expect(!message.contains(token))
    #expect(!message.contains("acct_stored"))
    for event in harness.events {
      #expect(!String(describing: event).contains(token), "\(event)")
      #expect(!String(describing: event).contains("acct_stored"), "\(event)")
    }
  }

  /// The client reads what the backend sends whether the events are named
  /// or carry their `type` in the JSON, and a backend that answers with the
  /// whole response object as JSON is understood too.
  @Test func dataOnlyStreamsAndPlainJSONRepliesAreParsed() async throws {
    let harness = try CodexHarness(retry: .none)
    defer { harness.stop() }
    harness.backend.enqueue(
      Scripts.rawEventStream(
        "data: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"typed\"}]}}\n\n"
          + "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":1,\"output_tokens\":2}}}\n\n"
      ))
    let typed = try await harness.client.complete(ClientHarness.request(format: .text))
    #expect(typed.text == "typed")
    #expect(typed.usage == LLMUsage(promptTokens: 1, completionTokens: 2, requests: 1))

    harness.backend.enqueue(
      .json(
        ResponsesResponse(
          id: "resp_1", status: "incomplete", model: "gpt-json",
          output: [
            ResponsesOutputItem(
              type: "message", role: "assistant",
              content: [.init(type: "output_text", text: "whole")])
          ],
          usage: .init(inputTokens: 5, outputTokens: 6),
          incompleteDetails: .init(reason: "max_output_tokens"))))
    let whole = try await harness.client.complete(ClientHarness.request(format: .text))
    #expect(whole.text == "whole")
    #expect(whole.finishReason == .length)
    #expect(whole.model == "gpt-json")

    harness.backend.enqueue(Scripts.rawCompletion("<html>not an API</html>"))
    let undecodable = await #expect(throws: LLMError.self) {
      try await harness.client.complete(ClientHarness.request(format: .text))
    }
    #expect(undecodable == .transport("undecodable response body: <html>not an API</html>"))
    #expect(harness.clock.pendingSleepers == 0)
  }

  @Test func settingsClampTheCodexContextAndRejectAnEmptyModel() throws {
    var settings = Settings()
    settings.llmProvider = .codex
    settings.codexConfirmedAt = Date()
    settings.codexModel = ""
    #expect(LLMEndpoint(settings: settings) == nil)
    settings.codexModel = "gpt-5.6-terra"
    settings.codexContextTokens = 10
    let endpoint = try #require(LLMEndpoint(settings: settings))
    #expect(endpoint.contextTokens == 1_024)
    settings.codexContextTokens = 272_000
    #expect(LLMEndpoint(settings: settings)?.contextTokens == 272_000)
    #expect(LLMEndpoint(settings: settings)?.maxConcurrentRequests == 2)
    #expect(LLMEndpoint(settings: settings)?.requestTimeout == .seconds(240))
    #expect(LLMEndpoint(settings: settings)?.structuredOutputMode == .jsonSchema)
    #expect(LLMEndpoint.codex(model: "m", contextTokens: 0).contextTokens == 1_024)
  }
}

/// The findings of the 2026-09-30 review as seen through the client.
@Suite struct CodexClientReviewTests {
  /// A plan limit delivered inside the stream is as final as one on the
  /// status line: no backoff.
  @Test func aPlanLimitInsideTheStreamIsFinal() async throws {
    let harness = try CodexHarness()
    defer { harness.stop() }
    harness.backend.enqueue(
      Scripts.responsesErrorEvent("Your plan does not include this.", code: "usage_not_included"))
    let error = await #expect(throws: LLMError.self) {
      try await harness.client.complete(ClientHarness.request(format: .text))
    }
    #expect(
      error
        == .http(status: 400, body: "ChatGPT plan limit reached: Your plan does not include this."))
    #expect(error?.isRetryable == false)
    #expect(harness.backend.requests.count == 1)

    let overloaded = try CodexHarness()
    defer { overloaded.stop() }
    overloaded.backend.enqueue(
      Scripts.responsesErrorEvent("busy", code: "server_is_overloaded"),
      Scripts.responsesStream("ok"))
    let driver = overloaded.driveRetries()
    defer { driver.cancel() }
    let response = try await overloaded.client.complete(ClientHarness.request(format: .text))
    #expect(response.text == "ok", "an overload is retried like a dropped connection")
    #expect(overloaded.backend.requests.count == 2)
  }

  /// A 5xx from the token endpoint is a transport failure: one backoff,
  /// then the refresh is tried again.
  @Test func aTokenEndpointHiccupBacksOffLikeATransportError() async throws {
    let harness = try CodexHarness()
    defer { harness.stop() }
    try harness.home.write(access: CodexHome.accessToken(expiresIn: 10))
    let fresh = CodexHome.accessToken(expiresIn: 3_600)
    harness.home.server.enqueue(
      Scripts.serverError(503), Scripts.tokenRefresh(access: fresh, refresh: "rt_2"))
    harness.backend.enqueue(Scripts.responsesStream("ok"))
    let driver = harness.driveRetries()
    defer { driver.cancel() }
    let response = try await harness.client.complete(ClientHarness.request(format: .text))
    #expect(response.text == "ok")
    #expect(harness.home.server.requests.count == 2)
    #expect(harness.backend.requests.first?.authorization == "Bearer \(fresh)")
    #expect(
      harness.events.contains {
        if case .retrying(_, 1, .transport) = $0 { return true } else { return false }
      })

    // A spent token stays final: no backoff, the credential error surfaces.
    let expired = try CodexHarness()
    defer { expired.stop() }
    try expired.home.write(access: CodexHome.accessToken(expiresIn: 10))
    expired.home.server.enqueue(Scripts.tokenRefreshRejected(code: "refresh_token_expired"))
    let error = await #expect(throws: CodexCredentialError.self) {
      try await expired.client.complete(ClientHarness.request(format: .text))
    }
    guard case .signInExpired? = error else {
      Issue.record("expected signInExpired, got \(String(describing: error))")
      return
    }
    #expect(expired.backend.requests.isEmpty)
  }

  /// A 401 when the CLI has rotated the file meanwhile: the file's token
  /// goes out next, the token endpoint is not called.
  @Test func unauthorizedWithARotatedFileRereadsInsteadOfRefreshing() async throws {
    let harness = try CodexHarness()
    defer { harness.stop() }
    let rotated = CodexHome.accessToken(expiresIn: 3_600, plan: "pro")
    let home = harness.home
    harness.backend.respond { request in
      if request.index == 0 {
        try? home.write(access: rotated, refresh: "rt_cli")
        return Scripts.unauthorized()
      }
      return Scripts.responsesStream("ok")
    }
    let response = try await harness.client.complete(ClientHarness.request(format: .text))
    #expect(response.text == "ok")
    #expect(
      harness.backend.requests.map(\.authorization) == [
        "Bearer \(CodexHome.accessToken(expiresIn: 3_600))", "Bearer \(rotated)",
      ])
    #expect(harness.home.server.requests.isEmpty)
  }

  /// The confirmation gate cannot be walked around by pasting the backend's
  /// address as a server: that stays an endpoint with the API key.
  @Test func aCodexAddressUnderTheEndpointProviderIsNotTheCodexBackend() throws {
    var settings = Settings()
    settings.llmBaseURL = LLMEndpoint.codexBackendURL
    settings.llmModel = "gpt-5.6-terra"
    let endpoint = try #require(LLMEndpoint(settings: settings))
    #expect(!endpoint.isCodexBackend)
    #expect(endpoint.provider == .endpoint)
    #expect(LLMEndpoint.codex(model: "m", contextTokens: 1).isCodexBackend)
  }
}
