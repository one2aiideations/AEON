//! Pure in-memory SwiGLU matrix slicing.
//!
//! Wg and Wu are row-major [d_ff, d_model] matrices.
//! Wd is row-major [d_model, d_ff].
//!
//! This module does not write GGUF files. The engine side must re-quantize
//! the sliced matrices and write the resulting GGUF file. Its
//! feed_forward_length metadata must equal the kept count.
//!
//! Rule::Budget can produce different widths per layer. The engine must
//! support per-layer widths or the caller must use Rule::Uniform.
//!
//! Matrix transformation correctness does not prove model quality.

use crate::SliceError;

/// The sliced `(Wg, Wu, Wd)` of one layer.
pub type SlicedLayer = (Vec<f32>, Vec<f32>, Vec<f32>);

/// Keeps selected rows of a row-major [d_ff, d_model] matrix.
///
/// Indices must be non-empty, strictly ascending, and less than d_ff.
/// Matrix length must equal exactly d_ff * d_model.
///
/// Does not prove that removing neurons preserves model quality.
///
/// # Errors
/// A [`SliceError`] for a wrong length, empty or bad `keep`.
pub fn slice_gate_or_up(
    w: &[f32],
    d_ff: usize,
    d_model: usize,
    keep: &[usize],
) -> Result<Vec<f32>, SliceError> {
    validate_matrix(w, d_ff, d_model, d_ff, keep)?;

    let capacity = keep
        .len()
        .checked_mul(d_model)
        .ok_or(SliceError::BadParameter("output size overflow"))?;

    let mut output = Vec::with_capacity(capacity);

    for &row in keep {
        let start = row
            .checked_mul(d_model)
            .ok_or(SliceError::BadParameter("row offset overflow"))?;
        let end = start
            .checked_add(d_model)
            .ok_or(SliceError::BadParameter("row end overflow"))?;

        output.extend_from_slice(
            w.get(start..end)
                .ok_or(SliceError::BadShape("row outside matrix"))?,
        );
    }

    Ok(output)
}

/// Keeps selected columns of a row-major [d_model, d_ff] matrix.
///
/// Indices must be non-empty, strictly ascending, and less than d_ff.
/// Matrix length must equal exactly d_model * d_ff.
///
/// Does not prove that removing neurons preserves model quality.
///
/// # Errors
/// A [`SliceError`] for a wrong length, empty or bad `keep`.
pub fn slice_down(
    w: &[f32],
    d_model: usize,
    d_ff: usize,
    keep: &[usize],
) -> Result<Vec<f32>, SliceError> {
    validate_matrix(w, d_model, d_ff, d_ff, keep)?;

    let capacity = d_model
        .checked_mul(keep.len())
        .ok_or(SliceError::BadParameter("output size overflow"))?;

    let mut output = Vec::with_capacity(capacity);

    for row in 0..d_model {
        let start = row
            .checked_mul(d_ff)
            .ok_or(SliceError::BadParameter("row offset overflow"))?;

        for &column in keep {
            let index = start
                .checked_add(column)
                .ok_or(SliceError::BadParameter("column offset overflow"))?;

            output.push(
                *w.get(index)
                    .ok_or(SliceError::BadShape("column outside matrix"))?,
            );
        }
    }

    Ok(output)
}

/// Slices Wg, Wu, and Wd consistently.
///
/// Returns the matrices in `(Wg, Wu, Wd)` order.
///
/// Output dimensions are `[keep.len(), d_model]`,
/// `[keep.len(), d_model]`, and `[d_model, keep.len()]`.
///
/// Does not execute the model or establish quality preservation.
///
/// # Errors
/// A [`SliceError`] from any of the three slices.
pub fn slice_layer(
    wg: &[f32],
    wu: &[f32],
    wd: &[f32],
    d_model: usize,
    d_ff: usize,
    keep: &[usize],
) -> Result<SlicedLayer, SliceError> {
    let new_wg = slice_gate_or_up(wg, d_ff, d_model, keep)?;
    let new_wu = slice_gate_or_up(wu, d_ff, d_model, keep)?;
    let new_wd = slice_down(wd, d_model, d_ff, keep)?;

    Ok((new_wg, new_wu, new_wd))
}

fn validate_matrix(
    w: &[f32],
    rows: usize,
    cols: usize,
    hidden: usize,
    keep: &[usize],
) -> Result<(), SliceError> {
    if rows == 0 || cols == 0 || hidden == 0 {
        return Err(SliceError::BadParameter("matrix dimensions"));
    }

    let expected = rows
        .checked_mul(cols)
        .ok_or(SliceError::BadParameter("matrix size overflow"))?;

    if w.len() != expected {
        return Err(SliceError::BadShape(
            "matrix length does not match dimensions",
        ));
    }

    if keep.is_empty() {
        return Err(SliceError::EmptyInput("kept neuron indices"));
    }

    let mut previous = None;

    for &index in keep {
        if index >= hidden {
            return Err(SliceError::BadParameter("kept neuron index out of range"));
        }

        if let Some(previous_index) = previous {
            if index <= previous_index {
                return Err(SliceError::BadParameter(
                    "keep indices must be strictly ascending",
                ));
            }
        }

        previous = Some(index);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::{make_plan, Rule};
    use crate::profile::Profiler;
    use crate::ActivationSink;

    fn silu(x: f64) -> f64 {
        x / (1.0 + (-x).exp())
    }

    fn mlp(wg: &[f32], wu: &[f32], wd: &[f32], d_model: usize, d_ff: usize, x: &[f64]) -> Vec<f64> {
        let mut hidden = vec![0.0; d_ff];

        for neuron in 0..d_ff {
            let mut gate = 0.0;
            let mut up = 0.0;

            for input in 0..d_model {
                gate += f64::from(wg[neuron * d_model + input]) * x[input];
                up += f64::from(wu[neuron * d_model + input]) * x[input];
            }

            hidden[neuron] = silu(gate) * up;
        }

        let mut output = vec![0.0; d_model];

        for row in 0..d_model {
            for neuron in 0..d_ff {
                output[row] += f64::from(wd[row * d_ff + neuron]) * hidden[neuron];
            }
        }

        output
    }

    #[test]
    fn identity_preserves_matrices() {
        let wg = vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0];
        let wu = vec![7.0, 8.0, 9.0, 10.0, 11.0, 12.0];
        let wd = vec![13.0, 14.0, 15.0, 16.0, 17.0, 18.0];

        let (a, b, c) = slice_layer(&wg, &wu, &wd, 2, 3, &[0, 1, 2]).unwrap();

        assert_eq!(a, wg);
        assert_eq!(b, wu);
        assert_eq!(c, wd);
    }

    #[test]
    fn zeroed_removed_neurons_preserve_output() {
        let d_model = 3;
        let d_ff = 4;

        let wg = vec![1.0, 2.0, 3.0, 0.0, 0.0, 0.0, 2.0, 1.0, 3.0, 0.0, 0.0, 0.0];

        let wu = vec![1.0, 2.0, 1.0, 3.0, 4.0, 5.0, 2.0, 3.0, 1.0, 6.0, 7.0, 8.0];

        let wd = vec![1.0, 2.0, 3.0, 4.0, 2.0, 3.0, 4.0, 5.0, 3.0, 4.0, 5.0, 6.0];

        let x = vec![0.5, 1.5, -0.25];
        let keep = vec![0, 2];

        let full = mlp(&wg, &wu, &wd, d_model, d_ff, &x);

        let (a, b, c) = slice_layer(&wg, &wu, &wd, d_model, d_ff, &keep).unwrap();

        let sliced = mlp(&a, &b, &c, d_model, keep.len(), &x);

        for (left, right) in full.iter().zip(&sliced) {
            assert!((left - right).abs() < 1e-10);
        }
    }

    #[test]
    fn removing_active_neurons_changes_output() {
        let wg = vec![1.0, 2.0, 3.0, 4.0];
        let wu = vec![1.0, 1.0, 2.0, 2.0];
        let wd = vec![1.0, 2.0, 3.0, 4.0];
        let x = vec![1.0, 0.5];

        let full = mlp(&wg, &wu, &wd, 2, 2, &x);

        let (a, b, c) = slice_layer(&wg, &wu, &wd, 2, 2, &[0]).unwrap();

        let sliced = mlp(&a, &b, &c, 2, 1, &x);

        assert_ne!(full, sliced);
    }

    #[test]
    fn rejects_unsorted_duplicate_and_out_of_range_indices() {
        let matrix = vec![1.0; 12];

        assert!(slice_gate_or_up(&matrix, 4, 3, &[2, 1]).is_err());
        assert!(slice_gate_or_up(&matrix, 4, 3, &[1, 1]).is_err());
        assert!(slice_gate_or_up(&matrix, 4, 3, &[4]).is_err());
        assert!(slice_down(&matrix, 3, 4, &[4]).is_err());
    }

    #[test]
    fn out_of_range_is_caught_when_d_model_exceeds_d_ff() {
        // d_model 5, d_ff 2: index 3 is outside d_ff but inside d_model.
        let down = vec![1.0; 5 * 2];
        assert!(slice_down(&down, 5, 2, &[3]).is_err());
        let gate = vec![1.0; 2 * 5];
        assert!(slice_gate_or_up(&gate, 2, 5, &[3]).is_err());
    }

    #[test]
    fn rejects_bad_shapes_and_empty_keep() {
        assert!(slice_gate_or_up(&[1.0], 4, 2, &[0]).is_err());
        assert!(slice_gate_or_up(&[1.0; 8], 4, 2, &[]).is_err());
        assert!(slice_gate_or_up(&[], 0, 2, &[0]).is_err());
    }

    #[test]
    fn row_and_column_shapes_are_correct() {
        let wg: Vec<f32> = (0..12).map(|x| x as f32).collect();
        let wd: Vec<f32> = (0..12).map(|x| x as f32).collect();

        assert_eq!(
            slice_gate_or_up(&wg, 4, 3, &[1, 3]).unwrap(),
            vec![3.0, 4.0, 5.0, 9.0, 10.0, 11.0]
        );

        assert_eq!(
            slice_down(&wd, 3, 4, &[1, 3]).unwrap(),
            vec![1.0, 3.0, 5.0, 7.0, 9.0, 11.0]
        );
    }

    // Deterministic xorshift generator. No external random dependency.
    struct XorShift64(u64);

    impl XorShift64 {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }

        fn small(&mut self, maximum: usize) -> usize {
            (self.next() as usize % maximum) + 1
        }

        fn value(&mut self) -> f32 {
            let value = (self.next() % 2001) as i32 - 1000;
            value as f32 / 1000.0
        }
    }

    #[test]
    fn deterministic_random_plans_produce_correct_matrix_sizes() {
        let mut rng = XorShift64(0x1234_5678_9ABC_DEF1);

        for _case in 0..100 {
            let layers = rng.small(5);
            let d_ff = rng.small(24);
            let d_model = rng.small(12);

            let mut profiler = Profiler::new(layers, d_ff).unwrap();

            for layer in 0..layers {
                let activations: Vec<f32> = (0..d_ff).map(|_| rng.value()).collect();

                profiler.observe(layer, &activations).unwrap();
            }

            let fraction = rng.small(10) as f64 / 10.0;

            for rule in [Rule::Uniform, Rule::Budget] {
                let plan = make_plan(&profiler, fraction, rule).unwrap();

                assert_eq!(plan.keep.len(), layers);

                for keep in &plan.keep {
                    assert!(!keep.is_empty());
                    assert!(keep.iter().all(|&index| index < d_ff));
                    assert!(keep.windows(2).all(|pair| pair[0] < pair[1]));

                    let wg: Vec<f32> = (0..d_ff * d_model).map(|_| rng.value()).collect();
                    let wu: Vec<f32> = (0..d_ff * d_model).map(|_| rng.value()).collect();
                    let wd: Vec<f32> = (0..d_model * d_ff).map(|_| rng.value()).collect();

                    let (new_wg, new_wu, new_wd) =
                        slice_layer(&wg, &wu, &wd, d_model, d_ff, keep).unwrap();

                    assert_eq!(new_wg.len(), keep.len() * d_model);
                    assert_eq!(new_wu.len(), keep.len() * d_model);
                    assert_eq!(new_wd.len(), d_model * keep.len());
                }
            }
        }
    }
}
