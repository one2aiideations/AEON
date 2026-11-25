//! Floating-point math for `no_std` builds (no `libm`, no new dependencies).
//!
//! `core` has no `sqrt`, `sin`, `cos`, `tan`, `atan`, `atan2` or `powi`. This module implements
//! them from basic arithmetic and exposes them through [`FloatMath`], so code written as
//! `x.sin()` builds with or without the standard library: when `std` is available its inherent
//! methods win and this trait is simply unused.
//!
//! Accuracy (measured against the standard library in the tests): about 1e-15 absolute for
//! `sin`, `cos`, `atan` and `atan2` on moderate arguments, and within a few ulp for `sqrt`.
//! Arguments of magnitude above 1e6 lose accuracy to the range reduction. These routines are
//! deterministic but are not a certified math library.

use core::f64::consts::{FRAC_PI_2, FRAC_PI_4, PI, TAU};

/// Two-part split of pi/2 (fdlibm constants) for accurate argument reduction.
const PIO2_HI: f64 = 1.570_796_326_734_125_6;
const PIO2_LO: f64 = 6.077_100_506_506_192e-11;
const TWO_OVER_PI: f64 = 0.636_619_772_367_581_3;

/// Square root by Newton iteration from a bit-level first guess.
pub fn sqrt(x: f64) -> f64 {
    if x.is_nan() || x < 0.0 {
        return f64::NAN;
    }
    if x == 0.0 || x.is_infinite() {
        return x;
    }
    let mut g = f64::from_bits((x.to_bits() >> 1) + 0x1FF8_0000_0000_0000);
    for _ in 0..8 {
        g = 0.5 * (g + x / g);
    }
    g
}

fn sin_poly(r: f64) -> f64 {
    let r2 = r * r;
    // Taylor series through r^15 (truncation error below 5e-17 for |r| <= pi/4).
    let mut term = r;
    let mut sum = r;
    let mut n = 1.0;
    for _ in 0..7 {
        term *= -r2 / ((n + 1.0) * (n + 2.0));
        sum += term;
        n += 2.0;
    }
    sum
}

fn cos_poly(r: f64) -> f64 {
    let r2 = r * r;
    // Taylor series through r^16.
    let mut term = 1.0;
    let mut sum = 1.0;
    let mut n = 0.0;
    for _ in 0..8 {
        term *= -r2 / ((n + 1.0) * (n + 2.0));
        sum += term;
        n += 2.0;
    }
    sum
}

/// Reduces `x` to `r` in about [-pi/4, pi/4] and the quadrant (0..=3).
fn reduce(x: f64) -> (f64, u32) {
    let x = if x.abs() > 1.0e6 { x % TAU } else { x };
    // Round to nearest by hand: `f64::round` is not in `core`. |x| <= 1e6 here, so the
    // truncating conversions below are exact and cannot overflow.
    let scaled = x * TWO_OVER_PI;
    let k = if scaled >= 0.0 {
        (scaled + 0.5) as i64
    } else {
        (scaled - 0.5) as i64
    } as f64;
    let r = (x - k * PIO2_HI) - k * PIO2_LO;
    // `k` is small (at most about 1e6 * 2/pi), so the conversion is exact.
    let q = (k as i64).rem_euclid(4) as u32;
    (r, q)
}

/// Sine. Non-finite input gives NaN.
pub fn sin(x: f64) -> f64 {
    if !x.is_finite() {
        return f64::NAN;
    }
    let (r, q) = reduce(x);
    match q {
        0 => sin_poly(r),
        1 => cos_poly(r),
        2 => -sin_poly(r),
        _ => -cos_poly(r),
    }
}

/// Cosine. Non-finite input gives NaN.
pub fn cos(x: f64) -> f64 {
    if !x.is_finite() {
        return f64::NAN;
    }
    let (r, q) = reduce(x);
    match q {
        0 => cos_poly(r),
        1 => -sin_poly(r),
        2 => -cos_poly(r),
        _ => sin_poly(r),
    }
}

/// Tangent, as sine over cosine.
pub fn tan(x: f64) -> f64 {
    sin(x) / cos(x)
}

/// Arctangent, range (-pi/2, pi/2).
pub fn atan(x: f64) -> f64 {
    if x.is_nan() {
        return f64::NAN;
    }
    let a = x.abs();
    let v = if a.is_infinite() {
        FRAC_PI_2
    } else if a > 1.0 {
        FRAC_PI_2 - atan_unit(1.0 / a)
    } else {
        atan_unit(a)
    };
    if x.is_sign_negative() {
        -v
    } else {
        v
    }
}

/// Arctangent for 0 <= x <= 1: two argument halvings, then a short series.
fn atan_unit(x: f64) -> f64 {
    let mut h = x;
    for _ in 0..2 {
        h /= 1.0 + sqrt(1.0 + h * h);
    }
    let h2 = h * h;
    let mut term = h;
    let mut sum = h;
    let mut k = 1.0;
    for _ in 0..12 {
        term *= -h2;
        k += 2.0;
        sum += term / k;
    }
    4.0 * sum
}

/// Four-quadrant arctangent of `y / x`, range [-pi, pi], with the usual signed-zero rules.
pub fn atan2(y: f64, x: f64) -> f64 {
    if x.is_nan() || y.is_nan() {
        return f64::NAN;
    }
    if x == 0.0 {
        if y == 0.0 {
            return if x.is_sign_negative() {
                if y.is_sign_negative() {
                    -PI
                } else {
                    PI
                }
            } else {
                y
            };
        }
        return if y > 0.0 { FRAC_PI_2 } else { -FRAC_PI_2 };
    }
    if y == 0.0 {
        return if x > 0.0 {
            y
        } else if y.is_sign_negative() {
            -PI
        } else {
            PI
        };
    }
    if x.is_infinite() {
        if y.is_infinite() {
            let q = if x > 0.0 { FRAC_PI_4 } else { 3.0 * FRAC_PI_4 };
            return if y > 0.0 { q } else { -q };
        }
        return if x > 0.0 {
            0.0 * y.signum()
        } else if y > 0.0 {
            PI
        } else {
            -PI
        };
    }
    if y.is_infinite() {
        return if y > 0.0 { FRAC_PI_2 } else { -FRAC_PI_2 };
    }
    let base = atan(y / x);
    if x > 0.0 {
        base
    } else if y > 0.0 {
        base + PI
    } else {
        base - PI
    }
}

/// Wraps an angle in radians into [-pi, pi] without looping, using `fmod`.
///
/// Non-finite input (NaN, infinity) is returned unchanged so that a validity check downstream
/// can reject it; looping `while x > pi { x -= 2*pi }` would never finish for infinity, or for
/// values so large that subtracting 2*pi no longer changes them.
pub fn wrap_pi(x: f32) -> f32 {
    if !x.is_finite() {
        return x;
    }
    let mut r = f64::from(x) % TAU;
    if r > PI {
        r -= TAU;
    } else if r < -PI {
        r += TAU;
    }
    r as f32
}

/// Integer power by repeated squaring.
pub fn powi(x: f64, n: i32) -> f64 {
    let mut e = n.unsigned_abs();
    let mut base = x;
    let mut acc = 1.0;
    while e > 0 {
        if e & 1 == 1 {
            acc *= base;
        }
        base *= base;
        e >>= 1;
    }
    if n < 0 {
        1.0 / acc
    } else {
        acc
    }
}

/// Math methods for `f32` and `f64`, usable as `x.sin()` when `std` is not available.
pub trait FloatMath: Sized {
    /// Square root.
    fn sqrt(self) -> Self;
    /// Sine (radians).
    fn sin(self) -> Self;
    /// Cosine (radians).
    fn cos(self) -> Self;
    /// Tangent (radians).
    fn tan(self) -> Self;
    /// Arctangent.
    fn atan(self) -> Self;
    /// Four-quadrant arctangent of `self / other`.
    fn atan2(self, other: Self) -> Self;
    /// Integer power.
    fn powi(self, n: i32) -> Self;
}

impl FloatMath for f64 {
    fn sqrt(self) -> f64 {
        sqrt(self)
    }
    fn sin(self) -> f64 {
        sin(self)
    }
    fn cos(self) -> f64 {
        cos(self)
    }
    fn tan(self) -> f64 {
        tan(self)
    }
    fn atan(self) -> f64 {
        atan(self)
    }
    fn atan2(self, other: f64) -> f64 {
        atan2(self, other)
    }
    fn powi(self, n: i32) -> f64 {
        powi(self, n)
    }
}

impl FloatMath for f32 {
    fn sqrt(self) -> f32 {
        sqrt(f64::from(self)) as f32
    }
    fn sin(self) -> f32 {
        sin(f64::from(self)) as f32
    }
    fn cos(self) -> f32 {
        cos(f64::from(self)) as f32
    }
    fn tan(self) -> f32 {
        tan(f64::from(self)) as f32
    }
    fn atan(self) -> f32 {
        atan(f64::from(self)) as f32
    }
    fn atan2(self, other: f32) -> f32 {
        atan2(f64::from(self), f64::from(other)) as f32
    }
    fn powi(self, n: i32) -> f32 {
        powi(f64::from(self), n) as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Maximum absolute error of `a` against `b` over `points`.
    fn max_err(points: &[f64], a: impl Fn(f64) -> f64, b: impl Fn(f64) -> f64) -> f64 {
        points
            .iter()
            .map(|&x| (a(x) - b(x)).abs())
            .fold(0.0, f64::max)
    }

    fn sweep(lo: f64, hi: f64, n: usize) -> Vec<f64> {
        (0..=n)
            .map(|i| lo + (hi - lo) * (i as f64) / (n as f64))
            .collect()
    }

    #[test]
    fn sqrt_matches_std_within_a_few_ulp() {
        for x in sweep(0.0, 1000.0, 20_000).into_iter().chain([
            1e-300,
            1e-10,
            1e10,
            1e300,
            2.0,
            0.5,
            f64::MIN_POSITIVE,
        ]) {
            let (a, b) = (sqrt(x), x.sqrt());
            assert!(
                (a - b).abs() <= 4.0 * f64::EPSILON * b.abs(),
                "sqrt({x}) = {a}, std {b}"
            );
        }
        assert_eq!(sqrt(0.0), 0.0);
        assert_eq!(sqrt(4.0), 2.0);
        assert!(sqrt(-1.0).is_nan() && sqrt(f64::NAN).is_nan());
        assert_eq!(sqrt(f64::INFINITY), f64::INFINITY);
        assert!(sqrt(-0.0) == 0.0);
    }

    #[test]
    fn sin_and_cos_match_std() {
        let xs = sweep(-100.0, 100.0, 40_000);
        assert!(
            max_err(&xs, sin, f64::sin) < 1e-14,
            "sin {}",
            max_err(&xs, sin, f64::sin)
        );
        assert!(
            max_err(&xs, cos, f64::cos) < 1e-14,
            "cos {}",
            max_err(&xs, cos, f64::cos)
        );
        let small = sweep(-1.0, 1.0, 20_000);
        assert!(max_err(&small, sin, f64::sin) < 1e-15);
        assert_eq!(sin(0.0), 0.0);
        assert_eq!(cos(0.0), 1.0);
    }

    #[test]
    fn exact_landmarks() {
        assert!((sin(PI / 6.0) - 0.5).abs() < 1e-15);
        assert!((cos(PI / 3.0) - 0.5).abs() < 1e-15);
        assert!((sin(PI / 2.0) - 1.0).abs() < 1e-15);
        assert!(cos(PI / 2.0).abs() < 1e-15);
        assert!(sin(PI).abs() < 1e-15);
        assert!((cos(PI) + 1.0).abs() < 1e-15);
        assert!((tan(PI / 4.0) - 1.0).abs() < 1e-15);
        assert!((atan(1.0) - PI / 4.0).abs() < 1e-15);
    }

    #[test]
    fn large_arguments_stay_usable() {
        for x in [1.0e5, -1.0e5, 3.0e6, -3.0e6, 1.0e9] {
            let s = sin(x);
            let c = cos(x);
            assert!((s * s + c * c - 1.0).abs() < 1e-12, "x={x}");
            assert!((s - x.sin()).abs() < 1e-3, "x={x}: {s} vs {}", x.sin());
        }
    }

    #[test]
    fn non_finite_inputs() {
        for f in [sin, cos, tan] {
            assert!(
                f(f64::NAN).is_nan() && f(f64::INFINITY).is_nan() && f(f64::NEG_INFINITY).is_nan()
            );
        }
    }

    #[test]
    fn atan_matches_std_including_extremes() {
        let xs = sweep(-50.0, 50.0, 40_000);
        assert!(
            max_err(&xs, atan, f64::atan) < 1e-15,
            "{}",
            max_err(&xs, atan, f64::atan)
        );
        for x in [1e-300, 1e300, f64::INFINITY, f64::NEG_INFINITY, 0.0, -0.0] {
            assert!((atan(x) - x.atan()).abs() < 1e-15, "{x}");
        }
        assert!(atan(f64::NAN).is_nan());
        assert!(atan(-0.0).is_sign_negative());
    }

    #[test]
    fn atan2_matches_std_in_every_quadrant_and_on_the_axes() {
        let vals = [
            -7.5, -2.0, -1.0, -0.25, -0.0, 0.0, 0.25, 1.0, 2.0, 7.5, 1e-9, -1e-9, 1e9, -1e9,
        ];
        for &y in &vals {
            for &x in &vals {
                let (a, b) = (atan2(y, x), y.atan2(x));
                assert!((a - b).abs() < 1e-14, "atan2({y}, {x}) = {a}, std {b}");
                assert_eq!(
                    a.is_sign_negative(),
                    b.is_sign_negative() && b != 0.0 || (b == 0.0 && b.is_sign_negative()),
                    "sign of atan2({y}, {x})"
                );
            }
        }
        let inf = f64::INFINITY;
        for &(y, x) in &[
            (inf, inf),
            (inf, -inf),
            (-inf, inf),
            (-inf, -inf),
            (1.0, inf),
            (-1.0, inf),
            (1.0, -inf),
            (-1.0, -inf),
            (inf, 1.0),
            (-inf, 1.0),
        ] {
            assert!((atan2(y, x) - y.atan2(x)).abs() < 1e-15, "atan2({y}, {x})");
        }
        assert!(atan2(f64::NAN, 1.0).is_nan() && atan2(1.0, f64::NAN).is_nan());
    }

    #[test]
    fn powi_is_exact_for_small_cases_and_handles_extremes() {
        assert_eq!(powi(2.0, 10), 1024.0);
        assert_eq!(powi(2.0, -3), 0.125);
        assert_eq!(powi(7.0, 0), 1.0);
        assert_eq!(powi(0.0, 0), 1.0);
        assert_eq!(powi(-3.0, 3), -27.0);
        assert_eq!(powi(-3.0, 4), 81.0);
        assert_eq!(powi(0.0, -1), f64::INFINITY);
        assert_eq!(powi(1.0, i32::MIN), 1.0);
        assert_eq!(powi(-1.0, i32::MAX), -1.0);
        for x in sweep(0.1, 5.0, 100) {
            for n in -8..=8 {
                let (a, b) = (powi(x, n), x.powi(n));
                assert!((a - b).abs() <= 1e-12 * b.abs().max(1.0), "{x}^{n}");
            }
        }
    }

    #[test]
    fn wrap_pi_stays_in_range_and_preserves_the_angle() {
        let mut x = -1000.0f32;
        while x <= 1000.0 {
            let w = wrap_pi(x);
            assert!(
                (-core::f32::consts::PI - 1e-6..=core::f32::consts::PI + 1e-6).contains(&w),
                "wrap_pi({x}) = {w}"
            );
            // The wrapped angle has the same sine and cosine as the original.
            assert!(
                (f64::from(w).sin() - f64::from(x).sin()).abs() < 2e-4,
                "sin at {x}"
            );
            x += 0.37;
        }
        assert_eq!(wrap_pi(0.0), 0.0);
        assert!((wrap_pi(core::f32::consts::TAU) - 0.0).abs() < 1e-5);
        assert!((wrap_pi(3.5) - (3.5 - core::f32::consts::TAU)).abs() < 1e-5);
        assert!((wrap_pi(-3.5) - (-3.5 + core::f32::consts::TAU)).abs() < 1e-5);
    }

    #[test]
    fn wrap_pi_never_loops_on_bad_input() {
        assert!(wrap_pi(f32::NAN).is_nan());
        assert_eq!(wrap_pi(f32::INFINITY), f32::INFINITY);
        assert_eq!(wrap_pi(f32::NEG_INFINITY), f32::NEG_INFINITY);
        for huge in [1e30f32, -1e30, f32::MAX, f32::MIN] {
            let w = wrap_pi(huge);
            assert!(
                w.is_finite() && w.abs() <= core::f32::consts::PI + 1e-6,
                "{huge} -> {w}"
            );
        }
    }

    #[test]
    fn the_trait_gives_the_same_answers_for_f32_and_f64() {
        use FloatMath as M;
        assert!((M::sin(1.0f64) - 1.0f64.sin()).abs() < 1e-15);
        assert!((M::sin(1.0f32) - 1.0f32.sin()).abs() < 1e-6);
        assert!((M::atan2(1.0f32, -1.0f32) - 1.0f32.atan2(-1.0)).abs() < 1e-6);
        assert!((M::powi(1.5f32, 5) - 1.5f32.powi(5)).abs() < 1e-5);
        assert_eq!(M::sqrt(9.0f32), 3.0);
        assert!((M::tan(0.5f32) - 0.5f32.tan()).abs() < 1e-6);
        assert!((M::cos(2.0f32) - 2.0f32.cos()).abs() < 1e-6);
        assert!((M::atan(0.3f32) - 0.3f32.atan()).abs() < 1e-6);
    }
}
