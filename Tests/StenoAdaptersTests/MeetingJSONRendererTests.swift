import Foundation
import StenoCore
import Testing

@testable import StenoAdapters

@Suite struct MeetingJSONRendererTests {
  let renderer = ArtifactRenderer()
  let export = FixtureMeeting.export()

  @Test func matchesTheGoldenAndTheFixtureFile() throws {
    let data = try renderer.renderJSON(export)
    try Snapshot.assert(data, matches: "snapshots/obsidian/meeting.json")
    try Snapshot.assert(data, matches: "meetings/produktstrategie.json")
    let fixture = try StenoJSON.decode(
      MeetingExport.self, from: Fixtures.data("meetings/produktstrategie.json"))
    #expect(fixture == export, "the fixture file on disk is the Swift fixture")
  }

  @Test func decodesBackAndEncodesByteIdentically() throws {
    let data = try renderer.renderJSON(export)
    let decoded = try StenoJSON.decode(MeetingExport.self, from: data)
    #expect(decoded == export)
    #expect(try renderer.renderJSON(decoded) == data)
    #expect(try StenoJSON.encode(export) == data, "byte-identical to steno export")
  }

  @Test func shapeIsCamelCaseWithoutEmbeddings() throws {
    let text = String(decoding: try renderer.renderJSON(export), as: UTF8.self)
    #expect(!text.contains("embedding"))
    #expect(text.contains("\"schemaVersion\" : 1"))
    #expect(text.contains("\"mixdownURL\" : \"file:///tmp/steno/"))
    let keys = try Regex("\"([A-Za-z0-9_]+)\" :")
    for match in text.matches(of: keys) {
      let key = String(match.output[1].substring ?? "")
      #expect(!key.contains("_"), "\(key) is not camelCase")
      #expect(
        key.first?.isLowercase == true || key.first?.isNumber == true, "\(key) starts lowercase")
    }
  }
}
