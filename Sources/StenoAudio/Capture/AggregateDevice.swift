#if canImport(CoreAudio)
  import CoreAudio
  import Foundation

  /// A private aggregate device: the default output device as clock master
  /// (`kAudioAggregateDeviceMainSubDeviceKey`), the microphone as a
  /// drift-compensated sub-device and the process tap as a drift-compensated
  /// sub-tap with `TapAutoStart`. One IOProc on it delivers every lane in one
  /// callback, sample-aligned by the HAL.
  final class AggregateDevice: @unchecked Sendable {
    let deviceID: AudioObjectID
    let uid: String
    private let destroyed = NSLock()
    private var isDestroyed = false

    init(name: String, mainSubDeviceUID: String, subDeviceUIDs: [String], tapUIDs: [String])
      throws
    {
      let uid = "uno.schmid.steno.aggregate." + UUID().uuidString
      let subDevices: [[String: Any]] = subDeviceUIDs.map { deviceUID in
        [
          kAudioSubDeviceUIDKey: deviceUID,
          kAudioSubDeviceDriftCompensationKey: deviceUID != mainSubDeviceUID,
        ]
      }
      let taps: [[String: Any]] = tapUIDs.map { tapUID in
        [kAudioSubTapUIDKey: tapUID, kAudioSubTapDriftCompensationKey: true]
      }
      let description: [String: Any] = [
        kAudioAggregateDeviceNameKey: name,
        kAudioAggregateDeviceUIDKey: uid,
        kAudioAggregateDeviceMainSubDeviceKey: mainSubDeviceUID,
        kAudioAggregateDeviceIsPrivateKey: true,
        kAudioAggregateDeviceIsStackedKey: false,
        kAudioAggregateDeviceTapAutoStartKey: true,
        kAudioAggregateDeviceSubDeviceListKey: subDevices,
        kAudioAggregateDeviceTapListKey: taps,
      ]
      var id = AudioObjectID.unknown
      let status = AudioHardwareCreateAggregateDevice(description as CFDictionary, &id)
      guard status == noErr, id.isValid else {
        throw CaptureError.aggregateCreationFailed(status == noErr ? -1 : status)
      }
      self.deviceID = id
      self.uid = uid
    }

    var nominalSampleRate: Double {
      (try? deviceID.readFloat64(AudioObjectPropertyAddress(kAudioDevicePropertyNominalSampleRate)))
        ?? 0
    }

    func setNominalSampleRate(_ rate: Double) throws {
      try deviceID.write(
        AudioObjectPropertyAddress(kAudioDevicePropertyNominalSampleRate), value: rate)
    }

    /// Channel count per input buffer, the shape the IOProc receives.
    var inputChannelCounts: [Int] {
      AudioDevices.inputChannelCounts(of: deviceID)
    }

    func destroy() {
      destroyed.lock()
      defer { destroyed.unlock() }
      guard !isDestroyed else { return }
      isDestroyed = true
      AudioHardwareDestroyAggregateDevice(deviceID)
    }

    deinit { destroy() }
  }
#endif
