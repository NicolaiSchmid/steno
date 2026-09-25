import Foundation
import StenoCore

/// There is no public status API for the system-audio capture permission
/// (`kTCCServiceAudioCapture`), so onboarding runs the real tap pipeline for
/// half a second while a separate process (`afplay`, which the tap does not
/// exclude) plays a tone, and treats non-silent buffers as "authorised".
/// The first run triggers the system prompt. TCC attributes a
/// Terminal-launched tool to Terminal: call this from the bundled, signed
/// app whose Info.plist carries `NSAudioCaptureUsageDescription` and
/// `NSMicrophoneUsageDescription` as literal keys. `tccutil reset
/// AudioCapture <bundle-id>` re-triggers the prompt.
public enum SystemAudioPermission {
  /// The tap delivered signal above -80 dBFS during the probe.
  public static func request(duration: TimeInterval = 0.5) async -> Bool {
    #if canImport(CoreAudio)
      let tone = FileManager.default.temporaryDirectory
        .appendingPathComponent("steno-permission-\(UUID().uuidString).wav")
      defer { try? FileManager.default.removeItem(at: tone) }
      do {
        try AudioFixtures.writeWAV(
          AudioFixtures.tone(frequency: 1_000, seconds: duration + 1, amplitude: 0.5), to: tone)
      } catch {
        return false
      }
      let player = Foundation.Process()
      player.executableURL = URL(fileURLWithPath: "/usr/bin/afplay")
      player.arguments = [tone.path]
      player.standardOutput = FileHandle.nullDevice
      player.standardError = FileHandle.nullDevice
      guard (try? player.run()) != nil else { return false }
      defer {
        if player.isRunning { player.terminate() }
      }

      let sink = LaneFrameSink(lanes: [.system])
      let backend = LiveCaptureBackend()
      do {
        try backend.start(lanes: [.system], inputDeviceUID: nil, sink: sink)
      } catch {
        return false
      }
      defer { backend.stop() }
      try? await Task.sleep(for: .milliseconds(Int(duration * 1_000)))
      let samples = sink.ring(0).drainAll()
      let peak = samples.reduce(0) { max($0, abs($1)) }
      return sink.callbacks > 0 && peak > 1e-4
    #else
      return false
    #endif
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
