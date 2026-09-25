#if canImport(CoreAudio)
  import CoreAudio
  import Foundation

  /// One IOProc on the aggregate device. The block only follows the
  /// precomputed `StreamLayout`: pointer arithmetic from the input buffer list
  /// into the sink's rings, one semaphore signal. No arrays are created, no
  /// closures called, nothing logged, nothing awaited inside the callback.
  final class IOProcRunner: @unchecked Sendable {
    private let deviceID: AudioObjectID
    private let sink: LaneFrameSink
    private let sources: [StreamLayout.LaneSource]
    /// A serial queue for the IOProc (non-nil, as the plan requires); the HAL
    /// drives it at real-time priority.
    private let queue = DispatchQueue(label: "uno.schmid.steno.audio.ioproc", qos: .userInteractive)
    private let lock = NSLock()
    private var procID: AudioDeviceIOProcID?

    init(deviceID: AudioObjectID, sink: LaneFrameSink, layout: StreamLayout) {
      self.deviceID = deviceID
      self.sink = sink
      self.sources = layout.sources
    }

    func start() throws {
      lock.lock()
      defer { lock.unlock() }
      guard procID == nil else { return }
      let sink = self.sink
      let sources = self.sources
      let block: AudioDeviceIOBlock = { _, inputData, _, _, _ in
        Self.deliver(
          UnsafeMutableAudioBufferListPointer(UnsafeMutablePointer(mutating: inputData)),
          sources: sources, sink: sink)
      }
      var procID: AudioDeviceIOProcID?
      let created = AudioDeviceCreateIOProcIDWithBlock(&procID, deviceID, queue, block)
      guard created == noErr, let procID else {
        throw CaptureError.coreAudio(
          operation: "AudioDeviceCreateIOProcIDWithBlock", status: created)
      }
      let started = AudioDeviceStart(deviceID, procID)
      guard started == noErr else {
        AudioDeviceDestroyIOProcID(deviceID, procID)
        throw CaptureError.coreAudio(operation: "AudioDeviceStart", status: started)
      }
      self.procID = procID
    }

    /// The IOProc body: one callback's input buffer list into the sink's rings
    /// following `sources`. The frame count comes from the first source's
    /// buffer; a buffer the HAL delivered without data becomes silence so the
    /// lanes stay aligned. Static so `IOProcRunnerTests` can hand it a buffer
    /// list built by hand for both HAL orderings; the block above only calls
    /// it.
    @inline(__always)
    static func deliver(
      _ list: UnsafeMutableAudioBufferListPointer, sources: [StreamLayout.LaneSource],
      sink: LaneFrameSink
    ) {
      let bufferCount = list.count
      guard bufferCount > 0, sources.count > 0 else { return }
      let firstIndex = sources[0].left.buffer
      guard firstIndex < bufferCount else { return }
      let first = list[firstIndex]
      let channels = Int(first.mNumberChannels)
      guard channels > 0 else { return }
      let frames = Int(first.mDataByteSize) / (channels * 4)
      guard frames > 0, sink.beginCallback(frameCount: frames) else { return }
      var index = 0
      while index < sources.count {
        let source = sources[index]
        if let left = samples(of: source.left, in: list) {
          if let right = source.right, let rightSamples = samples(of: right, in: list) {
            sink.writeMixed(
              lane: index, left: left, right: rightSamples, stride: source.left.stride,
              rightStride: right.stride)
          } else {
            sink.write(lane: index, from: left, stride: source.left.stride)
          }
        } else {
          sink.writeSilence(lane: index)
        }
        index += 1
      }
      sink.endCallback()
    }

    /// The first sample of `channel` in `list`, nil when the buffer is missing
    /// or the HAL delivered it without data.
    @inline(__always)
    private static func samples(
      of channel: StreamLayout.ChannelRef, in list: UnsafeMutableAudioBufferListPointer
    ) -> UnsafePointer<Float>? {
      guard channel.buffer < list.count, let data = list[channel.buffer].mData else { return nil }
      return UnsafePointer(data.assumingMemoryBound(to: Float.self)) + channel.offset
    }

    /// `AudioDeviceStop` then `AudioDeviceDestroyIOProcID`; idempotent.
    func stop() {
      lock.lock()
      defer { lock.unlock() }
      guard let procID else { return }
      AudioDeviceStop(deviceID, procID)
      AudioDeviceDestroyIOProcID(deviceID, procID)
      self.procID = nil
    }

    deinit { stop() }
  }
#endif
