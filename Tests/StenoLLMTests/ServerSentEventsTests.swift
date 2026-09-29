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
    #expect(
      ServerSentEvents.parse("event: a\rdata: {}\r\r") == [ServerSentEvent(event: "a", data: "{}")])
  }

  /// The specification's corners: an event with no `data:` line is not
  /// dispatched (so its name does not leak onto the next event), a field
  /// without a colon has an empty value, exactly one space after the colon
  /// is optional, and unknown fields are skipped.
  @Test func anEventWithoutDataIsDroppedAndFieldsParseAsTheSpecificationSays() {
    #expect(ServerSentEvents.parse("").isEmpty)
    #expect(ServerSentEvents.parse("event: response.completed\n\n").isEmpty)
    #expect(ServerSentEvents.parse(": keep-alive\n\nid: 3\nretry: 100\n\n").isEmpty)
    #expect(
      ServerSentEvents.parse("event: a\n\ndata: x\n\n") == [ServerSentEvent(event: nil, data: "x")])
    #expect(ServerSentEvents.parse("data\n\n") == [ServerSentEvent(event: nil, data: "")])
    #expect(ServerSentEvents.parse("data:{}\n\n") == [ServerSentEvent(event: nil, data: "{}")])
    #expect(
      ServerSentEvents.parse("data:  two\n\n") == [ServerSentEvent(event: nil, data: " two")])
    #expect(
      ServerSentEvents.parse("data: {\"k\": \"a:b\"}\n\n")
        == [ServerSentEvent(event: nil, data: "{\"k\": \"a:b\"}")],
      "only the first colon splits the field")
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
    let unknownReason = Scripts.responsesStream(
      "x", status: "incomplete", incompleteReason: "something_new")
    #expect(
      try CodexResponsesClient.parseStream(unknownReason.body, redact: { $0 }).finishReason
        == .other)
    #expect(throws: LLMError.transport("[redacted] said no")) {
      try CodexResponsesClient.parseStream(
        Scripts.responsesErrorEvent("acct_1 said no").body,
        redact: { $0.replacingOccurrences(of: "acct_1", with: "[redacted]") })
    }
  }

  /// The stream as the backend may vary it: `type` read from the JSON when
  /// no `event:` line names it, delta and unknown events skipped, a
  /// `reasoning` item skipped even when it carries text, a `[DONE]`
  /// sentinel skipped, several message items concatenated in order, the
  /// items from the events preferred over the terminal event's `output`,
  /// and a missing `usage` counted as one request of zero tokens.
  @Test func dataOnlyUnknownReasoningAndMultiItemStreams() throws {
    let body = #"""
      data: {"type":"response.created","response":{"id":"r","status":"in_progress"}}

      data: {"type":"response.output_text.delta","delta":"{\"ok"}

      data: {"type":"response.output_item.done","item":{"type":"reasoning","id":"rs_1","summary":[],"content":[{"type":"output_text","text":"thinking"}]}}

      data: {"type":"response.output_item.done","item":{"type":"message","id":"m1","role":"assistant","content":[{"type":"output_text","text":"{\"ok\":"}]}}

      event: some.future.event
      data: {"type":"some.future.event","whatever":1}

      data: {"type":"response.output_item.done","item":{"type":"function_call","id":"fc_1","name":"tool","arguments":"{}"}}

      data: {"type":"response.output_item.done","item":{"type":"message","id":"m2","role":"assistant","content":[{"type":"output_text","text":"true}"},{"type":"output_text","text":""}]}}

      data: [DONE]

      data: {"type":"response.completed","response":{"id":"r","status":"completed","model":"gpt-x","output":[{"type":"message","id":"m1","role":"assistant","content":[{"type":"output_text","text":"not this"}]}]}}

      """#
    let response = try CodexResponsesClient.parseStream(Data(body.utf8), redact: { $0 })
    #expect(response.text == "{\"ok\":true}")
    #expect(response.finishReason == .stop)
    #expect(response.usage == LLMUsage(promptTokens: 0, completionTokens: 0, requests: 1))
    #expect(response.model == "gpt-x")
  }

  /// A terminal event that arrives without any item event (a backend that
  /// only sends the final object) is read from its `output`; a partial
  /// `usage` fills the missing count with zero; and the `event:` name is
  /// what dispatches, so a `response.completed` event whose JSON says
  /// otherwise still completes the stream.
  @Test func aTerminalEventWithoutItemEventsUsesItsOutput() throws {
    let body = #"""
      event: response.completed
      data: {"type":"response.in_progress","response":{"status":"completed","output":[{"type":"reasoning","id":"rs"},{"type":"message","role":"assistant","content":[{"type":"output_text","text":"from "},{"type":"refusal","refusal":""}]},{"type":"message","role":"assistant","content":[{"type":"output_text","text":"output"}]}],"usage":{"input_tokens":3}}}

      """#
    let response = try CodexResponsesClient.parseStream(Data(body.utf8), redact: { $0 })
    #expect(response.text == "from output")
    #expect(response.usage == LLMUsage(promptTokens: 3, completionTokens: 0, requests: 1))
    #expect(response.model == nil)
    // Without a terminal event nothing else counts, however many items came.
    let items = #"""
      data: {"type":"response.output_item.done","item":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"x"}]}}

      data: {"type":"response.in_progress","response":{"status":"in_progress"}}

      """#
    #expect(throws: LLMError.transport("stream closed before response.completed")) {
      try CodexResponsesClient.parseStream(Data(items.utf8), redact: { $0 })
    }
  }
}
