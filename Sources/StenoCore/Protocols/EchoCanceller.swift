/// Acoustic echo cancellation on the mic lane with the tap as far-end
/// reference. Real-time safe: `process` allocates nothing and takes no locks.
/// `farEnd` is the far-end signal captured at the same instant as `nearEnd`;
/// implementations own delay handling.
public protocol EchoCanceller: Sendable {
  init(sampleRate: Double, frameSize: Int) throws
  func process(
    nearEnd: UnsafeBufferPointer<Float>,
    farEnd: UnsafeBufferPointer<Float>,
    out: UnsafeMutableBufferPointer<Float>
  )
}
