/// Module-level facts about `StenoAudio`: capture (tap and mic lanes), echo
/// cancellation, meeting detection, the recording writer and the AVFoundation
/// decoder. Everything real-time runs without allocation or locks; everything
/// else is an actor. See `.plans/2026-09-25-audio-capture.md`.
public enum StenoAudio {
  /// The rate the aggregate device runs at and the master file is written in.
  public static let sampleRate: Double = 48_000
  /// One processing frame: 10 ms at 48 kHz. The echo canceller, the level
  /// meter and the writer all work in this unit.
  public static let frameSize = 480
  /// The echo canceller's tail: 200 ms at 48 kHz.
  public static let echoTailLength = 9_600
}
