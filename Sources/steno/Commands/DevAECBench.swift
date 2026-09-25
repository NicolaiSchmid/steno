import ArgumentParser
import Foundation
import StenoAudio
import StenoCore

/// `steno dev aec-bench --mic FILE --far FILE[:channel] [--engine
/// speex|passthrough] [--out FILE]`: runs the canceller over two 48 kHz lanes
/// (CAF or WAV, `path:channel` picks a channel of a multi-channel master),
/// prints ERLE per second and over the whole file, and writes the processed
/// mic lane. `--synthetic` benches the built-in echo fixtures instead of
/// files (the far-end through a seeded room at 60 ms).
struct DevAECBench: AsyncParsableCommand {
  static let configuration = CommandConfiguration(
    commandName: "aec-bench",
    abstract: "Measure echo cancellation on recorded or synthetic lanes.")

  enum Engine: String, ExpressibleByArgument, CaseIterable {
    case speex, passthrough
  }

  @Option(help: "The microphone lane before cancellation (mic.raw.caf or a WAV).")
  var mic: String?

  @Option(help: "The far-end lane (recording.caf:1 for the system channel of a master).")
  var far: String?

  @Option(help: "speex or passthrough.")
  var engine: Engine = .speex

  @Option(help: "Echo tail in milliseconds.")
  var tailMilliseconds: Int = 200

  @Option(help: "Write the processed mic lane as 48 kHz WAV here.")
  var out: String?

  @Flag(help: "Use the synthetic six-second echo fixtures instead of --mic and --far.")
  var synthetic = false

  func validate() throws {
    if !synthetic, mic == nil || far == nil {
      throw ValidationError("Pass --mic and --far, or --synthetic.")
    }
  }

  func run() async throws {
    let nearEnd: [Float]
    let farEnd: [Float]
    if synthetic {
      farEnd = AudioFixtures.speechLikeFar(seconds: 6)
      nearEnd = AudioFixtures.echoMic(
        far: farEnd, impulseResponse: AudioFixtures.roomImpulseResponse())
    } else {
      let micLane = try LaneFileReader.read(mic!)
      let farLane = try LaneFileReader.read(far!)
      guard micLane.sampleRate == 48_000, farLane.sampleRate == 48_000 else {
        throw RuntimeFailure(
          description:
            "both lanes must be 48 kHz (mic \(Int(micLane.sampleRate)) Hz, far \(Int(farLane.sampleRate)) Hz)"
        )
      }
      nearEnd = micLane.samples
      farEnd = farLane.samples
    }
    let frame = StenoAudio.frameSize
    let processed: [Float]
    switch engine {
    case .speex:
      let canceller = try SpeexEchoCanceller(
        sampleRate: 48_000, frameSize: frame, tailLength: 48 * tailMilliseconds)
      processed = EchoMetrics.run(canceller, nearEnd: nearEnd, farEnd: farEnd, frameSize: frame)
    case .passthrough:
      let canceller = try PassthroughEchoCanceller(sampleRate: 48_000, frameSize: frame)
      processed = EchoMetrics.run(canceller, nearEnd: nearEnd, farEnd: farEnd, frameSize: frame)
    }
    let seconds = processed.count / 48_000
    print("engine: \(engine.rawValue), tail \(tailMilliseconds) ms, \(processed.count) frames")
    print(
      String(
        format: "mic %.1f dBFS, far %.1f dBFS, processed %.1f dBFS",
        EchoMetrics.decibels(EchoMetrics.rms(nearEnd[..<processed.count])),
        EchoMetrics.decibels(EchoMetrics.rms(farEnd[..<processed.count])),
        EchoMetrics.decibels(EchoMetrics.rms(processed))))
    for second in 0..<seconds {
      let range = (second * 48_000)..<((second + 1) * 48_000)
      let erle = EchoMetrics.erle(nearEnd: nearEnd, processed: processed, range: range)
      print(String(format: "  %3d s: ERLE %5.1f dB", second, erle))
    }
    let overall = EchoMetrics.erle(
      nearEnd: nearEnd, processed: processed, range: 0..<processed.count)
    let steady = EchoMetrics.erle(
      nearEnd: nearEnd, processed: processed,
      range: min(3 * 48_000, processed.count)..<processed.count)
    print(String(format: "ERLE overall %.1f dB, after 3 s %.1f dB", overall, steady))
    if let out {
      try AudioFixtures.writeWAV(processed, to: URL(fileURLWithPath: out))
      print("wrote \(out)")
    }
  }
}
