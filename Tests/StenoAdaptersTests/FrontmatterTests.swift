import Foundation
import StenoCore
import Testing

@testable import StenoAdapters

@Suite struct FrontmatterTests {
  static func sample() -> Frontmatter {
    var frontmatter = Frontmatter(timeZone: FixtureMeeting.berlin)
    frontmatter.append("title", .string("Produktstrategie: \"90/10\" & Roadmap für Q4"))
    frontmatter.append("colon", .string("key: value"))
    frontmatter.append("backslash", .string("C:\\Users\\anna"))
    frontmatter.append("dash", .string("- not a list"))
    frontmatter.append("hash", .string("tag #1 here"))
    frontmatter.append("answer", .string("yes"))
    frontmatter.append("date_like", .string("2026-09-24"))
    frontmatter.append("empty", .string(""))
    frontmatter.append("control", .string("bell\u{07}tab\tnewline\ndel\u{7F}"))
    frontmatter.append("date", .dateTime(FixtureMeeting.startedAt))
    frontmatter.append("day", .date(FixtureMeeting.startedAt))
    frontmatter.append("duration", .int(90))
    frontmatter.append("done", .bool(false))
    frontmatter.append("participants", .list(["[[Anna Müller]]", "Speaker 2"]))
    frontmatter.append("nothing", .list([]))
    // Tags go through the renderer's sanitiser, then the emitter quotes them
    // like every other string; the YAML-typed ones are the point.
    frontmatter.append(
      "tags",
      .list(
        ["meeting", "Kunde ACME", "#q4", "  ", "a/b", "-x-", "2026", "true", "null", "1e3"]
          .compactMap(MarkdownText.tag)))
    return frontmatter
  }

  @Test func encodesEveryValueKindAndMatchesTheGolden() throws {
    let encoded = Self.sample().encoded()
    #expect(encoded.hasPrefix("---\n"))
    #expect(encoded.hasSuffix("\n---\n"))
    #expect(encoded.contains("title: \"Produktstrategie: \\\"90/10\\\" & Roadmap für Q4\"\n"))
    #expect(encoded.contains("colon: \"key: value\"\n"))
    #expect(encoded.contains("backslash: \"C:\\\\Users\\\\anna\"\n"))
    #expect(encoded.contains("dash: \"- not a list\"\n"))
    #expect(
      encoded.contains("answer: \"yes\"\n"), "strings are always quoted, so yes stays a string")
    #expect(encoded.contains("date_like: \"2026-09-24\"\n"))
    #expect(encoded.contains("empty: \"\"\n"))
    #expect(encoded.contains("control: \"bell\\u0007tab\\tnewline\\ndel\\u007F\"\n"))
    #expect(encoded.contains("date: 2026-09-24T14:00:00\n"))
    #expect(encoded.contains("day: 2026-09-24\n"))
    #expect(encoded.contains("duration: 90\n"))
    #expect(encoded.contains("done: false\n"))
    #expect(encoded.contains("participants:\n  - \"[[Anna Müller]]\"\n  - \"Speaker 2\"\n"))
    #expect(encoded.contains("nothing: []\n"))
    #expect(
      encoded.contains(
        "tags:\n  - \"meeting\"\n  - \"Kunde-ACME\"\n  - \"q4\"\n  - \"a/b\"\n  - \"x\"\n  - \"2026\"\n  - \"true\"\n  - \"null\"\n  - \"1e3\"\n"
      ), "a tag YAML would type as int, bool, null or float is quoted like every string")
    try Snapshot.assert(encoded, matches: "snapshots/obsidian/frontmatter.md")
  }

  @Test func quotedEscapesOnlyWhatYAMLNeeds() {
    #expect(Frontmatter.quoted("plain") == "\"plain\"")
    #expect(Frontmatter.quoted("a\"b") == "\"a\\\"b\"")
    #expect(Frontmatter.quoted("a\\b") == "\"a\\\\b\"")
    #expect(Frontmatter.quoted("ü 日本 🎉") == "\"ü 日本 🎉\"")
    #expect(Frontmatter.quoted("\r\n") == "\"\\r\\n\"")
    #expect(Frontmatter.quoted("\u{1B}") == "\"\\u001B\"")
    // C1 controls, the line and paragraph separators and a stray BOM are
    // non-printable to YAML; one of them would void the whole block.
    #expect(Frontmatter.quoted("a\u{85}b\u{9F}c") == "\"a\\u0085b\\u009Fc\"")
    #expect(Frontmatter.quoted("\u{2028}\u{2029}") == "\"\\u2028\\u2029\"")
    #expect(Frontmatter.quoted("\u{FEFF}title") == "\"\\uFEFFtitle\"")
    #expect(Frontmatter.quoted("\u{A0}\u{2027}") == "\"\u{A0}\u{2027}\"", "neighbours stay")
  }

  @Test func rubyReadsBackTheSameKeysAndStrings() throws {
    // Only macOS ships /usr/bin/ruby; Linux runs the byte assertions above.
    #if os(macOS)
      let ruby = URL(fileURLWithPath: "/usr/bin/ruby")
      guard FileManager.default.isExecutableFile(atPath: ruby.path) else { return }
      let yaml = Self.sample().encoded()
      let body = String(yaml.dropFirst(4).dropLast(4))  // between the --- fences
      let process = Foundation.Process()
      process.executableURL = ruby
      process.arguments = [
        "-ryaml", "-rjson", "-e",
        "d = YAML.load(STDIN.read); d.each { |k, v| d[k] = v.to_s unless v.is_a?(String) || v.is_a?(Array) }; puts JSON.generate(d)",
      ]
      let input = Pipe()
      let output = Pipe()
      process.standardInput = input
      process.standardOutput = output
      try process.run()
      input.fileHandleForWriting.write(Data(body.utf8))
      try input.fileHandleForWriting.close()
      let data = output.fileHandleForReading.readDataToEndOfFile()
      process.waitUntilExit()
      #expect(process.terminationStatus == 0)
      let parsed = try #require(try JSONSerialization.jsonObject(with: data) as? [String: Any])
      #expect(Set(parsed.keys) == Set(Self.sample().fields.map(\.key)))
      #expect(parsed["title"] as? String == "Produktstrategie: \"90/10\" & Roadmap für Q4")
      #expect(parsed["colon"] as? String == "key: value")
      #expect(parsed["backslash"] as? String == "C:\\Users\\anna")
      #expect(parsed["dash"] as? String == "- not a list")
      #expect(parsed["hash"] as? String == "tag #1 here")
      #expect(parsed["answer"] as? String == "yes")
      #expect(parsed["date_like"] as? String == "2026-09-24")
      #expect(parsed["empty"] as? String == "")
      #expect(parsed["control"] as? String == "bell\u{07}tab\tnewline\ndel\u{7F}")
      #expect(parsed["duration"] as? String == "90")
      #expect(parsed["done"] as? String == "false")
      #expect(parsed["participants"] as? [String] == ["[[Anna Müller]]", "Speaker 2"])
      #expect(
        parsed["tags"] as? [String] == [
          "meeting", "Kunde-ACME", "q4", "a/b", "x", "2026", "true", "null", "1e3",
        ], "Ruby reads every tag back as a string")
      #expect((parsed["date"] as? String)?.hasPrefix("2026-09-24") == true)
    #endif
  }
}
