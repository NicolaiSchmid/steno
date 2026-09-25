import Foundation
import Synchronization

/// RMS and peak over a metering window, reported in dBFS. A value type the
/// processing thread mutates in place; `flush()` reads and resets it.
public struct LevelMeter: Sendable, Equatable {
  private var sumSquares: Double = 0
  private var peakValue: Float = 0
  private var count = 0

  public init() {}

  @inline(__always)
  public mutating func accumulate(_ samples: UnsafePointer<Float>, count: Int) {
    var index = 0
    var squares: Double = 0
    var peak = peakValue
    while index < count {
      let value = samples[index]
      squares += Double(value * value)
      let magnitude = abs(value)
      if magnitude > peak { peak = magnitude }
      index += 1
    }
    sumSquares += squares
    peakValue = peak
    self.count += count
  }

  /// The loudest magnitude of the window so far, linear.
  public var linearPeak: Float { peakValue }

  /// The window so far, without resetting.
  public var current: LaneLevel {
    guard count > 0 else { return .silence }
    let rms = Float((sumSquares / Double(count)).squareRoot())
    return LaneLevel(rms: Self.decibels(rms), peak: Self.decibels(peakValue))
  }

  public mutating func flush() -> LaneLevel {
    let level = current
    sumSquares = 0
    peakValue = 0
    count = 0
    return level
  }

  /// dBFS of a linear magnitude; `-160` for digital silence.
  public static func decibels(_ linear: Float) -> Float {
    guard linear > 1e-8 else { return -160 }
    return max(-160, 20 * log10(linear))
  }
}

/// The latest `LaneLevels` written by the processing thread as atomic bit
/// patterns, read by whoever publishes them (the session's writer thread).
/// The reader may observe a torn pair across a publish; levels feed a meter,
/// nothing else, so no lock is worth it.
public final class LevelSlot: @unchecked Sendable {
  private let micRMS = Atomic<UInt32>(LaneLevel.silence.rms.bitPattern)
  private let micPeak = Atomic<UInt32>(LaneLevel.silence.peak.bitPattern)
  private let systemRMS = Atomic<UInt32>(LaneLevel.silence.rms.bitPattern)
  private let systemPeak = Atomic<UInt32>(LaneLevel.silence.peak.bitPattern)
  private let hasSystem: Bool
  private let generation = Atomic<Int>(0)

  public init(hasSystem: Bool) {
    self.hasSystem = hasSystem
  }

  @inline(__always)
  public func publish(mic: LaneLevel, system: LaneLevel?) {
    micRMS.store(mic.rms.bitPattern, ordering: .relaxed)
    micPeak.store(mic.peak.bitPattern, ordering: .relaxed)
    if let system {
      systemRMS.store(system.rms.bitPattern, ordering: .relaxed)
      systemPeak.store(system.peak.bitPattern, ordering: .relaxed)
    }
    generation.wrappingAdd(1, ordering: .releasing)
  }

  /// The publish count so far; a reader republishes when it changed.
  public var currentGeneration: Int { generation.load(ordering: .acquiring) }

  public var levels: LaneLevels {
    LaneLevels(
      mic: LaneLevel(
        rms: Float(bitPattern: micRMS.load(ordering: .relaxed)),
        peak: Float(bitPattern: micPeak.load(ordering: .relaxed))),
      system: hasSystem
        ? LaneLevel(
          rms: Float(bitPattern: systemRMS.load(ordering: .relaxed)),
          peak: Float(bitPattern: systemPeak.load(ordering: .relaxed)))
        : nil)
  }
}
