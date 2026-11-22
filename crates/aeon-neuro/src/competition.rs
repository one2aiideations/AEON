//! k-winners-take-all: keep only the most active units, giving sparse codes.

/// Indices of the top `fraction` of `activations` (at least one), highest
/// first; ties keep the lower index first. Non-finite values never win.
#[must_use]
pub fn k_winners(activations: &[f32], fraction: f32) -> Vec<usize> {
    let mut idx: Vec<usize> = (0..activations.len())
        .filter(|&i| activations[i].is_finite())
        .collect();
    if idx.is_empty() {
        return idx;
    }
    let k = ((activations.len() as f32 * fraction.clamp(0.0, 1.0)) as usize).clamp(1, idx.len());
    idx.sort_by(|&a, &b| activations[b].total_cmp(&activations[a]).then(a.cmp(&b)));
    idx.truncate(k);
    idx
}

/// Zero every activation except the winners.
pub fn inhibit(activations: &mut [f32], fraction: f32) {
    let winners = k_winners(activations, fraction);
    for (i, a) in activations.iter_mut().enumerate() {
        if !winners.contains(&i) {
            *a = 0.0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_top_fraction() {
        let a = [0.1, 0.9, 0.5, 0.9, f32::NAN];
        assert_eq!(k_winners(&a, 0.4), vec![1, 3]);
        assert_eq!(k_winners(&a, 0.0), vec![1]);
        assert!(k_winners(&[], 0.5).is_empty());
        let mut b = [0.2f32, 0.8, 0.4, 0.6];
        inhibit(&mut b, 0.5);
        assert_eq!(b, [0.0, 0.8, 0.0, 0.6]);
    }
}
