import StenoCore

/// Copies the near-end to the output unchanged. For tests, `.inPerson` and
/// `steno dev aec-bench --engine passthrough`.
public final class PassthroughEchoCanceller: EchoCanceller, Sendable {
  public let sampleRate: Double
  public let frameSize: Int

  public init(sampleRate: Double, frameSize: Int) throws {
    self.sampleRate = sampleRate
    self.frameSize = frameSize
  }

  public func process(
    nearEnd: UnsafeBufferPointer<Float>,
    farEnd: UnsafeBufferPointer<Float>,
    out: UnsafeMutableBufferPointer<Float>
  ) {
    let count = min(nearEnd.count, out.count)
    guard count > 0, let source = nearEnd.baseAddress, let destination = out.baseAddress else {
      return
    }
    destination.update(from: source, count: count)
  }
}
