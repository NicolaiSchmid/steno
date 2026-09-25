import Foundation
import StenoCore

/// There is no public status API for the system-audio capture permission
/// (`kTCCServiceAudioCapture`), so onboarding runs the real tap pipeline
/// while a separate process (`afplay`, which the tap does not exclude) plays
/// a tone, and treats the first non-silent buffer as "authorised".
///
/// The first run surfaces the TCC prompt from inside `start` and the tap
/// delivers silence until the user answers, so the probe polls for signal up
/// to `timeout` (30 s by default) and restarts the tone whenever it has run
/// out; only the deadline means denied. TCC attributes a Terminal-launched
/// tool to Terminal: call this from the bundled, signed app whose Info.plist
/// carries `NSAudioCaptureUsageDescription` and
/// `NSMicrophoneUsageDescription` as literal keys. `tccutil reset
/// AudioCapture <bundle-id>` re-triggers the prompt.
public enum SystemAudioPermission {
  /// How often the ring is inspected while waiting for the first signal.
  static let pollInterval: Duration = .milliseconds(100)
  /// The tone `afplay` plays, restarted whenever it has finished.
  static let toneSeconds: TimeInterval = 1

  /// The tap delivered signal above `LaneLevel.silentPeakLinear` (-80 dBFS)
  /// within `timeout`.
  public static func request(timeout: Duration = .seconds(30)) async -> Bool {
    #if canImport(CoreAudio)
      let tone = FileManager.default.temporaryDirectory
        .appendingPathComponent("steno-permission-\(UUID().uuidString).wav")
      defer { try? FileManager.default.removeItem(at: tone) }
      do {
        try AudioFixtures.writeWAV(
          AudioFixtures.tone(frequency: 1_000, seconds: toneSeconds, amplitude: 0.5), to: tone)
      } catch {
        return false
      }

      // The tap first (this is where the prompt appears), then the tone.
      let sink = LaneFrameSink(lanes: [.system])
      let backend = LiveCaptureBackend()
      do {
        try backend.start(lanes: [.system], inputDeviceUID: nil, sink: sink)
      } catch {
        return false
      }
      defer { backend.stop() }

      var player: Foundation.Process?
      defer {
        if let player, player.isRunning { player.terminate() }
      }
      return await waitForSignal(
        timeout: timeout, clock: ContinuousClock(),
        sample: { sink.ring(0).drainAll() },
        keepPlaying: {
          if let player, player.isRunning { return }
          let next = Foundation.Process()
          next.executableURL = URL(fileURLWithPath: "/usr/bin/afplay")
          next.arguments = [tone.path]
          next.standardOutput = FileHandle.nullDevice
          next.standardError = FileHandle.nullDevice
          if (try? next.run()) != nil { player = next }
        })
    #else
      return false
    #endif
  }

  /// The probe loop, pure so it runs on a `ManualClock`: every
  /// `pollInterval` it calls `keepPlaying()` (which restarts the tone when it
  /// has finished), then `sample()` for what the tap delivered since the last
  /// poll. True as soon as one sample exceeds `LaneLevel.silentPeakLinear`;
  /// false once `timeout` has elapsed on `clock` without one.
  static func waitForSignal<C: Clock>(
    timeout: Duration, pollInterval: Duration = pollInterval, clock: C,
    sample: () -> [Float], keepPlaying: () -> Void
  ) async -> Bool where C.Duration == Duration {
    let deadline = clock.now.advanced(by: timeout)
    while true {
      keepPlaying()
      if sample().contains(where: { abs($0) > LaneLevel.silentPeakLinear }) { return true }
      if clock.now >= deadline { return false }
      try? await clock.sleep(for: pollInterval)
    }
  }

  /// `AVCaptureDevice.requestAccess(for: .audio)`: the microphone prompt.
  public static func microphone() async -> Bool {
    #if canImport(AVFoundation)
      return await AVCaptureDevice.requestAccess(for: .audio)
    #else
      return false
    #endif
  }
}

#if canImport(AVFoundation)
  import AVFoundation
#endif
