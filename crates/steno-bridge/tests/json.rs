//! The printer against what it imitates and against its own reader:
//! Foundation's measured spellings of doubles, and a round trip through
//! `serde_json` for values of every shape.

mod common;

use serde_json::{Map, Value};
use steno_bridge::json::{to_canonical_string, to_compact_string};

/// Every line of `tests/fixtures/foundation-doubles.txt` (its header says
/// how it was measured): both styles write the string Foundation wrote, and
/// the string reads back to the same bits.
#[test]
fn foundation_corpus() {
    let corpus = common::crate_fixture("foundation-doubles.txt");
    let mut lines = 0;
    for line in corpus.lines().filter(|line| !line.starts_with('#')) {
        let (bits, expected) = line.split_once(' ').expect("`<bits> <string>`");
        let value = f64::from_bits(u64::from_str_radix(bits, 16).expect("16 hex digits"));
        assert_eq!(to_compact_string(&value).unwrap(), expected, "bits {bits}");
        assert_eq!(
            to_canonical_string(&value).unwrap(),
            expected,
            "bits {bits}"
        );
        let back: f64 = serde_json::from_str(expected).unwrap();
        assert_eq!(back.to_bits(), value.to_bits(), "bits {bits} reads back");
        lines += 1;
    }
    assert!(lines > 200, "the corpus has {lines} values");
}

/// `parse(print(v)) == v` and `print(parse(print(v))) == print(v)` for
/// 20,000 values of every shape, in both styles. The seed is fixed so a
/// failure reproduces; the generator is a xorshift so the test needs no
/// crate.
#[test]
fn printing_round_trips_through_serde_json() {
    type Print = fn(&Value) -> Result<String, serde_json::Error>;
    let styles: [(&str, Print); 2] = [
        ("canonical", to_canonical_string::<Value>),
        ("compact", to_compact_string::<Value>),
    ];
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    for _ in 0..20_000 {
        let value = rng.value(0);
        for (style, print) in styles {
            let printed = print(&value).unwrap();
            let parsed: Value = serde_json::from_str(&printed)
                .unwrap_or_else(|e| panic!("{style} output does not parse: {e}\n{printed}"));
            assert!(
                same_value(&parsed, &value),
                "{style} round trip\n value: {value}\nparsed: {parsed}\nprinted: {printed}"
            );
            assert_eq!(print(&parsed).unwrap(), printed, "{style} is idempotent");
        }
    }
}

/// Equality that reads `1200` and `1200.0` as the same number: the printer
/// writes an integral double without a fraction, so it reads back as an
/// integer. Doubles compare by bits, so `-0` stays distinct from `0`.
fn same_value(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Number(l), Value::Number(r)) => match (l.as_f64(), r.as_f64()) {
            (Some(l), Some(r)) => l.to_bits() == r.to_bits(),
            _ => l == r,
        },
        (Value::Array(l), Value::Array(r)) => {
            l.len() == r.len() && l.iter().zip(r).all(|(l, r)| same_value(l, r))
        }
        (Value::Object(l), Value::Object(r)) => {
            l.len() == r.len()
                && l.iter()
                    .all(|(key, l)| r.get(key).is_some_and(|r| same_value(l, r)))
        }
        _ => left == right,
    }
}

/// xorshift64.
struct Rng(u64);

#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_wrap,
    clippy::cast_possible_truncation
)]
impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    /// A finite double of mixed magnitude: random bits, a random decade, an
    /// integral double, a `.25`/`.75` tie, or a few ulps around a boundary
    /// the printer names.
    fn double(&mut self) -> f64 {
        match self.below(5) {
            0 => loop {
                let d = f64::from_bits(self.next());
                if d.is_finite() {
                    return d;
                }
            },
            1 => {
                let exponent = self.below(629) as i32 - 320;
                let mantissa = 1.0 + (self.next() >> 11) as f64 / (1u64 << 53) as f64 * 9.0;
                let d = mantissa * 10f64.powi(exponent);
                if d.is_finite() && d != 0.0 { d } else { 1.5 }
            }
            2 => (self.next() >> 10) as f64 - (1u64 << 53) as f64,
            3 => {
                let whole = (1u64 << 49) + self.below((1 << 51) - (1 << 49));
                let d = whole as f64 + if self.below(2) == 0 { 0.25 } else { 0.75 };
                if self.below(2) == 0 { -d } else { d }
            }
            _ => {
                const BOUNDARIES: [f64; 9] = [
                    1e-4,
                    1e-5,
                    1e15,
                    1e16,
                    9_007_199_254_740_992.0,
                    562_949_953_421_312.0,
                    2_251_799_813_685_248.0,
                    1e21,
                    0.0,
                ];
                let mut d = BOUNDARIES[self.below(9) as usize];
                for _ in 0..self.below(5) {
                    let bits = d.to_bits();
                    d = f64::from_bits(if self.below(2) == 0 {
                        bits.wrapping_add(1)
                    } else {
                        bits.wrapping_sub(1)
                    });
                }
                if !d.is_finite() {
                    d = 0.0;
                }
                if self.below(2) == 0 { -d } else { d }
            }
        }
    }

    /// Up to five characters: C0 controls, the characters the escaper treats
    /// specially, non-ASCII, and plain printable ASCII.
    fn string(&mut self) -> String {
        (0..self.below(6))
            .map(|_| match self.below(6) {
                0 => char::from_u32(self.below(0x20) as u32).unwrap(),
                1 => ['"', '\\', '/', '\u{2028}', 'é', '😀'][self.below(6) as usize],
                _ => char::from_u32(0x20 + self.below(0x5f) as u32).unwrap(),
            })
            .collect()
    }

    fn value(&mut self, depth: u32) -> Value {
        match self.below(if depth > 4 { 5 } else { 7 }) {
            0 => Value::Null,
            1 => Value::Bool(self.below(2) == 0),
            2 => Value::from(self.next() as i64 >> self.below(64)),
            3 => Value::from(self.double()),
            4 => Value::String(self.string()),
            5 => Value::Array((0..self.below(4)).map(|_| self.value(depth + 1)).collect()),
            _ => {
                let mut map = Map::new();
                for _ in 0..self.below(4) {
                    map.insert(self.string(), self.value(depth + 1));
                }
                Value::Object(map)
            }
        }
    }
}
