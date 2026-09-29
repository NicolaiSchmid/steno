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
    let body = CodexResponsesClient.body(
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
    let plain = CodexResponsesClient.body(
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

  @Test func aFourOhOneRefreshesOnceThenStands() async throws {
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
        #expect(request.path.contains("client_version=99.0.0"))
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
}
