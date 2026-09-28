import Foundation
import StenoCore
import Testing

@testable import StenoAudio

/// The comparison the live backend makes after a notification burst settles,
/// without a HAL: the same devices, alive, at 48 kHz mean nothing to do; the
/// first difference names the reason, loss before movement.
@Suite struct DeviceSnapshotTests {
  static let baseline = DeviceSnapshot(
    outputUID: "AirPods", defaultOutputUID: "AirPods", inputUID: "AirPods", outputAlive: true,
    inputAlive: true, sampleRate: 48_000)

  @Test func identicalDevicesAreNoChange() {
    #expect(Self.baseline.difference(from: Self.baseline) == nil)
    // A notification that changed a value back to the same value after a
    // transient looks like this to the listener that fires late.
    var again = Self.baseline
    again.sampleRate = 48_000
    #expect(again.difference(from: Self.baseline) == nil)
  }

  @Test func aDefaultThatMovedIsReportedAsMoved() {
    var output = Self.baseline
    output.outputUID = "MacBook Pro Speakers"
    #expect(output.difference(from: Self.baseline) == .defaultOutputChanged)
    var input = Self.baseline
    input.inputUID = "MacBook Pro Microphone"
    #expect(input.difference(from: Self.baseline) == .defaultInputChanged)
    var both = output
    both.inputUID = "MacBook Pro Microphone"
    #expect(both.difference(from: Self.baseline) == .defaultOutputChanged, "the first difference")
  }

  /// The tap mirrors the default output device while the clock follows the
  /// system output: alerts pinned to the speakers and a call moved to
  /// headphones move only the first, and the far-end alignment with it.
  @Test func theDefaultOutputMovingAloneIsAChange() {
    var moved = Self.baseline
    moved.defaultOutputUID = "Headphones"
    #expect(moved.difference(from: Self.baseline) == .defaultOutputChanged)
    moved.inputUID = "MacBook Pro Microphone"
    #expect(moved.difference(from: Self.baseline) == .defaultOutputChanged, "output before input")
  }

  @Test func aDeadDeviceIsReportedBeforeTheDefaultThatMovedBecauseOfIt() {
    var output = Self.baseline
    output.outputAlive = false
    output.outputUID = "MacBook Pro Speakers"
    #expect(output.difference(from: Self.baseline) == .outputDeviceGone)
    var input = Self.baseline
    input.inputAlive = false
    input.inputUID = nil
    #expect(input.difference(from: Self.baseline) == .inputDeviceGone)
    var gone = Self.baseline
    gone.outputUID = nil
    gone.outputAlive = false
    #expect(gone.difference(from: Self.baseline) == .outputDeviceGone)
  }

  @Test func anAggregateOffFortyEightKilohertzIsAChange() {
    var rate = Self.baseline
    rate.sampleRate = 44_100
    #expect(rate.difference(from: Self.baseline) == .sampleRateChanged)
    rate.sampleRate = 0
    #expect(rate.difference(from: Self.baseline) == .sampleRateChanged, "an unreadable aggregate")
  }

  /// `[.system]` alone (the Continuity spike) records no microphone: no input
  /// in either snapshot is not a change, and the default input moving is
  /// invisible because it is not resolved.
  @Test func aCaptureWithoutAMicrophoneIgnoresTheInputSide() {
    let tapOnly = DeviceSnapshot(
      outputUID: "Speakers", defaultOutputUID: "Speakers", inputUID: nil, outputAlive: true,
      inputAlive: false, sampleRate: 48_000)
    #expect(tapOnly.difference(from: tapOnly) == nil)
    var moved = tapOnly
    moved.outputUID = "AirPods"
    #expect(moved.difference(from: tapOnly) == .defaultOutputChanged)
  }
}
