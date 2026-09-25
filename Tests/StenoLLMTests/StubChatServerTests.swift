import Foundation
import StenoCore
import Testing

@testable import StenoLLM

#if canImport(FoundationNetworking)
  import FoundationNetworking
#endif

@Suite struct StubChatServerTests {
  @Test func acceptsAPostReturnsTheScriptAndRecordsTheParsedRequest() async throws {
    let server = try StubChatServer()
    defer { server.stop() }
    server.enqueue(
      Scripts.completion(
        "hello", usage: LLMUsage(promptTokens: 3, completionTokens: 1, requests: 1)))

    let body = ChatCompletionRequest(
      model: "stub-model",
      messages: [ChatMessage(role: "user", content: "hi")],
      temperature: 0,
      maxTokens: 16,
      responseFormat: .jsonObject)
    var request = URLRequest(url: server.baseURL.appendingPathComponent("chat/completions"))
    request.httpMethod = "POST"
    request.setValue("Bearer sk-test", forHTTPHeaderField: "Authorization")
    request.setValue("cleanup", forHTTPHeaderField: "X-Steno-Purpose")
    request.setValue("application/json", forHTTPHeaderField: "Content-Type")
    request.httpBody = try WireJSON.encode(body)

    let (data, response) = try await URLSession.shared.data(for: request)
    let http = try #require(response as? HTTPURLResponse)
    #expect(http.statusCode == 200)
    let decoded = try WireJSON.decode(ChatCompletionResponse.self, from: data)
    #expect(decoded.choices.first?.message.content == "hello")
    #expect(decoded.choices.first?.finishReason == "stop")
    #expect(decoded.usage?.promptTokens == 3)

    await server.received(atLeast: 1)
    let recorded = try #require(server.requests.first)
    #expect(recorded.method == "POST")
    #expect(recorded.path == "/v1/chat/completions")
    #expect(recorded.authorization == "Bearer sk-test")
    #expect(recorded.purpose == "cleanup")
    #expect(recorded.chat == body)
    #expect(recorded.inFlightOnArrival == 1)
    #expect(server.maxInFlight == 1)
  }

  @Test func answersUnscriptedRequestsWith404AndConsultsTheResponder() async throws {
    let server = try StubChatServer()
    defer { server.stop() }
    var request = URLRequest(url: server.baseURL.appendingPathComponent("models"))
    request.httpMethod = "GET"
    let (_, first) = try await URLSession.shared.data(for: request)
    #expect((first as? HTTPURLResponse)?.statusCode == 404)

    server.respond(with: Scripts.server(models: ["a", "b"], completion: Scripts.completion("x")))
    let (data, second) = try await URLSession.shared.data(for: request)
    #expect((second as? HTTPURLResponse)?.statusCode == 200)
    #expect(try WireJSON.decode(ModelList.self, from: data).data.map(\.id) == ["a", "b"])
    #expect(server.requests.count == 2)
  }

  @Test func heldResponsesShowUpAsInFlightUntilReleased() async throws {
    let server = try StubChatServer()
    defer { server.stop() }
    server.enqueue(contentsOf: [Scripts.completion("1"), Scripts.completion("2")])
    server.holdResponses()
    var request = URLRequest(url: server.baseURL.appendingPathComponent("chat/completions"))
    request.httpMethod = "POST"
    request.httpBody = Data("{}".utf8)
    let first = Task { try await URLSession.shared.data(for: request) }
    let second = Task { try await URLSession.shared.data(for: request) }
    await server.received(atLeast: 2)
    #expect(server.inFlight == 2)
    server.release()
    _ = try await first.value
    _ = try await second.value
    #expect(server.maxInFlight == 2)
    #expect(server.inFlight == 0)
  }

  @Test func wireTypesRoundTripThroughTheirSnakeCaseKeys() throws {
    let request = ChatCompletionRequest(
      model: "m", messages: [ChatMessage(LLMMessage(role: .system, content: "s"))],
      temperature: 0.2, maxTokens: 100,
      responseFormat: .jsonSchema(name: "n", schema: ["type": "object"], strict: true))
    let json = String(decoding: try WireJSON.encode(request), as: UTF8.self)
    #expect(json.contains("\"max_tokens\":100"))
    #expect(json.contains("\"response_format\":{\"json_schema\":{\"name\":\"n\""))
    #expect(try WireJSON.decode(ChatCompletionRequest.self, from: Data(json.utf8)) == request)

    let parts = """
      {"choices":[{"message":{"role":"assistant","content":[{"type":"text","text":"a"},{"type":"text","text":"b"}]},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":2}}
      """
    let response = try WireJSON.decode(ChatCompletionResponse.self, from: Data(parts.utf8))
    #expect(response.choices.first?.message.content == "ab")
    #expect(response.usage?.completionTokens == 2)

    let error = try WireJSON.decode(
      ChatErrorEnvelope.self,
      from: Data("{\"error\":{\"message\":\"m\",\"code\":400}}".utf8))
    #expect(error.error.code == 400)
  }
}
