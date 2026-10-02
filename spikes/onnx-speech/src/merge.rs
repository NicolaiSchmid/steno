//! Overlap merge ported from `spikes/coreml-rs/src/pipeline.rs` (itself a port
//! of FluidAudio's `mergeChunks`): time-tolerant LCS over the overlap region,
//! midpoint cut when no matches exist. Tokens here are sherpa-onnx pieces with
//! absolute timestamps in seconds instead of ids with frame indices.

pub const FRAME_SECONDS: f32 = 0.08;
pub const WORD_BOUNDARY: &str = "\u{2581}";

#[derive(Clone, Debug)]
pub struct Tok {
    pub piece: String,
    pub t: f32,
}

pub fn starts_word(p: &str) -> bool {
    p.starts_with(WORD_BOUNDARY) || p.starts_with(' ')
}

pub fn strip_boundary(p: &str) -> &str {
    p.strip_prefix(WORD_BOUNDARY).or_else(|| p.strip_prefix(' ')).unwrap_or(p)
}

fn is_punctuation_piece(p: &str) -> bool {
    let core = strip_boundary(p).trim();
    !core.is_empty() && core.chars().all(|c| c.is_ascii_punctuation() || matches!(c, '。' | '？' | '！' | '，' | '、' | '…' | '’' | '“' | '”'))
}

fn safe(t: &Tok) -> bool {
    starts_word(&t.piece) || is_punctuation_piece(&t.piece)
}

pub fn merge_all(windows: &[Vec<Tok>], overlap_s: f32) -> Vec<Tok> {
    let mut merged: Vec<Tok> = windows.first().cloned().unwrap_or_default();
    for w in windows.iter().skip(1) {
        merged = merge_windows(&merged, w, overlap_s);
        enforce_monotonic(&mut merged);
    }
    merged
}

fn enforce_monotonic(tokens: &mut [Tok]) {
    let mut max = 0f32;
    for t in tokens.iter_mut() {
        if t.t < max {
            t.t = max;
        }
        max = t.t;
    }
}

pub fn merge_windows(left: &[Tok], right: &[Tok], overlap_s: f32) -> Vec<Tok> {
    if left.is_empty() {
        return right.to_vec();
    }
    if right.is_empty() {
        return left.to_vec();
    }
    let left_end = left.last().unwrap().t + FRAME_SECONDS;
    let right_start = right[0].t;
    if left_end <= right_start {
        return [left, right].concat();
    }
    let ol: Vec<usize> = (0..left.len()).filter(|&i| left[i].t + FRAME_SECONDS > right_start - overlap_s).collect();
    let or: Vec<usize> = (0..right.len()).filter(|&i| right[i].t < left_end + overlap_s).collect();
    if ol.len() < 2 || or.len() < 2 {
        return merge_by_midpoint(left, right, left_end, right_start);
    }
    let tol = (overlap_s / 2.0).max(0.5);
    let matches = |a: usize, b: usize| left[a].piece == right[b].piece && (left[a].t - right[b].t).abs() < tol;
    let (n, m) = (ol.len(), or.len());
    let mut dp = vec![vec![0u16; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[i][j] = if matches(ol[i], or[j]) { dp[i + 1][j + 1] + 1 } else { dp[i + 1][j].max(dp[i][j + 1]) };
        }
    }
    let mut pairs = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if matches(ol[i], or[j]) {
            pairs.push((ol[i], or[j]));
            i += 1;
            j += 1;
        } else if dp[i + 1][j] >= dp[i][j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    if pairs.is_empty() {
        return merge_by_midpoint(left, right, left_end, right_start);
    }
    let mut out: Vec<Tok> = Vec::new();
    out.extend_from_slice(&left[..pairs[0].0]);
    for k in 0..pairs.len() {
        let (li, ri) = pairs[k];
        out.push(left[li].clone());
        if k + 1 < pairs.len() {
            let (nl, nr) = pairs[k + 1];
            let gap_l = &left[li + 1..nl];
            let gap_r = &right[ri + 1..nr];
            out.extend_from_slice(if gap_r.len() > gap_l.len() { gap_r } else { gap_l });
        }
    }
    let (last_l, last_r) = *pairs.last().unwrap();
    let tail = &right[last_r + 1..];
    if let Some(first) = tail.first() {
        if !safe(first) {
            let mut cursor = last_l + 1;
            while cursor < left.len() && !safe(&left[cursor]) {
                out.push(left[cursor].clone());
                cursor += 1;
            }
            match tail.iter().position(safe) {
                Some(p) => out.extend_from_slice(&tail[p..]),
                None => out.extend_from_slice(tail),
            }
        } else {
            out.extend_from_slice(tail);
        }
    }
    out
}

fn merge_by_midpoint(left: &[Tok], right: &[Tok], left_end: f32, right_start: f32) -> Vec<Tok> {
    let cutoff = (left_end + right_start) / 2.0;
    let mut le = left.iter().position(|t| t.t >= cutoff).unwrap_or(left.len());
    let mut rs = right.iter().position(|t| t.t >= cutoff).unwrap_or(right.len());
    if le > 0 {
        while le < left.len() && !safe(&left[le]) {
            le += 1;
        }
    }
    let mut scan = rs;
    while scan < right.len() && !safe(&right[scan]) {
        scan += 1;
    }
    if scan < right.len() {
        rs = scan;
    }
    [&left[..le], &right[rs..]].concat()
}

/// Tokens to text and words (word = run of pieces after a word-boundary piece).
pub fn render(tokens: &[Tok]) -> (String, Vec<(String, f32)>) {
    let mut words: Vec<(String, f32)> = Vec::new();
    for t in tokens {
        let body = strip_boundary(&t.piece);
        if starts_word(&t.piece) || words.is_empty() {
            words.push((body.to_string(), t.t));
        } else if let Some(w) = words.last_mut() {
            w.0.push_str(body);
        }
    }
    let text = words.iter().map(|w| w.0.as_str()).collect::<Vec<_>>().join(" ").trim().to_string();
    (text, words)
}

/// Similarity of two piece sequences in [0, 1]: LCS length over the longer length.
pub fn similarity(a: &[Tok], b: &[Tok]) -> f32 {
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let (n, m) = (a.len(), b.len());
    let mut prev = vec![0u32; m + 1];
    let mut cur = vec![0u32; m + 1];
    for i in 1..=n {
        for j in 1..=m {
            cur[j] = if a[i - 1].piece == b[j - 1].piece { prev[j - 1] + 1 } else { prev[j].max(cur[j - 1]) };
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[m] as f32 / n.max(m) as f32
}
