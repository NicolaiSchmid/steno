//! Agglomerative clustering of the window embeddings: unit vectors, cosine
//! distance, average linkage, cut at a distance threshold. The dendrogram
//! comes from the nearest-neighbour chain algorithm (Müllner 2011), which
//! is exact for average linkage and runs in quadratic time, so an
//! hour-long lane with a few thousand embeddings clusters in well under a
//! second.
//!
//! `FluidAudio` cuts a centroid-linkage dendrogram at a Euclidean distance
//! on unit vectors (0.8 in Steno's calibration, about cosine 0.68); this
//! module's threshold is a cosine distance (`1 - cos`) between cluster
//! means, so the two are not the same number. The default in
//! [`crate::DiarizerConfig`] is the one the calibration harness chose.

/// The cut and the speaker-count constraints.
#[derive(Debug, Clone, PartialEq)]
pub struct ClusteringConfig {
    /// Cosine distance at or below which two clusters merge.
    pub threshold: f32,
    /// Stop merging at this many clusters even when pairs are closer than
    /// the threshold.
    pub min_speakers: Option<usize>,
    /// Keep merging past the threshold until this many clusters remain.
    pub max_speakers: Option<usize>,
}

/// One merge of the dendrogram: the two clusters joined, at what average
/// cosine distance, and the size of the result.
#[derive(Debug, Clone, PartialEq)]
pub struct Merge {
    pub left: usize,
    pub right: usize,
    pub distance: f32,
    pub size: usize,
}

/// Cluster labels for `embeddings`, `0..k` in order of first appearance.
/// Zero vectors land in a cluster of their own at distance 1 from
/// everything.
#[must_use]
pub fn cluster(embeddings: &[Vec<f32>], config: &ClusteringConfig) -> Vec<usize> {
    let merges = linkage(embeddings);
    cut(embeddings.len(), &merges, config)
}

/// The average-linkage dendrogram over cosine distances, `n - 1` merges.
/// Cluster ids are `0..n` for the inputs and `n + i` for merge `i`.
#[must_use]
pub fn linkage(embeddings: &[Vec<f32>]) -> Vec<Merge> {
    let n = embeddings.len();
    if n < 2 {
        return Vec::new();
    }
    let units: Vec<Vec<f32>> = embeddings.iter().map(|vector| normalized(vector)).collect();
    // Full square distance matrix over the active clusters; row `i` of
    // cluster `i`. Memory is n²·4 bytes: 3 000 embeddings take 36 MB.
    let mut distance = vec![0.0f32; n * n];
    for i in 0..n {
        for j in i + 1..n {
            let d = cosine_distance(&units[i], &units[j]);
            distance[i * n + j] = d;
            distance[j * n + i] = d;
        }
    }
    let mut size = vec![1usize; n];
    let mut active = vec![true; n];
    // The id each slot carries now: a merged slot takes the new cluster id.
    let mut id: Vec<usize> = (0..n).collect();
    let mut merges = Vec::with_capacity(n - 1);
    let mut chain: Vec<usize> = Vec::new();
    let mut remaining = n;
    while remaining > 1 {
        if chain.is_empty() {
            let first = (0..n)
                .find(|slot| active[*slot])
                .expect("an active cluster remains");
            chain.push(first);
        }
        loop {
            let a = *chain.last().expect("chain is not empty");
            let previous = chain.len().checked_sub(2).map(|index| chain[index]);
            let mut best: Option<(usize, f32)> = None;
            for slot in (0..n).filter(|slot| active[*slot] && *slot != a) {
                let d = distance[a * n + slot];
                let better = match best {
                    None => true,
                    // Prefer the chain's previous element on ties so the
                    // reciprocal check below terminates.
                    #[allow(clippy::float_cmp)]
                    Some((slot_b, d_b)) => {
                        d < d_b || (d == d_b && Some(slot) == previous && slot_b != slot)
                    }
                };
                if better {
                    best = Some((slot, d));
                }
            }
            let (c, d) = best.expect("two active clusters remain");
            if Some(c) == previous {
                // Reciprocal nearest neighbours: merge a and c.
                chain.pop();
                chain.pop();
                let (keep, drop) = if a < c { (a, c) } else { (c, a) };
                merges.push(Merge {
                    left: id[keep],
                    right: id[drop],
                    distance: d,
                    size: size[keep] + size[drop],
                });
                let new_size = size[keep] + size[drop];
                // Lance–Williams for average linkage.
                #[allow(clippy::cast_precision_loss)]
                let (weight_keep, weight_drop) = (
                    size[keep] as f32 / new_size as f32,
                    size[drop] as f32 / new_size as f32,
                );
                for slot in (0..n).filter(|slot| active[*slot] && *slot != keep && *slot != drop) {
                    let updated = weight_keep * distance[keep * n + slot]
                        + weight_drop * distance[drop * n + slot];
                    distance[keep * n + slot] = updated;
                    distance[slot * n + keep] = updated;
                }
                active[drop] = false;
                size[keep] = new_size;
                id[keep] = n + merges.len() - 1;
                remaining -= 1;
                break;
            }
            chain.push(c);
        }
    }
    merges
}

/// Applies `merges` in order of distance while a merge is at or below the
/// threshold, honouring the count constraints, and labels the leaves by
/// first appearance.
#[must_use]
pub fn cut(n: usize, merges: &[Merge], config: &ClusteringConfig) -> Vec<usize> {
    if n == 0 {
        return Vec::new();
    }
    let mut order: Vec<usize> = (0..merges.len()).collect();
    order.sort_by(|lhs, rhs| merges[*lhs].distance.total_cmp(&merges[*rhs].distance));
    // Union-find over leaf ids `0..n` and merge ids `n..`; a merge node is
    // linked to its children only when the merge is applied.
    let mut parent: Vec<usize> = (0..n + merges.len()).collect();
    let mut count = n;
    let min = config.min_speakers.unwrap_or(1).max(1);
    let max = config.max_speakers.unwrap_or(usize::MAX).max(1);
    for index in order {
        let merge = &merges[index];
        let needed = count > max;
        let allowed = merge.distance <= config.threshold && count > min;
        if !needed && !allowed {
            continue;
        }
        let node = n + index;
        let left = find(&mut parent, merge.left);
        let right = find(&mut parent, merge.right);
        parent[left] = node;
        parent[right] = node;
        count -= 1;
    }
    let mut labels = Vec::with_capacity(n);
    let mut seen: Vec<(usize, usize)> = Vec::new();
    for leaf in 0..n {
        let root = find(&mut parent, leaf);
        let label = if let Some((_, label)) = seen.iter().find(|(known, _)| *known == root) {
            *label
        } else {
            let label = seen.len();
            seen.push((root, label));
            label
        };
        labels.push(label);
    }
    labels
}

fn find(parent: &mut [usize], mut node: usize) -> usize {
    while parent[node] != node {
        parent[node] = parent[parent[node]];
        node = parent[node];
    }
    node
}

/// `vector` at unit length; the zero vector stays zero.
#[must_use]
pub fn normalized(vector: &[f32]) -> Vec<f32> {
    let magnitude = vector.iter().map(|value| value * value).sum::<f32>().sqrt();
    if magnitude > 0.0 {
        vector.iter().map(|value| value / magnitude).collect()
    } else {
        vector.to_vec()
    }
}

/// `1 - cos` between two unit vectors, clamped to `0...2`; 1 when either
/// is zero or the lengths differ.
#[must_use]
pub fn cosine_distance(lhs: &[f32], rhs: &[f32]) -> f32 {
    if lhs.len() != rhs.len() || lhs.is_empty() {
        return 1.0;
    }
    let dot: f32 = lhs.iter().zip(rhs).map(|(l, r)| l * r).sum();
    let norms = lhs.iter().map(|v| v * v).sum::<f32>().sqrt()
        * rhs.iter().map(|v| v * v).sum::<f32>().sqrt();
    if norms > 0.0 {
        (1.0 - dot / norms).clamp(0.0, 2.0)
    } else {
        1.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn axis(index: usize, scale: f32) -> Vec<f32> {
        let mut vector = vec![0.0f32; 8];
        vector[index] = scale;
        vector
    }

    fn config(threshold: f32) -> ClusteringConfig {
        ClusteringConfig {
            threshold,
            min_speakers: None,
            max_speakers: None,
        }
    }

    #[test]
    fn orthogonal_groups_stay_apart_and_scale_is_ignored() {
        let embeddings = vec![
            axis(0, 1.0),
            axis(1, 5.0),
            axis(0, 0.2),
            axis(1, 1.0),
            axis(0, 3.0),
        ];
        assert_eq!(cluster(&embeddings, &config(0.5)), vec![0, 1, 0, 1, 0]);
        // Everything merges at a threshold past orthogonality.
        assert_eq!(cluster(&embeddings, &config(1.0)), vec![0, 0, 0, 0, 0]);
        // Identical directions sit at distance zero and still merge there.
        assert_eq!(cluster(&embeddings, &config(0.0)), vec![0, 1, 0, 1, 0]);
        // Nothing merges below zero.
        assert_eq!(cluster(&embeddings, &config(-0.1)), vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn near_duplicates_merge_under_the_threshold() {
        let a = vec![1.0, 0.1, 0.0];
        let b = vec![1.0, 0.0, 0.1];
        let c = vec![0.0, 1.0, 0.0];
        let labels = cluster(&[a, b, c], &config(0.1));
        assert_eq!(labels, vec![0, 0, 1]);
    }

    #[test]
    fn the_dendrogram_has_increasing_average_distances_for_a_chain() {
        let embeddings = vec![axis(0, 1.0), axis(0, 1.0), axis(1, 1.0), axis(2, 1.0)];
        let merges = linkage(&embeddings);
        assert_eq!(merges.len(), 3);
        assert!(
            merges[0].distance.abs() < 1e-6,
            "the duplicate pair merges first"
        );
        assert_eq!(merges[0].size, 2);
        assert_eq!(merges.last().unwrap().size, 4);
        let mut sorted = merges.iter().map(|m| m.distance).collect::<Vec<_>>();
        sorted.sort_by(f32::total_cmp);
        assert_eq!(
            sorted,
            merges.iter().map(|m| m.distance).collect::<Vec<_>>()
        );
    }

    #[test]
    fn constraints_override_the_threshold() {
        let embeddings = vec![axis(0, 1.0), axis(1, 1.0), axis(2, 1.0), axis(0, 1.0)];
        let capped = ClusteringConfig {
            max_speakers: Some(2),
            ..config(0.0)
        };
        let labels = cluster(&embeddings, &capped);
        assert_eq!(labels.iter().max(), Some(&1));
        assert_eq!(labels[0], labels[3], "the duplicate pair merged first");
        let floored = ClusteringConfig {
            min_speakers: Some(3),
            ..config(2.0)
        };
        assert_eq!(cluster(&embeddings, &floored), vec![0, 1, 2, 0]);
    }

    #[test]
    fn degenerate_inputs_are_handled() {
        assert_eq!(cluster(&[], &config(0.5)).len(), 0);
        assert_eq!(cluster(&[axis(0, 1.0)], &config(0.5)), vec![0]);
        let zero = vec![0.0f32; 8];
        assert_eq!(
            cluster(&[axis(0, 1.0), zero.clone()], &config(0.5)),
            vec![0, 1]
        );
        assert_eq!(cosine_distance(&zero, &zero), 1.0);
        assert_eq!(cosine_distance(&[1.0], &[1.0, 0.0]), 1.0);
        assert!((cosine_distance(&[1.0, 0.0], &[-1.0, 0.0]) - 2.0).abs() < 1e-6);
    }

    /// The chain algorithm agrees with a naive agglomeration on random data.
    #[test]
    fn matches_a_naive_average_linkage_on_random_vectors() {
        let mut state = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            #[allow(clippy::cast_precision_loss)]
            let value = (state >> 40) as f32 / (1u64 << 24) as f32;
            value - 0.5
        };
        let embeddings: Vec<Vec<f32>> = (0..24).map(|_| (0..6).map(|_| next()).collect()).collect();
        let fast = cluster(&embeddings, &config(0.6));
        let naive = naive_cluster(&embeddings, 0.6);
        assert_eq!(fast, naive);
    }

    fn naive_cluster(embeddings: &[Vec<f32>], threshold: f32) -> Vec<usize> {
        let units: Vec<Vec<f32>> = embeddings.iter().map(|v| normalized(v)).collect();
        let mut clusters: Vec<Vec<usize>> = (0..units.len()).map(|i| vec![i]).collect();
        loop {
            let mut best: Option<(usize, usize, f32)> = None;
            for i in 0..clusters.len() {
                for j in i + 1..clusters.len() {
                    let mut total = 0.0f32;
                    for a in &clusters[i] {
                        for b in &clusters[j] {
                            total += cosine_distance(&units[*a], &units[*b]);
                        }
                    }
                    #[allow(clippy::cast_precision_loss)]
                    let mean = total / (clusters[i].len() * clusters[j].len()) as f32;
                    if best.is_none_or(|(_, _, d)| mean < d) {
                        best = Some((i, j, mean));
                    }
                }
            }
            match best {
                Some((i, j, d)) if d <= threshold => {
                    let merged = clusters.remove(j);
                    clusters[i].extend(merged);
                }
                _ => break,
            }
        }
        let mut labels = vec![0usize; units.len()];
        let mut order: Vec<usize> = clusters
            .iter()
            .map(|members| *members.iter().min().unwrap())
            .collect();
        order.sort_unstable();
        for members in &clusters {
            let first = *members.iter().min().unwrap();
            let label = order.iter().position(|f| *f == first).unwrap();
            for member in members {
                labels[*member] = label;
            }
        }
        labels
    }
}
