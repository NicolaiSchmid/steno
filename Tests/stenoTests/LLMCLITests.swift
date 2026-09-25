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
    #expect(probe.stdout.contains("reachable: true"))
    #expect(probe.stdout.contains("model listed: yes"))
    #expect(probe.stdout.contains("structured output: jsonSchema"))

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
}
