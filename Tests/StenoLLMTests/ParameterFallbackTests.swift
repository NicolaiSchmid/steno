import Foundation
import StenoCore
import Testing

@testable import StenoLLM

/// OpenAI's reasoning models reject `max_tokens` (wanting
/// `max_completion_tokens`) and any `temperature` with a 400 that names the
/// parameter in `error.param`. The client resends with the parameter
/// renamed or dropped, without consuming an attempt or sleeping, and keeps
/// spelling later requests that way. A 400 naming anything else, or naming
/// a parameter already adjusted, is a plain HTTP error.
@Suite struct ParameterFallbackTests {
  static let schemaRequest = ClientHarness.request(
    format: .jsonSchema(name: "reply", schema: ["type": "object"], strict: true))

  @Test func maxTokensAndTemperatureRejectionsAreResentAndRemembered() async throws {
    let harness = try ClientHarness()
    defer { harness.stop() }
    harness.server.respond(with: Scripts.reasoningModel(completion: Scripts.completion("{}")))

    let response = try await harness.client.complete(Self.schemaRequest)
    #expect(response.text == "{}")
    let wire = harness.server.requests.compactMap(\.chat)
    #expect(wire.map(\.maxTokens) == [64, nil, nil])
    #expect(wire.map(\.maxCompletionTokens) == [nil, 64, 64])
    #expect(wire.map(\.temperature) == [0, 0, nil])
    #expect(wire.allSatisfy { $0.responseFormat?.type == "json_schema" }, "the mode is untouched")
    #expect(harness.events.contains(.parameterRejected("max_tokens")))
    #expect(harness.events.contains(.parameterRejected("temperature")))
    #expect(harness.clock.pendingSleepers == 0, "an adjustment is not a retry and never sleeps")
    #expect(!harness.events.contains { if case .retrying = $0 { true } else { false } })

    // Remembered: the next request goes out in the accepted spelling at once.
    _ = try await harness.client.complete(ClientHarness.request(format: .text))
    #expect(harness.server.requests.count == 4)
    let last = try #require(harness.server.requests.last?.chat)
    #expect(last.maxTokens == nil)
    #expect(last.maxCompletionTokens == 64)
    #expect(last.temperature == nil)
    #expect(await harness.client.resolvedMode == .jsonSchema)
  }

  @Test func theEndpointCeilingIsRenamedToo() async throws {
    let harness = try ClientHarness { $0.maxOutputTokens = 777 }
    defer { harness.stop() }
    harness.server.respond(with: Scripts.reasoningModel(completion: Scripts.completion("ok")))
    var request = ClientHarness.request(format: .text)
    request.maxTokens = nil
    request.temperature = nil
    _ = try await harness.client.complete(request)
    let wire = harness.server.requests.compactMap(\.chat)
    #expect(wire.map(\.maxTokens) == [777, nil])
    #expect(wire.map(\.maxCompletionTokens) == [nil, 777])
    #expect(harness.events.filter { $0 == .parameterRejected("max_tokens") }.count == 1)
  }

  @Test func aRepeatedRejectionOfTheSameParameterIsAPlainHTTPError() async throws {
    let harness = try ClientHarness(retry: .none)
    defer { harness.stop() }
    harness.server.respond { _ in Scripts.rejectsParameter("max_tokens") }
    let error = await #expect(throws: LLMError.self) {
      try await harness.client.complete(Self.schemaRequest)
    }
    #expect(
      error
        == .http(
          status: 400,
          body: "Unsupported parameter: 'max_tokens' is not supported with this model."))
    #expect(harness.server.requests.count == 2, "renamed once, then given up")
    #expect(harness.server.requests.last?.chat?.maxCompletionTokens == 64)
  }

  @Test func a400NamingAnotherParameterIsAPlainHTTPError() async throws {
    let harness = try ClientHarness(retry: .none)
    defer { harness.stop() }
    harness.server.enqueue(Scripts.rejectsParameter("messages"))
    let error = await #expect(throws: LLMError.self) {
      try await harness.client.complete(Self.schemaRequest)
    }
    guard case .http(let status, _) = error else {
      Issue.record("expected http, got \(String(describing: error))")
      return
    }
    #expect(status == 400)
    #expect(harness.server.requests.count == 1)
    #expect(!harness.events.contains { if case .parameterRejected = $0 { true } else { false } })
    #expect(harness.server.requests.first?.chat?.maxTokens == 64)
  }

  @Test func theParamIsReadFromTheEnvelope() {
    let reply = OpenAICompatibleClient.Reply(
      status: 400, headers: [:],
      body: Data(
        "{\"error\":{\"message\":\"Unsupported value\",\"type\":\"invalid_request_error\",\"param\":\"temperature\",\"code\":\"unsupported_value\"}}"
          .utf8))
    #expect(OpenAICompatibleClient.rejectedParameter(reply) == "temperature")
    let plain = OpenAICompatibleClient.Reply(
      status: 400, headers: [:], body: Data("{\"error\":{\"message\":\"nope\"}}".utf8))
    #expect(OpenAICompatibleClient.rejectedParameter(plain) == nil)
    #expect(OpenAICompatibleClient.adjustableParameters == ["max_tokens", "temperature"])
  }
}
