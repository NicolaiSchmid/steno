//! Joining the windows' token streams: FluidAudio's `ChunkProcessor`
//! merge (0.17.4) and the `SequenceMatcher` it calls, then the seam-word
//! collapse and the splice rules of the seam-gap repair. Pure functions
//! over [`Token`] lists; the repair's re-decode lives in the pipeline.
//!
//! The order of a merged stream is the text order; frame timestamps are
//! metadata clamped non-decreasing afterwards (issue #825). Nothing here
//! sorts by timestamp.

use crate::Token;
use crate::chunking::{FRAME_SECONDS, OVERLAP_FRAMES, OVERLAP_SECONDS, frame_seconds};
use crate::vocab::{Vocab, is_punctuation, strip_word_boundary};

/// A token with its index in the window it came from and its start time
/// (`ChunkProcessor.IndexedToken`).
#[derive(Debug, Clone, Copy)]
struct Indexed {
    index: usize,
    token: Token,
    start: f64,
}

/// `(left, right)` index pairs into two sequences.
type Pairs = Vec<(usize, usize)>;

/// The longest run of consecutive matches between `left` and `right`
/// (`SequenceMatcher.findContiguousMatches`); the first run of the maximal
/// length wins.
fn find_contiguous_matches<T>(left: &[T], right: &[T], matches: impl Fn(&T, &T) -> bool) -> Pairs {
    let mut best: Pairs = Vec::new();
    for i in 0..left.len() {
        for j in 0..right.len() {
            if !matches(&left[i], &right[j]) {
                continue;
            }
            let mut current: Pairs = Vec::new();
            let (mut k, mut l) = (i, j);
            while k < left.len() && l < right.len() && matches(&left[k], &right[l]) {
                current.push((k, l));
                k += 1;
                l += 1;
            }
            if current.len() > best.len() {
                best = current;
            }
        }
    }
    best
}

/// The longest common subsequence as single-element matches
/// (`SequenceMatcher.findLongestCommonSubsequence`): the DP table and the
/// backtrack from the end, preferring the right-hand step on ties, exactly
/// as the Swift does, so the same pairs come out.
fn find_lcs<T>(left: &[T], right: &[T], matches: impl Fn(&T, &T) -> bool) -> Pairs {
    let (n, m) = (left.len(), right.len());
    let mut dp = vec![vec![0u32; m + 1]; n + 1];
    for i in 1..=n {
        for j in 1..=m {
            dp[i][j] = if matches(&left[i - 1], &right[j - 1]) {
                dp[i - 1][j - 1] + 1
            } else {
                dp[i - 1][j].max(dp[i][j - 1])
            };
        }
    }
    let mut pairs: Pairs = Vec::new();
    let (mut i, mut j) = (n, m);
    while i > 0 && j > 0 {
        if matches(&left[i - 1], &right[j - 1]) {
            pairs.push((i - 1, j - 1));
            i -= 1;
            j -= 1;
        } else if dp[i - 1][j] > dp[i][j - 1] {
            i -= 1;
        } else {
            j -= 1;
        }
    }
    pairs.reverse();
    pairs
}

/// Merge the next window's tokens onto the stream so far
/// (`ChunkProcessor.mergeChunks`): tokens inside the 2 s overlap are
/// aligned by id (case variants equal) within 1 s, first as the longest
/// contiguous run when it covers half the left overlap, else as the LCS;
/// the gaps between anchors take the longer side; the seam word is
/// re-spliced at a word boundary. Without two tokens on each side, or
/// without any match, the streams are cut at the time midpoint.
#[must_use]
pub fn merge_chunks(left: &[Token], right: &[Token], vocab: &Vocab) -> Vec<Token> {
    if left.is_empty() {
        return right.to_vec();
    }
    if right.is_empty() {
        return left.to_vec();
    }
    let start = |token: &Token| frame_seconds(token.frame);
    let left_end = start(&left[left.len() - 1]) + FRAME_SECONDS;
    let right_start = start(&right[0]);
    if left_end <= right_start {
        return [left, right].concat();
    }

    let indexed = |(index, token): (usize, &Token)| Indexed {
        index,
        token: *token,
        start: start(token),
    };
    let overlap_left: Vec<Indexed> = left
        .iter()
        .enumerate()
        .filter(|(_, token)| start(token) + FRAME_SECONDS > right_start - OVERLAP_SECONDS)
        .map(indexed)
        .collect();
    let overlap_right: Vec<Indexed> = right
        .iter()
        .enumerate()
        .filter(|(_, token)| start(token) < left_end + OVERLAP_SECONDS)
        .map(indexed)
        .collect();
    if overlap_left.len() < 2 || overlap_right.len() < 2 {
        return merge_by_midpoint(left, right, left_end, right_start, vocab);
    }

    let minimum_pairs = (overlap_left.len() / 2).max(1);
    let half_overlap = OVERLAP_SECONDS / 2.0;
    let matches = |l: &Indexed, r: &Indexed| {
        vocab.ids_match(l.token.id, r.token.id) && (l.start - r.start).abs() < half_overlap
    };

    let contiguous = find_contiguous_matches(&overlap_left, &overlap_right, matches);
    if contiguous.len() >= minimum_pairs {
        return merge_using_matches(
            &contiguous,
            &overlap_left,
            &overlap_right,
            left,
            right,
            vocab,
        );
    }
    let lcs = find_lcs(&overlap_left, &overlap_right, matches);
    if lcs.is_empty() {
        return merge_by_midpoint(left, right, left_end, right_start, vocab);
    }
    merge_using_matches(&lcs, &overlap_left, &overlap_right, left, right, vocab)
}

/// Stitch `left` and `right` through anchor pairs
/// (`ChunkProcessor.mergeUsingMatches`), one pair per matched token.
fn merge_using_matches(
    pairs: &[(usize, usize)],
    overlap_left: &[Indexed],
    overlap_right: &[Indexed],
    left: &[Token],
    right: &[Token],
    vocab: &Vocab,
) -> Vec<Token> {
    let left_indices: Vec<usize> = pairs.iter().map(|(l, _)| overlap_left[*l].index).collect();
    let right_indices: Vec<usize> = pairs.iter().map(|(_, r)| overlap_right[*r].index).collect();
    let mut result: Vec<Token> = Vec::with_capacity(left.len() + right.len());

    if let Some(&first_left) = left_indices.first()
        && first_left > 0
    {
        result.extend_from_slice(&left[..first_left]);
    }

    for idx in 0..pairs.len() {
        let left_index = left_indices[idx];
        let right_index = right_indices[idx];
        result.push(left[left_index]);
        if idx + 1 >= pairs.len() {
            continue;
        }
        let next_left = left_indices[idx + 1];
        let next_right = right_indices[idx + 1];
        let gap_left = left.get(left_index + 1..next_left).unwrap_or(&[]);
        let gap_right = right.get(right_index + 1..next_right).unwrap_or(&[]);
        if gap_right.len() > gap_left.len() {
            result.extend_from_slice(gap_right);
        } else {
            result.extend_from_slice(gap_left);
        }
    }

    let Some(&last_right) = right_indices.last() else {
        return result;
    };
    if last_right + 1 >= right.len() {
        return result;
    }
    let tail = &right[last_right + 1..];
    let first_tail = tail[0];
    if vocab.is_splice_safe(first_tail.id) {
        result.extend_from_slice(tail);
        return result;
    }
    // Issue #683: the splice lands mid-word; re-splice at a word boundary
    // so exactly one window segments the seam word.
    if let Some(word_start) = word_initial_index(right, last_right, vocab)
        && pop_seam_word(&mut result, vocab)
    {
        // Right heard the seam word from its start: adopt its segmentation.
        result.extend_from_slice(&right[word_start..]);
        return result;
    }
    // Right begins mid-word: left owns the seam word; resume right at its
    // next word-initial piece instead of gluing.
    if let Some(&last_left) = left_indices.last() {
        result.extend(
            left[last_left + 1..]
                .iter()
                .take_while(|token| !vocab.is_splice_safe(token.id)),
        );
    }
    match tail.iter().position(|token| vocab.is_splice_safe(token.id)) {
        Some(resume) => result.extend_from_slice(&tail[resume..]),
        // No word-initial piece in the tail: keep it verbatim, a possible
        // glue beats dropping a word (FluidAudio PR #759).
        None => result.extend_from_slice(tail),
    }
    result
}

/// Index of the word-initial (or punctuation) piece starting the word that
/// contains `anchor`, or `None` when the stream begins mid-word
/// (`ChunkProcessor.wordInitialIndex`).
fn word_initial_index(stream: &[Token], anchor: usize, vocab: &Vocab) -> Option<usize> {
    stream[..=anchor]
        .iter()
        .rposition(|token| vocab.is_splice_safe(token.id))
}

/// Remove the trailing seam word from `result`; `false` and untouched when
/// `result` has no word-initial piece (`ChunkProcessor.popSeamWord`).
fn pop_seam_word(result: &mut Vec<Token>, vocab: &Vocab) -> bool {
    let Some(cursor) = result
        .iter()
        .rposition(|token| vocab.is_splice_safe(token.id))
    else {
        return false;
    };
    result.truncate(cursor);
    true
}

/// Cut both streams at the time midpoint of the overlap, letting left
/// finish its word and right resume at a word start
/// (`ChunkProcessor.mergeByMidpoint`).
fn merge_by_midpoint(
    left: &[Token],
    right: &[Token],
    left_end: f64,
    right_start: f64,
    vocab: &Vocab,
) -> Vec<Token> {
    let cutoff = f64::midpoint(left_end, right_start);
    let at_or_after = |token: &Token| frame_seconds(token.frame) >= cutoff;
    let splice_safe = |token: &Token| vocab.is_splice_safe(token.id);
    let mut left_end_index = left.iter().position(at_or_after).unwrap_or(left.len());
    let mut right_start_index = right.iter().position(at_or_after).unwrap_or(right.len());
    if left_end_index > 0 {
        left_end_index = left[left_end_index..]
            .iter()
            .position(splice_safe)
            .map_or(left.len(), |offset| left_end_index + offset);
    }
    // Adopt the advanced cutoff only if a splice-safe token exists ahead;
    // otherwise the whole right window would be discarded (PR #759).
    if let Some(offset) = right[right_start_index..].iter().position(splice_safe) {
        right_start_index += offset;
    }
    [&left[..left_end_index], &right[right_start_index..]].concat()
}

/// Make timestamps non-decreasing without reordering the stream
/// (`ChunkProcessor.enforceMonotonicTimestamps`).
#[must_use]
pub fn enforce_monotonic(mut tokens: Vec<Token>) -> Vec<Token> {
    let mut last = 0usize;
    for token in &mut tokens {
        if token.frame < last {
            token.frame = last;
        } else {
            last = token.frame;
        }
    }
    tokens
}

/// Drop an adjacent case-only duplicate of a seam word ("…have Have a…")
/// left by a false sentence start, at word granularity, keeping the
/// lower-case copy (`ChunkProcessor.collapseSeamWordDuplicates`, issue
/// #706). Two words are a seam duplicate when their cores differ only in
/// case, the second starts with a letter, the first does not end a
/// sentence and they start within the overlap of each other.
#[must_use]
pub fn collapse_seam_word_duplicates(tokens: &[Token], vocab: &Vocab) -> Vec<Token> {
    struct Word {
        tokens: Vec<Token>,
        core: String,
        start_frame: usize,
        ends_sentence: bool,
    }
    if vocab.is_empty() || tokens.len() < 2 {
        return tokens.to_vec();
    }
    let mut groups: Vec<Vec<Token>> = Vec::new();
    for token in tokens {
        match groups.last_mut() {
            Some(group) if !vocab.starts_word(token.id) => group.push(*token),
            _ => groups.push(vec![*token]),
        }
    }
    let words: Vec<Word> = groups
        .into_iter()
        .map(|tokens| {
            let text: String = tokens
                .iter()
                .map(|token| strip_word_boundary(vocab.piece(token.id)))
                .collect();
            Word {
                core: text
                    .trim_matches(|c: char| is_punctuation(c) || c.is_whitespace())
                    .to_owned(),
                start_frame: tokens[0].frame,
                ends_sentence: text.chars().last().is_some_and(|c| ".?!:".contains(c)),
                tokens,
            }
        })
        .collect();

    let mut keep = vec![true; words.len()];
    let mut last_kept: Option<usize> = None;
    for index in 0..words.len() {
        let Some(previous_index) = last_kept else {
            last_kept = Some(index);
            continue;
        };
        let previous = &words[previous_index];
        let current = &words[index];
        let is_seam_duplicate = !previous.core.is_empty()
            && !current.core.is_empty()
            && previous.core != current.core
            && previous.core.to_lowercase() == current.core.to_lowercase()
            && current.core.chars().next().is_some_and(char::is_alphabetic)
            && !previous.ends_sentence
            && current.start_frame.saturating_sub(previous.start_frame) <= OVERLAP_FRAMES;
        if !is_seam_duplicate {
            last_kept = Some(index);
            continue;
        }
        // Keep the lower-cased copy; if neither is lower case keep the
        // earlier (left-context) one.
        if current.core == current.core.to_lowercase()
            && previous.core != previous.core.to_lowercase()
        {
            keep[previous_index] = false;
            last_kept = Some(index);
        } else {
            keep[index] = false;
        }
    }
    words
        .iter()
        .zip(&keep)
        .filter(|(_, keep)| **keep)
        .flat_map(|(word, _)| word.tokens.iter().copied())
        .collect()
}

/// The token of the word bordering a gap, walking past punctuation-only
/// pieces: `step` -1 walks left from the token before the gap, +1 right
/// from the token after it (`ChunkProcessor.wordNeighbor`).
#[must_use]
pub fn word_neighbor(stream: &[Token], from: usize, step: isize, vocab: &Vocab) -> Token {
    let mut index = from;
    loop {
        let next = index.checked_add_signed(step);
        match next {
            Some(next) if next < stream.len() && vocab.is_punctuation_only(stream[index].id) => {
                index = next;
            }
            _ => return stream[index],
        }
    }
}

/// Frames of tolerance when a probe re-hears a word bordering the gap.
const EDGE_TOLERANCE_FRAMES: usize = 6;

/// Filter a probe window's tokens to the spliceable run: strictly inside
/// the gap, starting on a word-initial piece, border words the probe
/// re-heard removed (`ChunkProcessor.spliceCandidate`).
#[must_use]
pub fn splice_candidate(
    window: &[Token],
    gap_start_frame: usize,
    gap_end_frame: usize,
    lead: Token,
    tail: Token,
    vocab: &Vocab,
) -> Vec<Token> {
    let mut candidate: Vec<Token> = window
        .iter()
        .filter(|token| token.frame > gap_start_frame && token.frame + 1 < gap_end_frame)
        .copied()
        .collect();
    let within = |a: usize, b: usize| a.abs_diff(b) <= EDGE_TOLERANCE_FRAMES;

    while candidate
        .first()
        .is_some_and(|first| !vocab.is_splice_safe(first.id))
    {
        candidate.remove(0);
    }
    while candidate
        .first()
        .is_some_and(|first| vocab.same_piece(first.id, lead.id) && within(first.frame, lead.frame))
    {
        candidate.remove(0);
    }
    while candidate
        .last()
        .is_some_and(|last| vocab.same_piece(last.id, tail.id) && within(tail.frame, last.frame))
    {
        candidate.pop();
    }
    // Removing an edge token can expose continuation pieces or orphaned
    // punctuation at the head: re-trim.
    while candidate
        .first()
        .is_some_and(|first| !vocab.is_splice_safe(first.id) || vocab.is_punctuation_only(first.id))
    {
        candidate.remove(0);
    }
    candidate
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vocab::tests::sample;

    fn token(id: usize, frame: usize) -> Token {
        Token {
            id,
            frame,
            confidence: 0.9,
            duration: 1,
        }
    }

    fn ids(tokens: &[Token]) -> Vec<usize> {
        tokens.iter().map(|t| t.id).collect()
    }

    #[test]
    fn contiguous_matches_take_the_longest_run() {
        let left = [1, 2, 3, 4, 9];
        let right = [7, 2, 3, 4, 1];
        let pairs = find_contiguous_matches(&left, &right, |a, b| a == b);
        assert_eq!(pairs, vec![(1, 1), (2, 2), (3, 3)]);
        assert_eq!(
            find_contiguous_matches(&left, &[8, 8], |a, b| a == b),
            Vec::new()
        );
    }

    #[test]
    fn lcs_backtracks_like_the_swift() {
        let left = [1, 2, 3, 4];
        let right = [2, 9, 4, 1];
        let pairs = find_lcs(&left, &right, |a, b| a == b);
        assert_eq!(pairs, vec![(1, 0), (3, 2)]);
        // Ties step the right index first: of `1` at two places, the later
        // occurrence in `right` is chosen.
        let pairs = find_lcs(&[1], &[1, 1], |a, b| a == b);
        assert_eq!(pairs, vec![(0, 1)]);
        assert_eq!(find_lcs(&[1, 2], &[3], |a, b| a == b), Vec::new());
    }

    #[test]
    fn lcs_anchors_match_only_within_half_the_overlap() {
        // Fewer contiguous pairs than `minimum_pairs` (2 here), so the LCS
        // decides; its tolerance is one second (`overlapSeconds / 2`).
        let vocab = sample();
        let left = vec![
            token(12, 100),
            token(13, 105),
            token(14, 110),
            token(1, 115),
        ];
        let near = vec![token(5, 108), token(1, 127)]; // 0.96 s apart
        let far = vec![token(5, 108), token(1, 128)]; // 1.04 s apart
        // Anchored: the seam token keeps the left copy's frame and the
        // right token before the anchor is dropped.
        let anchored = merge_chunks(&left, &near, &vocab);
        assert_eq!(ids(&anchored), vec![12, 13, 14, 1]);
        assert_eq!(anchored[3].frame, 115);
        // No anchor: the midpoint cut keeps the right copy instead.
        let cut = merge_chunks(&left, &far, &vocab);
        assert_eq!(ids(&cut), vec![12, 13, 14, 1]);
        assert_eq!(cut[3].frame, 128);
    }

    #[test]
    fn exactly_minimum_pairs_of_contiguous_matches_are_enough() {
        // Four overlap tokens on the left, so two contiguous pairs anchor
        // the merge; the LCS would find a third, non-contiguous pair and
        // arbitrate the gap differently.
        let vocab = sample();
        let left = vec![
            token(12, 100),
            token(13, 102),
            token(14, 104),
            token(1, 106),
        ];
        let right = vec![
            token(12, 100),
            token(5, 101),
            token(15, 102),
            token(14, 104),
            token(1, 106),
            token(13, 110),
        ];
        assert_eq!(
            find_lcs(&left, &right, |l, r| l.id == r.id),
            vec![(0, 0), (2, 3), (3, 4)]
        );
        let merged = merge_chunks(&left, &right, &vocab);
        assert_eq!(ids(&merged), vec![12, 13, 14, 1, 13]);
    }

    #[test]
    fn disjoint_streams_concatenate_and_empty_sides_pass_through() {
        let vocab = sample();
        let left = vec![token(12, 0), token(13, 2)];
        let right = vec![token(14, 10), token(15, 11)];
        assert_eq!(
            ids(&merge_chunks(&left, &right, &vocab)),
            vec![12, 13, 14, 15]
        );
        assert_eq!(ids(&merge_chunks(&[], &right, &vocab)), vec![14, 15]);
        assert_eq!(ids(&merge_chunks(&left, &[], &vocab)), vec![12, 13]);
    }

    #[test]
    fn overlapping_streams_merge_through_contiguous_anchors() {
        let vocab = sample();
        // left: hello world a | b c   (frames 0..)
        // right:      world a b c ? (same frames within 1 s)
        let left = vec![
            token(12, 100),
            token(13, 110),
            token(14, 120),
            token(15, 121),
        ];
        let right = vec![
            token(13, 111),
            token(14, 120),
            token(15, 121),
            token(16, 122),
            token(8, 123),
        ];
        let merged = merge_chunks(&left, &right, &vocab);
        assert_eq!(ids(&merged), vec![12, 13, 14, 15, 16, 8]);
    }

    #[test]
    fn a_mid_word_tail_is_respliced_at_the_word_start() {
        let vocab = sample();
        // Anchors on " hello"(12) and " a"(14); right's tail begins with the
        // continuation "c"(16) while right also holds the word start " a".
        let left = vec![token(12, 100), token(14, 110), token(15, 111)];
        let right = vec![
            token(12, 100),
            token(14, 110),
            token(16, 111),
            token(13, 115),
        ];
        let merged = merge_chunks(&left, &right, &vocab);
        // Left's "a b" is popped and right's "a c world" adopted.
        assert_eq!(ids(&merged), vec![12, 14, 16, 13]);
    }

    #[test]
    fn a_tail_without_a_word_start_is_kept_verbatim() {
        let vocab = sample();
        // Right begins mid-word (no splice-safe piece before the anchor on
        // "b"): left finishes its word, right's tail is only continuations.
        let left = vec![
            token(12, 100),
            token(14, 110),
            token(15, 111),
            token(16, 112),
        ];
        let right = vec![token(15, 111), token(16, 112), token(7, 113)];
        let merged = merge_chunks(&left, &right, &vocab);
        // Two overlap tokens on the left side count; contiguous run b,c
        // covers half, tail "s" is not splice safe, right has no word start
        // before the anchor -> left owns the word, tail kept verbatim.
        assert_eq!(ids(&merged), vec![12, 14, 15, 16, 7]);
    }

    #[test]
    fn midpoint_merge_cuts_at_word_boundaries() {
        let vocab = sample();
        // One token on the right: not enough overlap for matching.
        let left = vec![token(12, 100), token(14, 124), token(15, 125)];
        let right = vec![token(13, 126)];
        let merged = merge_chunks(&left, &right, &vocab);
        // Cutoff is (126*0.08 + 0.08 + 126*0.08)/2 = 10.12 s = frame 126.5;
        // left keeps everything before it (and finishes its word).
        assert_eq!(ids(&merged), vec![12, 14, 15, 13]);
        // Right beginning with continuation pieces resumes at its word start.
        let right = vec![token(16, 126), token(13, 130)];
        let left = vec![token(12, 100), token(14, 126), token(15, 126)];
        let merged = merge_chunks(&left, &right, &vocab);
        assert_eq!(ids(&merged), vec![12, 14, 15, 13]);
    }

    #[test]
    fn midpoint_merge_lets_left_finish_a_word_cut_by_the_cutoff() {
        let vocab = sample();
        // No token matches, so the LCS is empty and the midpoint decides.
        // left_end = 141 frames, right_start = 130: the cutoff is frame
        // 135.5, between "b"(134) and "c"(136) of " a b c".
        let left = vec![
            token(12, 100),
            token(14, 133),
            token(15, 134),
            token(16, 136),
            token(13, 140),
        ];
        let right = vec![token(1, 130), token(5, 137)];
        let merged = merge_chunks(&left, &right, &vocab);
        // Left keeps "c" (its cut word's last piece) and stops at the word
        // start " world"; right resumes at " meeting", the first piece at
        // or after the cutoff.
        assert_eq!(ids(&merged), vec![12, 14, 15, 16, 5]);
    }

    #[test]
    fn monotonic_clamps_without_reordering() {
        let tokens = vec![token(1, 5), token(2, 3), token(3, 7), token(4, 6)];
        let frames: Vec<usize> = enforce_monotonic(tokens).iter().map(|t| t.frame).collect();
        assert_eq!(frames, vec![5, 5, 7, 7]);
    }

    #[test]
    fn seam_word_duplicates_collapse_to_the_lower_case_copy() {
        let vocab = sample();
        // " the"(1) " The"(2) within the overlap: the capitalised copy goes.
        let tokens = vec![token(12, 10), token(2, 20), token(1, 22), token(13, 30)];
        assert_eq!(
            ids(&collapse_seam_word_duplicates(&tokens, &vocab)),
            vec![12, 1, 13]
        );
        let tokens = vec![token(12, 10), token(1, 20), token(2, 22), token(13, 30)];
        assert_eq!(
            ids(&collapse_seam_word_duplicates(&tokens, &vocab)),
            vec![12, 1, 13]
        );
        // After a sentence end the capital is a real sentence start.
        let tokens = vec![token(1, 20), token(4, 21), token(2, 22)];
        assert_eq!(
            ids(&collapse_seam_word_duplicates(&tokens, &vocab)),
            vec![1, 4, 2]
        );
        // Too far apart: both stay.
        let tokens = vec![token(1, 20), token(2, 60)];
        assert_eq!(
            ids(&collapse_seam_word_duplicates(&tokens, &vocab)),
            vec![1, 2]
        );
        // Identical cores are not case duplicates.
        let tokens = vec![token(1, 20), token(1, 22)];
        assert_eq!(
            ids(&collapse_seam_word_duplicates(&tokens, &vocab)),
            vec![1, 1]
        );
        assert_eq!(collapse_seam_word_duplicates(&[], &vocab), Vec::new());
    }

    #[test]
    fn word_neighbor_walks_past_punctuation() {
        let vocab = sample();
        let stream = vec![token(12, 1), token(4, 2), token(13, 10)];
        assert_eq!(word_neighbor(&stream, 1, -1, &vocab).id, 12);
        assert_eq!(word_neighbor(&stream, 2, 1, &vocab).id, 13);
        assert_eq!(word_neighbor(&stream, 0, -1, &vocab).id, 12);
        let only_punct = vec![token(4, 2)];
        assert_eq!(word_neighbor(&only_punct, 0, 1, &vocab).id, 4);
    }

    #[test]
    fn splice_candidate_keeps_only_new_in_gap_words() {
        let vocab = sample();
        let lead = token(12, 100);
        let tail = token(13, 140);
        let window = vec![
            token(12, 101), // the lead word re-heard: dropped
            token(15, 102), // continuation at the head: dropped
            token(14, 110), // " a": kept
            token(15, 111), // "b": kept
            token(4, 112),  // ".": kept (not at the head)
            token(13, 138), // the tail word re-heard: dropped
            token(13, 139), // outside the gap (needs frame + 1 < 140)
        ];
        let candidate = splice_candidate(&window, 100, 140, lead, tail, &vocab);
        assert_eq!(ids(&candidate), vec![14, 15, 4]);
        // Only punctuation left after the trims: nothing to splice.
        let window = vec![token(4, 110)];
        assert_eq!(
            splice_candidate(&window, 100, 140, lead, tail, &vocab),
            Vec::new()
        );
    }
}
