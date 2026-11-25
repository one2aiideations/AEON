//! Quantisation error.

use crate::AnalysisError;

/// Relative L2 error of symmetric per-tensor integer quantisation at `bits` bits:
/// `‖dequant(quant(x)) - x‖ / (‖x‖ + 1e-12)`, with `qmax = 2^(bits-1) - 1` and
/// `scale = max|x| / qmax` (floored at 1e-12).
///
/// This measures representational error only; it never alters the weights.
/// `bits` must be 2..=16. Empty input gives `0.0`.
pub fn symmetric_quantization_error(x: &[f32], bits: u32) -> Result<f64, AnalysisError> {
    if !(2..=16).contains(&bits) {
        return Err(AnalysisError::BadParameter("bits must be 2..=16"));
    }
    if x.iter().any(|v| !v.is_finite()) {
        return Err(AnalysisError::NonFinite);
    }
    let qmax = f64::from((1u32 << (bits - 1)) - 1);
    let max_abs = x.iter().map(|v| f64::from(v.abs())).fold(0.0, f64::max);
    let scale = (max_abs / qmax.max(1.0)).max(1e-12);
    let (mut err, mut norm) = (0.0f64, 0.0f64);
    for v in x {
        let v = f64::from(*v);
        let q = (v / scale).round().clamp(-qmax, qmax);
        err += (q * scale - v).powi(2);
        norm += v * v;
    }
    Ok(err.sqrt() / (norm.sqrt() + 1e-12))
}

/// Mean and maximum of [`symmetric_quantization_error`] over several tensors, with the
/// number measured. Empty tensors are skipped. `None` mean/max when nothing was measured.
pub fn quantization_summary(
    tensors: &[&[f32]],
    bits: u32,
) -> Result<(Option<f64>, Option<f64>, usize), AnalysisError> {
    let mut errs = Vec::new();
    for t in tensors.iter().filter(|t| !t.is_empty()) {
        errs.push(symmetric_quantization_error(t, bits)?);
    }
    if errs.is_empty() {
        return Ok((None, None, 0));
    }
    let mean = errs.iter().sum::<f64>() / errs.len() as f64;
    let max = errs.iter().copied().fold(f64::MIN, f64::max);
    Ok((Some(mean), Some(max), errs.len()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exactly_representable_values_have_zero_error() {
        // 8-bit: qmax 127, values k * (127/127) are exact when max_abs = 127
        let x: Vec<f32> = (-127..=127).map(|v| v as f32).collect();
        assert!(symmetric_quantization_error(&x, 8).unwrap() < 1e-9);
    }

    #[test]
    fn more_bits_means_less_error() {
        let x: Vec<f32> = (0..500)
            .map(|i| ((i * 37) % 200) as f32 / 10.0 - 10.0)
            .collect();
        let e4 = symmetric_quantization_error(&x, 4).unwrap();
        let e8 = symmetric_quantization_error(&x, 8).unwrap();
        let e16 = symmetric_quantization_error(&x, 16).unwrap();
        assert!(e4 > e8 && e8 > e16, "{e4} {e8} {e16}");
        assert!(e16 < 1e-4);
    }

    #[test]
    fn two_bit_is_ternary() {
        // bits=2 -> qmax 1: values snap to {-s, 0, s}, s = max|x|
        let e = symmetric_quantization_error(&[1.0, -1.0, 0.4, -0.4], 2).unwrap();
        // errors 0, 0, 0.4, 0.4 over norm sqrt(2.32)
        let expected = (0.32f64).sqrt() / (2.32f64).sqrt();
        assert!((e - expected).abs() < 1e-6, "{e} vs {expected}");
    }

    #[test]
    fn zeros_and_empty() {
        assert_eq!(symmetric_quantization_error(&[0.0; 8], 8).unwrap(), 0.0);
        assert_eq!(symmetric_quantization_error(&[], 8).unwrap(), 0.0);
    }

    #[test]
    fn rejects_bad_parameters() {
        assert!(symmetric_quantization_error(&[1.0], 1).is_err());
        assert!(symmetric_quantization_error(&[1.0], 17).is_err());
        assert_eq!(
            symmetric_quantization_error(&[f32::NAN], 8).unwrap_err(),
            AnalysisError::NonFinite
        );
    }

    #[test]
    fn summary_skips_empty_tensors() {
        let a: Vec<f32> = (0..64).map(|i| i as f32).collect();
        let b: Vec<f32> = (0..64).map(|i| (i * i % 13) as f32).collect();
        let (mean, max, n) = quantization_summary(&[&a, &[], &b], 4).unwrap();
        assert_eq!(n, 2);
        let (ea, eb) = (
            symmetric_quantization_error(&a, 4).unwrap(),
            symmetric_quantization_error(&b, 4).unwrap(),
        );
        assert!((mean.unwrap() - (ea + eb) / 2.0).abs() < 1e-12);
        assert!((max.unwrap() - ea.max(eb)).abs() < 1e-12);
        assert_eq!(quantization_summary(&[], 4).unwrap(), (None, None, 0));
    }
}
