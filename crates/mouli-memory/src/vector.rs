//! Exact cosine-similarity index (fine for a personal device's memory size).

use crate::DocId;
use std::collections::BTreeMap;

/// Vector index errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VecError {
    /// Wrong dimension.
    Dim,
    /// Zero or non-finite vector.
    Bad,
}

/// Stores unit-normalised embeddings.
#[derive(Debug, Clone)]
pub struct VectorIndex {
    dim: usize,
    vecs: BTreeMap<DocId, Vec<f32>>,
}

fn normalise(v: &[f32]) -> Option<Vec<f32>> {
    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    (n.is_finite() && n > 0.0).then(|| v.iter().map(|x| x / n).collect())
}

impl VectorIndex {
    /// Index for `dim`-dimensional vectors.
    #[must_use]
    pub fn new(dim: usize) -> Self {
        Self {
            dim,
            vecs: BTreeMap::new(),
        }
    }

    /// Insert or replace.
    pub fn insert(&mut self, id: DocId, v: &[f32]) -> Result<(), VecError> {
        if v.len() != self.dim {
            return Err(VecError::Dim);
        }
        self.vecs.insert(id, normalise(v).ok_or(VecError::Bad)?);
        Ok(())
    }

    /// Remove.
    pub fn remove(&mut self, id: DocId) -> bool {
        self.vecs.remove(&id).is_some()
    }

    /// Top `k` by cosine similarity.
    pub fn search(&self, q: &[f32], k: usize) -> Result<Vec<(DocId, f32)>, VecError> {
        if q.len() != self.dim {
            return Err(VecError::Dim);
        }
        let q = normalise(q).ok_or(VecError::Bad)?;
        let mut v: Vec<(DocId, f32)> = self
            .vecs
            .iter()
            .map(|(&id, e)| (id, e.iter().zip(&q).map(|(a, b)| a * b).sum()))
            .collect();
        v.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        v.truncate(k);
        Ok(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cosine_order() {
        let mut ix = VectorIndex::new(2);
        ix.insert(1, &[1.0, 0.0]).unwrap();
        ix.insert(2, &[0.0, 3.0]).unwrap();
        ix.insert(3, &[1.0, 1.0]).unwrap();
        let r = ix.search(&[2.0, 0.1], 2).unwrap();
        assert_eq!(r[0].0, 1);
        assert_eq!(r[1].0, 3);
        assert_eq!(ix.insert(4, &[0.0, 0.0]), Err(VecError::Bad));
        assert_eq!(ix.search(&[1.0], 1), Err(VecError::Dim));
        assert!(ix.remove(2));
    }
}
