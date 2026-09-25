/// Host-time alignment for the two-IOProc fallback (spike S2 no-go): when
/// the microphone cannot join the tap aggregate, its IOProc stamps every
/// callback with `AudioTimeStamp.mHostTime`, and the aligner converts the
/// difference between the two lanes' first frames into whole frames to skip
/// so both lanes start at the same instant. Pure arithmetic; the tests feed
/// synthetic host times.
///
/// `nanosecondsPerHostTick` is `mach_timebase_info.numer / denom` (125 / 3 on
/// Apple Silicon, so one tick is 41.67 ns); 1 when host times already are
/// nanoseconds, as `DispatchTime.uptimeNanoseconds` and the synthetic backend
/// deliver.
public struct LaneAligner: Sendable, Equatable {
  public var sampleRate: Double
  public var nanosecondsPerHostTick: Double

  public init(sampleRate: Double = StenoAudio.sampleRate, nanosecondsPerHostTick: Double = 1) {
    self.sampleRate = sampleRate
    self.nanosecondsPerHostTick = nanosecondsPerHostTick
  }

  /// Whole frames between two host times, positive when `later` is after
  /// `earlier`.
  public func frames(from earlier: UInt64, to later: UInt64) -> Int {
    let ticks = later >= earlier ? Double(later - earlier) : -Double(earlier - later)
    return Int((ticks * nanosecondsPerHostTick / 1_000_000_000 * sampleRate).rounded())
  }

  /// How many frames to drop from the start of each lane so the first kept
  /// frame of both lanes was captured at the same instant: the lane that
  /// started earlier skips the difference, the other skips nothing.
  public func initialSkips(hostTimeA: UInt64, hostTimeB: UInt64) -> (skipA: Int, skipB: Int) {
    let offset = frames(from: hostTimeA, to: hostTimeB)
    return offset >= 0 ? (offset, 0) : (0, -offset)
  }

  /// Frames a lane delivered beyond what the clock says it should have
  /// (positive) or short of it (negative) between two of its own timestamps.
  /// A steady non-zero value is clock drift the aggregate would have
  /// compensated for.
  public func drift(framesDelivered: Int, from start: UInt64, to end: UInt64) -> Int {
    framesDelivered - frames(from: start, to: end)
  }

  /// True when `offsetFrames` stays inside the echo canceller's alignment
  /// budget (20 ms by default).
  public func isWithinBudget(offsetFrames: Int, budgetSeconds: Double = 0.020) -> Bool {
    Double(abs(offsetFrames)) / sampleRate <= budgetSeconds
  }
}
