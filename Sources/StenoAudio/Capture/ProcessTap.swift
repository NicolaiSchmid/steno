#if canImport(CoreAudio)
  import CoreAudio
  import Foundation

  /// A private, unmuted global process tap of everything the Mac plays except
  /// the excluded processes (Steno itself). Created with
  /// `AudioHardwareCreateProcessTap`, destroyed exactly once.
  final class ProcessTap: @unchecked Sendable {
    let tapID: AudioObjectID
    /// The tap's UID as `kAudioSubTapUIDKey` wants it.
    let uid: String
    let format: AudioStreamBasicDescription
    private let destroyed = NSLock()
    private var isDestroyed = false

    init(excluding processObjects: [AudioObjectID], name: String = "Steno system lane") throws {
      let description = CATapDescription(stereoGlobalTapButExcludeProcesses: processObjects)
      description.name = name
      description.isPrivate = true
      description.muteBehavior = .unmuted
      var id = AudioObjectID.unknown
      let status = AudioHardwareCreateProcessTap(description, &id)
      guard status == noErr, id.isValid else {
        throw CaptureError.tapCreationFailed(status == noErr ? -1 : status)
      }
      tapID = id
      uid = description.uuid.uuidString
      format =
        (try? id.read(
          AudioObjectPropertyAddress(kAudioTapPropertyFormat),
          defaultValue: AudioStreamBasicDescription())) ?? AudioStreamBasicDescription()
    }

    var channelCount: Int { Int(format.mChannelsPerFrame) }

    /// The tap's buffers as the aggregate exposes them: one interleaved
    /// buffer, or one buffer per channel when the format is non-interleaved.
    var bufferChannelCounts: [Int] {
      let channels = max(channelCount, 1)
      if format.mFormatFlags & kAudioFormatFlagIsNonInterleaved != 0 {
        return Array(repeating: 1, count: channels)
      }
      return [channels]
    }

    func destroy() {
      destroyed.lock()
      defer { destroyed.unlock() }
      guard !isDestroyed else { return }
      isDestroyed = true
      AudioHardwareDestroyProcessTap(tapID)
    }

    deinit { destroy() }
  }
#endif
