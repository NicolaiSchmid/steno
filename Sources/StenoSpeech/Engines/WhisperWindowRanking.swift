import Foundation

/// Picks the windows Whisper's language detection should look at: the
/// `count` most energetic `windowSeconds` windows of the buffer, by RMS,
/// highest first. No VAD model needed; speech-dense windows beat silence and
/// room noise by a wide margin.
enum WhisperWindowRanking {
  struct Window: Sendable, Equatable {
    var samples: Range<Int>
    var rms: Float
  }

  static func topWindows(
    samples: [Float], sampleRate: Double = 16_000, windowSeconds: TimeInterval = 30,
    count: Int = 3
  ) -> [Window] {
    guard !samples.isEmpty, count > 0 else { return [] }
    let size = max(1, Int(windowSeconds * sampleRate))
    var windows: [Window] = []
    var start = 0
    while start < samples.count {
      let end = min(samples.count, start + size)
      var sum: Double = 0
      for value in samples[start..<end] { sum += Double(value) * Double(value) }
      windows.append(
        Window(samples: start..<end, rms: Float((sum / Double(end - start)).squareRoot())))
      start = end
    }
    return
      windows
      .sorted { lhs, rhs in
        if lhs.rms != rhs.rms { return lhs.rms > rhs.rms }
        return lhs.samples.lowerBound < rhs.samples.lowerBound
      }
      .prefix(count)
      .map { $0 }
  }

  /// Majority vote over detected languages; ties go to the earliest entry,
  /// which is the most energetic window when the input is `topWindows` order.
  static func majority(_ languages: [String]) -> String? {
    var counts: [String: Int] = [:]
    var firstSeen: [String: Int] = [:]
    for (index, language) in languages.enumerated() {
      counts[language, default: 0] += 1
      if firstSeen[language] == nil { firstSeen[language] = index }
    }
    return counts.max { lhs, rhs in
      if lhs.value != rhs.value { return lhs.value < rhs.value }
      return firstSeen[lhs.key, default: 0] > firstSeen[rhs.key, default: 0]
    }?.key
  }
}
