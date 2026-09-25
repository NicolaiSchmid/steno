import Foundation
import Testing

@testable import StenoCore

@Suite struct ModelCodableTests {
  private func roundTrip<T: Codable & Equatable>(_ value: T, _ comment: Comment) throws {
    let data = try StenoJSON.encode(value)
    let decoded = try StenoJSON.decode(T.self, from: data)
    #expect(decoded == value, comment)
    let again = try StenoJSON.encode(decoded)
    #expect(again == data, "second encode of \(comment) is byte-identical")
  }

  @Test func everyModelTypeRoundTripsThroughStenoJSON() throws {
    try roundTrip(SampleData.meeting(), "Meeting")
    try roundTrip(SampleData.meeting(state: .failed(reason: "summarize: boom")), "Meeting failed")
    try roundTrip(SampleData.summaryDocument(), "SummaryDocument")
    try roundTrip(SampleData.participants(), "[Participant]")
    try roundTrip(SampleData.segments(), "[TranscriptSegment]")
    try roundTrip(SampleData.tasks(), "[MeetingTask]")
    try roundTrip(SampleData.decisions(), "[Decision]")
    try roundTrip(SampleData.audioAsset(), "AudioAsset")
    try roundTrip(SampleData.delivery(), "Delivery")
    try roundTrip(
      Delivery(
        meetingID: SampleData.meetingID, destinationID: "x", status: .failed("vault missing")),
      "Delivery failed")
    try roundTrip(SampleData.pairedDevice(), "PairedDevice")
    try roundTrip(SampleData.handoverReceipt(), "HandoverReceipt")
    try roundTrip(SampleData.recordingMetadata(), "RecordingMetadata")
    try roundTrip(SampleData.settings(), "Settings")
    try roundTrip(Settings(), "Settings defaults")
    try roundTrip(SampleData.summaryOutput(), "SummaryOutput")
    try roundTrip(SampleData.llmRequest(), "LLMRequest")
    try roundTrip(SampleData.llmResponse(), "LLMResponse")
    try roundTrip(SampleData.rawSegments(), "[RawSegment]")
    try roundTrip(SampleData.diarization(), "DiarizationResult")
    try roundTrip(SampleData.template(), "SummaryTemplate")
    try roundTrip(PipelineStage.allCases, "[PipelineStage]")
    try roundTrip(
      [AudioRetention.deleteAfterProcessing, .keepDays(30), .keepForever], "[AudioRetention]")
    try roundTrip(
      [
        SpeakerAssignment.unknown, .suggested(personID: SampleData.uuid(1), similarity: 0.5),
        .confirmed(personID: SampleData.uuid(2)),
      ], "[SpeakerAssignment]")
  }

  @Test func embeddingsAreNotEncoded() throws {
    let persons = try StenoJSON.encode(SampleData.persons())
    let speakers = try StenoJSON.encode(SampleData.speakers())
    #expect(!String(decoding: persons, as: UTF8.self).contains("embedding"))
    #expect(!String(decoding: speakers, as: UTF8.self).contains("embedding"))

    let decodedPersons = try StenoJSON.decode([Person].self, from: persons)
    var expected = SampleData.persons()
    for index in expected.indices { expected[index].embedding = nil }
    #expect(decodedPersons == expected)

    let decodedSpeakers = try StenoJSON.decode([Speaker].self, from: speakers)
    var expectedSpeakers = SampleData.speakers()
    for index in expectedSpeakers.indices { expectedSpeakers[index].embedding = nil }
    #expect(decodedSpeakers == expectedSpeakers)
  }

  @Test func exportMatchesTheGolden() throws {
    let export = SampleData.export()
    let data = try StenoJSON.encode(export)
    #expect(!String(decoding: data, as: UTF8.self).contains("embedding"))
    #expect(try StenoJSON.encode(export) == data)
    try Snapshot.assert(data, matches: "exports/meeting-export.json")

    let decoded = try StenoJSON.decode(MeetingExport.self, from: data)
    var expected = export
    for index in expected.persons.indices { expected.persons[index].embedding = nil }
    for index in expected.speakers.indices { expected.speakers[index].embedding = nil }
    #expect(decoded == expected)
    #expect(decoded.displayName(forSpeaker: SampleData.speakerOneID) == "Nicolai")
    #expect(decoded.displayName(forSpeaker: SampleData.speakerTwoID) == "Jérôme")
    #expect(decoded.displayName(forSpeaker: SampleData.uuid(999)) == "Unknown")
  }

  @Test func languageEncodesAsBCP47Tag() throws {
    let tags = ["de", "en", "en-US", "de-DE", "zh-Hant-TW"]
    for tag in tags {
      let language = LanguageTag(rawValue: tag)
      #expect(
        LanguageTag(language.language) == language,
        "the Locale.Language round trip keeps the explicit subtags")
      let document = SummaryDocument(templateID: "default", language: language, sections: [])
      let json = String(decoding: try StenoJSON.encode(document), as: UTF8.self)
      #expect(json.contains("\"language\" : \"\(tag)\""))
      #expect(try StenoJSON.decode(SummaryDocument.self, from: Data(json.utf8)) == document)
    }
    let untagged = SummaryDocument(templateID: "default", language: nil, sections: [])
    let json = String(decoding: try StenoJSON.encode(untagged), as: UTF8.self)
    #expect(!json.contains("language"))
    #expect(try StenoJSON.decode(SummaryDocument.self, from: Data(json.utf8)) == untagged)
  }

  @Test func datesUseISO8601WithFractionalSeconds() throws {
    let date = Date(timeIntervalSince1970: 1_790_240_400.25)
    #expect(StenoJSON.format(date) == "2026-09-24T09:00:00.250Z")
    #expect(StenoJSON.parse("2026-09-24T09:00:00.250Z") == date)
    #expect(StenoJSON.parse("2026-09-24T09:00:00Z") == Date(timeIntervalSince1970: 1_790_240_400))
    #expect(StenoJSON.parse("yesterday") == nil)
  }

  @Test func payloadEnumsEncodeReadably() throws {
    func json<T: Encodable>(_ value: T) throws -> String {
      let encoder = StenoJSON.encoder()
      encoder.outputFormatting = [.sortedKeys, .withoutEscapingSlashes]
      return String(decoding: try encoder.encode(value), as: UTF8.self)
    }
    #expect(try json(MeetingState.ready) == #""ready""#)
    #expect(try json(MeetingState.failed(reason: "boom")) == #"{"failed":"boom"}"#)
    #expect(try json(AudioRetention.keepDays(30)) == #"{"keepDays":30}"#)
    #expect(try json(AudioRetention.keepForever) == #""keepForever""#)
    #expect(try json(DeliveryStatus.failed("vault missing")) == #"{"failed":"vault missing"}"#)
    #expect(
      try json(SpeakerAssignment.suggested(personID: SampleData.uuid(1), similarity: 0.5))
        == #"{"suggested":{"personID":"00000000-0000-0000-0000-000000000001","similarity":0.5}}"#)
    #expect(
      try json(HandoverState.complete(meetingID: SampleData.uuid(1)))
        == #"{"complete":{"meetingID":"00000000-0000-0000-0000-000000000001"}}"#)
    #expect(try json(LLMResponseFormat.jsonObject) == #""jsonObject""#)
    #expect(
      try json(LLMResponseFormat.jsonSchema(name: "s", schema: ["type": "object"], strict: true))
        == #"{"jsonSchema":{"name":"s","schema":{"type":"object"},"strict":true}}"#)
    #expect(throws: DecodingError.self) {
      try StenoJSON.decode(MeetingState.self, from: Data(#""paused""#.utf8))
    }
    #expect(throws: DecodingError.self) {
      try StenoJSON.decode(AudioRetention.self, from: Data(#"{"keepDays":1,"keepForever":1}"#.utf8))
    }
    #expect(MeetingState.failed(reason: "x").kind == .failed)
    #expect(
      MeetingState.Kind.allCases.map(\.rawValue) == [
        "recording", "queued", "processing", "ready", "failed",
      ])
    #expect(HandoverState.complete(meetingID: SampleData.uuid(1)).kind == .complete)
  }

  @Test func anUnknownStateInTheDatabaseFailsTheFetchInsteadOfBecomingFailed() async throws {
    let store = try MeetingStore.inMemory()
    try await store.save(SampleData.meeting())
    try await store.writer.write { db in
      try db.execute(sql: "UPDATE meeting SET state = 'paused'")
    }
    await #expect(throws: (any Error).self) {
      _ = try await store.meeting(id: SampleData.meetingID)
    }
  }

  @Test func dataUsesStandardBase64() throws {
    let file = DeliveredFile(relativePath: "a", ownership: .owned, sha256: Data([0xFB, 0xFF]))
    let json = String(decoding: try StenoJSON.encode(file), as: UTF8.self)
    #expect(json.contains("\"+/8=\""))
  }

  @Test func jsonValueDecodesEveryKind() throws {
    let text = #"{"a":1.5,"b":"x","c":true,"d":null,"e":[1,{"f":false}],"g":{}}"#
    let value = try StenoJSON.decode(JSONValue.self, from: Data(text.utf8))
    #expect(value["a"] == .number(1.5))
    #expect(value["b"] == "x")
    #expect(value["c"] == true)
    #expect(value["d"] == .null)
    #expect(value["e"] == [1, ["f": false]])
    #expect(value["g"] == .object([:]))
    let again = try StenoJSON.decode(JSONValue.self, from: try StenoJSON.encode(value))
    #expect(again == value)
  }

  @Test func embeddingBytesRoundTrip() {
    let embedding = Embedding([0.5, -1, 3.25, 0])
    #expect(embedding.data.count == 16)
    #expect(Embedding(data: embedding.data) == embedding)
    #expect(Embedding(data: Data([1, 2, 3])) == nil)
    #expect(Embedding([3, 4]).normalized() == Embedding([0.6, 0.8]))
    #expect(Embedding([1, 0]).cosineSimilarity(to: Embedding([1, 0])) == 1)
    #expect(Embedding([1, 0]).cosineSimilarity(to: Embedding([0, 1])) == 0)
    #expect(Embedding([1, 0]).cosineSimilarity(to: Embedding([1])) == 0)
    let mean = Embedding.weightedMean(Embedding([1, 0]), weight: 3, Embedding([0, 1]), weight: 1)
    #expect(abs(mean.values[0] - 0.9487) < 0.001)
    #expect(abs(mean.values[1] - 0.3162) < 0.001)
  }

  @Test func speakerAssignmentExposesPersonID() {
    #expect(SpeakerAssignment.unknown.personID == nil)
    #expect(
      SpeakerAssignment.suggested(personID: SampleData.uuid(1), similarity: 0.7).personID
        == SampleData.uuid(1))
    #expect(SpeakerAssignment.confirmed(personID: SampleData.uuid(2)).isConfirmed)
    #expect(!SpeakerAssignment.suggested(personID: SampleData.uuid(1), similarity: 0.7).isConfirmed)
  }

  @Test func retentionExpiry() {
    let now = SampleData.updatedAt
    #expect(AudioRetention.deleteAfterProcessing.expiry(from: now) == now)
    #expect(AudioRetention.keepDays(2).expiry(from: now) == now.addingTimeInterval(172_800))
    #expect(AudioRetention.keepForever.expiry(from: now) == nil)
  }

  @Test func usageAdds() {
    let sum =
      LLMUsage(promptTokens: 1, completionTokens: 2, requests: 1)
      + LLMUsage(promptTokens: 10, completionTokens: 20, requests: 2)
    #expect(sum == LLMUsage(promptTokens: 11, completionTokens: 22, requests: 3))
    #expect(LLMUsage.zero + sum == sum)
  }
}
