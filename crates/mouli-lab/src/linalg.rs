//! Small dense matrices (row-major, f64).

use crate::F;

#[derive(Clone, Debug)]
pub struct Mat {
    pub r: usize,
    pub c: usize,
    pub d: Vec<F>,
}

impl Mat {
    pub fn zeros(r: usize, c: usize) -> Self {
        Mat {
            r,
            c,
            d: vec![0.0; r * c],
        }
    }

    pub fn from_vec(r: usize, c: usize, d: Vec<F>) -> Self {
        assert_eq!(r * c, d.len(), "Mat::from_vec: r*c != d.len()");
        Mat { r, c, d }
    }

    /// self * b
    pub fn matmul(&self, b: &Mat) -> Mat {
        assert_eq!(
            self.c, b.r,
            "matmul: dim mismatch ({}x{})*({}x{})",
            self.r, self.c, b.r, b.c
        );
        let mut out = Mat::zeros(self.r, b.c);
        for i in 0..self.r {
            for k in 0..self.c {
                let a = self.d[i * self.c + k];
                if a == 0.0 {
                    continue;
                }
                for j in 0..b.c {
                    out.d[i * b.c + j] += a * b.d[k * b.c + j];
                }
            }
        }
        out
    }

    /// self^T * b
    pub fn t_matmul(&self, b: &Mat) -> Mat {
        assert_eq!(self.r, b.r, "t_matmul: dim mismatch");
        let mut out = Mat::zeros(self.c, b.c);
        for k in 0..self.r {
            for i in 0..self.c {
                let a = self.d[k * self.c + i];
                if a == 0.0 {
                    continue;
                }
                for j in 0..b.c {
                    out.d[i * b.c + j] += a * b.d[k * b.c + j];
                }
            }
        }
        out
    }

    /// self * b^T
    pub fn matmul_t(&self, b: &Mat) -> Mat {
        assert_eq!(self.c, b.c, "matmul_t: dim mismatch");
        let mut out = Mat::zeros(self.r, b.r);
        let k = self.c;
        for i in 0..self.r {
            let a = &self.d[i * k..(i + 1) * k];
            let o = &mut out.d[i * b.r..(i + 1) * b.r];
            // Four outputs at a time: each keeps its own accumulator and adds its terms in the
            // same order (k = 0, 1, ...) as the plain loop, so every result is bit-identical;
            // the four independent chains just let the processor overlap the additions.
            let mut j = 0;
            while j + 4 <= b.r {
                let b0 = &b.d[j * k..(j + 1) * k];
                let b1 = &b.d[(j + 1) * k..(j + 2) * k];
                let b2 = &b.d[(j + 2) * k..(j + 3) * k];
                let b3 = &b.d[(j + 3) * k..(j + 4) * k];
                let (mut s0, mut s1, mut s2, mut s3) = (0.0, 0.0, 0.0, 0.0);
                for t in 0..k {
                    let x = a[t];
                    s0 += x * b0[t];
                    s1 += x * b1[t];
                    s2 += x * b2[t];
                    s3 += x * b3[t];
                }
                o[j] = s0;
                o[j + 1] = s1;
                o[j + 2] = s2;
                o[j + 3] = s3;
                j += 4;
            }
            while j < b.r {
                let bj = &b.d[j * k..(j + 1) * k];
                let mut acc = 0.0;
                for t in 0..k {
                    acc += a[t] * bj[t];
                }
                o[j] = acc;
                j += 1;
            }
        }
        out
    }

    pub fn transpose(&self) -> Mat {
        let mut out = Mat::zeros(self.c, self.r);
        for i in 0..self.r {
            for j in 0..self.c {
                out.d[j * self.r + i] = self.d[i * self.c + j];
            }
        }
        out
    }

    pub fn colsum(&self) -> Vec<F> {
        let mut v = vec![0.0; self.c];
        for i in 0..self.r {
            for j in 0..self.c {
                v[j] += self.d[i * self.c + j];
            }
        }
        v
    }
}

/// Row-wise softmax, max-subtracted; degenerate rows become uniform.
pub fn softmax_rows(m: &Mat) -> Mat {
    let mut out = Mat::zeros(m.r, m.c);
    for i in 0..m.r {
        let mut mx = f64::NEG_INFINITY;
        for j in 0..m.c {
            if m.d[i * m.c + j] > mx {
                mx = m.d[i * m.c + j];
            }
        }
        if !mx.is_finite() {
            mx = 0.0;
        }
        let mut sum = 0.0;
        for j in 0..m.c {
            let e = (m.d[i * m.c + j] - mx).exp();
            out.d[i * m.c + j] = e;
            sum += e;
        }
        if sum <= 0.0 || !sum.is_finite() {
            let u = 1.0 / m.c.max(1) as F;
            for j in 0..m.c {
                out.d[i * m.c + j] = u;
            }
        } else {
            for j in 0..m.c {
                out.d[i * m.c + j] /= sum;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rng::Rng;

    /// The plain loop this module started with, kept as the reference.
    fn matmul_t_reference(a: &Mat, b: &Mat) -> Mat {
        let mut out = Mat::zeros(a.r, b.r);
        for i in 0..a.r {
            for j in 0..b.r {
                let mut acc = 0.0;
                for k in 0..a.c {
                    acc += a.d[i * a.c + k] * b.d[j * b.c + k];
                }
                out.d[i * b.r + j] = acc;
            }
        }
        out
    }

    fn random(r: usize, c: usize, rng: &mut Rng) -> Mat {
        Mat::from_vec(r, c, (0..r * c).map(|_| rng.normal() * 3.0).collect())
    }

    #[test]
    fn matmul_t_is_bit_identical_to_the_plain_loop_for_every_shape_class() {
        let mut rng = Rng::new(5);
        // Row counts of b below, at and above a multiple of 4; odd inner sizes; empty edges.
        for (r, c, br) in [
            (1, 1, 1),
            (3, 7, 3),
            (5, 9, 4),
            (4, 16, 5),
            (6, 33, 8),
            (7, 13, 11),
            (2, 0, 3),
            (0, 4, 4),
            (3, 5, 0),
        ] {
            let a = random(r, c, &mut rng);
            let b = random(br, c, &mut rng);
            let fast = a.matmul_t(&b);
            let slow = matmul_t_reference(&a, &b);
            assert_eq!((fast.r, fast.c), (slow.r, slow.c));
            for (x, y) in fast.d.iter().zip(&slow.d) {
                assert_eq!(x.to_bits(), y.to_bits(), "{r}x{c} * ({br}x{c})^T");
            }
        }
    }

    #[test]
    fn nan_and_infinity_propagate_exactly_as_before() {
        let mut rng = Rng::new(6);
        let mut a = random(3, 9, &mut rng);
        let mut b = random(6, 9, &mut rng);
        a.d[4] = F::NAN;
        b.d[2 * 9 + 3] = F::INFINITY;
        b.d[5 * 9] = F::NEG_INFINITY;
        a.d[9 + 3] = 0.0; // 0 * inf must still give NaN, not be skipped
        let fast = a.matmul_t(&b);
        let slow = matmul_t_reference(&a, &b);
        for (x, y) in fast.d.iter().zip(&slow.d) {
            assert_eq!(x.to_bits(), y.to_bits());
        }
        assert!(fast.d.iter().any(|v| v.is_nan()));
    }

    /// Timing, not a check: `cargo test --release -p mouli-lab -- --ignored --nocapture matmul_t_speed`.
    #[test]
    #[ignore = "timing only"]
    fn matmul_t_speed() {
        let mut rng = Rng::new(7);
        // The first layer of the benchmark: 32 images of 784 pixels against 480 weight rows.
        let a = random(32, 784, &mut rng);
        let b = random(480, 784, &mut rng);
        let time = |f: &dyn Fn() -> Mat| {
            let t = std::time::Instant::now();
            let mut sink = 0.0;
            for _ in 0..200 {
                sink += f().d[0];
            }
            (t.elapsed().as_secs_f64() / 200.0 * 1e3, sink)
        };
        let (slow, _) = time(&|| matmul_t_reference(&a, &b));
        let (fast, _) = time(&|| a.matmul_t(&b));
        println!(
            "matmul_t 32x784 * 480x784: reference {slow:.3} ms, now {fast:.3} ms, {:.2}x",
            slow / fast
        );
    }
}
