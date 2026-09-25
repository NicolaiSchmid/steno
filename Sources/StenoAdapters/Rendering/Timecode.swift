import Foundation

/// In-recording offsets as text. Integer arithmetic over milliseconds, so the
/// output never depends on a locale or a platform's `String(format:)`.
public enum Timecode {
  /// `"00:12:34"`: whole seconds, floored. Negative offsets clamp to zero.
  public static func clock(_ offset: TimeInterval) -> String {
    let seconds = milliseconds(offset) / 1000
    return "\(pad(seconds / 3600)):\(pad(seconds / 60 % 60)):\(pad(seconds % 60))"
  }

  /// `"00:12:34.567"`: the WebVTT timestamp, milliseconds rounded.
  public static func vtt(_ offset: TimeInterval) -> String {
    let total = milliseconds(offset)
    return "\(clock(TimeInterval(total) / 1000)).\(pad(total % 1000, width: 3))"
  }

  static func milliseconds(_ offset: TimeInterval) -> Int {
    guard offset.isFinite, offset > 0 else { return 0 }
    return Int((offset * 1000).rounded())
  }

  /// `value` in `radix`, zero-padded on the left to at least `width` digits.
  static func pad(_ value: Int, width: Int = 2, radix: Int = 10) -> String {
    let digits = String(value, radix: radix, uppercase: true)
    return String(repeating: "0", count: max(0, width - digits.count)) + digits
  }
}
