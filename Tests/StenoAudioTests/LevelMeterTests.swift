import Foundation
import Testing

@testable import StenoAudio

@Suite struct LevelMeterTests {
  func sine(amplitude: Float, frequency: Double = 1_000, count: Int = 48_000) -> [Float] {
    (0..<count).map { amplitude * Float(sin(2 * Double.pi * frequency * Double($0) / 48_000)) }
  }

  @Test func fullScaleSineIsMinusThreeDecibelsRMS() {
    var meter = LevelMeter()
    let samples = sine(amplitude: 1)
    samples.withUnsafeBufferPointer { meter.accumulate($0.baseAddress!, count: $0.count) }
    let level = meter.flush()
    #expect(abs(level.rms - -3.01) < 0.05)
    #expect(abs(level.peak) < 0.01)
    #expect(meter.current == .silence, "flush resets the window")
  }

  @Test func quietSineScalesInDecibels() {
    var meter = LevelMeter()
    let samples = sine(amplitude: 0.1)
    samples.withUnsafeBufferPointer { meter.accumulate($0.baseAddress!, count: $0.count) }
    let level = meter.current
    #expect(abs(level.rms - -23.01) < 0.05)
    #expect(abs(level.peak - -20) < 0.01)
    #expect(abs(meter.linearPeak - 0.1) < 0.001)
  }

  @Test func silenceReportsTheFloor() {
    var meter = LevelMeter()
    let zeros = [Float](repeating: 0, count: 480)
    zeros.withUnsafeBufferPointer { meter.accumulate($0.baseAddress!, count: $0.count) }
    #expect(meter.flush() == .silence)
    #expect(LevelMeter.decibels(0) == -160)
    #expect(LevelMeter.decibels(1) == 0)
    #expect(abs(LevelMeter.decibels(0.5) - -6.02) < 0.01)
  }

  @Test func windowsAccumulateAcrossCalls() {
    var meter = LevelMeter()
    let loud = [Float](repeating: 1, count: 100)
    let quiet = [Float](repeating: 0, count: 300)
    loud.withUnsafeBufferPointer { meter.accumulate($0.baseAddress!, count: 100) }
    quiet.withUnsafeBufferPointer { meter.accumulate($0.baseAddress!, count: 300) }
    // A quarter of the window at full scale: rms 0.5.
    #expect(abs(meter.current.rms - -6.02) < 0.01)
    #expect(meter.current.peak == 0)
  }

  @Test func levelSlotPublishesGenerations() {
    let slot = LevelSlot(hasSystem: true)
    #expect(slot.currentGeneration == 0)
    #expect(slot.levels == LaneLevels(mic: .silence, system: .silence))
    slot.publish(mic: LaneLevel(rms: -10, peak: -3), system: LaneLevel(rms: -20, peak: -12))
    #expect(slot.currentGeneration == 1)
    #expect(
      slot.levels
        == LaneLevels(mic: LaneLevel(rms: -10, peak: -3), system: LaneLevel(rms: -20, peak: -12)))
    let mono = LevelSlot(hasSystem: false)
    mono.publish(mic: LaneLevel(rms: -1, peak: 0), system: nil)
    #expect(mono.levels.system == nil)
  }
}
