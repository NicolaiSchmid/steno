import Foundation
import StenoCore
import Testing

@testable import StenoLLM

@Suite struct StructuredDecoderTests {
  struct Reply: Codable, Sendable, Equatable {
    var ok: Bool
    var items: [String]
  }

  let decoder = StructuredOutputDecoder()

  private func response(_ text: String, finish: LLMFinishReason = .stop) -> LLMResponse {
    LLMResponse(text: text, finishReason: finish)
  }

  @Test func decodesBareJSON() throws {
    let reply = try decoder.decode(Reply.self, from: response("{\"ok\":true,\"items\":[\"a\"]}"))
    #expect(reply == Reply(ok: true, items: ["a"]))
  }

  @Test func stripsAFenceWithALanguageTag() throws {
    let text = "Sure! Here it is:\n```json\n{\"ok\": true, \"items\": []}\n```\nLet me know."
    #expect(StructuredOutputDecoder.extractJSON(text) == "{\"ok\": true, \"items\": []}")
    #expect(try decoder.decode(Reply.self, from: response(text)) == Reply(ok: true, items: []))
  }

  @Test func stripsAFenceWithoutATagAndWithoutAClosingFence() throws {
    #expect(
      StructuredOutputDecoder.extractJSON("```\n{\"ok\":false,\"items\":[]}\n```")
        == "{\"ok\":false,\"items\":[]}")
    #expect(
      StructuredOutputDecoder.extractJSON("```json\n{\"ok\":false,\"items\":[]}")
        == "{\"ok\":false,\"items\":[]}")
  }

  @Test func stripsProseBeforeAndAfterTheObject() throws {
    let text = "The cleaned segments are: {\"ok\": true, \"items\": [\"x}\"]} — done."
    #expect(StructuredOutputDecoder.extractJSON(text) == "{\"ok\": true, \"items\": [\"x}\"]}")
    #expect(try decoder.decode(Reply.self, from: response(text)) == Reply(ok: true, items: ["x}"]))
  }

  @Test func handlesArraysAtTheRoot() throws {
    #expect(StructuredOutputDecoder.extractJSON("Result: [1, 2, 3].") == "[1, 2, 3]")
    #expect(try decoder.decode([Int].self, from: response("Result: [1, 2, 3].")) == [1, 2, 3])
  }

  @Test func truncatedJSONIsInvalidJSONWithAPath() {
    let error = #expect(throws: LLMError.self) {
      try decoder.decode(Reply.self, from: response("{\"ok\": true, \"items\": [\"a\", \"b"))
    }
    guard case .invalidJSON(let detail) = error else {
      Issue.record("expected invalidJSON, got \(String(describing: error))")
      return
    }
    #expect(detail.contains("malformed JSON"))
  }

  @Test func missingKeysAndWrongTypesNameThePath() {
    let missing = #expect(throws: LLMError.self) {
      try decoder.decode(Reply.self, from: response("{\"ok\": true}"))
    }
    #expect(missing == .invalidJSON("missing key items at root"))
    let mismatch = #expect(throws: LLMError.self) {
      try decoder.decode(Reply.self, from: response("{\"ok\": true, \"items\": [1]}"))
    }
    guard case .invalidJSON(let detail) = mismatch else {
      Issue.record("expected invalidJSON")
      return
    }
    #expect(detail.hasPrefix("expected String at items.[0]"))
  }

  @Test func lengthFinishReasonIsTruncatedBeforeAnyParsing() {
    let error = #expect(throws: LLMError.self) {
      try decoder.decode(Reply.self, from: response("{\"ok\":true,\"items\":[]}", finish: .length))
    }
    #expect(error == .truncated)
  }

  @Test func emptyAnswerIsInvalidJSON() {
    let error = #expect(throws: LLMError.self) {
      try decoder.decode(Reply.self, from: response("   \n"))
    }
    #expect(error == .invalidJSON("empty answer"))
  }
}
