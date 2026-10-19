//! The printer against what it imitates: Foundation's measured spellings
//! of doubles.

mod common;

use steno_bridge::json::{to_canonical_string, to_compact_string};

/// Every line of `tests/fixtures/foundation-doubles.txt`: both styles write
/// the string Foundation wrote, which reads back to the same bits. Ties
/// round to the even digit, as Foundation does, unlike Rust's `Display`.
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
    assert!(lines > 450, "the corpus has {lines} values");
}
