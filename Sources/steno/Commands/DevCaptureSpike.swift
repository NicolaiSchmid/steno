import ArgumentParser
import Foundation
import StenoAudio
import StenoCore

/// `steno dev capture-spike --seconds 10 --out DIR [--lanes system|call|
/// in-person]`: the Continuity (S3) and mic-alignment (S2) spike. Records
/// through the live backend without echo cancellation, prints the resolved
/// stream layout, the aggregate rate and latency, then per lane the RMS and
/// peak in dBFS, the 1 kHz tone level, the quietest one-second window (the
/// idle floor) and the -30 dBFS onset time, plus the mic-versus-system onset
/// difference. Run it from a bundled, signed app for the TCC prompt; from
/// Terminal, TCC attributes the tap to Terminal.
struct DevCaptureSpike: AsyncParsableCommand {
  static let configuration = CommandConfiguration(
    commandName: "capture-spike",
    abstract: "Record the live lanes and report levels, layout and onset alignment.")

  enum Lanes: String, ExpressibleByArgument, CaseIterable {
    case system, call
    case inPerson = "in-person"

    var configuration: (mode: CaptureMode, override: [AudioLane]?) {
      switch self {
      case .system: (.call, [.system])
      case .call: (.call, nil)
      case .inPerson: (.inPerson, nil)
      }
    }
  }

  @Option(help: "How long to record.")
  var seconds: Double = 10

  @Option(help: "The audio folder; the meeting folder is created inside it.")
  var out: String

  @Option(help: "system (tap only), call (mic + system) or in-person (mic only).")
  var lanes: Lanes = .call

  @Option(name: .customLong("input-device"), help: "Input device UID; default input otherwise.")
  var inputDeviceUID: String?

  func run() async throws {
    let (mode, override) = lanes.configuration
    let configuration = CaptureConfiguration(
      mode: mode, inputDeviceUID: inputDeviceUID, echoCancellation: false,
      outputDirectory: URL(fileURLWithPath: out, isDirectory: true), laneOverride: override)
    let backend = LiveCaptureBackend()
    let session = try CaptureSession(configuration: configuration, backend: backend)
    let meetingID = UUID()
    do {
      try await session.start(meetingID: meetingID)
    } catch {
      throw RuntimeFailure(description: "could not start: \(error)")
    }
    if let stream = await session.stream {
      if let layout = stream.layout {
        print("layout: tap first = \(layout.tapFirst)")
        for source in layout.sources {
          func describe(_ channel: StreamLayout.ChannelRef) -> String {
            "buffer \(channel.buffer) channel \(channel.offset) stride \(channel.stride)"
          }
          print(
            "  \(source.lane.rawValue): \(describe(source.left))"
              + (source.right.map { " + \(describe($0))" } ?? ""))
        }
      }
      print("aggregate rate: \(Int(stream.sampleRate)) Hz")
      print("input latency + safety offset: \(stream.inputLatencyFrames) frames")
      print("output latency + safety offset: \(stream.outputLatencyFrames) frames")
    }
    let levelTask = session.printLevelsToStandardError()
    try? await Task.sleep(for: .milliseconds(Int(seconds * 1_000)))
    let result: CaptureResult
    do {
      result = try await session.stop()
    } catch {
      levelTask.cancel()
      throw RuntimeFailure(description: "could not stop: \(error)")
    }
    levelTask.cancel()

    let master = try CAFFile.read(result.asset.url)
    print("master: \(result.asset.url.path) (\(String(format: "%.2f", master.duration)) s)")
    print(
      "dropped frames: \(result.statistics.droppedFrames.isEmpty ? "none" : "\(result.statistics.droppedFrames)")"
    )
    var onsets: [AudioLane: Double] = [:]
    for (index, lane) in result.asset.lanes.enumerated() where index < master.channels.count {
      let samples = master.channels[index]
      let rms = EchoMetrics.decibels(EchoMetrics.rms(samples))
      let peak = EchoMetrics.decibels(samples.reduce(0) { max($0, abs($1)) })
      let tone = EchoMetrics.toneLevel(samples[...], frequency: 1_000, sampleRate: 48_000)
      var floor: Float = 0
      var onset: Double?
      let window = 48_000
      var quietest: Float = .greatestFiniteMagnitude
      var start = 0
      while start + window <= samples.count {
        let level = EchoMetrics.rms(samples[start..<(start + window)])
        quietest = min(quietest, level)
        start += window
      }
      floor = quietest == .greatestFiniteMagnitude ? 0 : quietest
      var frame = 0
      while frame + 480 <= samples.count {
        if EchoMetrics.decibels(EchoMetrics.rms(samples[frame..<(frame + 480)])) > -30 {
          onset = Double(frame) / 48_000
          break
        }
        frame += 480
      }
      if let onset { onsets[lane] = onset }
      print(
        String(
          format:
            "%@: rms %.1f dBFS, peak %.1f dBFS, 1 kHz %.1f dBFS, idle floor %.1f dBFS, onset %@",
          lane.rawValue, rms, peak, EchoMetrics.decibels(tone), EchoMetrics.decibels(floor),
          onset.map { String(format: "%.3f s", $0) } ?? "none"))
    }
    if let mic = onsets[.mic], let system = onsets[.system] {
      print(String(format: "mic - system onset: %.1f ms", (mic - system) * 1_000))
    }
    print("system lane silent: \(result.statistics.systemLaneSilent)")
  }
}
