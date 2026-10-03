//! The overlap merge: a time-tolerant longest common subsequence over the
//! tokens two neighbouring windows share, with a midpoint cut when nothing
//! matches, and the seam word owned by the left window unless that window
//! ends inside it. Ported from
//! `spikes/coreml-rs/src/pipeline.rs`, itself a port of `FluidAudio`'s
//! `mergeChunks`; the spike D harness ran the same code on sherpa-onnx
//! pieces (`spikes/onnx-speech/src/merge.rs`).
//! Swift: `FluidAudio`'s `ChunkProcessor.mergeChunks`.

use crate::backend::FRAME_SECONDS;
use crate::decoder::Token;
use crate::vocab::Vocab;

/// Merges windows in order; token frames come out non-decreasing.
#[must_use]
pub fn merge_all(windows: &[Vec<Token>], overlap_seconds: f64, vocab: &Vocab) -> Vec<Token> {
    let mut merged = windows.first().cloned().unwrap_or_default();
    for window in windows.iter().skip(1) {
        merged = merge_windows(&merged, window, overlap_seconds, vocab);
        enforce_monotonic(&mut merged);
    }
    merged
}

fn enforce_monotonic(tokens: &mut [Token]) {
    let mut max = 0;
    for token in tokens {
        if token.frame < max {
            token.frame = max;
        }
        max = token.frame;
    }
}

fn seconds(token: &Token) -> f64 {
    token.frame as f64 * FRAME_SECONDS
}

/// Merges `right` onto `left`, which ends inside `right`'s start.
#[must_use]
pub fn merge_windows(
    left: &[Token],
    right: &[Token],
    overlap_seconds: f64,
    vocab: &Vocab,
) -> Vec<Token> {
    let (Some(last_left), Some(first_right)) = (left.last(), right.first()) else {
        return [left, right].concat();
    };
    let left_end = seconds(last_left) + FRAME_SECONDS;
    let right_start = seconds(first_right);
    let tolerance = (overlap_seconds / 2.0).max(0.5);
    // Windows further apart than the match tolerance share no token. Closer
    // ones go through the LCS even with one token a side: a right window
    // that starts with the left's last word one frame later is a copy.
    if right_start >= left_end + tolerance {
        return [left, right].concat();
    }
    // The overlap reaches at least the tolerance, so a copy the LCS could
    // match never falls outside it and through to the midpoint cut.
    let reach = overlap_seconds.max(tolerance);
    let overlap_left: Vec<usize> = (0..left.len())
        .filter(|&i| seconds(&left[i]) + FRAME_SECONDS > right_start - reach)
        .collect();
    let overlap_right: Vec<usize> = (0..right.len())
        .filter(|&i| seconds(&right[i]) < left_end + reach)
        .collect();
    let matches = |a: usize, b: usize| {
        left[a].id == right[b].id && (seconds(&left[a]) - seconds(&right[b])).abs() < tolerance
    };
    let (rows, cols) = (overlap_left.len(), overlap_right.len());
    let mut table = vec![vec![0u32; cols + 1]; rows + 1];
    for row in (0..rows).rev() {
        for col in (0..cols).rev() {
            table[row][col] = if matches(overlap_left[row], overlap_right[col]) {
                table[row + 1][col + 1] + 1
            } else {
                table[row + 1][col].max(table[row][col + 1])
            };
        }
    }
    let mut pairs = Vec::new();
    let (mut row, mut col) = (0, 0);
    while row < rows && col < cols {
        if matches(overlap_left[row], overlap_right[col]) {
            pairs.push((overlap_left[row], overlap_right[col]));
            row += 1;
            col += 1;
        } else if table[row + 1][col] >= table[row][col + 1] {
            row += 1;
        } else {
            col += 1;
        }
    }
    let (Some(&(first_left, _)), Some(&(last_left, last_right))) = (pairs.first(), pairs.last())
    else {
        return merge_by_midpoint(left, right, left_end, right_start, vocab);
    };
    let mut out: Vec<Token> = left[..first_left].to_vec();
    for (k, &(li, ri)) in pairs.iter().enumerate() {
        out.push(left[li]);
        if let Some(&(next_left, next_right)) = pairs.get(k + 1) {
            let gap_left = &left[li + 1..next_left];
            let gap_right = &right[ri + 1..next_right];
            out.extend_from_slice(if gap_right.len() > gap_left.len() {
                gap_right
            } else {
                gap_left
            });
        }
    }
    let tail = &right[last_right + 1..];
    let left_rest = &left[last_left + 1..];
    let next = tail.iter().position(|t| vocab.is_splice_safe(t.id));
    // A left window that ends inside the seam word, as an Energy cut can,
    // holds only part of it; a right that heard more of the word finishes it.
    if !left_rest.iter().any(|t| vocab.is_splice_safe(t.id))
        && next.unwrap_or(tail.len()) > left_rest.len()
    {
        out.extend_from_slice(tail);
        return out;
    }
    match next {
        Some(0) => out.extend_from_slice(tail),
        Some(next) => {
            // The left window owns the seam word; the right resumes at its
            // next splice point.
            out.extend(left_rest.iter().take_while(|t| !vocab.is_splice_safe(t.id)));
            out.extend_from_slice(&tail[next..]);
        }
        // Nothing past the seam word on the right, or only the rest of that
        // word: its window ended inside the left one's span, so the left
        // keeps the rest.
        None => out.extend_from_slice(left_rest),
    }
    out
}

fn merge_by_midpoint(
    left: &[Token],
    right: &[Token],
    left_end: f64,
    right_start: f64,
    vocab: &Vocab,
) -> Vec<Token> {
    // Without a shared token, windows that do not overlap in time join as
    // they are.
    if right_start >= left_end {
        return [left, right].concat();
    }
    let cutoff = f64::midpoint(left_end, right_start);
    let mut left_end_index = left
        .iter()
        .position(|t| seconds(t) >= cutoff)
        .unwrap_or(left.len());
    let mut right_start_index = right
        .iter()
        .position(|t| seconds(t) >= cutoff)
        .unwrap_or(right.len());
    // The right resumes at its first splice point (a word start or a
    // punctuation piece) from the cutoff.
    let resume = right[right_start_index..]
        .iter()
        .position(|t| vocab.is_splice_safe(t.id));
    match (resume, left_end_index) {
        // Without one the right adds no word of its own: its window ended
        // inside the left's span, which keeps everything.
        (None, 1..) => return left.to_vec(),
        // A left with nothing before the cutoff keeps nothing, and the
        // right everything from the cutoff.
        (None, 0) => {}
        (Some(offset), _) => right_start_index += offset,
    }
    // The left finishes the word it is in.
    if left_end_index > 0 {
        left_end_index += left[left_end_index..]
            .iter()
            .take_while(|t| !vocab.is_splice_safe(t.id))
            .count();
    }
    [&left[..left_end_index], &right[right_start_index..]].concat()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pieces 0..4 start words, 4..8 continue them, 8 is punctuation, 9 blank.
    fn vocab() -> Vocab {
        Vocab::from_pieces(
            ["▁a", "▁b", "▁c", "▁d", "x", "y", "z", "w", ".", "<blk>"]
                .map(str::to_owned)
                .to_vec(),
        )
        .unwrap()
    }

    fn token(id: u32, frame: usize) -> Token {
        Token {
            id,
            frame,
            confidence: 1.0,
            duration: 1,
        }
    }

    fn ids(tokens: &[Token]) -> Vec<(u32, usize)> {
        tokens.iter().map(|t| (t.id, t.frame)).collect()
    }

    #[test]
    fn windows_apart_in_time_concatenate() {
        // Right starts 2.24 s after left ends, past the 0.75 s tolerance.
        let left = vec![token(0, 0), token(4, 1)];
        let right = vec![token(1, 30), token(5, 31)];
        assert_eq!(
            ids(&merge_all(&[left.clone(), right.clone()], 1.5, &vocab())),
            ids(&[left.clone(), right.clone()].concat())
        );
        assert!(merge_all(&[], 1.5, &vocab()).is_empty());
        assert_eq!(merge_windows(&[], &right, 1.5, &vocab()), right);
    }

    #[test]
    fn shared_tokens_in_the_overlap_are_emitted_once() {
        // Left: a x | b y c ; right (starting 1 s earlier than left's end): b y c | d z
        let left = vec![
            token(0, 0),
            token(4, 1),
            token(1, 20),
            token(5, 21),
            token(2, 24),
        ];
        let right = vec![
            token(1, 20),
            token(5, 21),
            token(2, 24),
            token(3, 30),
            token(6, 31),
        ];
        let merged = merge_all(&[left, right], 1.5, &vocab());
        assert_eq!(
            ids(&merged),
            vec![(0, 0), (4, 1), (1, 20), (5, 21), (2, 24), (3, 30), (6, 31)]
        );
    }

    #[test]
    fn a_right_tail_that_continues_a_word_leaves_the_seam_word_to_the_left() {
        // Pairs end at (2, 24); right continues with a suffix piece (z), then a new word.
        let left = vec![token(1, 20), token(5, 21), token(2, 24), token(7, 25)];
        let right = vec![
            token(1, 20),
            token(5, 21),
            token(2, 24),
            token(6, 25),
            token(3, 30),
        ];
        let merged = merge_all(&[left, right], 1.5, &vocab());
        assert_eq!(
            ids(&merged),
            vec![(1, 20), (5, 21), (2, 24), (7, 25), (3, 30)]
        );
    }

    #[test]
    fn a_left_window_that_ends_inside_the_seam_word_takes_the_rest_from_the_right() {
        // Left ends on "c"; the right heard "c z w" and then "d".
        let left = vec![token(1, 20), token(5, 21), token(2, 24)];
        let right = vec![
            token(1, 20),
            token(5, 21),
            token(2, 24),
            token(6, 25),
            token(7, 26),
            token(3, 30),
        ];
        assert_eq!(
            ids(&merge_windows(&left, &right, 1.5, &vocab())),
            ids(&right)
        );
        // The right ends inside the word too, one piece further than the left.
        let right = &right[..4];
        assert_eq!(ids(&merge_windows(&left, right, 1.5, &vocab())), ids(right));
    }

    #[test]
    fn a_right_window_that_ends_inside_the_left_leaves_the_left_intact() {
        // Right matches b y c and has nothing after; left goes on with z and d.
        let left = vec![
            token(0, 0),
            token(4, 1),
            token(1, 20),
            token(5, 21),
            token(2, 24),
            token(6, 25),
            token(3, 28),
        ];
        let right = vec![token(1, 20), token(5, 21), token(2, 24)];
        let merged = merge_all(&[left.clone(), right], 1.5, &vocab());
        assert_eq!(ids(&merged), ids(&left));
    }

    #[test]
    fn without_matches_the_midpoint_cuts_at_word_starts() {
        // Left ends at frame 30 (2.48 s), right starts at frame 20 (1.6 s): overlap, no shared ids.
        let left = vec![
            token(0, 10),
            token(4, 11),
            token(1, 26),
            token(5, 27),
            token(6, 30),
        ];
        let right = vec![token(2, 20), token(7, 21), token(3, 28), token(4, 29)];
        let merged = merge_all(&[left, right], 1.5, &vocab());
        // Cutoff 2.04 s = frame 25.5: left keeps up to frame 11, right resumes at the word start on frame 28.
        assert_eq!(ids(&merged), vec![(0, 10), (4, 11), (3, 28), (4, 29)]);
    }

    #[test]
    fn windows_close_in_time_without_a_shared_token_concatenate() {
        // Right starts 0.16 s after left ends and opens with the rest of
        // left's last word: no copy, no time overlap, nothing to cut.
        let left = vec![token(0, 0), token(1, 9)];
        let right = vec![token(5, 12), token(2, 14)];
        assert_eq!(
            ids(&merge_windows(&left, &right, 1.5, &vocab())),
            ids(&[left, right].concat())
        );
        // Touching: the right starts on the frame the left ends (frames
        // 10 and 11, whose seconds are exact in `f64`).
        let left = vec![token(0, 0), token(4, 10)];
        let right = vec![token(5, 11), token(1, 13)];
        assert_eq!(
            ids(&merge_windows(&left, &right, 1.5, &vocab())),
            ids(&[left, right].concat())
        );
    }

    #[test]
    fn a_right_window_repeating_the_left_s_last_word_a_frame_later_keeps_one_copy() {
        // Left ends with c on frame 24, right opens with c on frame 25: the
        // windows touch, so a plain join would read "c c d".
        let left = vec![token(1, 20), token(2, 24)];
        let right = vec![token(2, 25), token(3, 30)];
        assert_eq!(
            ids(&merge_windows(&left, &right, 1.5, &vocab())),
            vec![(1, 20), (2, 24), (3, 30)]
        );
        // With a 0.4 s overlap the 0.5 s tolerance reaches further than the
        // overlap: the copy 0.4 s later is still matched, not cut at the
        // midpoint and kept twice.
        let right = vec![token(2, 30), token(3, 40)];
        assert_eq!(
            ids(&merge_windows(&left, &right, 0.4, &vocab())),
            vec![(1, 20), (2, 24), (3, 40)]
        );
    }

    #[test]
    fn a_right_tail_without_a_word_start_leaves_the_left_its_words() {
        // Pairs end at c; right only adds a continuation (x), left goes on
        // with w, d and z.
        let left = vec![token(2, 24), token(7, 25), token(3, 28), token(6, 29)];
        let right = vec![token(2, 24), token(4, 25)];
        assert_eq!(
            ids(&merge_windows(&left, &right, 1.5, &vocab())),
            ids(&left)
        );
    }

    #[test]
    fn a_midpoint_cut_with_no_word_start_on_the_right_keeps_the_left() {
        // Cutoff at frame 24. Past it the right has only continuations (y
        // z), which would glue onto the left's "a".
        let left = vec![token(0, 10), token(1, 24), token(4, 27)];
        let right = vec![token(3, 20), token(5, 28), token(6, 29)];
        assert_eq!(
            ids(&merge_windows(&left, &right, 1.5, &vocab())),
            ids(&left)
        );
        // Cutoff at frame 28. The right has nothing past it, the left has
        // "c" there.
        let left = vec![token(0, 10), token(1, 26), token(2, 40)];
        let right = vec![token(3, 15), token(6, 27)];
        assert_eq!(
            ids(&merge_windows(&left, &right, 1.5, &vocab())),
            ids(&left)
        );
    }

    #[test]
    fn a_midpoint_cut_without_a_splice_point_keeps_the_right_from_the_cutoff() {
        // Cutoff at frame 25. Left's first token (x, frame 26) already lies
        // past it, so left contributes nothing; right after the cutoff has
        // no word start, so all of it is kept.
        let left = vec![token(4, 26), token(5, 27), token(0, 29)];
        let right = vec![token(1, 21), token(6, 26), token(7, 27)];
        assert_eq!(
            ids(&merge_windows(&left, &right, 1.5, &vocab())),
            vec![(6, 26), (7, 27)]
        );
    }

    #[test]
    fn frames_never_run_backwards_after_a_merge() {
        let left = vec![token(0, 10), token(1, 12)];
        let right = vec![token(1, 12), token(2, 11)];
        let merged = merge_all(&[left, right], 1.5, &vocab());
        assert!(merged.windows(2).all(|w| w[0].frame <= w[1].frame));
    }
}
