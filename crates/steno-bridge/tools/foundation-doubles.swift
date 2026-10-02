// Measures how Foundation's JSONEncoder spells doubles, so the Rust printer
// in `src/json.rs` (`write_number`) can be checked against the real thing.
//
// Run on a Mac (Swift 6.4, macOS 26 measured the committed corpus):
//
//     swift crates/steno-bridge/tools/foundation-doubles.swift [out.txt]
//
// Writes one line per value, `<bit pattern as 16 hex digits> <string>`, the
// string being exactly what `JSONEncoder` with `StenoJSON`'s options
// (`.sortedKeys`, `.prettyPrinted`, `.withoutEscapingSlashes`) emits for the
// value. The sample mixes random doubles across the whole exponent range,
// the special values, twenty neighbours either side of every boundary the
// printer's rules name, random bit patterns, integral doubles and the
// `.25`/`.75` ties between 2^49 and 2^51 (`write_tie_break`). A trimmed copy
// is committed as `tests/fixtures/foundation-doubles.txt` and asserted by
// `tests/json.rs`; the trimming is described in that file's header.

import Foundation

struct Wrapper: Encodable { let v: Double }

func encoder() -> JSONEncoder {
  let e = JSONEncoder()
  e.outputFormatting = [.sortedKeys, .prettyPrinted, .withoutEscapingSlashes]
  return e
}

var rng = SystemRandomNumberGenerator()
var values: [Double] = []
// 2000 random doubles: uniform exponent in -320..308, random mantissa, random sign
for _ in 0..<2000 {
  let exp = Int.random(in: -320...308, using: &rng)
  let mant = Double.random(in: 1.0..<10.0, using: &rng)
  var d = mant * pow(10.0, Double(exp))
  if exp < -307 { // subnormal territory: build from random bits instead
    let bits = UInt64.random(in: 1..<(1 << 52), using: &rng)
    d = Double(bitPattern: bits)
  }
  if Bool.random(using: &rng) { d = -d }
  if d.isFinite && d != 0 { values.append(d) }
}
// special values
values += [0.0, -0.0, 1.0, -1.0, Double.greatestFiniteMagnitude, -Double.greatestFiniteMagnitude,
  Double.leastNonzeroMagnitude, Double.leastNormalMagnitude, Double.ulpOfOne, Double.pi]
// boundaries
for base in [1e-4, 1e-5, 1e15, 1e16, 9007199254740992.0, 1e21, 1e-3] {
  var d = base
  for _ in 0..<20 { d = d.nextDown; values.append(d) }
  d = base
  for _ in 0..<20 { values.append(d); d = d.nextUp }
  values.append(-base)
}
// random bit patterns across the whole finite range
for _ in 0..<500 {
  let bits = UInt64.random(in: 0...UInt64.max, using: &rng)
  let d = Double(bitPattern: bits)
  if d.isFinite { values.append(d) }
}
// integers up to 2^53 and above, random
for _ in 0..<200 {
  let i = Int64.random(in: -(1 << 60)...(1 << 60), using: &rng)
  values.append(Double(i))
}
// the ties: 2^49 <= |d| < 2^51 with a fraction of .25 or .75, where two
// one-digit decimals both round-trip and the printer has to pick one
for _ in 0..<200 {
  let whole = UInt64.random(in: (1 << 49)..<(1 << 51), using: &rng)
  var d = Double(whole) + (Bool.random(using: &rng) ? 0.25 : 0.75)
  if Bool.random(using: &rng) { d = -d }
  values.append(d)
}

let enc = encoder()
var out = ""
for v in values {
  let data = try enc.encode(Wrapper(v: v))
  var s = String(decoding: data, as: UTF8.self)
  // {\n  "v" : X\n}
  s = s.replacingOccurrences(of: "{\n  \"v\" : ", with: "").replacingOccurrences(of: "\n}", with: "")
  out += String(format: "%016llx", v.bitPattern) + " " + s + "\n"
}
let path = CommandLine.arguments.count > 1 ? CommandLine.arguments[1] : "floats.txt"
try out.write(toFile: path, atomically: true, encoding: .utf8)
print("\(values.count) values written to \(path)")
