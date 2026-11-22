//! When a job runs.

/// Seconds per day.
pub const DAY: u64 = 86_400;

/// A job schedule. Times are Unix seconds (UTC).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Schedule {
    /// Run once at `at`.
    Once {
        /// Run time.
        at: u64,
    },
    /// Run every `period` seconds, first at `start`.
    Every {
        /// First run.
        start: u64,
        /// Interval in seconds (> 0).
        period: u64,
    },
    /// Run daily at `second_of_day` (0..86400) shifted by `utc_offset` seconds.
    Daily {
        /// Local time of day in seconds.
        second_of_day: u32,
        /// Local offset from UTC in seconds (for example +19800 for UTC+5:30).
        utc_offset: i32,
    },
}

impl Schedule {
    /// Check parameters.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        match *self {
            Self::Once { .. } => true,
            Self::Every { period, .. } => period > 0,
            Self::Daily {
                second_of_day,
                utc_offset,
            } => u64::from(second_of_day) < DAY && i64::from(utc_offset).unsigned_abs() < DAY,
        }
    }

    /// First run time at or after `t`.
    #[must_use]
    pub fn next_at_or_after(&self, t: u64) -> Option<u64> {
        match t.checked_sub(1) {
            Some(p) => self.next_after(p),
            None if !self.is_valid() => None,
            None => Some(match *self {
                Self::Once { at } => at,
                Self::Every { start, .. } => start,
                Self::Daily {
                    second_of_day,
                    utc_offset,
                } => daily_target(second_of_day, utc_offset),
            }),
        }
    }

    /// First run time strictly after `after`, or `None` if it never runs again.
    #[must_use]
    pub fn next_after(&self, after: u64) -> Option<u64> {
        if !self.is_valid() {
            return None;
        }
        match *self {
            Self::Once { at } => (at > after).then_some(at),
            Self::Every { start, period } => {
                if start > after {
                    Some(start)
                } else {
                    let k = (after - start) / period + 1;
                    start.checked_add(k.checked_mul(period)?)
                }
            }
            Self::Daily {
                second_of_day,
                utc_offset,
            } => {
                let target = daily_target(second_of_day, utc_offset);
                let day_start = after - after % DAY;
                let today = day_start + target;
                Some(if today > after { today } else { today + DAY })
            }
        }
    }
}

/// UTC second-of-day at which local `second_of_day` is reached.
fn daily_target(second_of_day: u32, utc_offset: i32) -> u64 {
    (i64::from(second_of_day) - i64::from(utc_offset)).rem_euclid(DAY as i64) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn once() {
        let s = Schedule::Once { at: 100 };
        assert_eq!(s.next_after(50), Some(100));
        assert_eq!(s.next_after(100), None);
    }

    #[test]
    fn every() {
        let s = Schedule::Every {
            start: 10,
            period: 5,
        };
        assert_eq!(s.next_after(0), Some(10));
        assert_eq!(s.next_after(10), Some(15));
        assert_eq!(s.next_after(17), Some(20));
        assert!(!Schedule::Every {
            start: 0,
            period: 0
        }
        .is_valid());
    }

    #[test]
    fn daily_with_offset() {
        // 09:00 at UTC+5:30 is 03:30 UTC.
        let s = Schedule::Daily {
            second_of_day: 9 * 3600,
            utc_offset: 19_800,
        };
        let day0 = 20_000 * DAY;
        assert_eq!(s.next_after(day0), Some(day0 + 3 * 3600 + 1800));
        assert_eq!(
            s.next_after(day0 + 4 * 3600),
            Some(day0 + DAY + 3 * 3600 + 1800)
        );
        // 02:00 at UTC+5:30 is 20:30 UTC the previous day.
        let early = Schedule::Daily {
            second_of_day: 2 * 3600,
            utc_offset: 19_800,
        };
        assert_eq!(early.next_after(day0), Some(day0 + 20 * 3600 + 1800));
        assert!(!Schedule::Daily {
            second_of_day: 90_000,
            utc_offset: 0
        }
        .is_valid());
    }
}
