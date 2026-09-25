import Foundation
import StenoCore
import Testing

@testable import StenoAudio

#if canImport(CoreAudio)
  import CoreAudio

  /// The IOProc body on input buffer lists built by hand: both HAL orderings
  /// `StreamLayout` resolves reach the rings through the real pointer
  /// arithmetic (stride, channel offset, stereo fold), a buffer without data
  /// becomes silence, and a callback that does not fit is refused for every
  /// lane. Core Audio types only; no device is opened.
  @Suite struct IOProcRunnerTests {
    /// One input buffer: its channel count and interleaved samples, or nil
    /// samples for a buffer the HAL delivered without data.
    struct Buffer {
      var channels: Int
      var samples: [Float]?
    }

    func withBufferList(
      _ buffers: [Buffer], frames: Int, _ body: (UnsafeMutableAudioBufferListPointer) -> Void
    ) {
      let list = AudioBufferList.allocate(maximumBuffers: buffers.count)
      var storage: [UnsafeMutablePointer<Float>] = []
      defer {
        for pointer in storage { pointer.deallocate() }
        free(list.unsafeMutablePointer)
      }
      for (index, buffer) in buffers.enumerated() {
        var audioBuffer = AudioBuffer()
        audioBuffer.mNumberChannels = UInt32(buffer.channels)
        audioBuffer.mDataByteSize = UInt32(frames * buffer.channels * 4)
        if let samples = buffer.samples {
          let pointer = UnsafeMutablePointer<Float>.allocate(capacity: samples.count)
          samples.withUnsafeBufferPointer {
            pointer.initialize(from: $0.baseAddress!, count: samples.count)
          }
          storage.append(pointer)
          audioBuffer.mData = UnsafeMutableRawPointer(pointer)
        }
        list[index] = audioBuffer
      }
      body(list)
    }

    func read(_ sink: LaneFrameSink, lane: Int, count: Int) -> [Float] {
      var out = [Float](repeating: .nan, count: count)
      out.withUnsafeMutableBufferPointer {
        _ = sink.ring(lane).read(into: $0.baseAddress!, count: count)
      }
      return out
    }

    /// Built-in mic (mono) then the tap (interleaved stereo).
    @Test func subDevicesFirstWithAnInterleavedTap() throws {
      let layout = try StreamLayout.resolve(
        lanes: [.mic, .system], aggregate: [1, 2], subDevices: [[], [1]], tap: [2], micSubDevice: 1)
      let sink = LaneFrameSink(lanes: [.mic, .system])
      withBufferList(
        [
          Buffer(channels: 1, samples: [1, 2, 3, 4]),
          Buffer(channels: 2, samples: [10, 20, 30, 40, 50, 60, 70, 80]),
        ], frames: 4
      ) { list in
        IOProcRunner.deliver(list, sources: layout.sources, sink: sink)
      }
      #expect(sink.availableToRead == 4)
      #expect(read(sink, lane: 0, count: 4) == [1, 2, 3, 4])
      #expect(read(sink, lane: 1, count: 4) == [15, 35, 55, 75], "stereo folded to mono")
      #expect(sink.wake.wait(timeout: .now()) == .success)
      #expect(sink.wake.wait(timeout: .now()) == .timedOut, "one wake per callback")
      #expect(sink.droppedSamples.isEmpty)
    }

    /// The tap first and non-interleaved, the microphone a stereo device: the
    /// mic lane is channel 0 of buffer 2 read with stride 2.
    @Test func tapFirstNonInterleavedWithAStereoMicrophone() throws {
      let layout = try StreamLayout.resolve(
        lanes: [.mic, .system], aggregate: [1, 1, 2], subDevices: [[], [2]], tap: [1, 1],
        micSubDevice: 1)
      #expect(layout.tapFirst)
      let sink = LaneFrameSink(lanes: [.mic, .system])
      withBufferList(
        [
          Buffer(channels: 1, samples: [1, 1, 1, 1]),
          Buffer(channels: 1, samples: [3, 3, 3, 3]),
          Buffer(channels: 2, samples: [5, -5, 6, -6, 7, -7, 8, -8]),
        ], frames: 4
      ) { list in
        IOProcRunner.deliver(list, sources: layout.sources, sink: sink)
      }
      #expect(read(sink, lane: 0, count: 4) == [5, 6, 7, 8], "left channel of the stereo mic")
      #expect(read(sink, lane: 1, count: 4) == [2, 2, 2, 2], "two mono tap buffers averaged")
    }

    /// In person: the mic alone, the frame count from its own buffer.
    @Test func inPersonSingleLane() throws {
      let layout = try StreamLayout.resolve(
        lanes: [.mixed], aggregate: [1], subDevices: [[], [1]], tap: [], micSubDevice: 1)
      let sink = LaneFrameSink(lanes: [.mixed])
      withBufferList([Buffer(channels: 1, samples: [0.5, -0.5, 0.25])], frames: 3) { list in
        IOProcRunner.deliver(list, sources: layout.sources, sink: sink)
      }
      #expect(read(sink, lane: 0, count: 3) == [0.5, -0.5, 0.25])
    }

    /// A buffer without data keeps its lane aligned with zeros instead of
    /// skipping the callback.
    @Test func aBufferWithoutDataBecomesSilence() throws {
      let layout = try StreamLayout.resolve(
        lanes: [.mic, .system], aggregate: [1, 2], subDevices: [[], [1]], tap: [2], micSubDevice: 1)
      let sink = LaneFrameSink(lanes: [.mic, .system])
      withBufferList(
        [Buffer(channels: 1, samples: [1, 2, 3, 4]), Buffer(channels: 2, samples: nil)], frames: 4
      ) { list in
        IOProcRunner.deliver(list, sources: layout.sources, sink: sink)
      }
      #expect(sink.availableToRead == 4)
      #expect(read(sink, lane: 0, count: 4) == [1, 2, 3, 4])
      #expect(read(sink, lane: 1, count: 4) == [0, 0, 0, 0])
    }

    /// Rings of four samples: the second callback of four is refused for both
    /// lanes and counted, and nothing is half-written.
    @Test func aCallbackThatDoesNotFitIsRefusedForEveryLane() throws {
      let layout = try StreamLayout.resolve(
        lanes: [.mic, .system], aggregate: [1, 2], subDevices: [[], [1]], tap: [2], micSubDevice: 1)
      let sink = LaneFrameSink(lanes: [.mic, .system], sampleRate: 4, ringSeconds: 1)
      for _ in 0..<2 {
        withBufferList(
          [
            Buffer(channels: 1, samples: [1, 2, 3, 4]),
            Buffer(channels: 2, samples: [1, 1, 1, 1, 1, 1, 1, 1]),
          ], frames: 4
        ) { list in
          IOProcRunner.deliver(list, sources: layout.sources, sink: sink)
        }
      }
      #expect(sink.availableToRead == 4)
      #expect(sink.droppedSamples == [.mic: 4, .system: 4])
      #expect(sink.wake.wait(timeout: .now()) == .success)
      #expect(sink.wake.wait(timeout: .now()) == .timedOut, "no wake for the refused callback")
    }
  }
#endif
