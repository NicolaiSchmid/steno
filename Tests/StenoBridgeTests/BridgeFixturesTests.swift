import Foundation
import StenoBridge
import Testing

/// Pins the bridge contract to the JSON files the web UI tests parse. Swift
/// is the source of truth: run with `STENO_RECORD_FIXTURES=1` to rewrite
/// `apps/macos/web/fixtures/bridge/` after changing a type, commit the
/// result, and the web side's zod schemas either still parse it or fail
/// their CI. Without the variable the test asserts that every fixture on
/// disk is byte-identical to what the samples encode to today.
@Suite struct BridgeFixturesTests {
  static let repository = URL(fileURLWithPath: #filePath)
    .deletingLastPathComponent()  // StenoBridgeTests
    .deletingLastPathComponent()  // Tests
    .deletingLastPathComponent()  // repository root

  static let fixturesDirectory = repository.appendingPathComponent("apps/macos/web/fixtures/bridge")

  static var recording: Bool {
    ProcessInfo.processInfo.environment["STENO_RECORD_FIXTURES"] == "1"
  }

  static func url(_ name: String) -> URL {
    fixturesDirectory.appendingPathComponent("\(name).json")
  }

  @Test func fixtureNamesAreUnique() {
    let names = BridgeSamples.fixtures.map(\.name)
    #expect(Set(names).count == names.count)
  }

  @Test func everyTopicHasASnapshotFixture() {
    let names = Set(BridgeSamples.fixtures.map(\.name))
    for topic in BridgeTopic.allCases {
      #expect(names.contains(topic.rawValue), "no fixture for topic \(topic.rawValue)")
    }
  }

  @Test func everyFixtureRoundTrips() throws {
    for fixture in BridgeSamples.fixtures {
      let data = try fixture.encode()
      let again = try fixture.reencode(data)
      #expect(again == data, "\(fixture.name) does not round-trip byte for byte")
    }
  }

  @Test func fixturesOnDiskMatchTheSamples() throws {
    let directory = Self.fixturesDirectory
    if Self.recording {
      try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
      var index: [String] = []
      for fixture in BridgeSamples.fixtures {
        var data = try fixture.encode()
        data.append(0x0A)
        try data.write(to: Self.url(fixture.name))
        index.append(fixture.name)
      }
      let manifest = try BridgeJSON.encode(index) + Data([0x0A])
      try manifest.write(to: directory.appendingPathComponent("index.json"))
      return
    }
    for fixture in BridgeSamples.fixtures {
      let file = Self.url(fixture.name)
      let onDisk = try Data(contentsOf: file)
      var expected = try fixture.encode()
      expected.append(0x0A)
      #expect(
        onDisk == expected,
        "\(fixture.name).json is stale; run the suite with STENO_RECORD_FIXTURES=1 and commit")
    }
    let manifest = try Data(contentsOf: directory.appendingPathComponent("index.json"))
    let names = try BridgeJSON.decode([String].self, from: manifest)
    #expect(names == BridgeSamples.fixtures.map(\.name))
  }

  @Test func methodsAndTopicsUseDottedLowerCamelNames() {
    let pattern = try! Regex(#"^[a-z][A-Za-z0-9]*(\.[a-z][A-Za-z0-9]*)*$"#)
    for method in BridgeMethod.allCases {
      #expect(method.rawValue.wholeMatch(of: pattern) != nil, "\(method.rawValue)")
    }
    for topic in BridgeTopic.allCases {
      #expect(topic.rawValue.wholeMatch(of: pattern) != nil, "\(topic.rawValue)")
    }
  }

  @Test func datesEncodeAsUTCWithMilliseconds() throws {
    let data = try BridgeJSON.encode(BridgeSamples.meetingDetail)
    let text = String(decoding: data, as: UTF8.self)
    #expect(text.contains("\"startedAt\" : \"2026-09-29T12:50:00.000Z\""))
  }
}
