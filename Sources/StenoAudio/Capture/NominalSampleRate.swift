/// The HAL applies `kAudioDevicePropertyNominalSampleRate` asynchronously:
/// a successful write can still read back the old rate for a few cycles, and
/// a device that cannot run at the requested rate keeps its own for good.
/// `settle` reads until the rate matches or the attempts run out, so the
/// backend fails loudly instead of labelling a 44.1 kHz master 48 kHz.
enum NominalSampleRate {
  /// Ten reads, 20 ms apart: 200 ms at most.
  static let attempts = 10
  static let interval: Double = 0.02

  /// Returns the first rate `read()` reports that equals `target`, or the last
  /// rate read once `attempts` are used up; `wait()` runs between attempts.
  static func settle(
    to target: Double, attempts: Int = attempts, read: () -> Double, wait: () -> Void
  ) -> Double {
    var rate = read()
    var attempt = 1
    while rate != target, attempt < attempts {
      wait()
      rate = read()
      attempt += 1
    }
    return rate
  }
}
