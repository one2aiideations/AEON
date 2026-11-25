//! Datasets: IDX (MNIST) loading and synthetic blobs.

use crate::linalg::Mat;
use crate::F;

pub struct Dataset {
    pub x: Mat,
    pub y: Vec<u8>,
}

impl Dataset {
    pub fn subset(&self, idx: &[usize]) -> Dataset {
        let mut x = Mat::zeros(idx.len(), self.x.c);
        let mut y = Vec::with_capacity(idx.len());
        for (r, &i) in idx.iter().enumerate() {
            x.d[r * self.x.c..(r + 1) * self.x.c]
                .copy_from_slice(&self.x.d[i * self.x.c..(i + 1) * self.x.c]);
            y.push(self.y[i]);
        }
        Dataset { x, y }
    }
}

/// Reads standard uncompressed IDX files (magic 2051 for images, 2049 for labels).
pub fn load_idx(images: &str, labels: &str) -> Result<Dataset, String> {
    let img = std::fs::read(images).map_err(|e| format!("reading {}: {}", images, e))?;
    let lab = std::fs::read(labels).map_err(|e| format!("reading {}: {}", labels, e))?;
    parse_idx(&img, &lab, images, labels)
}

/// The IDX parser on bytes (so it can be tested without files).
pub fn parse_idx(img: &[u8], lab: &[u8], images: &str, labels: &str) -> Result<Dataset, String> {
    if img.len() < 16 {
        return Err(format!("{}: too small for IDX image header", images));
    }
    let magic = u32::from_be_bytes([img[0], img[1], img[2], img[3]]);
    if magic != 2051 {
        return Err(format!("{}: magic {} != 2051 (images)", images, magic));
    }
    let n = u32::from_be_bytes([img[4], img[5], img[6], img[7]]) as usize;
    let r = u32::from_be_bytes([img[8], img[9], img[10], img[11]]) as usize;
    let c = u32::from_be_bytes([img[12], img[13], img[14], img[15]]) as usize;
    let need = n
        .checked_mul(r)
        .and_then(|v| v.checked_mul(c))
        .ok_or_else(|| format!("{}: header sizes overflow", images))?;
    if img.len() < 16 + need {
        return Err(format!("{}: truncated pixel data", images));
    }
    if lab.len() < 8 {
        return Err(format!("{}: too small for IDX label header", labels));
    }
    let lmagic = u32::from_be_bytes([lab[0], lab[1], lab[2], lab[3]]);
    if lmagic != 2049 {
        return Err(format!("{}: magic {} != 2049 (labels)", labels, lmagic));
    }
    let ln = u32::from_be_bytes([lab[4], lab[5], lab[6], lab[7]]) as usize;
    if ln != n {
        return Err(format!("image count {} != label count {}", n, ln));
    }
    if lab.len() < 8 + ln {
        return Err(format!("{}: truncated label data", labels));
    }
    let mut x = Mat::zeros(n, r * c);
    for i in 0..need {
        x.d[i] = img[16 + i] as F;
    }
    Ok(Dataset {
        x,
        y: lab[8..8 + ln].to_vec(),
    })
}

pub fn synthetic_blobs(n: usize, d: usize, classes: usize, seed: u64) -> Dataset {
    let mut rng = crate::rng::Rng::new(seed);
    let mut means = Vec::with_capacity(classes);
    for _ in 0..classes {
        means.push((0..d).map(|_| rng.normal() * 1.5).collect::<Vec<F>>());
    }
    let mut x = Mat::zeros(n, d);
    let mut y = Vec::with_capacity(n);
    for i in 0..n {
        let c = i % classes.max(1);
        for j in 0..d {
            x.d[i * d + j] = means[c][j] + rng.normal();
        }
        y.push(c as u8);
    }
    let mut idx: Vec<usize> = (0..n).collect();
    rng.shuffle(&mut idx);
    let mut x2 = Mat::zeros(n, d);
    let mut y2 = Vec::with_capacity(n);
    for (r, &i) in idx.iter().enumerate() {
        x2.d[r * d..(r + 1) * d].copy_from_slice(&x.d[i * d..(i + 1) * d]);
        y2.push(y[i]);
    }
    Dataset { x: x2, y: y2 }
}

pub fn select_classes(ds: &Dataset, cls: &[u8]) -> Dataset {
    let idx: Vec<usize> = (0..ds.y.len())
        .filter(|&i| cls.contains(&ds.y[i]))
        .collect();
    ds.subset(&idx)
}

pub fn concat_datasets(parts: &[Dataset]) -> Dataset {
    let d = parts.first().map(|p| p.x.c).unwrap_or(0);
    let total: usize = parts.iter().map(|p| p.y.len()).sum();
    let mut x = Mat::zeros(total, d);
    let mut y = Vec::with_capacity(total);
    let mut off = 0usize;
    for p in parts {
        let rows = p.y.len();
        x.d[off * d..(off + rows) * d].copy_from_slice(&p.x.d);
        y.extend_from_slice(&p.y);
        off += rows;
    }
    Dataset { x, y }
}

pub fn normalise_mnist(ds: &mut Dataset) {
    for v in ds.x.d.iter_mut() {
        *v = (*v / 255.0 - 0.1307) / 0.3081;
    }
}
