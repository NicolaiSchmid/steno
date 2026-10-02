// Measures how Foundation's JSONEncoder spells doubles, so the Rust printer
// in `src/json.rs` (`write_number`) can be checked against the real thing.
//
// Run on a Mac (Swift 6.4, macOS 26 measured the committed corpus):
//
//     swift crates/steno-bridge/tools/foundation-doubles.swift [out.txt]
//
// Writes a `# <sampler>` line before each sampler's values, then one line per
// value, `<bit pattern as 16 hex digits> <string>`, the string being exactly
// what `JSONEncoder` with `StenoJSON`'s options (`.sortedKeys`,
// `.prettyPrinted`, `.withoutEscapingSlashes`) emits for the value. The
// samplers, in order:
//
//   random decades       random doubles across the whole exponent range
//   special values       zeros, ones, the extremes, pi
//   boundaries           twenty neighbours either side of every boundary the
//                        printer's rules name, and the negative base
//   random bits          random bit patterns across the whole finite range
//   integral doubles     integers either side of 2^53
//   one-digit ties       2^49 <= |d| < 2^51 with a `.25` or `.75` fraction: two
//                        one-digit decimals round-trip and the printer picks one
//   tie bands            exponents 2^-20..2^60 with the low t mantissa bits
//                        cleared, so the exact expansion is short and ties
//                        between two shortest candidates appear at every
//                        digit count (where Rust's `Display` takes the upper
//                        candidate and Foundation the even one)
//   multi-digit ties     2^(52-k) <= whole < 2^(54-k) plus odd/2^(d+1): a
//                        fraction that is exactly half way at d digits
//   odd dyadics          M·2^e with M odd, e in -60...-1, in the plain range
//   sub-1 ties           the same below 1, where the plain form has leading
//                        zeros
//
// A trimmed copy is committed as `tests/fixtures/foundation-doubles.txt` and
// asserted by `tests/json.rs`; the trimming is described in that file's header.

import Foundation

struct Wrapper: Encodable { let v: Double }

func encoder() -> JSONEncoder {
  let e = JSONEncoder()
  e.outputFormatting = [.sortedKeys, .prettyPrinted, .withoutEscapingSlashes]
  return e
}

var rng = SystemRandomNumberGenerator()
var sections: [(String, [Double])] = []

func section(_ name: String, _ fill: (inout [Double]) -> Void) {
  var values: [Double] = []
  fill(&values)
  sections.append((name, values))
}

func randomSign(_ d: Double) -> Double { Bool.random(using: &rng) ? -d : d }

section("random decades") { values in
  // 2000 random doubles: uniform exponent in -320..308, random mantissa, random sign
  for _ in 0..<2000 {
    let exp = Int.random(in: -320...308, using: &rng)
    let mant = Double.random(in: 1.0..<10.0, using: &rng)
    var d = mant * pow(10.0, Double(exp))
    if exp < -307 { // subnormal territory: build from random bits instead
      d = Double(bitPattern: UInt64.random(in: 1..<(1 << 52), using: &rng))
    }
    d = randomSign(d)
    if d.isFinite && d != 0 { values.append(d) }
  }
}

section("special values") { values in
  values += [0.0, -0.0, 1.0, -1.0, Double.greatestFiniteMagnitude, -Double.greatestFiniteMagnitude,
    Double.leastNonzeroMagnitude, Double.leastNormalMagnitude, Double.ulpOfOne, Double.pi]
}

section("boundaries") { values in
  for base in [1e-4, 1e-5, 1e15, 1e16, 9007199254740992.0, 1e21, 1e-3] {
    var d = base
    for _ in 0..<20 { d = d.nextDown; values.append(d) }
    d = base
    for _ in 0..<20 { values.append(d); d = d.nextUp }
    values.append(-base)
  }
}

section("random bits") { values in
  for _ in 0..<500 {
    let d = Double(bitPattern: UInt64.random(in: 0...UInt64.max, using: &rng))
    if d.isFinite { values.append(d) }
  }
}

section("integral doubles") { values in
  for _ in 0..<200 {
    values.append(Double(Int64.random(in: -(1 << 60)...(1 << 60), using: &rng)))
  }
}

section("one-digit ties") { values in
  for _ in 0..<200 {
    let whole = UInt64.random(in: (1 << 49)..<(1 << 51), using: &rng)
    values.append(randomSign(Double(whole) + (Bool.random(using: &rng) ? 0.25 : 0.75)))
  }
}

section("tie bands") { values in
  for _ in 0..<5000 {
    let exp = UInt64.random(in: (1023 - 20)...(1023 + 60), using: &rng)
    let t = UInt64.random(in: 0...52, using: &rng)
    var mant = UInt64.random(in: 0..<(1 << 52), using: &rng)
    if t > 0 { mant &= ~((1 << t) - 1) }
    let bits = (exp << 52) | mant | (Bool.random(using: &rng) ? (1 << 63) : 0)
    values.append(Double(bitPattern: bits))
  }
}

section("multi-digit ties") { values in
  for d in 1...8 {
    let denominator = UInt64(1) << UInt64(d + 1)
    for k in max(2 * d - 2, 1)...(2 * d + 4) {
      guard 52 - k >= 1 else { continue }
      let low = UInt64(1) << UInt64(52 - k)
      for _ in 0..<40 {
        let whole = UInt64.random(in: low..<(low << 1), using: &rng)
        let odd = UInt64.random(in: 0..<(denominator / 2), using: &rng) * 2 + 1
        values.append(randomSign(Double(whole) + Double(odd) / Double(denominator)))
      }
    }
  }
}

section("odd dyadics") { values in
  for _ in 0..<3000 {
    let e = Int.random(in: -60...(-1), using: &rng)
    let bits = Int.random(in: 1...53, using: &rng)
    var m = UInt64.random(in: 0..<(1 << UInt64(bits)), using: &rng) | 1
    if bits == 53 { m |= 1 << 52 }
    let d = randomSign(Double(m) * pow(2.0, Double(e)))
    if d.isFinite && d != 0 { values.append(d) }
  }
}

section("sub-1 ties") { values in
  for _ in 0..<2000 {
    let e = Int.random(in: -60...(-1), using: &rng)
    let bits = Int.random(in: 1...min(53, -e), using: &rng)
    let m = UInt64.random(in: 0..<(1 << UInt64(bits)), using: &rng) | 1
    values.append(randomSign(Double(m) * pow(2.0, Double(e))))
  }
}

let enc = encoder()
var out = ""
var count = 0
for (name, values) in sections {
  out += "# \(name)\n"
  for v in values {
    let data = try enc.encode(Wrapper(v: v))
    var s = String(decoding: data, as: UTF8.self)
    // {\n  "v" : X\n}
    s = s.replacingOccurrences(of: "{\n  \"v\" : ", with: "").replacingOccurrences(of: "\n}", with: "")
    out += String(format: "%016llx", v.bitPattern) + " " + s + "\n"
    count += 1
  }
}
let path = CommandLine.arguments.count > 1 ? CommandLine.arguments[1] : "floats.txt"
try out.write(toFile: path, atomically: true, encoding: .utf8)
print("\(count) values written to \(path)")
