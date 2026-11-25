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
        for i in 0..self.r {
            for j in 0..b.r {
                let mut acc = 0.0;
                for k in 0..self.c {
                    acc += self.d[i * self.c + k] * b.d[j * b.c + k];
                }
                out.d[i * b.r + j] = acc;
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
