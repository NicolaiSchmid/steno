import Foundation

/// One HAL device as `steno dev audio-devices` and the app's input picker see
/// it. `uid` is the stable identifier `Settings.inputDeviceUID` stores.
public struct AudioDeviceInfo: Sendable, Equatable, Hashable, Identifiable {
  public var id: UInt32
  public var uid: String
  public var name: String
  public var inputChannels: Int
  public var outputChannels: Int
  public var nominalSampleRate: Double
  public var transportType: String
  public var isRunningSomewhere: Bool
  public var isDefaultInput: Bool
  public var isDefaultOutput: Bool
  public var isDefaultSystemOutput: Bool

  public var isInput: Bool { inputChannels > 0 }
  public var isOutput: Bool { outputChannels > 0 }
}

#if canImport(CoreAudio)
  import CoreAudio

  /// Device enumeration, UID lookup and default-device reads over the HAL.
  public enum AudioDevices {
    /// Every device the HAL lists, in HAL order. Private aggregates Steno
    /// creates are included while they exist.
    public static func all() throws -> [AudioDeviceInfo] {
      let ids = try AudioObjectID.system.readArray(
        AudioObjectPropertyAddress(kAudioHardwarePropertyDevices), of: AudioObjectID.self)
      let defaultInput = try? defaultDevice(kAudioHardwarePropertyDefaultInputDevice)
      let defaultOutput = try? defaultDevice(kAudioHardwarePropertyDefaultOutputDevice)
      let defaultSystem = try? defaultDevice(kAudioHardwarePropertyDefaultSystemOutputDevice)
      return ids.compactMap { id in
        guard var device = try? Self.info(id) else { return nil }
        device.isDefaultInput = id == defaultInput
        device.isDefaultOutput = id == defaultOutput
        device.isDefaultSystemOutput = id == defaultSystem
        return device
      }
    }

    /// The app's input picker.
    public static func inputs() throws -> [AudioDeviceInfo] { try all().filter(\.isInput) }

    /// The device with `uid`, or nil when it is not connected.
    public static func device(uid: String) throws -> AudioDeviceInfo? {
      try all().first { $0.uid == uid }
    }

    public static func defaultInput() throws -> AudioDeviceInfo {
      try info(try defaultDevice(kAudioHardwarePropertyDefaultInputDevice))
    }

    /// The device alerts and system sounds play through; the tap aggregate's
    /// clock master.
    public static func defaultSystemOutput() throws -> AudioDeviceInfo {
      try info(try defaultDevice(kAudioHardwarePropertyDefaultSystemOutputDevice))
    }

    static func defaultDevice(_ selector: AudioObjectPropertySelector) throws -> AudioObjectID {
      let id = try AudioObjectID.system.readObjectID(AudioObjectPropertyAddress(selector))
      guard id.isValid else {
        throw CoreAudioError(operation: "default device", objectID: .system, status: -1)
      }
      return id
    }

    static func uid(of id: AudioObjectID) throws -> String {
      try id.readString(AudioObjectPropertyAddress(kAudioDevicePropertyDeviceUID))
    }

    static func inputChannelCounts(of id: AudioObjectID) -> [Int] {
      (try? id.readBufferChannelCounts(
        AudioObjectPropertyAddress(
          kAudioDevicePropertyStreamConfiguration, scope: kAudioObjectPropertyScopeInput))) ?? []
    }

    /// `kAudioDevicePropertyLatency` plus `kAudioDevicePropertySafetyOffset`
    /// of one device in `scope`, in frames: the input path of the microphone
    /// or the output path of the loudspeaker, for the far-end delay.
    static func latencyFrames(of id: AudioObjectID, scope: AudioObjectPropertyScope) -> Int {
      let latency =
        (try? id.readUInt32(AudioObjectPropertyAddress(kAudioDevicePropertyLatency, scope: scope)))
        ?? 0
      let safety =
        (try? id.readUInt32(
          AudioObjectPropertyAddress(kAudioDevicePropertySafetyOffset, scope: scope))) ?? 0
      return Int(latency) + Int(safety)
    }

    static func outputChannelCounts(of id: AudioObjectID) -> [Int] {
      (try? id.readBufferChannelCounts(
        AudioObjectPropertyAddress(
          kAudioDevicePropertyStreamConfiguration, scope: kAudioObjectPropertyScopeOutput))) ?? []
    }

    static func info(_ id: AudioObjectID) throws -> AudioDeviceInfo {
      let transport =
        (try? id.readUInt32(AudioObjectPropertyAddress(kAudioDevicePropertyTransportType))) ?? 0
      return AudioDeviceInfo(
        id: id,
        uid: try uid(of: id),
        name: (try? id.readString(AudioObjectPropertyAddress(kAudioObjectPropertyName))) ?? "",
        inputChannels: inputChannelCounts(of: id).reduce(0, +),
        outputChannels: outputChannelCounts(of: id).reduce(0, +),
        nominalSampleRate: (try? id.readFloat64(
          AudioObjectPropertyAddress(kAudioDevicePropertyNominalSampleRate))) ?? 0,
        transportType: transportName(transport),
        isRunningSomewhere: (try? id.readBool(
          AudioObjectPropertyAddress(kAudioDevicePropertyDeviceIsRunningSomewhere))) ?? false,
        isDefaultInput: false, isDefaultOutput: false, isDefaultSystemOutput: false)
    }

    static func transportName(_ type: UInt32) -> String {
      switch type {
      case kAudioDeviceTransportTypeBuiltIn: "built-in"
      case kAudioDeviceTransportTypeAggregate: "aggregate"
      case kAudioDeviceTransportTypeVirtual: "virtual"
      case kAudioDeviceTransportTypeUSB: "usb"
      case kAudioDeviceTransportTypeBluetooth, kAudioDeviceTransportTypeBluetoothLE: "bluetooth"
      case kAudioDeviceTransportTypeHDMI: "hdmi"
      case kAudioDeviceTransportTypeDisplayPort: "displayport"
      case kAudioDeviceTransportTypeAirPlay: "airplay"
      case kAudioDeviceTransportTypeThunderbolt: "thunderbolt"
      case kAudioDeviceTransportTypeContinuityCaptureWired,
        kAudioDeviceTransportTypeContinuityCaptureWireless:
        "continuity-capture"
      case 0: "unknown"
      default: fourCharCode(OSStatus(bitPattern: type))
      }
    }
  }
#endif
