import Foundation
import StenoCore
import Synchronization
import Testing

@testable import StenoAudio

#if os(macOS)
  import Darwin

  /// libmalloc calls `malloc_logger` for every allocation in the process while
  /// it is set (the hook Instruments' allocation recording uses). The hook here
  /// counts the allocations made on one thread; it allocates nothing and takes
  /// no locks itself. Darwin only: glibc has no equivalent, so on Linux the
  /// real-time promise rests on the reviewer grep alone.
  enum AllocationHook {
    typealias Logger = @convention(c) (UInt32, UInt, UInt, UInt, UInt, UInt32) -> Void
    /// `MALLOC_LOG_TYPE_ALLOCATE`; realloc carries it too.
    static let allocateFlag: UInt32 = 2
    static let count = Atomic<Int>(0)
    static let thread = Atomic<UInt>(0)
    /// The first few offending stacks, captured with `backtrace` (no malloc)
    /// into storage allocated before the hook goes in; symbolised afterwards.
    static let traceDepth = 32
    static let maxTraces = 4
    nonisolated(unsafe) static let traces = UnsafeMutablePointer<UnsafeMutableRawPointer?>
      .allocate(capacity: traceDepth * maxTraces)

    /// The global `malloc_logger` pointer, looked up by name so the test does
    /// not depend on the header being visible to Swift.
    static var slot: UnsafeMutablePointer<Logger?>? {
      dlsym(dlopen(nil, RTLD_NOW), "malloc_logger").map {
        $0.assumingMemoryBound(to: Logger?.self)
      }
    }

    /// Allocations the calling thread made while running `body`, and the
    /// symbolised stacks of the first few, for the failure message.
    static func allocations(during body: () -> Void) throws -> (count: Int, stacks: String) {
      let slot = try #require(Self.slot, "malloc_logger is not exported by libmalloc")
      count.store(0, ordering: .relaxed)
      traces.update(repeating: nil, count: traceDepth * maxTraces)
      thread.store(UInt(bitPattern: pthread_self()), ordering: .relaxed)
      slot.pointee = { type, _, _, _, _, _ in
        guard type & AllocationHook.allocateFlag != 0,
          UInt(bitPattern: pthread_self()) == AllocationHook.thread.load(ordering: .relaxed)
        else { return }
        let index = AllocationHook.count.loadThenWrappingAdd(1, ordering: .relaxed)
        if index < AllocationHook.maxTraces {
          _ = backtrace(
            AllocationHook.traces + index * AllocationHook.traceDepth,
            Int32(AllocationHook.traceDepth))
        }
      }
      body()
      slot.pointee = nil
      let total = count.load(ordering: .relaxed)
      var stacks = ""
      for index in 0..<min(total, maxTraces) {
        let frames = traces + index * traceDepth
        var depth = 0
        while depth < traceDepth, frames[depth] != nil { depth += 1 }
        guard let symbols = backtrace_symbols(frames, Int32(depth)) else { continue }
        stacks += "\nallocation \(index + 1):\n"
        for frame in 0..<depth {
          if let symbol = symbols[frame] { stacks += "  " + String(cString: symbol) + "\n" }
        }
        free(symbols)
      }
      return (total, stacks)
    }
  }

  /// The real-time promise, checked rather than grepped: the IOProc body
  /// (`LaneFrameSink` producer calls), the processing loop with the real Speex
  /// canceller, its far-end delay line, metering, the raw-mic copy and the
  /// relay hand-off run one second of audio on the test's thread under the
  /// allocation hook and allocate nothing. Serialized because the hook is
  /// process-wide.
  @Suite(.serialized) struct RealTimeAllocationTests {
    static let frameSize = StenoAudio.frameSize
    static let callbackFrames = 512

    /// Preallocated 48 kHz material: a mono mic and an interleaved stereo tap.
    struct Material {
      let mic: UnsafeMutablePointer<Float>
      let tap: UnsafeMutablePointer<Float>
      let frames: Int

      init(seconds: Double) {
        frames = Int(seconds * StenoAudio.sampleRate)
        mic = .allocate(capacity: frames)
        tap = .allocate(capacity: frames * 2)
        let near = AudioFixtures.tone(frequency: 320, seconds: seconds, amplitude: 0.25)
        let far = AudioFixtures.tone(frequency: 1_000, seconds: seconds, amplitude: 0.5)
        for index in 0..<frames {
          mic[index] = near[index] + (index >= 2_880 ? 0.5 * far[index - 2_880] : 0)
          tap[2 * index] = far[index]
          tap[2 * index + 1] = far[index] * 0.5
        }
      }

      func release() {
        mic.deallocate()
        tap.deallocate()
      }

      /// What the IOProc block does per callback, for every callback of the
      /// material: mono mic, stereo tap folded to mono.
      func deliver(to sink: LaneFrameSink) {
        var offset = 0
        while offset < frames {
          let count = min(RealTimeAllocationTests.callbackFrames, frames - offset)
          if sink.beginCallback(frameCount: count) {
            sink.write(lane: 0, from: mic + offset)
            sink.writeMixed(lane: 1, left: tap + 2 * offset, right: tap + 2 * offset + 1, stride: 2)
            sink.endCallback()
          }
          offset += count
        }
      }
    }

    @Test func theHookSeesAnAllocationOnThisThread() throws {
      let seen = try AllocationHook.allocations {
        let pointer = UnsafeMutablePointer<Float>.allocate(capacity: 100_000)
        pointer.initialize(repeating: 1, count: 100_000)
        pointer.deallocate()
      }
      #expect(
        seen.count >= 1, "the hook must observe a deliberate allocation, or it proves nothing")
      #expect(seen.stacks.contains("allocation 1:"), "the hook captures the allocating stack")
    }

    @Test func producerProcessingAndRelayAllocateNothingAfterWarmUp() throws {
      let lanes: [AudioLane] = [.mic, .system]
      let sink = LaneFrameSink(lanes: lanes)
      let relay = FrameRelay(channels: 3, frameSize: Self.frameSize, capacityFrames: 128)
      let thread = ProcessingThread(
        sink: sink, relay: relay,
        configuration: .init(
          lanes: lanes,
          echoCanceller: try SpeexEchoCanceller(sampleRate: 48_000, frameSize: Self.frameSize),
          farEndDelayFrames: 7_200, keepRawMic: true))
      // Never started: `drain()` runs the loop body on this thread instead.
      defer { thread.stop() }

      // Warm-up outside the hook: ten frames let Swift and Speex touch any
      // lazily initialised state once (the plan's "after the first second").
      let warmUp = Material(seconds: 0.1)
      defer { warmUp.release() }
      warmUp.deliver(to: sink)
      thread.drain()
      #expect(thread.framesProcessed == 10)

      let second = Material(seconds: 1)
      defer { second.release() }
      let allocations = try AllocationHook.allocations {
        second.deliver(to: sink)
        thread.drain()
      }
      #expect(
        allocations.count == 0,
        "\(allocations.count) allocations on the real-time path\(allocations.stacks)")
      #expect(thread.framesProcessed == 110)
      #expect(sink.droppedSamples.isEmpty)
      #expect(relay.droppedFrames == [0, 0, 0])
      #expect(relay.availableFrames == 110)
      #expect(thread.levels.currentGeneration == 11)
      #expect(
        abs(thread.systemPeak - 0.375) < 0.01, "the folded stereo tap peaks at (0.5 + 0.25) / 2")
    }

    @Test func theSidecarResamplerAllocatesNothingAfterInit() throws {
      let resampler = Resampler48kTo16k()
      let input = UnsafeMutablePointer<Float>.allocate(capacity: Self.frameSize)
      defer { input.deallocate() }
      let tone = AudioFixtures.tone(frequency: 1_000, seconds: 0.01)
      tone.withUnsafeBufferPointer {
        input.initialize(from: $0.baseAddress!, count: Self.frameSize)
      }
      let output = UnsafeMutablePointer<Int16>.allocate(capacity: resampler.outputFrameSize)
      defer { output.deallocate() }
      resampler.process(input, into: output)
      let allocations = try AllocationHook.allocations {
        for _ in 0..<100 { resampler.process(input, into: output) }
      }
      #expect(
        allocations.count == 0,
        "\(allocations.count) allocations in 100 resampler frames\(allocations.stacks)")
    }
  }
#endif
