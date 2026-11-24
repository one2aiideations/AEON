//! Deterministic retry, HTTP rate-limit, token-bucket, and circuit-breaker
//! utilities.
//!
//! All time values are supplied by the caller in milliseconds or Unix
//! seconds, as documented. Randomness is supplied explicitly where needed.
//! This module performs no sleeping, clock reads, I/O, or random generation.
//!
//! The implementation uses only the Rust standard library and is suitable
//! for deterministic testing and embedding in asynchronous or synchronous
//! applications.

use std::fmt;

/// Exponential retry configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Backoff {
    /// Initial retry delay in milliseconds.
    pub base_ms: u64,
    /// Maximum retry delay in milliseconds.
    pub max_ms: u64,
    /// Maximum number of retry attempts.
    pub max_attempts: u32,
    /// Maximum jitter percentage, from 0 through 100.
    pub jitter_percent: u8,
}

impl Backoff {
    /// Creates an exponential backoff configuration.
    ///
    /// `base_ms` and `max_ms` are clamped to at least one millisecond.
    /// `max_ms` is clamped to at least `base_ms`.
    /// Jitter is clamped to 0 through 100 percent.
    #[must_use]
    pub fn new(base_ms: u64, max_ms: u64, max_attempts: u32, jitter_percent: u8) -> Self {
        let base_ms = base_ms.max(1);
        let max_ms = max_ms.max(base_ms);

        Self {
            base_ms,
            max_ms,
            max_attempts,
            jitter_percent: jitter_percent.min(100),
        }
    }

    /// Calculates the retry delay for an attempt.
    ///
    /// Attempts start at zero. Returns `None` when `attempt` is greater
    /// than or equal to `max_attempts`.
    ///
    /// The exponential delay is capped at `max_ms`. Jitter is deterministic
    /// and derived from `random`, with a maximum reduction controlled by
    /// `jitter_percent`. The result is always at least one millisecond.
    #[must_use]
    pub fn delay_ms(&self, attempt: u32, random: u64) -> Option<u64> {
        if attempt >= self.max_attempts {
            return None;
        }

        let mut delay = self.base_ms;

        for _ in 0..attempt {
            delay = delay.saturating_mul(2).min(self.max_ms);

            if delay == self.max_ms {
                break;
            }
        }

        if self.jitter_percent == 0 {
            return Some(delay.max(1));
        }

        let jitter = u64::from(self.jitter_percent);
        let random_factor = random % 1001;

        let reduction = delay.saturating_mul(jitter).saturating_mul(random_factor) / 100_000;

        Some(delay.saturating_sub(reduction).max(1))
    }
}

/// HTTP retry classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Retry {
    /// Retry using the configured exponential backoff.
    Yes,
    /// Do not retry.
    No,
    /// Prefer the Retry-After header when present.
    ///
    /// The caller should invoke [`retry_after_ms`] when a valid header
    /// exists. If the header is absent or invalid, use ordinary backoff.
    AfterHeader,
}

/// Classifies an HTTP status code for retry handling.
///
/// Status 408, 425, 500, 502, and 504 return [`Retry::Yes`].
/// Status 429 and 503 return [`Retry::AfterHeader`].
/// Other status codes return [`Retry::No`].
///
/// For [`Retry::AfterHeader`], the caller should use
/// [`retry_after_ms`] when a valid Retry-After header is available;
/// otherwise, fall back to exponential backoff.
#[must_use]
pub fn classify_http_status(status: u16) -> Retry {
    match status {
        429 | 503 => Retry::AfterHeader,
        408 | 425 | 500 | 502 | 504 => Retry::Yes,
        _ => Retry::No,
    }
}

const MAX_RETRY_AFTER_MS: u64 = 3_600_000;
const SECONDS_PER_DAY: i64 = 86_400;

/// Parses an HTTP Retry-After header.
///
/// Supports:
/// - Delta-seconds, such as `120`.
/// - IMF-fixdate, such as `Sun, 06 Nov 1994 08:49:37 GMT`.
///
/// Returns the number of milliseconds until retry, relative to
/// `now_unix_secs`. Dates in the past return zero. Results are capped
/// at one hour. Malformed values return `None`.
#[must_use]
pub fn retry_after_ms(header_value: &str, now_unix_secs: i64) -> Option<u64> {
    let value = header_value.trim();

    if value.is_empty() {
        return None;
    }

    if value.bytes().all(|byte| byte.is_ascii_digit()) {
        let seconds = value.parse::<u64>().ok()?;

        return Some(seconds.saturating_mul(1000).min(MAX_RETRY_AFTER_MS));
    }

    let timestamp = parse_imf_fixdate(value)?;

    let difference = timestamp.saturating_sub(now_unix_secs).max(0);

    Some(
        u64::try_from(difference)
            .unwrap_or(u64::MAX)
            .saturating_mul(1000)
            .min(MAX_RETRY_AFTER_MS),
    )
}

/// Returns the numeric month for an IMF-fixdate month abbreviation.
fn parse_month(value: &str) -> Option<u32> {
    match value {
        "Jan" => Some(1),
        "Feb" => Some(2),
        "Mar" => Some(3),
        "Apr" => Some(4),
        "May" => Some(5),
        "Jun" => Some(6),
        "Jul" => Some(7),
        "Aug" => Some(8),
        "Sep" => Some(9),
        "Oct" => Some(10),
        "Nov" => Some(11),
        "Dec" => Some(12),
        _ => None,
    }
}

/// Returns whether a year is a Gregorian leap year.
fn is_leap_year(year: i64) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

/// Returns the number of days in a Gregorian month.
fn days_in_month(year: i64, month: u32) -> Option<u32> {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => Some(31),
        4 | 6 | 9 | 11 => Some(30),
        2 if is_leap_year(year) => Some(29),
        2 => Some(28),
        _ => None,
    }
}

/// Converts a Gregorian calendar date into days since the Unix epoch.
///
/// Uses the civil-calendar algorithm and supports dates before 1970.
fn days_from_civil(year: i64, month: u32, day: u32) -> Option<i64> {
    if !(1..=12).contains(&month) {
        return None;
    }

    let maximum_day = days_in_month(year, month)?;

    if day == 0 || day > maximum_day {
        return None;
    }

    let adjusted_year = year.checked_sub(i64::from(month <= 2))?;

    let era = adjusted_year.div_euclid(400);
    let year_of_era = adjusted_year - era * 400;

    let adjusted_month = i64::from(month) + if month > 2 { -3 } else { 9 };

    let day_of_year = (153 * adjusted_month + 2) / 5 + i64::from(day) - 1;

    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;

    era.checked_mul(146_097)?
        .checked_add(day_of_era)?
        .checked_sub(719_468)
}

/// Parses an IMF-fixdate into Unix seconds.
///
/// The weekday is validated against the calendar date.
fn parse_imf_fixdate(value: &str) -> Option<i64> {
    let parts: Vec<&str> = value.split_whitespace().collect();

    if parts.len() != 6 {
        return None;
    }

    let weekday = parts.first()?.strip_suffix(',')?;

    let day = parts.get(1)?.parse::<u32>().ok()?;
    let month = parse_month(parts.get(2)?)?;
    let year = parts.get(3)?.parse::<i64>().ok()?;

    if !(1601..=9999).contains(&year) {
        return None;
    }

    let time = parts.get(4)?;
    let time_parts: Vec<&str> = time.split(':').collect();

    if time_parts.len() != 3 {
        return None;
    }

    let hour = time_parts.first()?.parse::<u32>().ok()?;
    let minute = time_parts.get(1)?.parse::<u32>().ok()?;
    let second = time_parts.get(2)?.parse::<u32>().ok()?;

    if hour > 23 || minute > 59 || second > 59 || *parts.get(5)? != "GMT" {
        return None;
    }

    let days = days_from_civil(year, month, day)?;

    let expected_weekday = match days.rem_euclid(7) {
        0 => "Thu",
        1 => "Fri",
        2 => "Sat",
        3 => "Sun",
        4 => "Mon",
        5 => "Tue",
        6 => "Wed",
        _ => return None,
    };

    if weekday != expected_weekday {
        return None;
    }

    days.checked_mul(SECONDS_PER_DAY)?
        .checked_add(i64::from(hour) * 3600)?
        .checked_add(i64::from(minute) * 60)?
        .checked_add(i64::from(second))
}

/// A deterministic token bucket rate limiter.
///
/// Fractional token refill is tracked in milli-tokens so repeated
/// fractional intervals do not lose precision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenBucket {
    capacity: u32,
    refill_per_sec: u32,
    tokens: u64,
    remainder: u64,
    last_refill_ms: u64,
}

impl TokenBucket {
    /// Creates a token bucket.
    ///
    /// The bucket starts full. `refill_per_sec` specifies the number
    /// of tokens generated per second.
    #[must_use]
    pub fn new(capacity: u32, refill_per_sec: u32, now_ms: u64) -> Self {
        Self {
            capacity,
            refill_per_sec,
            tokens: u64::from(capacity),
            remainder: 0,
            last_refill_ms: now_ms,
        }
    }

    /// Refills the bucket using the supplied timestamp.
    ///
    /// Clock values earlier than the last refill timestamp are ignored.
    /// Fractional refill is preserved between calls.
    fn refill(&mut self, now_ms: u64) {
        if now_ms <= self.last_refill_ms {
            return;
        }

        let elapsed = now_ms - self.last_refill_ms;
        self.last_refill_ms = now_ms;

        if self.tokens >= u64::from(self.capacity) {
            self.tokens = u64::from(self.capacity);
            self.remainder = 0;
            return;
        }

        if self.refill_per_sec == 0 {
            return;
        }

        let milli_tokens = elapsed
            .saturating_mul(u64::from(self.refill_per_sec))
            .saturating_add(self.remainder);

        let added_tokens = milli_tokens / 1000;

        self.remainder = milli_tokens % 1000;

        let capacity = u64::from(self.capacity);

        if added_tokens >= capacity.saturating_sub(self.tokens) {
            self.tokens = capacity;
            self.remainder = 0;
        } else {
            self.tokens = self.tokens.saturating_add(added_tokens);
        }
    }

    /// Attempts to consume `n` tokens.
    ///
    /// Returns `Ok(())` when enough tokens are available.
    /// Otherwise returns `Err(milliseconds_until_available)`.
    ///
    /// Requests larger than the bucket capacity return `Err(u64::MAX)`.
    /// A zero-token request always succeeds.
    pub fn try_take(&mut self, n: u32, now_ms: u64) -> Result<(), u64> {
        if n == 0 {
            return Ok(());
        }

        if n > self.capacity {
            return Err(u64::MAX);
        }

        self.refill(now_ms);

        let requested = u64::from(n);

        if self.tokens >= requested {
            self.tokens -= requested;
            return Ok(());
        }

        if self.refill_per_sec == 0 {
            return Err(u64::MAX);
        }

        let missing = requested - self.tokens;

        let required_milli = missing.saturating_mul(1000).saturating_sub(self.remainder);

        let rate = u64::from(self.refill_per_sec);

        Err(required_milli.div_ceil(rate))
    }

    /// Returns the number of available whole tokens after refilling.
    pub fn available(&mut self, now_ms: u64) -> u32 {
        self.refill(now_ms);

        u32::try_from(self.tokens).unwrap_or(self.capacity)
    }
}

/// Circuit-breaker operating state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Requests are allowed and failures are counted.
    Closed,
    /// Requests are blocked until the open interval expires.
    Open,
    /// One probe request is permitted.
    HalfOpen,
}

/// Deterministic circuit breaker.
///
/// Consecutive failures reaching the configured threshold open the
/// circuit. After the configured interval, one probe is permitted.
/// Probe success closes the circuit; probe failure reopens it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CircuitBreaker {
    failure_threshold: u32,
    open_ms: u64,
    failures: u32,
    state: State,
    opened_at_ms: u64,
    probe_in_flight: bool,
}

impl CircuitBreaker {
    /// Creates a circuit breaker.
    ///
    /// A failure threshold of zero is clamped to one.
    #[must_use]
    pub fn new(failure_threshold: u32, open_ms: u64) -> Self {
        Self {
            failure_threshold: failure_threshold.max(1),
            open_ms,
            failures: 0,
            state: State::Closed,
            opened_at_ms: 0,
            probe_in_flight: false,
        }
    }

    /// Checks whether a request is permitted at the supplied timestamp.
    ///
    /// Closed circuits always allow requests. Open circuits allow exactly
    /// one probe after `open_ms` has elapsed. Additional requests remain
    /// blocked until the probe is recorded as successful or failed.
    pub fn allow(&mut self, now_ms: u64) -> bool {
        match self.state {
            State::Closed => true,
            State::Open => {
                let elapsed = now_ms.saturating_sub(self.opened_at_ms);

                if elapsed < self.open_ms {
                    return false;
                }

                self.state = State::HalfOpen;
                self.probe_in_flight = true;

                true
            }
            State::HalfOpen => false,
        }
    }

    /// Records a successful request.
    ///
    /// Success closes the circuit and resets the failure count.
    /// This also completes a successful half-open probe.
    pub fn record_success(&mut self) {
        self.failures = 0;
        self.state = State::Closed;
        self.probe_in_flight = false;
    }

    /// Records a failed request at the supplied timestamp.
    ///
    /// Reaching the configured threshold opens the circuit.
    /// A failed half-open probe immediately reopens it.
    pub fn record_failure(&mut self, now_ms: u64) {
        match self.state {
            State::Closed => {
                self.failures = self.failures.saturating_add(1);

                if self.failures >= self.failure_threshold {
                    self.open(now_ms);
                }
            }
            State::HalfOpen => {
                if self.probe_in_flight {
                    self.open(now_ms);
                }
            }
            State::Open => {}
        }
    }

    /// Returns the current circuit-breaker state.
    #[must_use]
    pub fn state(&self) -> State {
        self.state
    }

    /// Opens the circuit and resets probe state.
    fn open(&mut self, now_ms: u64) {
        self.state = State::Open;
        self.opened_at_ms = now_ms;
        self.probe_in_flight = false;
    }
}

impl fmt::Display for State {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::Closed => "Closed",
            Self::Open => "Open",
            Self::HalfOpen => "HalfOpen",
        };

        formatter.write_str(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_exponential_sequence_without_jitter() {
        let backoff = Backoff::new(100, 10_000, 8, 0);

        let expected = [100, 200, 400, 800, 1600, 3200, 6400, 10_000];

        for (attempt, expected_delay) in expected.iter().enumerate() {
            assert_eq!(
                backoff.delay_ms(u32::try_from(attempt).unwrap_or(0), 0),
                Some(*expected_delay)
            );
        }

        assert_eq!(backoff.delay_ms(8, 0), None);
    }

    #[test]
    fn backoff_respects_maximum_delay() {
        let backoff = Backoff::new(250, 1000, 10, 0);

        assert_eq!(backoff.delay_ms(0, 0), Some(250));
        assert_eq!(backoff.delay_ms(1, 0), Some(500));
        assert_eq!(backoff.delay_ms(2, 0), Some(1000));
        assert_eq!(backoff.delay_ms(3, 0), Some(1000));
        assert_eq!(backoff.delay_ms(9, 0), Some(1000));
    }

    #[test]
    fn backoff_clamps_configuration() {
        let backoff = Backoff::new(0, 0, 3, 255);

        assert_eq!(backoff.base_ms, 1);
        assert_eq!(backoff.max_ms, 1);
        assert_eq!(backoff.jitter_percent, 100);
        assert_eq!(backoff.delay_ms(0, 0), Some(1));
    }

    #[test]
    fn backoff_respects_attempt_limit() {
        let backoff = Backoff::new(100, 1000, 2, 0);

        assert_eq!(backoff.delay_ms(0, 0), Some(100));
        assert_eq!(backoff.delay_ms(1, 0), Some(200));
        assert_eq!(backoff.delay_ms(2, 0), None);
        assert_eq!(backoff.delay_ms(u32::MAX, 0), None);

        let disabled = Backoff::new(100, 1000, 0, 0);

        assert_eq!(disabled.delay_ms(0, 0), None);
    }

    #[test]
    fn jitter_random_extremes() {
        let backoff = Backoff::new(10_000, 10_000, 3, 20);

        assert_eq!(backoff.delay_ms(0, 0), Some(10_000));
        assert_eq!(backoff.delay_ms(0, 1000), Some(8000));
        assert_eq!(backoff.delay_ms(0, 1001), Some(10_000));

        // Whatever the random value, the delay stays within [80%, 100%].
        for random in [u64::MAX, u64::MAX - 1, 1 << 40, 123_456_789] {
            let delay = backoff.delay_ms(0, random).unwrap_or(0);
            assert!((8000..=10_000).contains(&delay), "{random} -> {delay}");
        }
    }

    #[test]
    fn jitter_one_hundred_percent_never_returns_zero() {
        let backoff = Backoff::new(1, 1, 1, 100);

        assert_eq!(backoff.delay_ms(0, 1000), Some(1));

        let backoff = Backoff::new(100, 100, 1, 100);

        assert_eq!(backoff.delay_ms(0, 1000), Some(1));
    }

    #[test]
    fn backoff_handles_large_attempts_without_overflow() {
        let backoff = Backoff::new(1, u64::MAX, u32::MAX, 0);

        assert_eq!(backoff.delay_ms(63, 0), Some(1_u64 << 63));
        assert_eq!(backoff.delay_ms(64, 0), Some(u64::MAX));
        assert_eq!(backoff.delay_ms(100, 0), Some(u64::MAX));
    }

    #[test]
    fn retry_after_delta_seconds() {
        assert_eq!(retry_after_ms("120", 0), Some(120_000));
        assert_eq!(retry_after_ms("1", 0), Some(1000));
        assert_eq!(retry_after_ms("0", 0), Some(0));
        assert_eq!(retry_after_ms("3600", 0), Some(3_600_000));
        assert_eq!(retry_after_ms("3601", 0), Some(3_600_000));
    }

    #[test]
    fn retry_after_rejects_malformed_delta() {
        assert_eq!(retry_after_ms("", 0), None);
        assert_eq!(retry_after_ms("abc", 0), None);
        assert_eq!(retry_after_ms("-10", 0), None);
        assert_eq!(retry_after_ms("1.5", 0), None);
        assert_eq!(retry_after_ms("+10", 0), None);
        assert_eq!(retry_after_ms("18446744073709551616", 0), None);
    }

    #[test]
    fn retry_after_http_date_in_future() {
        // Sunday, 06 November 1994, 08:49:37 GMT.
        let timestamp = 784_111_777_i64;

        let result = retry_after_ms("Sun, 06 Nov 1994 08:49:37 GMT", timestamp - 60);

        assert_eq!(result, Some(60_000));
    }

    #[test]
    fn retry_after_http_date_in_past() {
        assert_eq!(
            retry_after_ms("Sun, 06 Nov 1994 08:49:37 GMT", 784_111_778),
            Some(0)
        );
    }

    #[test]
    fn retry_after_http_date_ten_hours_ahead_is_capped() {
        let now = 784_111_777_i64;

        let result = retry_after_ms("Sun, 06 Nov 1994 18:49:37 GMT", now);

        assert_eq!(result, Some(3_600_000));
    }

    #[test]
    fn retry_after_rejects_invalid_http_dates() {
        assert_eq!(retry_after_ms("Sun, 32 Nov 1994 08:49:37 GMT", 0), None);
        assert_eq!(retry_after_ms("Sun, 06 Foo 1994 08:49:37 GMT", 0), None);
        assert_eq!(retry_after_ms("Mon, 06 Nov 1994 08:49:37 GMT", 0), None);
        assert_eq!(retry_after_ms("Sun, 06 Nov 1994 25:49:37 GMT", 0), None);
        assert_eq!(retry_after_ms("Sun, 06 Nov 1994 08:49:37 UTC", 0), None);
        assert_eq!(retry_after_ms("1994-11-06T08:49:37Z", 0), None);
    }

    #[test]
    fn retry_after_leap_year_validation() {
        assert!(parse_imf_fixdate("Tue, 29 Feb 2000 12:00:00 GMT").is_some());
        assert!(parse_imf_fixdate("Thu, 01 Mar 1900 12:00:00 GMT").is_some());
        assert!(parse_imf_fixdate("Mon, 29 Feb 1900 12:00:00 GMT").is_none());
    }

    #[test]
    fn retry_after_handles_pre_epoch_dates() {
        let result = retry_after_ms("Wed, 31 Dec 1969 23:59:59 GMT", -2);

        assert_eq!(result, Some(1000));
    }

    #[test]
    fn classify_http_status_codes() {
        for status in [408, 425, 500, 502, 504] {
            assert_eq!(classify_http_status(status), Retry::Yes);
        }

        for status in [429, 503] {
            assert_eq!(classify_http_status(status), Retry::AfterHeader);
        }

        for status in [200, 201, 301, 302, 400, 401, 403, 404, 422, 499] {
            assert_eq!(classify_http_status(status), Retry::No);
        }
    }

    #[test]
    fn token_bucket_starts_full() {
        let mut bucket = TokenBucket::new(10, 2, 1000);

        assert_eq!(bucket.available(1000), 10);
        assert_eq!(bucket.try_take(5, 1000), Ok(()));
        assert_eq!(bucket.available(1000), 5);
    }

    #[test]
    fn token_bucket_refills_over_whole_seconds() {
        let mut bucket = TokenBucket::new(10, 2, 0);

        assert_eq!(bucket.try_take(10, 0), Ok(()));
        assert_eq!(bucket.available(1000), 2);
        assert_eq!(bucket.available(2000), 4);
        assert_eq!(bucket.available(3000), 6);
    }

    #[test]
    fn token_bucket_preserves_fractional_refill() {
        let mut bucket = TokenBucket::new(10, 3, 0);

        assert_eq!(bucket.try_take(10, 0), Ok(()));

        assert_eq!(bucket.available(100), 0);
        assert_eq!(bucket.available(200), 0);
        assert_eq!(bucket.available(300), 0);

        assert_eq!(bucket.available(334), 1);

        assert_eq!(bucket.available(667), 2);
        assert_eq!(bucket.available(1000), 3);
    }

    #[test]
    fn token_bucket_calculates_wait_time() {
        let mut bucket = TokenBucket::new(10, 2, 0);

        assert_eq!(bucket.try_take(10, 0), Ok(()));

        assert_eq!(bucket.try_take(1, 0), Err(500));
        assert_eq!(bucket.try_take(1, 250), Err(250));
        assert_eq!(bucket.try_take(1, 500), Ok(()));
    }

    #[test]
    fn token_bucket_handles_multiple_missing_tokens() {
        let mut bucket = TokenBucket::new(10, 4, 0);

        assert_eq!(bucket.try_take(10, 0), Ok(()));
        assert_eq!(bucket.try_take(4, 0), Err(1000));
        assert_eq!(bucket.try_take(5, 0), Err(1250));
    }

    #[test]
    fn token_bucket_ignores_backwards_clock() {
        let mut bucket = TokenBucket::new(10, 2, 1000);

        assert_eq!(bucket.try_take(10, 1000), Ok(()));

        assert_eq!(bucket.available(500), 0);
        assert_eq!(bucket.try_take(1, 500), Err(500));

        assert_eq!(bucket.available(1500), 1);
    }

    #[test]
    fn token_bucket_handles_capacity_and_zero_requests() {
        let mut bucket = TokenBucket::new(5, 1, 0);

        assert_eq!(bucket.try_take(0, 0), Ok(()));
        assert_eq!(bucket.try_take(6, 0), Err(u64::MAX));

        assert_eq!(bucket.try_take(5, 0), Ok(()));
        assert_eq!(bucket.available(10_000), 5);
        assert_eq!(bucket.available(20_000), 5);
    }

    #[test]
    fn token_bucket_without_refill_rate() {
        let mut bucket = TokenBucket::new(3, 0, 0);

        assert_eq!(bucket.try_take(3, 0), Ok(()));
        assert_eq!(bucket.try_take(1, 1000), Err(u64::MAX));
        assert_eq!(bucket.available(5000), 0);
    }

    #[test]
    fn token_bucket_handles_large_timestamps() {
        let mut bucket = TokenBucket::new(10, u32::MAX, 0);

        assert_eq!(bucket.try_take(10, 0), Ok(()));
        assert_eq!(bucket.available(u64::MAX), 10);
    }

    #[test]
    fn circuit_breaker_starts_closed() {
        let mut breaker = CircuitBreaker::new(3, 1000);

        assert_eq!(breaker.state(), State::Closed);
        assert!(breaker.allow(0));
        assert!(breaker.allow(100));
    }

    #[test]
    fn circuit_breaker_opens_after_threshold() {
        let mut breaker = CircuitBreaker::new(3, 1000);

        breaker.record_failure(100);
        assert_eq!(breaker.state(), State::Closed);

        breaker.record_failure(200);
        assert_eq!(breaker.state(), State::Closed);

        breaker.record_failure(300);
        assert_eq!(breaker.state(), State::Open);

        assert!(!breaker.allow(500));
        assert!(!breaker.allow(1299));
    }

    #[test]
    fn circuit_breaker_full_cycle() {
        let mut breaker = CircuitBreaker::new(2, 1000);

        breaker.record_failure(100);
        breaker.record_failure(200);

        assert_eq!(breaker.state(), State::Open);
        assert!(!breaker.allow(1199));

        assert!(breaker.allow(1200));
        assert_eq!(breaker.state(), State::HalfOpen);

        assert!(!breaker.allow(1201));

        breaker.record_success();

        assert_eq!(breaker.state(), State::Closed);
        assert!(breaker.allow(1202));
    }

    #[test]
    fn circuit_breaker_failed_probe_reopens() {
        let mut breaker = CircuitBreaker::new(2, 1000);

        breaker.record_failure(100);
        breaker.record_failure(200);

        assert_eq!(breaker.state(), State::Open);
        assert!(breaker.allow(1200));

        breaker.record_failure(1250);

        assert_eq!(breaker.state(), State::Open);
        assert!(!breaker.allow(2249));

        assert!(breaker.allow(2250));
        breaker.record_success();

        assert_eq!(breaker.state(), State::Closed);
    }

    #[test]
    fn circuit_breaker_requires_exactly_one_probe() {
        let mut breaker = CircuitBreaker::new(1, 500);

        breaker.record_failure(100);

        assert!(!breaker.allow(599));
        assert!(breaker.allow(600));

        for timestamp in 601..610 {
            assert!(!breaker.allow(timestamp));
        }

        breaker.record_success();

        assert!(breaker.allow(610));
    }

    #[test]
    fn circuit_breaker_consecutive_failures_reset_on_success() {
        let mut breaker = CircuitBreaker::new(3, 1000);

        breaker.record_failure(100);
        breaker.record_failure(200);
        breaker.record_success();

        assert_eq!(breaker.state(), State::Closed);

        breaker.record_failure(300);
        breaker.record_failure(400);

        assert_eq!(breaker.state(), State::Closed);
    }

    #[test]
    fn circuit_breaker_ignores_backwards_clock() {
        let mut breaker = CircuitBreaker::new(1, 1000);

        breaker.record_failure(2000);

        assert!(!breaker.allow(1000));
        assert!(!breaker.allow(2999));

        assert!(breaker.allow(3000));
    }

    #[test]
    fn circuit_breaker_zero_threshold_is_clamped() {
        let mut breaker = CircuitBreaker::new(0, 100);

        breaker.record_failure(10);

        assert_eq!(breaker.state(), State::Open);
        assert!(breaker.allow(110));
    }

    #[test]
    fn circuit_breaker_zero_open_interval() {
        let mut breaker = CircuitBreaker::new(1, 0);

        breaker.record_failure(100);

        assert!(breaker.allow(100));
        assert_eq!(breaker.state(), State::HalfOpen);
        assert!(!breaker.allow(100));

        breaker.record_success();

        assert_eq!(breaker.state(), State::Closed);
    }

    #[test]
    fn circuit_breaker_failure_count_saturates() {
        let mut breaker = CircuitBreaker::new(u32::MAX, 1000);

        for _ in 0..10 {
            breaker.record_failure(100);
        }

        assert_eq!(breaker.state(), State::Closed);
    }

    #[test]
    fn state_display() {
        assert_eq!(State::Closed.to_string(), "Closed");
        assert_eq!(State::Open.to_string(), "Open");
        assert_eq!(State::HalfOpen.to_string(), "HalfOpen");
    }
}
