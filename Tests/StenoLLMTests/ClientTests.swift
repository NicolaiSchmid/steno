import Foundation
import StenoCore
import Testing

@testable import StenoLLM

@Suite struct ClientTests {
  @Test func sendsBearerAuthBodyAndPurposeAndParsesTheReply() async throws {
    let harness = try ClientHarness()
    defer { harness.stop() }
    harness.server.enqueue(
      Scripts.completion(
        "{\"hi\":true}", usage: LLMUsage(promptTokens: 12, completionTokens: 4, requests: 1),
        model: "served-model"))

    let request = ClientHarness.request(
      purpose: "cleanup",
      format: .jsonSchema(name: "reply", schema: ["type": "object"], strict: true))
    let response = try await harness.client.complete(request)
    #expect(response.text == "{\"hi\":true}")
    #expect(response.finishReason == .stop)
    #expect(response.usage == LLMUsage(promptTokens: 12, completionTokens: 4, requests: 1))
    #expect(response.model == "served-model")

    let recorded = try #require(harness.server.requests.first)
    #expect(recorded.path == "/v1/chat/completions")
    #expect(recorded.authorization == "Bearer \(ClientHarness.apiKey)")
    #expect(recorded.purpose == "cleanup")
    #expect(recorded.headers["content-type"] == "application/json")
    let chat = try #require(recorded.chat)
    #expect(chat.model == "stub-model")
    #expect(chat.messages.map(\.role) == ["system", "user"])
    #expect(chat.temperature == 0)
    #expect(chat.maxTokens == 64)
    #expect(chat.responseFormat?.type == "json_schema")
    #expect(chat.responseFormat?.jsonSchema?.strict == true)
    #expect(chat.responseFormat?.jsonSchema?.name == "reply")
    #expect(await harness.client.resolvedMode == .jsonSchema)
  }

  @Test func omitsAuthorizationWithoutAKeyAndDefaultsMaxTokens() async throws {
    let harness = try ClientHarness(apiKey: nil) { $0.maxOutputTokens = 777 }
    defer { harness.stop() }
    harness.server.enqueue(Scripts.completion("ok"))
    var request = ClientHarness.request(format: .text)
    request.maxTokens = nil
    _ = try await harness.client.complete(request)
    let recorded = try #require(harness.server.requests.first)
    #expect(recorded.authorization == nil)
    #expect(recorded.chat?.maxTokens == 777)
    #expect(recorded.chat?.responseFormat == nil)
  }

  @Test func unauthorizedIsNotRetried() async throws {
    let harness = try ClientHarness()
    defer { harness.stop() }
    harness.server.enqueue(Scripts.unauthorized())
    let error = await #expect(throws: LLMError.self) {
      try await harness.client.complete(ClientHarness.request())
    }
    #expect(error == .http(status: 401, body: "Incorrect API key provided"))
    #expect(harness.server.requests.count == 1)
    #expect(harness.clock.pendingSleepers == 0)
  }

  @Test func lengthAndRefusalSurfaceAsFinishReasonAndError() async throws {
    let harness = try ClientHarness()
    defer { harness.stop() }
    harness.server.enqueue(Scripts.truncated("{\"segments\": [{\"index\": 0, \"te"))
    let response = try await harness.client.complete(ClientHarness.request())
    #expect(response.finishReason == .length)

    harness.server.enqueue(Scripts.refusal("I cannot help with that."))
    let error = await #expect(throws: LLMError.self) {
      try await harness.client.complete(ClientHarness.request())
    }
    #expect(error == .refused("I cannot help with that."))
  }

  @Test func cancelThrowsCancellationErrorAfterExactlyOneRequest() async throws {
    let harness = try ClientHarness()
    defer { harness.stop() }
    harness.server.enqueue(.hang)
    let task = Task { try await harness.client.complete(ClientHarness.request()) }
    await harness.server.received(atLeast: 1)
    task.cancel()
    await #expect(throws: CancellationError.self) { try await task.value }
    #expect(harness.server.requests.count == 1)
  }

  @Test func redactsTheKeyFromEveryErrorAndEvent() async throws {
    let key = ClientHarness.apiKey
    let harness = try ClientHarness(retry: .none)
    defer { harness.stop() }
    harness.server.enqueue(Scripts.badRequest("rejected key \(key) for this model"))
    let http = await #expect(throws: LLMError.self) {
      try await harness.client.complete(ClientHarness.request(format: .text))
    }
    #expect(http == .http(status: 400, body: "rejected key [redacted] for this model"))

    harness.server.enqueue(Scripts.refusal("no, \(key)"))
    let refused = await #expect(throws: LLMError.self) {
      try await harness.client.complete(ClientHarness.request())
    }
    #expect(refused == .refused("no, [redacted]"))

    let everyCase: [LLMError] = [
      http!, .transport("boom"), .timeout,
      .rateLimited(retryAfter: .seconds(3)), .rateLimited(retryAfter: nil),
      .invalidJSON("x"), .truncated, refused!,
      .transcriptTooLong(estimatedTokens: 1, budget: 2),
    ]
    for error in everyCase {
      #expect(!String(describing: error).contains(key), "\(error)")
      #expect(!error.description.isEmpty)
    }
    for event in harness.events {
      #expect(!String(describing: event).contains(key), "\(event)")
    }
    #expect(OpenAICompatibleClient.redact("a \(key) b", apiKey: key) == "a [redacted] b")
    #expect(OpenAICompatibleClient.redact("a b", apiKey: nil) == "a b")
  }

  @Test func probeReportsModelsModeAndRoundTrip() async throws {
    let harness = try ClientHarness()
    defer { harness.stop() }
    harness.server.respond(
      with: Scripts.server(
        models: ["other", "stub-model"], completion: Scripts.completion("{\"ok\":true}")))
    let probe = try await harness.client.probe()
    #expect(
      probe
        == EndpointProbe(
          modelListed: true, resolvedMode: .jsonSchema, roundTrip: .zero))
    let paths = harness.server.requests.map { "\($0.method) \($0.path)" }
    #expect(paths == ["GET /v1/models", "POST /v1/chat/completions"])
    #expect(harness.server.requests.last?.purpose == "probe")
  }

  @Test func probeWithoutAModelListStillSucceeds() async throws {
    let harness = try ClientHarness()
    defer { harness.stop() }
    harness.server.respond { request in
      request.method == "GET"
        ? Scripts.badRequest("no such route") : Scripts.completion("{\"ok\":true}")
    }
    let probe = try await harness.client.probe()
    #expect(probe.modelListed == nil)
  }

  @Test func endpointFromSettingsNeedsBothURLAndModel() throws {
    var settings = Settings()
    #expect(LLMEndpoint(settings: settings) == nil)
    settings.llmBaseURL = URL(string: "http://127.0.0.1:1234/v1/")
    #expect(LLMEndpoint(settings: settings) == nil)
    settings.llmModel = ""
    #expect(LLMEndpoint(settings: settings) == nil)
    settings.llmModel = "local-model"
    settings.llmContextTokens = 16_000
    let endpoint = try #require(LLMEndpoint(settings: settings))
    #expect(endpoint.model == "local-model")
    #expect(endpoint.contextTokens == 16_000)
    #expect(
      endpoint.chatCompletionsURL.absoluteString == "http://127.0.0.1:1234/v1/chat/completions")
    #expect(endpoint.modelsURL.absoluteString == "http://127.0.0.1:1234/v1/models")
    #expect(endpoint.structuredOutputMode == .jsonSchema)
    #expect(endpoint.maxConcurrentRequests == 2)
    #expect(endpoint.requestTimeout == .seconds(240))
  }

  @Test func retryAfterAndErrorMessageParsing() {
    #expect(OpenAICompatibleClient.retryAfter("7") == .seconds(7))
    #expect(OpenAICompatibleClient.retryAfter(" 1.5 ") == .milliseconds(1500))
    #expect(OpenAICompatibleClient.retryAfter("Wed, 21 Oct 2026 07:28:00 GMT") == nil)
    #expect(OpenAICompatibleClient.retryAfter(nil) == nil)
    // Values `Double` parses but `Duration.seconds` would trap on, or that
    // make no sense as a wait: nil or the cap, never a crash.
    #expect(OpenAICompatibleClient.retryAfter("inf") == nil)
    #expect(OpenAICompatibleClient.retryAfter("infinity") == nil)
    #expect(OpenAICompatibleClient.retryAfter("nan") == nil)
    #expect(OpenAICompatibleClient.retryAfter("-1") == nil)
    #expect(OpenAICompatibleClient.retryAfter("1e300") == .seconds(3_600))
    #expect(OpenAICompatibleClient.retryAfter("1e19") == .seconds(3_600))
    #expect(OpenAICompatibleClient.retryAfter("3601") == .seconds(3_600))
    #expect(
      OpenAICompatibleClient.complainsAboutResponseFormat("Invalid parameter: 'response_format'"))
    #expect(OpenAICompatibleClient.complainsAboutResponseFormat("json_schema is not supported"))
    #expect(!OpenAICompatibleClient.complainsAboutResponseFormat("model not found"))
    let html = OpenAICompatibleClient.Reply(
      status: 502, headers: [:], body: Data("<html>bad gateway</html>".utf8))
    #expect(OpenAICompatibleClient.errorMessage(html) == "<html>bad gateway</html>")
  }
}
