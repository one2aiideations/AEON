//! Bounded frame of recent sensor samples (oldest dropped when full).

use std::collections::VecDeque;

/// One sample.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sample {
    /// Sensor kind.
    pub kind: u32,
    /// Timestamp (caller units).
    pub timestamp: u64,
    /// Reading.
    pub value: f32,
}

/// Recent samples, newest last.
#[derive(Debug, Clone)]
pub struct SensorFrame {
    samples: VecDeque<Sample>,
    cap: usize,
}

/// Non-finite reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BadReading;

impl SensorFrame {
    /// Frame keeping at most `cap` samples (minimum 1).
    #[must_use]
    pub fn new(cap: usize) -> Self {
        let cap = cap.max(1);
        Self {
            samples: VecDeque::with_capacity(cap),
            cap,
        }
    }

    /// Add a reading.
    pub fn push(&mut self, kind: u32, timestamp: u64, value: f32) -> Result<(), BadReading> {
        if !value.is_finite() {
            return Err(BadReading);
        }
        if self.samples.len() == self.cap {
            self.samples.pop_front();
        }
        self.samples.push_back(Sample {
            kind,
            timestamp,
            value,
        });
        Ok(())
    }

    /// Newest sample.
    #[must_use]
    pub fn latest(&self) -> Option<Sample> {
        self.samples.back().copied()
    }

    /// Newest sample of `kind`.
    #[must_use]
    pub fn latest_of(&self, kind: u32) -> Option<Sample> {
        self.samples.iter().rev().find(|s| s.kind == kind).copied()
    }

    /// Number of samples.
    #[must_use]
    pub fn len(&self) -> usize {
        self.samples.len()
    }

    /// True when empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drops_oldest() {
        let mut f = SensorFrame::new(2);
        f.push(1, 0, 1.0).unwrap();
        f.push(2, 1, 2.0).unwrap();
        f.push(1, 2, 3.0).unwrap();
        assert_eq!(f.len(), 2);
        assert_eq!(f.latest_of(2).unwrap().value, 2.0);
        assert_eq!(f.latest().unwrap().value, 3.0);
        assert_eq!(f.push(0, 0, f32::INFINITY), Err(BadReading));
    }
}
