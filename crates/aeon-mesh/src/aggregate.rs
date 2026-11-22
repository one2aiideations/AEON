//! Collecting the results of a task that was split into numbered parts.
//!
//! A coordinator splits work into `parts`, hands them to nodes, and submits each result by
//! part index as it arrives (in any order, possibly after a retry). The aggregator accepts each
//! part once, reports what is still missing, and returns the results in part order when
//! complete.

use std::collections::BTreeMap;
use std::fmt;

/// Why a result was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AggregateError {
    /// The part index is not below the part count.
    OutOfRange,
    /// That part already has a result.
    Duplicate,
    /// The part count is zero.
    NoParts,
}

impl fmt::Display for AggregateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            AggregateError::OutOfRange => "part index out of range",
            AggregateError::Duplicate => "part already has a result",
            AggregateError::NoParts => "a task needs at least one part",
        })
    }
}

impl std::error::Error for AggregateError {}

/// Gathers one result per part.
#[derive(Debug, Clone)]
pub struct Aggregator {
    parts: usize,
    results: BTreeMap<usize, Vec<u8>>,
}

impl Aggregator {
    /// Expect results for parts `0..parts`.
    pub fn new(parts: usize) -> Result<Self, AggregateError> {
        if parts == 0 {
            return Err(AggregateError::NoParts);
        }
        Ok(Aggregator {
            parts,
            results: BTreeMap::new(),
        })
    }

    /// Record the result of one part. A second result for the same part is refused, so a
    /// late duplicate from a retried node cannot overwrite the accepted one.
    pub fn submit(&mut self, part: usize, result: Vec<u8>) -> Result<(), AggregateError> {
        if part >= self.parts {
            return Err(AggregateError::OutOfRange);
        }
        if self.results.contains_key(&part) {
            return Err(AggregateError::Duplicate);
        }
        self.results.insert(part, result);
        Ok(())
    }

    /// Parts received so far.
    pub fn received(&self) -> usize {
        self.results.len()
    }

    /// Total parts expected.
    pub fn parts(&self) -> usize {
        self.parts
    }

    /// `true` once every part has a result.
    pub fn is_complete(&self) -> bool {
        self.results.len() == self.parts
    }

    /// Part indices still waiting for a result, ascending.
    pub fn missing(&self) -> Vec<usize> {
        (0..self.parts)
            .filter(|p| !self.results.contains_key(p))
            .collect()
    }

    /// Results in part order, or `None` while incomplete.
    pub fn finish(self) -> Option<Vec<Vec<u8>>> {
        self.is_complete()
            .then(|| self.results.into_values().collect())
    }

    /// All results concatenated in part order, or `None` while incomplete.
    pub fn finish_concat(self) -> Option<Vec<u8>> {
        self.finish().map(|v| v.concat())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_parts_is_an_error() {
        assert_eq!(Aggregator::new(0).unwrap_err(), AggregateError::NoParts);
    }

    #[test]
    fn collects_out_of_order_and_returns_in_order() {
        let mut a = Aggregator::new(3).unwrap();
        a.submit(2, b"c".to_vec()).unwrap();
        assert_eq!(a.missing(), vec![0, 1]);
        assert!(!a.is_complete());
        a.submit(0, b"a".to_vec()).unwrap();
        a.submit(1, b"b".to_vec()).unwrap();
        assert!(a.is_complete());
        assert_eq!((a.received(), a.parts()), (3, 3));
        assert_eq!(
            a.clone().finish().unwrap(),
            vec![b"a".to_vec(), b"b".to_vec(), b"c".to_vec()]
        );
        assert_eq!(a.finish_concat().unwrap(), b"abc");
    }

    #[test]
    fn incomplete_gives_nothing() {
        let mut a = Aggregator::new(2).unwrap();
        a.submit(1, vec![1]).unwrap();
        assert!(a.clone().finish().is_none());
        assert!(a.finish_concat().is_none());
    }

    #[test]
    fn refuses_duplicates_and_out_of_range() {
        let mut a = Aggregator::new(2).unwrap();
        a.submit(0, b"first".to_vec()).unwrap();
        assert_eq!(
            a.submit(0, b"late".to_vec()),
            Err(AggregateError::Duplicate)
        );
        assert_eq!(a.submit(2, vec![]), Err(AggregateError::OutOfRange));
        assert_eq!(
            a.submit(usize::MAX, vec![]),
            Err(AggregateError::OutOfRange)
        );
        a.submit(1, vec![]).unwrap();
        // the first accepted result is the one kept
        assert_eq!(a.finish_concat().unwrap(), b"first");
    }

    #[test]
    fn empty_results_are_valid() {
        let mut a = Aggregator::new(1).unwrap();
        a.submit(0, vec![]).unwrap();
        assert_eq!(a.finish().unwrap(), vec![Vec::<u8>::new()]);
    }

    #[test]
    fn errors_display() {
        assert!(AggregateError::Duplicate.to_string().contains("already"));
    }
}
