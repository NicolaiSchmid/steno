import Foundation
import StenoCore
import Testing

@testable import StenoLLM

@Suite struct ServerSentEventsTests {
  @Test func parsesNamedEventsAndMultilineData() {
    let body = """
      : a comment the client ignores
      event: response.created
      data: {"a":1}

      event: response.output_text.delta
      data: {"b":
      data: 2}
      id: 7

      data: {"unnamed":true}

      """
    let events = ServerSentEvents.parse(body)
    #expect(
      events == [
        ServerSentEvent(event: "response.created", data: #"{"a":1}"#),
        ServerSentEvent(event: "response.output_text.delta", data: "{\"b\":\n2}"),
        ServerSentEvent(event: nil, data: #"{"unnamed":true}"#),
      ])
  }

  @Test func acceptsCRLFAndAStreamWithoutATrailingBlankLine() {
    let body = "event: response.completed\r\ndata: {}\r\n"
    #expect(
      ServerSentEvents.parse(body) == [ServerSentEvent(event: "response.completed", data: "{}")])
  }

  @Test func detectsAnEventStreamByHeaderOrShape() {
    #expect(
      ServerSentEvents.looksLikeEventStream(
        contentType: "text/event-stream; charset=utf-8", body: Data()))
    #expect(
      ServerSentEvents.looksLikeEventStream(
        contentType: nil, body: Data("event: x\ndata: {}\n\n".utf8)))
    #expect(
      ServerSentEvents.looksLikeEventStream(contentType: nil, body: Data("data: {}\n\n".utf8)))
    #expect(
      !ServerSentEvents.looksLikeEventStream(contentType: "application/json", body: Data("{}".utf8))
    )
    #expect(!ServerSentEvents.looksLikeEventStream(contentType: nil, body: Data("{\"id\":1}".utf8)))
  }

  @Test func aStreamBecomesOneResponse() throws {
    let stub = Scripts.responsesStream(
      "{\"ok\":true}", usage: LLMUsage(promptTokens: 43, completionTokens: 12, requests: 1),
      model: "gpt-5.6-luna")
    let response = try CodexResponsesClient.parseStream(stub.body, redact: { $0 })
    #expect(response.text == "{\"ok\":true}")
    #expect(response.finishReason == .stop)
    #expect(response.usage == LLMUsage(promptTokens: 43, completionTokens: 12, requests: 1))
    #expect(response.model == "gpt-5.6-luna")
  }

  @Test func incompleteFailedRefusedAndCutStreams() throws {
    let cut = Scripts.responsesStream(
      "partial", status: "incomplete", incompleteReason: "max_output_tokens")
    #expect(try CodexResponsesClient.parseStream(cut.body, redact: { $0 }).finishReason == .length)
    let filtered = Scripts.responsesStream(
      "", status: "incomplete", incompleteReason: "content_filter")
    #expect(
      try CodexResponsesClient.parseStream(filtered.body, redact: { $0 }).finishReason
        == .contentFilter)
    #expect(throws: LLMError.transport("boom")) {
      try CodexResponsesClient.parseStream(Scripts.responsesFailed("boom").body, redact: { $0 })
    }
    #expect(throws: LLMError.refused("no")) {
      try CodexResponsesClient.parseStream(Scripts.responsesRefusal("no").body, redact: { $0 })
    }
    #expect(throws: LLMError.transport("stream closed before response.completed")) {
      try CodexResponsesClient.parseStream(Scripts.responsesTruncatedStream().body, redact: { $0 })
    }
  }
}
