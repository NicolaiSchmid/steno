//! The parity scorer: word error rate of one transcript against another
//! and the agreement of word start times where the words match. The
//! normalisation is the spike's (`spikes/coreml-rs/tools/wer.py`): lower
//! case, every character that is not a letter, digit or underscore
//! becomes a space, whitespace collapses. The 8.7 % of the spike report
//! was measured this way, so the numbers stay comparable.

/// Lower-cased words with punctuation stripped.
#[must_use]
pub fn normalize(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .filter(|word| !word.is_empty())
        .map(str::to_owned)
        .collect()
}

/// One step of the word alignment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edit {
    /// `reference[i]` equals `hypothesis[j]`.
    Match(usize, usize),
    /// `reference[i]` was replaced by `hypothesis[j]`.
    Substitute(usize, usize),
    /// `reference[i]` is missing from the hypothesis.
    Delete(usize),
    /// `hypothesis[j]` has no counterpart.
    Insert(usize),
}

/// Levenshtein alignment over words, matches first on ties so timing
/// comparisons pair as many words as the distance allows.
#[must_use]
pub fn align<T: PartialEq>(reference: &[T], hypothesis: &[T]) -> Vec<Edit> {
    let (n, m) = (reference.len(), hypothesis.len());
    let mut dp = vec![vec![0usize; m + 1]; n + 1];
    for (i, row) in dp.iter_mut().enumerate() {
        row[0] = i;
    }
    for (j, cell) in dp[0].iter_mut().enumerate() {
        *cell = j;
    }
    for i in 1..=n {
        for j in 1..=m {
            let substitution =
                dp[i - 1][j - 1] + usize::from(reference[i - 1] != hypothesis[j - 1]);
            dp[i][j] = substitution.min(dp[i - 1][j] + 1).min(dp[i][j - 1] + 1);
        }
    }
    let mut edits = Vec::new();
    let (mut i, mut j) = (n, m);
    while i > 0 || j > 0 {
        if i > 0 && j > 0 {
            let same = reference[i - 1] == hypothesis[j - 1];
            if dp[i][j] == dp[i - 1][j - 1] + usize::from(!same) {
                edits.push(if same {
                    Edit::Match(i - 1, j - 1)
                } else {
                    Edit::Substitute(i - 1, j - 1)
                });
                i -= 1;
                j -= 1;
                continue;
            }
        }
        if i > 0 && dp[i][j] == dp[i - 1][j] + 1 {
            edits.push(Edit::Delete(i - 1));
            i -= 1;
        } else {
            edits.push(Edit::Insert(j - 1));
            j -= 1;
        }
    }
    edits.reverse();
    edits
}

/// Edits and reference length of `hypothesis` against `reference`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WordErrors {
    pub edits: usize,
    pub reference_words: usize,
    pub hypothesis_words: usize,
}

impl WordErrors {
    /// Edits over reference words; `0` for two empty texts, `1` for an
    /// empty reference against a non-empty hypothesis.
    #[must_use]
    pub fn rate(&self) -> f64 {
        if self.reference_words == 0 {
            return if self.hypothesis_words == 0 { 0.0 } else { 1.0 };
        }
        // Word counts are far below 2^53.
        #[allow(clippy::cast_precision_loss)]
        let rate = self.edits as f64 / self.reference_words as f64;
        rate
    }

    /// Add another file's counts.
    pub fn add(&mut self, other: WordErrors) {
        self.edits += other.edits;
        self.reference_words += other.reference_words;
        self.hypothesis_words += other.hypothesis_words;
    }
}

/// Word error rate of `hypothesis` against `reference`, both normalised.
#[must_use]
pub fn word_errors(reference: &str, hypothesis: &str) -> WordErrors {
    let reference = normalize(reference);
    let hypothesis = normalize(hypothesis);
    let edits = align(&reference, &hypothesis)
        .iter()
        .filter(|edit| !matches!(edit, Edit::Match(..)))
        .count();
    WordErrors {
        edits,
        reference_words: reference.len(),
        hypothesis_words: hypothesis.len(),
    }
}

/// A word with its start time, the unit the timing comparison aligns.
#[derive(Debug, Clone, PartialEq)]
pub struct TimedText {
    pub word: String,
    pub start: f64,
}

/// How the start times of matched words compare.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct TimingAgreement {
    /// Words the alignment matched.
    pub matched: usize,
    /// Matched words whose starts differ by at most 10 ms.
    pub within_10ms: usize,
    /// Matched words whose starts differ by at most 100 ms.
    pub within_100ms: usize,
    /// Mean absolute start difference over matched words, in seconds.
    pub mean_abs_seconds: f64,
}

impl TimingAgreement {
    /// Fraction of matched words within 10 ms, `1` when nothing matched.
    #[must_use]
    pub fn fraction_within_10ms(&self) -> f64 {
        if self.matched == 0 {
            return 1.0;
        }
        #[allow(clippy::cast_precision_loss)]
        let fraction = self.within_10ms as f64 / self.matched as f64;
        fraction
    }
}

/// Align the words (one normalised key per word; words that normalise to
/// nothing are skipped) and compare start times where they match.
#[must_use]
pub fn timing_agreement(reference: &[TimedText], hypothesis: &[TimedText]) -> TimingAgreement {
    fn keyed(words: &[TimedText]) -> Vec<(String, f64)> {
        words
            .iter()
            .filter_map(|word| {
                let key = normalize(&word.word).join("");
                (!key.is_empty()).then_some((key, word.start))
            })
            .collect()
    }
    let reference = keyed(reference);
    let hypothesis = keyed(hypothesis);
    let reference_keys: Vec<&str> = reference.iter().map(|(key, _)| key.as_str()).collect();
    let hypothesis_keys: Vec<&str> = hypothesis.iter().map(|(key, _)| key.as_str()).collect();
    let mut agreement = TimingAgreement::default();
    let mut total_abs = 0.0;
    for edit in align(&reference_keys, &hypothesis_keys) {
        let Edit::Match(i, j) = edit else { continue };
        let delta = (reference[i].1 - hypothesis[j].1).abs();
        agreement.matched += 1;
        if delta <= 0.010 + 1e-9 {
            agreement.within_10ms += 1;
        }
        if delta <= 0.100 + 1e-9 {
            agreement.within_100ms += 1;
        }
        total_abs += delta;
    }
    if agreement.matched > 0 {
        #[allow(clippy::cast_precision_loss)]
        let matched = agreement.matched as f64;
        agreement.mean_abs_seconds = total_abs / matched;
    }
    agreement
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalisation_matches_the_spike_script() {
        assert_eq!(
            normalize("Hallo, Welt! Co-Pilot's 30%"),
            vec!["hallo", "welt", "co", "pilot", "s", "30"]
        );
        assert_eq!(normalize("  Ärger_über  "), vec!["ärger_über"]);
        assert_eq!(normalize("..."), Vec::<String>::new());
    }

    #[test]
    fn word_errors_count_substitutions_insertions_and_deletions() {
        let errors = word_errors("the cat sat on the mat", "the cat sits on mat");
        assert_eq!(errors.edits, 2);
        assert_eq!(errors.reference_words, 6);
        assert!((errors.rate() - 2.0 / 6.0).abs() < 1e-12);
        assert_eq!(word_errors("", "").rate(), 0.0);
        assert_eq!(word_errors("", "x").rate(), 1.0);
        assert_eq!(word_errors("a b", "").edits, 2);
        let mut total = WordErrors::default();
        total.add(errors);
        total.add(word_errors("a", "b"));
        assert_eq!(total.edits, 3);
        assert_eq!(total.reference_words, 7);
    }

    #[test]
    fn alignment_pairs_matches_and_names_the_edits() {
        let edits = align(&["a", "b", "c"], &["a", "x", "c", "d"]);
        assert_eq!(
            edits,
            vec![
                Edit::Match(0, 0),
                Edit::Substitute(1, 1),
                Edit::Match(2, 2),
                Edit::Insert(3)
            ]
        );
        let edits = align(&["a", "b"], &["b"]);
        assert_eq!(edits, vec![Edit::Delete(0), Edit::Match(1, 0)]);
    }

    #[test]
    fn timing_agreement_compares_matched_starts() {
        let timed = |word: &str, start: f64| TimedText {
            word: word.to_owned(),
            start,
        };
        let reference = [
            timed("Hallo", 0.0),
            timed("Welt.", 0.5),
            timed("!", 0.6),
            timed("ja", 1.0),
        ];
        let hypothesis = [timed("hallo", 0.0), timed("welt", 0.55), timed("nein", 1.0)];
        let agreement = timing_agreement(&reference, &hypothesis);
        assert_eq!(agreement.matched, 2);
        assert_eq!(agreement.within_10ms, 1);
        assert_eq!(agreement.within_100ms, 2);
        assert!((agreement.mean_abs_seconds - 0.025).abs() < 1e-9);
        assert!((agreement.fraction_within_10ms() - 0.5).abs() < 1e-12);
        assert_eq!(timing_agreement(&[], &[]).fraction_within_10ms(), 1.0);
    }
}
