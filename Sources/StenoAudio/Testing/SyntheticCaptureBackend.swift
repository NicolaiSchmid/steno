import Foundation
import StenoCore
import Synchronization

/// One synthetic lane: a sine, optionally plus a delayed, attenuated copy of
/// another lane (the loudspeaker echo the mic hears in a call).
public struct SyntheticLane: Sendable, Equatable {
  public struct Echo: Sendable, Equatable {
    public var of: AudioLane
    public var delay: TimeInterval
    /// Linear gain; 0.5 is -6 dB.
    public var gain: Double

    public init(of lane: AudioLane, delay: TimeInterval, gain: Double) {
      self.of = lane
      self.delay = delay
      self.gain = gain
    }
  }

  public var frequency: Double
  public var amplitude: Double
  public var echo: Echo?

  public init(frequency: Double, amplitude: Double = 0.5, echo: Echo? = nil) {
    self.frequency = frequency
    self.amplitude = amplitude
    self.echo = echo
  }

  public static let silence = SyntheticLane(frequency: 0, amplitude: 0)
}

/// A `CaptureBackend` without the HAL: deterministic sines per lane
/// (integer phase accumulators, identical on every machine), delivered from
/// a producer thread through the same `LaneFrameSink` protocol the IOProc
/// uses, in callbacks of `callbackFrames`. By default it runs as fast as the
/// rings accept (a 30 s recording takes milliseconds); `realTime: true`
/// paces it at wall-clock speed for the CLI. `loseDeviceAfter` reports device
/// loss at that point and stops delivering, like an unplugged microphone.
public final class SyntheticCaptureBackend: CaptureBackend, @unchecked Sendable {
  public let signals: [AudioLane: SyntheticLane]
  public let seconds: TimeInterval
  public let callbackFrames: Int
  public let realTime: Bool
  public let loseDeviceAfter: TimeInterval?
  private let sampleRate = StenoAudio.sampleRate
  private let lock = NSLock()
  private var thread: Thread?
  private let stopRequested = Atomic<Bool>(false)
  private let finished = DispatchSemaphore(value: 0)
  private let framesDeliveredCount = Atomic<Int>(0)
  private let completion = Completion()

  public init(
    signals: [AudioLane: SyntheticLane], seconds: TimeInterval, callbackFrames: Int = 512,
    realTime: Bool = false, loseDeviceAfter: TimeInterval? = nil
  ) {
    self.signals = signals
    self.seconds = seconds
    self.callbackFrames = callbackFrames
    self.realTime = realTime
    self.loseDeviceAfter = loseDeviceAfter
  }

  /// The plan's spelling: one tone per lane at amplitude 0.5.
  public convenience init(
    lanes: [AudioLane], tone: [AudioLane: Double], seconds: TimeInterval,
    loseDeviceAfter: TimeInterval? = nil, realTime: Bool = false
  ) {
    var signals: [AudioLane: SyntheticLane] = [:]
    for lane in lanes {
      signals[lane] = tone[lane].map { SyntheticLane(frequency: $0) } ?? .silence
    }
    self.init(
      signals: signals, seconds: seconds, realTime: realTime, loseDeviceAfter: loseDeviceAfter)
  }

  /// Frames delivered to the sink so far (including refused callbacks).
  public var framesDelivered: Int { framesDeliveredCount.load(ordering: .relaxed) }

  /// Suspends until the producer thread has delivered `seconds` of audio,
  /// reported device loss or been stopped. Tests wait on this instead of
  /// wall time.
  public func waitUntilFinished() async {
    await completion.wait()
  }

  public func start(lanes: [AudioLane], inputDeviceUID: String?, sink: LaneFrameSink) throws {
    lock.lock()
    defer { lock.unlock() }
    guard thread == nil else {
      throw CaptureError.invalidState("synthetic backend already started")
    }
    stopRequested.store(false, ordering: .releasing)
    completion.reset()
    let generator = Generator(
      lanes: lanes, signals: signals, sampleRate: sampleRate, callbackFrames: callbackFrames)
    let totalFrames = Int(seconds * sampleRate)
    let lossFrame = loseDeviceAfter.map { Int($0 * sampleRate) }
    let realTime = self.realTime
    let callbackFrames = self.callbackFrames
    let thread = Thread { [self] in
      var delivered = 0
      let start = DispatchTime.now().uptimeNanoseconds
      while delivered < totalFrames, !stopRequested.load(ordering: .acquiring) {
        if let lossFrame, delivered >= lossFrame {
          sink.reportDeviceLost()
          break
        }
        let frames = min(callbackFrames, totalFrames - delivered)
        if realTime {
          let due = start + UInt64(Double(delivered) / sampleRate * 1_000_000_000)
          let now = DispatchTime.now().uptimeNanoseconds
          if due > now { Thread.sleep(forTimeInterval: Double(due - now) / 1_000_000_000) }
        } else {
          var spins = 0
          while !sink.hasRoom(for: frames), !stopRequested.load(ordering: .acquiring) {
            spins += 1
            Thread.sleep(forTimeInterval: spins < 100 ? 0.0002 : 0.002)
          }
        }
        generator.fill(frames: frames)
        let hostTime = UInt64(Double(delivered) / sampleRate * 1_000_000_000)
        if sink.beginCallback(frameCount: frames, hostTime: hostTime) {
          var lane = 0
          while lane < lanes.count {
            sink.write(lane: lane, from: generator.buffer(lane))
            lane += 1
          }
          sink.endCallback()
        }
        delivered += frames
        framesDeliveredCount.store(delivered, ordering: .relaxed)
      }
      completion.finish()
      finished.signal()
    }
    thread.name = "uno.schmid.steno.audio.synthetic"
    thread.qualityOfService = .userInteractive
    self.thread = thread
    thread.start()
  }

  public func stop() {
    lock.lock()
    let thread = self.thread
    self.thread = nil
    lock.unlock()
    guard thread != nil else { return }
    stopRequested.store(true, ordering: .releasing)
    finished.wait()
  }

  /// One-shot completion any number of tasks can await.
  private final class Completion: @unchecked Sendable {
    private let lock = NSLock()
    private var isFinished = false
    private var waiters: [CheckedContinuation<Void, Never>] = []

    func reset() {
      lock.lock()
      isFinished = false
      lock.unlock()
    }

    func finish() {
      lock.lock()
      isFinished = true
      let pending = waiters
      waiters = []
      lock.unlock()
      for waiter in pending { waiter.resume() }
    }

    func wait() async {
      await withCheckedContinuation { (continuation: CheckedContinuation<Void, Never>) in
        lock.lock()
        if isFinished {
          lock.unlock()
          continuation.resume()
          return
        }
        waiters.append(continuation)
        lock.unlock()
      }
    }
  }

  /// Per-lane phase accumulators and preallocated output buffers.
  private final class Generator: @unchecked Sendable {
    private struct LaneState {
      var increment: UInt32
      var phase: UInt32
      var amplitude: Float
      var echoLane: Int
      var echoDelaySamples: Int
      var echoGain: Float
    }

    private var states: [LaneState]
    /// Every lane's phase at the start of the current fill, so an echo of a
    /// lane already advanced in this fill still reads the right phase.
    private var startPhases: [UInt32]
    private let buffers: [UnsafeMutablePointer<Float>]
    private let sampleRate: Double
    private var position = 0

    init(
      lanes: [AudioLane], signals: [AudioLane: SyntheticLane], sampleRate: Double,
      callbackFrames: Int
    ) {
      self.sampleRate = sampleRate
      states = lanes.map { lane in
        let signal = signals[lane] ?? .silence
        let echoLane = signal.echo.flatMap { echo in lanes.firstIndex(of: echo.of) } ?? -1
        return LaneState(
          increment: UInt32((signal.frequency / sampleRate * 4_294_967_296.0).rounded()),
          phase: 0,
          amplitude: Float(signal.amplitude),
          echoLane: echoLane,
          echoDelaySamples: Int(((signal.echo?.delay ?? 0) * sampleRate).rounded()),
          echoGain: Float(signal.echo?.gain ?? 0))
      }
      startPhases = [UInt32](repeating: 0, count: lanes.count)
      buffers = lanes.map { _ in
        let pointer = UnsafeMutablePointer<Float>.allocate(capacity: callbackFrames)
        pointer.initialize(repeating: 0, count: callbackFrames)
        return pointer
      }
    }

    deinit {
      for buffer in buffers { buffer.deallocate() }
    }

    func buffer(_ lane: Int) -> UnsafeMutablePointer<Float> { buffers[lane] }

    @inline(__always)
    private static func sine(_ phase: UInt32) -> Float {
      Float(sin(2 * Double.pi * Double(phase) / 4_294_967_296.0))
    }

    /// Writes `frames` samples per lane. Echo terms are the source lane's
    /// sine at a phase offset, silent until the delay has elapsed.
    func fill(frames: Int) {
      var lane = 0
      while lane < states.count {
        startPhases[lane] = states[lane].phase
        lane += 1
      }
      lane = 0
      while lane < states.count {
        var state = states[lane]
        var index = 0
        while index < frames {
          var value = state.amplitude * Self.sine(state.phase)
          if state.echoLane >= 0, position + index >= state.echoDelaySamples {
            let source = states[state.echoLane]
            let sourcePhase =
              startPhases[state.echoLane] &+ UInt32(truncatingIfNeeded: index) &* source.increment
              &- UInt32(truncatingIfNeeded: state.echoDelaySamples) &* source.increment
            value += state.echoGain * source.amplitude * Self.sine(sourcePhase)
          }
          buffers[lane][index] = value
          state.phase = state.phase &+ state.increment
          index += 1
        }
        states[lane] = state
        lane += 1
      }
      position += frames
    }
  }
}

extension LaneFrameSink {
  /// Whether every ring can take `frameCount` more samples right now.
  public func hasRoom(for frameCount: Int) -> Bool {
    for ring in rings where !ring.hasRoom(for: frameCount) { return false }
    return true
  }
}
