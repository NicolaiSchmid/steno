/// `StenoAudio`: capture (tap and mic lanes), echo cancellation, meeting
/// detection, the recording writer and the AVFoundation decoder. See
/// `.plans/2026-09-25-audio-capture.md`.
///
/// ## Threads and hand-offs
///
/// One recording runs on four threads; every arrow is a hand-off through a
/// type that owns exactly that boundary. The first two arrows are real-time:
/// nothing on them allocates, locks, logs or awaits.
///
/// ```
/// HAL IOProc thread            `IOProcRunner.deliver` (RealTime/)
///   │  pointer arithmetic over `StreamLayout`, one semaphore signal
///   ▼  ── real-time: no allocation, no locks ──
/// `LaneFrameSink` rings        `LaneRings` (RealTime/): all-or-nothing per callback
///   │
///   ▼
/// processing thread            `ProcessingThread` (RealTime/): 10 ms frames,
///   │  echo cancellation, metering into `LevelSlot` atomics
///   ▼  ── real-time: no allocation, no locks ──
/// `FrameRelay` rings           `LaneRings` (RealTime/): all-or-nothing per frame
///   │
///   ▼
/// writer thread                `WriterThread` (Writer/): `RecordingWriter`,
///   │  `Resampler48kTo16k`, files; republishes `LevelSlot` on change
///   ▼
/// `CaptureSession` actor       (Capture/): state machine, `states` and
///                              `levels` streams, the asset on `stop()`
/// ```
///
/// The synthetic backend (Testing/) is a producer thread speaking the
/// `LaneFrameSink` protocol in place of the IOProc; everything below it is
/// the production path, which is what makes the pipeline testable on CI.
/// The reviewer's grep for `[`, `Array`, closures and `map` on the real-time
/// path covers `RealTime/` and nothing else; `RealTimeAllocationTests` runs
/// the same path under libmalloc's hook on macOS.
public enum StenoAudio {
  /// The rate the aggregate device runs at and the master file is written in.
  public static let sampleRate: Double = 48_000
  /// One processing frame: 10 ms at 48 kHz. The echo canceller, the level
  /// meter and the writer all work in this unit.
  public static let frameSize = 480
  /// The echo canceller's tail: 200 ms at 48 kHz.
  public static let echoTailLength = 9_600
}
