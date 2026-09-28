import ArgumentParser
import Foundation
import StenoCore

/// `--source mac-call|mac-in-person|phone`: the one `MeetingSource` enum,
/// spelled with hyphens on the command line.
extension MeetingSource: ExpressibleByArgument {
  static let arguments: [String: MeetingSource] = [
    "mac-call": .macCall, "mac-in-person": .macInPerson, "phone": .phone,
  ]

  public init?(argument: String) {
    guard let source = Self.arguments[argument] else { return nil }
    self = source
  }

  public var defaultValueDescription: String {
    Self.arguments.first { $0.value == self }?.key ?? rawValue
  }

  public static var allValueStrings: [String] { arguments.keys.sorted() }
}

/// `steno process <wav>`: copies the 16 kHz mono WAV (and the system lane
/// for a call) into `<audio folder>/<meetingID>/`, enqueues the meeting and
/// waits for the pipeline. Prints one line per progress event to standard
/// error, `stage percent remaining`, and the meeting id alone to standard
/// output. Those lines are how the stage rates get measured on a Mac.
struct Process: AsyncParsableCommand {
  static let configuration = CommandConfiguration(
    abstract: "Run the processing pipeline over a WAV file.")

  @Argument(help: "16 kHz mono WAV: the mic lane of a call, or the room recording.")
  var input: String

  @Option(name: .customLong("system-lane"), help: "The system lane WAV of a call.")
  var systemLane: String?

  @Option(help: "mac-call, mac-in-person or phone.")
  var source: MeetingSource = .macInPerson

  @Option(help: "Meeting title; defaults to the input file name.")
  var title: String?

  @Option(help: "Summary template id; defaults to the settings' default template.")
  var template: String?

  @Option(
    name: .customLong("audio-folder"),
    help: "Where this meeting's folder is created; defaults to the settings' audio folder.")
  var audioFolder: String?

  @OptionGroup var database: DatabaseOptions

  @OptionGroup var speech: SpeechOptions

  func validate() throws {
    switch source {
    case .macCall:
      guard systemLane != nil else {
        throw ValidationError("--source mac-call needs --system-lane <wav>.")
      }
    case .macInPerson, .phone:
      guard systemLane == nil else {
        throw ValidationError("--system-lane only applies to --source mac-call.")
      }
    }
    if let template, SummaryTemplate.bundled(id: template) == nil {
      throw ValidationError(
        "Unknown template \(template). Bundled: \(SummaryTemplate.bundledIDs.joined(separator: ", "))."
      )
    }
    guard FileManager.default.fileExists(atPath: input) else {
      throw ValidationError("No such file: \(input)")
    }
    if let systemLane, !FileManager.default.fileExists(atPath: systemLane) {
      throw ValidationError("No such file: \(systemLane)")
    }
  }

  func run() async throws {
    let opened = try Wiring.open(database)
    let settings = try await opened.settings.load()
    // `--audio-folder` is a plain path for this run; the stored setting is
    // the app's and never changes here.
    let root = audioFolder.map { URL(fileURLWithPath: $0, isDirectory: true) }
    let meetingID = UUID()
    let layout = RecordingLayout(audioFolder: root ?? settings.audioFolder, meetingID: meetingID)
    try layout.createDirectories()
    let inputURL = URL(fileURLWithPath: input)
    let info = try WAVAudioDecoder.info(inputURL)
    let duration = Double(info.frameCount) / Double(max(info.sampleRate, 1))

    let asset: AudioAsset
    if source == .macCall, let systemLane {
      let mic = layout.sidecar(.mic)
      let system = layout.sidecar(.system)
      try FileManager.default.copyItem(at: inputURL, to: mic)
      try FileManager.default.copyItem(at: URL(fileURLWithPath: systemLane), to: system)
      asset = AudioAsset(
        id: UUID(), meetingID: meetingID, url: mic, format: .wav16kInt16, lanes: [.mic, .system],
        sidecars16k: [.mic: mic, .system: system], retention: settings.defaultRetention)
    } else {
      let recording = layout.master(.wav16kInt16)
      try FileManager.default.copyItem(at: inputURL, to: recording)
      asset = AudioAsset(
        id: UUID(), meetingID: meetingID, url: recording, format: .wav16kInt16, lanes: [.mixed],
        retention: settings.defaultRetention)
    }

    let now = Date()
    let meeting = Meeting(
      id: meetingID,
      title: title ?? inputURL.deletingPathExtension().lastPathComponent,
      startedAt: now.addingTimeInterval(-duration),
      duration: duration,
      source: source,
      state: .queued,
      templateID: template ?? settings.defaultTemplateID,
      createdAt: now,
      updatedAt: now
    )

    let events = MeetingEventBus()
    let stream = await events.subscribe()
    let pipeline = ProcessingPipeline(
      dependencies: try Wiring.dependencies(
        store: opened.store, settings: opened.settings, engine: speech.engine,
        modelsDirectory: settings.modelsDirectory,
        llm: try await Wiring.llmComponents(settings: settings), events: events))
    let laneCount = asset.lanes.count
    let printer = Task {
      var transcribeEvents = 0
      for await event in stream {
        switch event {
        case .progress(let id, let progress) where id == meetingID:
          if progress.stage == .transcribe { transcribeEvents += 1 }
          let lane =
            progress.stage == .transcribe && laneCount > 1
            ? ", lane \(transcribeEvents) of \(laneCount)" : ""
          FileHandle.standardError.write(
            Data((Self.progressLine(progress) + lane + "\n").utf8))
        default:
          continue
        }
      }
    }
    try await pipeline.enqueue(meeting, asset: asset)
    await pipeline.waitUntilIdle()
    // The bus is private to this command: every progress line is posted by
    // now, so ending the subscription lets the printer finish after them.
    await events.finish()
    await printer.value

    guard let result = try await opened.store.meeting(id: meetingID) else {
      throw RuntimeFailure(description: "meeting \(meetingID) vanished during processing")
    }
    if case .failed(let reason) = result.state {
      throw RuntimeFailure(description: "processing failed: \(reason)")
    }
    print(meetingID.uuidString)
  }

  /// `stage percent remaining`, the stage padded to the longest name so the
  /// percents line up: `transcribe     3% 1m 20s`.
  static func progressLine(_ progress: ProcessingProgress) -> String {
    let stage = progress.stage.rawValue
    let width = PipelineStage.allCases.map(\.rawValue.count).max() ?? stage.count
    let padded = stage + String(repeating: " ", count: max(0, width - stage.count))
    let percent = String(Int((progress.fraction * 100).rounded(.down)))
    let aligned = String(repeating: " ", count: max(0, 3 - percent.count)) + percent
    return "\(padded) \(aligned)% \(remainingText(progress.estimatedRemaining))"
  }

  /// `1m 20s` or `4s`, seconds rounded up so a run never reads as done early.
  static func remainingText(_ remaining: Duration) -> String {
    let attoseconds = remaining.components.attoseconds
    let seconds = Int(remaining.components.seconds) + (attoseconds > 0 ? 1 : 0)
    guard seconds >= 60 else { return "\(max(0, seconds))s" }
    return "\(seconds / 60)m \(seconds % 60)s"
  }
}
