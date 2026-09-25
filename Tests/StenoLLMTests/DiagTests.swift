import Foundation
import StenoCore
import Testing

@testable import StenoLLM

#if canImport(FoundationNetworking)
  import FoundationNetworking
#endif

/// Temporary: locates the Darwin-only startup trap by logging each step to
/// stderr (unbuffered) before performing it.
@Suite(.serialized) struct DiagTests {
  static func log(_ message: String) {
    FileHandle.standardError.write(Data("DIAG: \(message)\n".utf8))
  }

  @Test func a_pureTypes() throws {
    Self.log("start pure types")
    let policy = RetryPolicy()
    Self.log("policy delay \(policy.delay(beforeRetry: 3))")
    let schema = JSONSchema.object(["a": .string(), "b": .integer().nullable])
    Self.log("schema prompt \(schema.promptText)")
    Self.log("schema json \(schema.jsonValue)")
    let decoded = try StructuredOutputDecoder().decode(
      [String: Int].self, from: LLMResponse(text: "```json\n{\"x\": 1}\n```", finishReason: .stop))
    Self.log("decoded \(decoded)")
    let endpoint = LLMEndpoint(baseURL: URL(string: "http://127.0.0.1:1/v1")!, model: "m")
    Self.log("endpoint \(endpoint.chatCompletionsURL)")
    Self.log("error \(LLMError.rateLimited(retryAfter: .seconds(3)))")
    Self.log("clock start")
    let clock = ManualClock()
    Self.log("clock now \(clock.now)")
    Self.log("end pure types")
  }

  @Test func b_serverInit() throws {
    Self.log("server init start")
    let server = try StubChatServer()
    Self.log("server bound on \(server.port) \(server.baseURL)")
    server.enqueue(Scripts.completion("hi"))
    Self.log("enqueued")
    server.stop()
    Self.log("stopped")
  }

  @Test func c_serverRoundTrip() async throws {
    Self.log("roundtrip start")
    let server = try StubChatServer()
    defer { server.stop() }
    server.enqueue(Scripts.completion("hi"))
    var request = URLRequest(url: server.baseURL.appendingPathComponent("chat/completions"))
    request.httpMethod = "POST"
    request.httpBody = Data("{}".utf8)
    Self.log("sending")
    let (data, response) = try await URLSession.shared.data(for: request)
    Self.log("got \((response as? HTTPURLResponse)?.statusCode ?? -1) \(data.count) bytes")
    await server.received(atLeast: 1)
    Self.log("recorded \(server.requests.count)")
  }

  @Test func d_client() async throws {
    Self.log("client start")
    let server = try StubChatServer()
    defer { server.stop() }
    server.enqueue(Scripts.completion("{}"))
    let endpoint = LLMEndpoint(baseURL: server.baseURL, model: "m")
    let clock = ManualClock()
    Self.log("making client")
    let client = OpenAICompatibleClient(
      endpoint: endpoint, apiKey: "k", retry: .none, clock: clock,
      observer: { event in Self.log("event \(event)") })
    Self.log("completing")
    let response = try await client.complete(
      LLMRequest(messages: [LLMMessage(role: .user, content: "x")], purpose: "diag"))
    Self.log("response \(response.text)")
  }

  @Test func e_clientTimeoutAndCancel() async throws {
    Self.log("timeout start")
    let server = try StubChatServer()
    defer { server.stop() }
    server.enqueue(Scripts.hang)
    let endpoint = LLMEndpoint(baseURL: server.baseURL, model: "m")
    let clock = ManualClock()
    let client = OpenAICompatibleClient(
      endpoint: endpoint, apiKey: "k", retry: .none, clock: clock,
      observer: { event in Self.log("event \(event)") })
    let task = Task {
      try await client.complete(
        LLMRequest(messages: [LLMMessage(role: .user, content: "x")], purpose: "diag"))
    }
    await server.received(atLeast: 1)
    Self.log("received hang request; cancelling")
    task.cancel()
    do {
      _ = try await task.value
      Self.log("no error?")
    } catch {
      Self.log("cancel error \(error)")
    }
    Self.log("timeout end")
  }
}
