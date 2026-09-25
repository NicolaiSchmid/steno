import ArgumentParser
import Foundation
import StenoCore
import StenoLLM

/// `steno dev llm probe|cleanup|summarize`: the two passes and the endpoint
/// probe against the configured endpoint (settings, overridable by flags).
/// The API key comes from `STENO_LLM_API_KEY` or the secrets file, never
/// from the command line.
struct DevLLM: AsyncParsableCommand {
  static let configuration = CommandConfiguration(
    commandName: "llm",
    abstract: "Probe the LLM endpoint, run the cleanup or the summary pass over a meeting.json.",
    subcommands: [Probe.self, Cleanup.self, Summarize.self]
  )

  /// Endpoint flags shared by the three subcommands; flags win over settings.
  struct EndpointOptions: ParsableArguments {
    @Option(
      name: .customLong("base-url"), help: "OpenAI-compatible root, e.g. http://127.0.0.1:1234/v1.")
    var baseURL: String?

    @Option(help: "Model name as the server knows it.")
    var model: String?

    @Option(name: .customLong("context-tokens"), help: "The model's context window.")
    var contextTokens: Int?

    @Option(name: .customLong("max-output-tokens"), help: "Ceiling for one answer.")
    var maxOutputTokens: Int?

    @Option(name: .customLong("timeout"), help: "Per-attempt timeout in seconds.")
    var timeoutSeconds: Int?

    @Flag(help: "Log every request, retry and mode change to stderr.")
    var verbose = false

    @OptionGroup var database: DatabaseOptions

    /// Settings are read only for what the flags leave out, so
    /// `probe --base-url … --model …` opens no database.
    func endpoint() async throws -> LLMEndpoint {
      var settings =
        baseURL != nil && model != nil
        ? Settings() : try await Wiring.open(database).settings.load()
      if let baseURL {
        guard let url = URL(string: baseURL), url.scheme != nil else {
          throw ValidationError("--base-url is not a URL: \(baseURL)")
        }
        settings.llmBaseURL = url
      }
      if let model { settings.llmModel = model }
      if let contextTokens { settings.llmContextTokens = contextTokens }
      guard var endpoint = LLMEndpoint(settings: settings) else {
        throw ValidationError(
          "No LLM endpoint configured. Pass --base-url and --model or set Settings.llmBaseURL and llmModel."
        )
      }
      if let maxOutputTokens { endpoint.maxOutputTokens = maxOutputTokens }
      if let timeoutSeconds { endpoint.requestTimeout = .seconds(timeoutSeconds) }
      return endpoint
    }

    func client() async throws -> OpenAICompatibleClient {
      OpenAICompatibleClient(
        endpoint: try await endpoint(),
        apiKey: try await Wiring.secretStore().secret(for: .llmAPIKey),
        observer: verbose ? Self.log : nil)
    }

    static func log(_ event: LLMClientEvent) {
      FileHandle.standardError.write(Data("llm: \(event)\n".utf8))
    }
  }

  static func loadExport(_ path: String) throws -> MeetingExport {
    let url = URL(fileURLWithPath: path)
    guard FileManager.default.fileExists(atPath: url.path) else {
      throw ValidationError("No such file: \(path)")
    }
    do {
      return try StenoJSON.decode(MeetingExport.self, from: Data(contentsOf: url))
    } catch {
      throw RuntimeFailure(description: "\(path) is not a meeting.json: \(error)")
    }
  }

  static func describe(_ usage: LLMUsage) -> String {
    "usage: \(usage.requests) request(s), \(usage.promptTokens) prompt + \(usage.completionTokens) completion tokens"
  }

  /// `steno dev llm probe`: model list, structured output mode and round
  /// trip; a failure of the probe completion is the exit code. `--json`
  /// prints `{modelListed, structuredOutput, roundTripMilliseconds}` for a
  /// "Test connection" script.
  struct Probe: AsyncParsableCommand {
    static let configuration = CommandConfiguration(
      abstract: "GET /models and one tiny structured completion.")

    @Flag(help: "Print the result as one JSON object.")
    var json = false

    @OptionGroup var options: EndpointOptions

    struct Report: Encodable {
      var modelListed: Bool?
      var structuredOutput: String
      var roundTripMilliseconds: Int
    }

    func run() async throws {
      let client = try await options.client()
      let endpoint = client.endpoint
      let probe: EndpointProbe
      do {
        probe = try await client.probe()
      } catch let error as LLMError {
        throw RuntimeFailure(description: "probe failed: \(error)")
      }
      let milliseconds = Int(probe.roundTrip / .milliseconds(1))
      if json {
        let report = Report(
          modelListed: probe.modelListed, structuredOutput: probe.resolvedMode.rawValue,
          roundTripMilliseconds: milliseconds)
        print(String(decoding: try StenoJSON.encode(report), as: UTF8.self))
        return
      }
      print("endpoint: \(endpoint.baseURL.absoluteString) model \(endpoint.model)")
      print("model listed: \(probe.modelListed.map { $0 ? "yes" : "no" } ?? "no model list")")
      print("structured output: \(probe.resolvedMode.rawValue)")
      print("round trip: \(milliseconds) ms")
    }
  }

  /// `steno dev llm cleanup <meeting.json>`: pass 1 over the export's
  /// segments, printing changed lines and usage.
  struct Cleanup: AsyncParsableCommand {
    static let configuration = CommandConfiguration(
      abstract: "Run the cleanup pass over a meeting.json and print the corrected segments.")

    @Argument(help: "A meeting.json (MeetingExport), for example from `steno export`.")
    var input: String

    @Option(help: "Write the export with cleaned segments to this path.")
    var out: String?

    @OptionGroup var options: EndpointOptions

    func run() async throws {
      var export = try DevLLM.loadExport(input)
      let client = try await options.client()
      let cleaner = LLMTranscriptCleaner(model: client, endpoint: client.endpoint)
      let output: CleanupOutput
      do {
        output = try await cleaner.clean(CleanupInput(export: export))
      } catch let error as LLMError {
        throw RuntimeFailure(description: "cleanup failed: \(error)")
      }
      let labels = SpeakerLabels(speakers: export.speakers)
      var changed = 0
      for (index, (before, after)) in zip(export.segments, output.segments).enumerated() {
        if before.text != after.text {
          changed += 1
          print("[\(index)] \(labels.label(for: after.speakerID)):")
          print("  - \(before.text)")
          print("  + \(after.text)")
        }
      }
      print("\(changed) of \(output.segments.count) segments changed")
      if !output.failedChunks.isEmpty {
        print(
          "failed chunks kept raw: \(output.failedChunks.map(String.init).joined(separator: ", "))")
      }
      print(DevLLM.describe(output.usage))
      if let out {
        export.segments = output.segments
        try StenoJSON.encode(export).write(to: URL(fileURLWithPath: out), options: .atomic)
        print("wrote \(out)")
      }
    }
  }

  /// `steno dev llm summarize <meeting.json> --template <id>`: pass 2,
  /// printing the rendered summary, decisions, tasks, name suggestions and
  /// usage.
  struct Summarize: AsyncParsableCommand {
    static let configuration = CommandConfiguration(
      abstract: "Run the summary pass over a meeting.json and print the result.")

    @Argument(help: "A meeting.json (MeetingExport), for example from `steno export`.")
    var input: String

    @Option(help: "Summary template id; defaults to the meeting's template.")
    var template: String?

    @Flag(help: "Print the SummaryOutput as JSON instead of text.")
    var json = false

    @OptionGroup var options: EndpointOptions

    func validate() throws {
      if let template, SummaryTemplate.bundled(id: template) == nil {
        throw ValidationError(
          "Unknown template \(template). Bundled: \(SummaryTemplate.bundledIDs.joined(separator: ", "))."
        )
      }
    }

    func run() async throws {
      var export = try DevLLM.loadExport(input)
      let client = try await options.client()
      let summarizer = LLMMeetingSummarizer(model: client, endpoint: client.endpoint)
      let selected = template.flatMap { SummaryTemplate.bundled(id: $0) }
      let output: SummaryOutput
      do {
        output = try await summarizer.summarize(SummaryInput(export: export, template: selected))
      } catch let error as LLMError {
        throw RuntimeFailure(description: "summarize failed: \(error)")
      }
      if json {
        print(String(decoding: try StenoJSON.encode(output), as: UTF8.self))
        return
      }
      export.meeting.summary = output.summary
      export.meeting.title = output.title
      print("# \(output.title)")
      print("")
      print(SummaryMarkdown.render(export))
      if !output.decisions.isEmpty {
        print("## Decisions")
        print("")
        for decision in output.decisions { print("- \(decision)") }
        print("")
      }
      if !output.tasks.isEmpty {
        print("## Tasks")
        print("")
        for task in output.tasks {
          var line = "- [ ] \(task.text)"
          if let assignee = task.assigneeName { line += " (\(assignee))" }
          line += " [\(task.priority.rawValue)]"
          if let dueDate = task.dueDate { line += " due \(StenoJSON.format(dueDate).prefix(10))" }
          print(line)
        }
        print("")
      }
      if !output.speakerNames.isEmpty {
        print("## Speaker name suggestions")
        print("")
        let labels = SpeakerLabels(speakers: export.speakers)
        for suggestion in output.speakerNames {
          print(
            "- \(labels.label(for: suggestion.speakerID)) → \(suggestion.name ?? "?") (\(suggestion.confidence)): \(suggestion.evidence)"
          )
        }
        print("")
      }
      print(DevLLM.describe(output.usage))
    }
  }
}
