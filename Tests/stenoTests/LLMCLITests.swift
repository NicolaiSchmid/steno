import Foundation
import StenoCore
import StenoLLM
import Testing

/// `steno dev llm probe|cleanup|summarize` against the loopback stub, with
/// `HOME` in a temporary directory so no settings or secrets file of the
/// real user is read.
@Suite(.serialized) struct LLMCLITests {
  @Test func probeCleanupAndSummarizeRunAgainstTheStub() async throws {
    let home = try Fixtures.temporaryDirectory("steno-llm-home")
    defer { try? FileManager.default.removeItem(at: home) }
    let server = try StubChatServer()
    defer { server.stop() }
    let summary = try String(
      contentsOf: Fixtures.url("llm/responses/summary-default-standup.json"), encoding: .utf8)
    let echo = Scripts.cleanupEcho { _, text in text.prefix(1).uppercased() + text.dropFirst() }
    server.respond(with: { request in
      if request.method == "GET" { return Scripts.models(["stub-model"]) }
      switch request.purpose {
      case "probe": return Scripts.completion("{\"ok\":true}")
      case "summary": return Scripts.completion(summary)
      default: return echo(request)
      }
    })
    let endpoint = ["--base-url", server.baseURL.absoluteString, "--model", "stub-model"]
    let db = ["--db", home.appendingPathComponent("steno.sqlite").path]

    let probe = try CLITests.run(["dev", "llm", "probe"] + endpoint + db, home: home)
    #expect(probe.status == 0, "\(probe.stderr)")
    #expect(probe.stdout.contains("model listed: yes"))
    #expect(probe.stdout.contains("structured output: jsonSchema"))

    // `--json` for a "Test connection" script, and with both endpoint flags
    // the settings are not read, so no database is opened or created.
    let probeJSON = try CLITests.run(["dev", "llm", "probe", "--json"] + endpoint, home: home)
    #expect(probeJSON.status == 0, "\(probeJSON.stderr)")
    let decoded = try JSONSerialization.jsonObject(with: Data(probeJSON.stdout.utf8))
    let object = try #require(decoded as? [String: Any])
    #expect(object["modelListed"] as? Bool == true)
    #expect(object["structuredOutput"] as? String == "jsonSchema")
    #expect(object["roundTripMilliseconds"] is NSNumber)
    #expect(
      !FileManager.default.fileExists(atPath: home.appendingPathComponent("steno.sqlite").path))

    let fixture = Fixtures.url("llm/transcripts/denglish-standup.json").path
    let out = home.appendingPathComponent("cleaned.json").path
    let cleanup = try CLITests.run(
      ["dev", "llm", "cleanup", fixture, "--out", out] + endpoint + db, home: home)
    #expect(cleanup.status == 0, "\(cleanup.stderr)")
    #expect(cleanup.stdout.contains("24 of 24 segments changed"))
    #expect(cleanup.stdout.contains("usage: "))
    let cleaned = try StenoJSON.decode(
      MeetingExport.self, from: Data(contentsOf: URL(fileURLWithPath: out)))
    #expect(cleaned.segments.first?.text.hasPrefix("Okay") == true)
    #expect(cleaned.segments.first?.rawText.hasPrefix("okay") == true)

    let summarize = try CLITests.run(
      ["dev", "llm", "summarize", fixture, "--template", "default"] + endpoint + db, home: home)
    #expect(summarize.status == 0, "\(summarize.stderr)")
    #expect(summarize.stdout.hasPrefix("# Daily Standup: Onboarding, CI und Obsidian-Export\n"))
    #expect(summarize.stdout.contains("## Executive Summary"))
    #expect(
      summarize.stdout.contains(
        "- [ ] Export nach Obsidian für die Firma Müller umsetzen. (Speaker 2) [high]"))
    #expect(summarize.stdout.contains("Speaker 2 → Nicolai (0.9)"))
    #expect(summarize.stdout.contains("usage: 1 request(s)"))

    let unknown = try CLITests.run(
      ["dev", "llm", "summarize", fixture, "--template", "nope"] + endpoint + db, home: home)
    #expect(unknown.status == 1)
    #expect(unknown.stderr.contains("Unknown template nope"))

    let unconfigured = try CLITests.run(["dev", "llm", "probe"] + db, home: home)
    #expect(unconfigured.status == 1)
    #expect(unconfigured.stderr.contains("No LLM endpoint configured"))
  }

  /// Exit codes and messages on the failure paths: a rejected key exits 2
  /// with the redacted error, a missing or malformed input exits 1, a bad
  /// `--base-url` exits 1, failed chunks are reported and `--json` prints a
  /// decodable `SummaryOutput`.
  @Test func failurePathsExitWithTheDocumentedCodes() async throws {
    let home = try Fixtures.temporaryDirectory("steno-llm-home")
    defer { try? FileManager.default.removeItem(at: home) }
    let server = try StubChatServer()
    defer { server.stop() }
    let summary = try String(
      contentsOf: Fixtures.url("llm/responses/summary-default-standup.json"), encoding: .utf8)
    // The cleanup chunk (the standup is one chunk of 24) loses its last
    // segment both times; everything else answers normally.
    let dropping = Scripts.cleanupEcho { index, text in index == 23 ? nil : text }
    server.respond(with: { request in
      if request.method == "GET" { return Scripts.models(["stub-model"]) }
      switch request.purpose {
      case "probe": return Scripts.unauthorized()
      case "summary": return Scripts.completion(summary)
      case "cleanup", "cleanup-retry": return dropping(request)
      default: return nil
      }
    })
    let endpoint = ["--base-url", server.baseURL.absoluteString, "--model", "stub-model"]
    let db = ["--db", home.appendingPathComponent("steno.sqlite").path]
    let fixture = Fixtures.url("llm/transcripts/denglish-standup.json").path

    let probe = try CLITests.run(
      ["dev", "llm", "probe"] + endpoint + db, home: home,
      environment: ["STENO_LLM_API_KEY": "sk-cli-secret"])
    #expect(probe.status == 2)
    #expect(probe.stderr.contains("probe failed: HTTP 401: Incorrect API key provided"))
    #expect(!probe.stderr.contains("sk-cli-secret"))
    #expect(!probe.stdout.contains("sk-cli-secret"))
    #expect(
      server.requests.first { $0.purpose == "probe" }?.authorization == "Bearer sk-cli-secret",
      "the key from the environment reaches the header and nothing else")

    let cleanup = try CLITests.run(["dev", "llm", "cleanup", fixture] + endpoint + db, home: home)
    #expect(cleanup.status == 0, "\(cleanup.stderr)")
    #expect(cleanup.stdout.contains("0 of 24 segments changed"))
    #expect(cleanup.stdout.contains("failed chunks kept raw: 0"))
    #expect(cleanup.stdout.contains("usage: 2 request(s)"))

    let json = try CLITests.run(
      ["dev", "llm", "summarize", fixture, "--template", "default", "--json"] + endpoint + db,
      home: home)
    #expect(json.status == 0, "\(json.stderr)")
    let output = try StenoJSON.decode(SummaryOutput.self, from: Data(json.stdout.utf8))
    #expect(output.title == "Daily Standup: Onboarding, CI und Obsidian-Export")
    #expect(output.summary.sections.first?.id == "executive-summary")
    #expect(output.usage.requests == 1)

    let missing = try CLITests.run(
      ["dev", "llm", "cleanup", home.appendingPathComponent("nope.json").path] + endpoint + db,
      home: home)
    #expect(missing.status == 1)
    #expect(missing.stderr.contains("No such file"))

    let notAnExport = home.appendingPathComponent("not-an-export.json")
    try Data("{\"hello\": 1}".utf8).write(to: notAnExport)
    let malformed = try CLITests.run(
      ["dev", "llm", "summarize", notAnExport.path] + endpoint + db, home: home)
    #expect(malformed.status == 2)
    #expect(malformed.stderr.contains("is not a meeting.json"))

    let badURL = try CLITests.run(
      ["dev", "llm", "probe", "--base-url", "not a url", "--model", "m"] + db, home: home)
    #expect(badURL.status == 1)
    #expect(badURL.stderr.contains("--base-url is not a URL"))
  }
}
