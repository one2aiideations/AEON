//! Reciprocal-rank fusion: combine ranked lists without comparing their
//! raw scores. A document's fused score is `sum 1 / (k + rank)`.

use std::collections::HashMap;
use std::hash::Hash;

/// Fuse `lists` (each best first) with constant `k` (60 is the usual choice).
#[must_use]
pub fn rrf<T: Copy + Eq + Hash + Ord>(lists: &[Vec<T>], k: f32, top: usize) -> Vec<(T, f32)> {
    let mut score: HashMap<T, f32> = HashMap::new();
    for list in lists {
        for (rank, &id) in list.iter().enumerate() {
            *score.entry(id).or_insert(0.0) += 1.0 / (k + rank as f32 + 1.0);
        }
    }
    let mut v: Vec<(T, f32)> = score.into_iter().collect();
    v.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    v.truncate(top);
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agreement_wins() {
        let lexical = vec![1u64, 2, 3];
        let vector = vec![2u64, 4, 1];
        let fused = rrf(&[lexical, vector], 60.0, 3);
        assert_eq!(fused[0].0, 2);
        assert_eq!(fused[1].0, 1);
        assert_eq!(fused.len(), 3);
        assert!(rrf::<u64>(&[], 60.0, 3).is_empty());
    }
}
