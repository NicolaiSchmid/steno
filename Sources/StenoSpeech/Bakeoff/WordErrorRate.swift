import Foundation

/// Word error rate: Levenshtein distance over words divided by the reference
/// length, after lower-casing, stripping punctuation and (optionally) folding
/// umlauts and ß so `Muenchen` and `München` agree. An empty reference gives
/// 0 for an empty hypothesis and 1 otherwise.
public enum WordErrorRate {
  public static func compute(reference: String, hypothesis: String, foldUmlauts: Bool = true)
    -> Double
  {
    let ref = normalise(reference, foldUmlauts: foldUmlauts)
    let hyp = normalise(hypothesis, foldUmlauts: foldUmlauts)
    guard !ref.isEmpty else { return hyp.isEmpty ? 0 : 1 }
    return Double(levenshtein(ref, hyp)) / Double(ref.count)
  }

  /// Lower-case words with punctuation removed; digits and letters kept.
  public static func normalise(_ text: String, foldUmlauts: Bool = true) -> [String] {
    var lowered = text.lowercased()
    if foldUmlauts {
      for (from, to) in [("ä", "ae"), ("ö", "oe"), ("ü", "ue"), ("ß", "ss")] {
        lowered = lowered.replacingOccurrences(of: from, with: to)
      }
    }
    return lowered.split { !($0.isLetter || $0.isNumber) }.map(String.init)
  }

  static func levenshtein(_ lhs: [String], _ rhs: [String]) -> Int {
    if lhs.isEmpty { return rhs.count }
    if rhs.isEmpty { return lhs.count }
    var previous = Array(0...rhs.count)
    var current = [Int](repeating: 0, count: rhs.count + 1)
    for (i, left) in lhs.enumerated() {
      current[0] = i + 1
      for (j, right) in rhs.enumerated() {
        let substitution = previous[j] + (left == right ? 0 : 1)
        current[j + 1] = min(previous[j + 1] + 1, current[j] + 1, substitution)
      }
      swap(&previous, &current)
    }
    return previous[rhs.count]
  }
}
