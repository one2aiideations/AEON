//! BM25 keyword index.

use crate::DocId;
use std::collections::{BTreeMap, HashMap};

/// Lowercase alphanumeric tokens.
#[must_use]
pub fn tokenize(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// Inverted index with BM25 scoring.
#[derive(Debug, Clone)]
pub struct Bm25 {
    postings: HashMap<String, BTreeMap<DocId, u32>>,
    lengths: BTreeMap<DocId, u32>,
    total_len: u64,
    /// Term-frequency saturation.
    pub k1: f32,
    /// Length normalisation.
    pub b: f32,
}

impl Default for Bm25 {
    fn default() -> Self {
        Self {
            postings: HashMap::new(),
            lengths: BTreeMap::new(),
            total_len: 0,
            k1: 1.2,
            b: 0.75,
        }
    }
}

impl Bm25 {
    /// Empty index.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Index (or re-index) a document.
    pub fn add(&mut self, id: DocId, text: &str) {
        self.remove(id);
        let toks = tokenize(text);
        let len = u32::try_from(toks.len()).unwrap_or(u32::MAX);
        for t in toks {
            *self.postings.entry(t).or_default().entry(id).or_insert(0) += 1;
        }
        self.lengths.insert(id, len);
        self.total_len += u64::from(len);
    }

    /// Remove a document. Returns whether it was indexed.
    pub fn remove(&mut self, id: DocId) -> bool {
        let Some(len) = self.lengths.remove(&id) else {
            return false;
        };
        self.total_len -= u64::from(len);
        self.postings.retain(|_, p| {
            p.remove(&id);
            !p.is_empty()
        });
        true
    }

    /// Number of documents.
    #[must_use]
    pub fn len(&self) -> usize {
        self.lengths.len()
    }

    /// True when empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.lengths.is_empty()
    }

    /// Top `k` documents for `query`, best first.
    #[must_use]
    pub fn search(&self, query: &str, k: usize) -> Vec<(DocId, f32)> {
        if self.lengths.is_empty() {
            return Vec::new();
        }
        let n = self.lengths.len() as f32;
        let avg = self.total_len as f32 / n;
        let mut scores: HashMap<DocId, f32> = HashMap::new();
        let mut terms = tokenize(query);
        terms.sort();
        terms.dedup();
        for t in terms {
            let Some(p) = self.postings.get(&t) else {
                continue;
            };
            let df = p.len() as f32;
            let idf = ((n - df + 0.5) / (df + 0.5) + 1.0).ln();
            for (&d, &tf) in p {
                let tf = tf as f32;
                let dl = self.lengths[&d] as f32;
                let norm = tf * (self.k1 + 1.0) / (tf + self.k1 * (1.0 - self.b + self.b * dl / avg.max(1e-6)));
                *scores.entry(d).or_insert(0.0) += idf * norm;
            }
        }
        let mut v: Vec<(DocId, f32)> = scores.into_iter().collect();
        v.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        v.truncate(k);
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranks_relevant_first() {
        let mut ix = Bm25::new();
        ix.add(1, "The laptop GPU runs the local model");
        ix.add(2, "Cooking pasta with tomato sauce");
        ix.add(3, "GPU drivers for Vulkan on Linux laptops; GPU GPU");
        let r = ix.search("gpu vulkan", 10);
        assert_eq!(r[0].0, 3);
        assert_eq!(r.len(), 2);
        assert!(ix.search("nothing-matches", 5).is_empty());
        assert!(ix.remove(3));
        assert_eq!(ix.search("vulkan", 5).len(), 0);
        ix.add(1, "pasta");
        assert_eq!(ix.search("laptop", 5).len(), 0);
        assert_eq!(tokenize("Hi, AEON-OS 2!"), vec!["hi", "aeon", "os", "2"]);
    }
}
