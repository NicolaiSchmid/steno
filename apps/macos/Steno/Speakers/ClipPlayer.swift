import AVFoundation
import Foundation

/// Plays a speaker's ten-second sample clip (`Speaker.sampleClipURL`, 16 kHz
/// WAV) through `AVAudioPlayer`. One clip at a time.
@MainActor
@Observable
final class ClipPlayer: NSObject, AVAudioPlayerDelegate {
  private(set) var playingURL: URL?
  private var player: AVAudioPlayer?

  /// Starts the clip; false when the file is missing or unreadable.
  func play(_ url: URL) -> Bool {
    stop()
    guard FileManager.default.fileExists(atPath: url.path) else { return false }
    do {
      let player = try AVAudioPlayer(contentsOf: url)
      player.delegate = self
      self.player = player
      playingURL = url
      return player.play()
    } catch {
      playingURL = nil
      return false
    }
  }

  func stop() {
    player?.stop()
    player = nil
    playingURL = nil
  }

  nonisolated func audioPlayerDidFinishPlaying(_ player: AVAudioPlayer, successfully flag: Bool) {
    Task { @MainActor in self.finished() }
  }

  private func finished() {
    player = nil
    playingURL = nil
  }
}
