import ArgumentParser
import Foundation
import StenoAudio

/// `steno dev audio-devices`: every HAL device with channel counts, rate and
/// running flag, the own process object, and every process object with its
/// `IsRunningInput` / `IsRunningOutput` flags. The manual check for the
/// property layer (audio plan step 2).
struct DevAudioDevices: AsyncParsableCommand {
  static let configuration = CommandConfiguration(
    commandName: "audio-devices",
    abstract: "List audio devices and the processes using them.")

  @Flag(help: "Only processes with a running input stream.")
  var runningOnly = false

  func run() async throws {
    #if canImport(CoreAudio)
      print("Devices")
      for device in try AudioDevices.all() {
        var marks: [String] = []
        if device.isDefaultInput { marks.append("default-input") }
        if device.isDefaultOutput { marks.append("default-output") }
        if device.isDefaultSystemOutput { marks.append("default-system-output") }
        if device.isRunningSomewhere { marks.append("running") }
        print(
          "  \(device.id)\t\(device.name)\tin:\(device.inputChannels) out:\(device.outputChannels)"
            + "\t\(Int(device.nominalSampleRate)) Hz\t\(device.transportType)"
            + "\tuid=\(device.uid)" + (marks.isEmpty ? "" : "\t[\(marks.joined(separator: ", "))]"))
      }
      print("")
      print("Own process object: \(LiveProcessAudioActivity.ownProcessObject())")
      print("")
      print("Processes")
      let activities = try LiveProcessAudioActivity().snapshot()
        .filter { !runningOnly || $0.isRunningInput }
        .sorted { $0.pid < $1.pid }
      for activity in activities {
        var flags: [String] = []
        if activity.isRunningInput { flags.append("input") }
        if activity.isRunningOutput { flags.append("output") }
        print(
          "  \(activity.pid)\t\(activity.bundleID ?? "-")"
            + (flags.isEmpty ? "" : "\t[\(flags.joined(separator: ", "))]"))
      }
    #else
      throw RuntimeFailure(description: "steno dev audio-devices needs macOS (Core Audio).")
    #endif
  }
}
