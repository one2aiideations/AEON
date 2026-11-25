//! Removing hunks the tests do not need, so a repair changes as little as possible.
//!
//! This is delta debugging over hunks: drop one hunk, ask the oracle (for example "do the target
//! tests still pass?") and keep the drop only if the answer is still yes. The result is
//! 1-minimal: removing any single remaining hunk makes the oracle say no, unless the call budget
//! ran out first. Creations and deletions are never split; they are all or nothing.

use crate::diff::FilePatch;

/// What minimisation produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Minimized {
    /// The reduced patches.
    pub patches: Vec<FilePatch>,
    /// Hunks that were dropped.
    pub hunks_removed: usize,
    /// How many times the oracle was asked.
    pub oracle_calls: usize,
    /// True when the call budget ended the search before it reached a fixed point.
    pub budget_exhausted: bool,
}

/// Greedily drops hunks while `accepts` still returns true for the reduced patch set.
///
/// `accepts` must return true for the full input (this is not checked; if it is false for the
/// input, nothing is removed because no reduction is accepted). At most `max_calls` oracle calls
/// are made.
pub fn minimize<F>(patches: &[FilePatch], max_calls: usize, mut accepts: F) -> Minimized
where
    F: FnMut(&[FilePatch]) -> bool,
{
    let mut current: Vec<FilePatch> = patches.to_vec();
    let (mut calls, mut removed) = (0usize, 0usize);
    let mut exhausted = false;
    'outer: loop {
        let mut progressed = false;
        let mut fi = 0;
        while fi < current.len() {
            let splittable = !current[fi].is_creation() && !current[fi].is_deletion();
            let mut hi = 0;
            let mut file_removed = false;
            while splittable && hi < current[fi].hunks.len() {
                if calls >= max_calls {
                    exhausted = true;
                    break 'outer;
                }
                let mut candidate = current.clone();
                candidate[fi].hunks.remove(hi);
                let emptied = candidate[fi].hunks.is_empty();
                if emptied {
                    candidate.remove(fi);
                }
                calls += 1;
                if accepts(&candidate) {
                    current = candidate;
                    removed += 1;
                    progressed = true;
                    if emptied {
                        // The next file slid into index `fi`; start over on it.
                        file_removed = true;
                        break;
                    }
                    // Otherwise the next hunk slid into position `hi`; test it without advancing.
                } else {
                    hi += 1;
                }
            }
            if !file_removed {
                fi += 1;
            }
        }
        if !progressed {
            break;
        }
    }
    Minimized {
        patches: current,
        hunks_removed: removed,
        oracle_calls: calls,
        budget_exhausted: exhausted,
    }
}
