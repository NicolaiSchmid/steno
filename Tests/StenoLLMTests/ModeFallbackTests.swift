import Foundation
import StenoCore
import Testing

@testable import StenoLLM

@Suite struct ModeFallbackTests {
  static let schemaRequest = ClientHarness.request(
    format: .jsonSchema(name: "reply", schema: ["type": "object"], strict: true))

  @Test func a400NamingResponseFormatFlipsToJSONObjectAndIsRemembered() async throws {
    let harness = try ClientHarness()
    defer { harness.stop() }
    harness.server.respond(
      with: Scripts.server(rejecting: ["json_schema"], completion: Scripts.completion("{}")))

    #expect(await harness.client.resolvedMode == .jsonSchema)
    _ = try await harness.client.complete(Self.schemaRequest)
    #expect(await harness.client.resolvedMode == .jsonObject)
    #expect(
      harness.server.requests.map { $0.chat?.responseFormat?.type } == [
        "json_schema", "json_object",
      ])
    #expect(harness.events.contains(.modeDowngraded(to: .jsonObject)))

    _ = try await harness.client.complete(Self.schemaRequest)
    #expect(harness.server.requests.count == 3)
    #expect(harness.server.requests.last?.chat?.responseFormat?.type == "json_object")
    #expect(harness.clock.pendingSleepers == 0, "a mode downgrade is not a retry and never sleeps")
  }

  @Test func aSecond400FallsToPromptOnlyAndSendsNoResponseFormat() async throws {
    let harness = try ClientHarness()
    defer { harness.stop() }
    harness.server.respond(
      with: Scripts.server(
        rejecting: ["json_schema", "json_object"], completion: Scripts.completion("{}")))
    _ = try await harness.client.complete(Self.schemaRequest)
    #expect(await harness.client.resolvedMode == .promptOnly)
    #expect(
      harness.server.requests.map { $0.chat?.responseFormat?.type } == [
        "json_schema", "json_object", nil,
      ])
  }

  @Test func a400WithoutAResponseFormatComplaintIsAPlainHTTPError() async throws {
    let harness = try ClientHarness()
    defer { harness.stop() }
    harness.server.enqueue(Scripts.badRequest("model `stub-model` does not exist"))
    let error = await #expect(throws: LLMError.self) {
      try await harness.client.complete(Self.schemaRequest)
    }
    #expect(error == .http(status: 400, body: "model `stub-model` does not exist"))
    #expect(await harness.client.resolvedMode == .jsonSchema)
    #expect(harness.server.requests.count == 1)
  }

  @Test func aConfiguredModeIsTheStartingPoint() async throws {
    let harness = try ClientHarness { $0.structuredOutputMode = .promptOnly }
    defer { harness.stop() }
    harness.server.enqueue(Scripts.completion("{}"))
    _ = try await harness.client.complete(Self.schemaRequest)
    #expect(harness.server.requests.first?.chat?.responseFormat == nil)
    #expect(await harness.client.resolvedMode == .promptOnly)
  }

  @Test func wireResponseFormatPerMode() {
    let schema = LLMResponseFormat.jsonSchema(name: "n", schema: ["type": "object"], strict: true)
    #expect(
      OpenAICompatibleClient.responseFormat(for: schema, mode: .jsonSchema)?.type == "json_schema")
    #expect(OpenAICompatibleClient.responseFormat(for: schema, mode: .auto)?.type == "json_schema")
    #expect(OpenAICompatibleClient.responseFormat(for: schema, mode: .jsonObject) == .jsonObject)
    #expect(OpenAICompatibleClient.responseFormat(for: schema, mode: .promptOnly) == nil)
    #expect(
      OpenAICompatibleClient.responseFormat(for: .jsonObject, mode: .jsonSchema) == .jsonObject)
    #expect(OpenAICompatibleClient.responseFormat(for: .text, mode: .jsonSchema) == nil)
    #expect(StructuredOutputMode.auto.downgraded == .jsonObject)
    #expect(StructuredOutputMode.jsonObject.downgraded == .promptOnly)
    #expect(StructuredOutputMode.promptOnly.downgraded == nil)
  }
}
