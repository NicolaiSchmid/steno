import ArgumentParser
import Dispatch
import Foundation
import StenoAudio
import StenoCore

/// `--mode call|in-person`.
extension CaptureMode: ExpressibleByArgument {
  static let arguments: [String: CaptureMode] = ["call": .call, "in-person": .inPerson]

  public init?(argument: String) {
    guard let mode = Self.arguments[argument] else { return nil }
    self = mode
  }

  public var defaultValueDescription: String {
    Self.arguments.first { $0.value == self }?.key ?? rawValue
  }

  public static var allValueStrings: [String] { arguments.keys.sorted() }
}

/// `steno record --mode call|in-person --out DIR [--seconds N] [--backend
/// live|synthetic]`: records one meeting folder under DIR, prints the lane
/// levels at 10 Hz to stderr and the files plus statistics at the end.
/// Stops after `--seconds` or on Ctrl-C. `--backend synthetic` needs no
/// devices (deterministic tones, paced to wall time), which is what the
/// SIGKILL test and CI use.
struct Record: AsyncParsableCommand {
  static let configuration = CommandConfiguration(
    abstract: "Record a meeting from the Mac's microphone and system audio.")

  enum Backend: String, ExpressibleByArgument, CaseIterable {
    case live, synthetic
  }

  @Option(help: "call (mic + system lanes) or in-person (one room lane).")
  var mode: CaptureMode = .call

  @Option(help: "The audio folder; the meeting folder is created inside it.")
  var out: String

  @Option(help: "Stop after this many seconds; otherwise until Ctrl-C.")
  var seconds: Double?

  @Option(help: "live (Core Audio) or synthetic (tones, no devices).")
  var backend: Backend = .live

  @Option(name: .customLong("input-device"), help: "Input device UID; default input otherwise.")
  var inputDeviceUID: String?

  @Option(
    name: .customLong("meeting-id"), help: "Meeting id for the folder name; random otherwise.")
  var meetingID: String?

  @Flag(name: .customLong("no-aec"), help: "Disable echo cancellation in call mode.")
  var noEchoCancellation = false

  @Flag(
    name: .customLong("keep-raw-mic"), help: "Also write mic.raw.caf (before echo cancellation).")
  var keepRawMic = false

  @Flag(help: "Do not print levels.")
  var quiet = false

  func validate() throws {
    if let meetingID, UUID(uuidString: meetingID) == nil {
      throw ValidationError("--meeting-id must be a UUID.")
    }
    if let seconds, seconds <= 0 {
      throw ValidationError("--seconds must be positive.")
    }
  }

  func run() async throws {
    let configuration = CaptureConfiguration(
      mode: mode, inputDeviceUID: inputDeviceUID, echoCancellation: !noEchoCancellation,
      keepRawMicLane: keepRawMic, outputDirectory: URL(fileURLWithPath: out, isDirectory: true))
    let captureBackend: any CaptureBackend
    switch backend {
    case .live:
      captureBackend = LiveCaptureBackend()
    case .synthetic:
      captureBackend = SyntheticCaptureBackend(
        lanes: mode.lanes, tone: [.mic: 440, .system: 1_000, .mixed: 440],
        seconds: seconds ?? 3_600, realTime: true)
    }
    let session = try CaptureSession(configuration: configuration, backend: captureBackend)
    let id = meetingID.flatMap(UUID.init(uuidString:)) ?? UUID()

    let levelTask = quiet ? nil : session.printLevelsToStandardError()
    do {
      try await session.start(meetingID: id)
    } catch {
      levelTask?.cancel()
      throw RuntimeFailure(description: "could not start recording: \(error)")
    }
    FileHandle.standardError.write(
      Data("recording \(id.uuidString) into \(out) (\(mode.defaultValueDescription))\n".utf8))

    let stopSignal = StopSignal()
    let interrupt = DispatchSource.makeSignalSource(signal: SIGINT, queue: .global())
    signal(SIGINT, SIG_IGN)
    interrupt.setEventHandler { stopSignal.fire() }
    interrupt.resume()
    let deadline = seconds.map { seconds in
      Task {
        try? await Task.sleep(for: .milliseconds(Int(seconds * 1_000)))
        if !Task.isCancelled { stopSignal.fire() }
      }
    }
    let stateWatch = Task {
      for await state in await session.states {
        if case .failed(let error, _) = state {
          FileHandle.standardError.write(Data("capture failed: \(error)\n".utf8))
          stopSignal.fire()
          return
        }
      }
    }
    await stopSignal.wait()
    deadline?.cancel()
    stateWatch.cancel()
    interrupt.cancel()

    let result: CaptureResult
    do {
      result = try await session.stop()
    } catch {
      levelTask?.cancel()
      throw RuntimeFailure(description: "could not stop recording: \(error)")
    }
    levelTask?.cancel()
    print("meeting: \(id.uuidString)")
    print("master: \(result.asset.url.path)")
    for lane in result.asset.lanes {
      if let sidecar = result.asset.sidecars16k[lane] {
        print("sidecar \(lane.rawValue): \(sidecar.path)")
      }
    }
    print(String(format: "duration: %.2f s", result.statistics.duration))
    let dropped = result.statistics.droppedFrames.sorted { $0.key.rawValue < $1.key.rawValue }
      .map { "\($0.key.rawValue)=\($0.value)" }.joined(separator: " ")
    print("dropped frames: \(dropped.isEmpty ? "none" : dropped)")
    print("system lane silent: \(result.statistics.systemLaneSilent)")
    print("ended on device loss: \(result.statistics.endedOnDeviceLoss)")
    if case .failed(let error, _) = await session.state {
      throw RuntimeFailure(description: "recording ended with \(error)")
    }
  }
}

extension CaptureSession {
  /// Prints one line per level update to stderr until the task is cancelled.
  /// Shared by `steno record` and `steno dev capture-spike`.
  nonisolated func printLevelsToStandardError() -> Task<Void, Never> {
    Task {
      for await levels in await self.levels {
        var line = String(format: "mic %6.1f dBFS", levels.mic.rms)
        if let system = levels.system { line += String(format: "  system %6.1f dBFS", system.rms) }
        FileHandle.standardError.write(Data((line + "\n").utf8))
      }
    }
  }
}

/// A one-shot signal any thread can fire and one task awaits.
final class StopSignal: @unchecked Sendable {
  private let lock = NSLock()
  private var fired = false
  private var continuation: CheckedContinuation<Void, Never>?

  func fire() {
    lock.lock()
    let pending = continuation
    continuation = nil
    fired = true
    lock.unlock()
    pending?.resume()
  }

  func wait() async {
    await withCheckedContinuation { (continuation: CheckedContinuation<Void, Never>) in
      lock.lock()
      if fired {
        lock.unlock()
        continuation.resume()
        return
      }
      self.continuation = continuation
      lock.unlock()
    }
  }
}
