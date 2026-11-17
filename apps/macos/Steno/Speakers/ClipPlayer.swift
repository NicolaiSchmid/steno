import AVFoundation
import Foundation

/// Plays a speaker's ten-second sample clip (`Speaker.sampleClipURL`, 16 kHz
/// WAV), or a span of the meeting's recording for one transcript turn,
/// through `AVAudioPlayer`. One thing at a time: playing a clip stops a
/// turn and the other way round, and `stop()` stops either.
@MainActor
@Observable
final class ClipPlayer: NSObject, AVAudioPlayerDelegate {
  /// The file playing, the clip or the recording.
  private(set) var playingURL: URL?
  /// The turn playing, by its first segment id; nil while a clip plays.
  private(set) var playingTurn: UUID?
  /// The turn whose audio stopped on a decode error after it started, or
  /// whose player finished unsuccessfully, until the next play or stop. A
  /// `stop()` is never a failure: `AVAudioPlayer.stop()` calls no delegate.
  private(set) var failedTurn: UUID?
  private var player: AVAudioPlayer?
  /// Stops a turn at the end of its span.
  private var endOfSpan: Task<Void, Never>?

  /// Starts the clip; false when the file is missing or unreadable.
  func play(_ url: URL) -> Bool {
    stop()
    guard let player = start(url) else { return false }
    guard player.play() else {
      stop()
      return false
    }
    return true
  }

  /// Plays `url` from `start` until `end` seconds and marks `turn` playing
  /// meanwhile; false when the file is missing or unreadable.
  func play(_ url: URL, from start: TimeInterval, until end: TimeInterval, turn: UUID) -> Bool {
    stop()
    guard let player = self.start(url) else { return false }
    player.currentTime = max(0, start)
    guard player.play() else {
      stop()
      return false
    }
    playingTurn = turn
    let duration = max(0, end - max(0, start))
    let playing = ObjectIdentifier(player)
    endOfSpan = Task { @MainActor [weak self] in
      try? await Task.sleep(for: .seconds(duration))
      guard !Task.isCancelled, let self, self.current == playing
      else { return }
      self.stop()
    }
    return true
  }

  func stop() {
    failedTurn = nil
    endOfSpan?.cancel()
    endOfSpan = nil
    player?.stop()
    player = nil
    playingURL = nil
    playingTurn = nil
  }

  /// The current player's identity, to tell it from one that was replaced.
  private var current: ObjectIdentifier? { player.map { ObjectIdentifier($0) } }

  /// The player for `url`, installed as the current one; nil when the file
  /// is missing or unreadable.
  private func start(_ url: URL) -> AVAudioPlayer? {
    guard FileManager.default.fileExists(atPath: url.path) else { return nil }
    do {
      let player = try AVAudioPlayer(contentsOf: url)
      player.delegate = self
      self.player = player
      playingURL = url
      return player
    } catch {
      return nil
    }
  }

  /// Stops the current player, `player`, keeping its turn as failed when
  /// `failed`. A player that ended after another one started is not the
  /// current one and changes nothing.
  private func ended(_ player: ObjectIdentifier, failed: Bool) {
    guard current == player else { return }
    let turn = playingTurn
    stop()
    if failed { failedTurn = turn }
  }

  nonisolated func audioPlayerDecodeErrorDidOccur(_ player: AVAudioPlayer, error: (any Error)?) {
    let failed = ObjectIdentifier(player)
    Task { @MainActor in self.ended(failed, failed: true) }
  }

  nonisolated func audioPlayerDidFinishPlaying(_ player: AVAudioPlayer, successfully flag: Bool) {
    let finished = ObjectIdentifier(player)
    Task { @MainActor in self.ended(finished, failed: !flag) }
  }
}
