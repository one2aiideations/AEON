//! Pure download-planning and verification logic for large HTTP downloads.
//!
//! This module provides deterministic helpers for resumable downloads,
//! HTTP Range requests, Content-Range validation, storage-space checks,
//! SHA-256 verification through a caller-provided Hasher implementation,
//! safe model filenames, and download progress calculations.
//!
//! It performs no network requests, filesystem operations, hashing,
//! sleeping, or clock reads. All external operations are delegated
//! to the caller.

use std::error::Error;
use std::fmt;

/// Maximum accepted download size: 1 TiB.
pub const MAX_DOWNLOAD_SIZE: u64 = 1_099_511_627_776;

/// Expected SHA-256 hexadecimal string length.
pub const SHA256_HEX_LENGTH: usize = 64;

/// Caller-provided hashing implementation.
///
/// Implement this trait using a SHA-256 implementation of your choice.
/// The download planner itself does not implement cryptographic hashing.
pub trait Hasher {
    /// Feeds additional bytes into the hash state.
    fn update(&mut self, bytes: &[u8]);

    /// Finalizes the hash and returns lowercase hexadecimal output.
    fn finish_hex(self: Box<Self>) -> String;
}

/// Download configuration and current resume position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// Download URL.
    pub url: String,
    /// Expected total download size in bytes.
    pub expected_size: Option<u64>,
    /// Expected SHA-256 digest as 64 lowercase hexadecimal characters.
    pub expected_sha256: Option<String>,
    /// Number of bytes already downloaded.
    pub already_have: u64,
}

/// Action required after inspecting an HTTP response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResumeAction {
    /// Append the response body to the existing partial file.
    Append {
        /// Byte offset at which the response body begins.
        from: u64,
        /// Number of bytes remaining after this response, if known.
        remaining: Option<u64>,
    },
    /// Discard the existing partial file and restart from byte zero.
    Restart {
        /// Expected bytes remaining in the restarted download, if known.
        remaining: Option<u64>,
    },
}

/// Download validation and verification errors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DownloadError {
    /// HTTP response status was not supported.
    HttpStatus(u16),

    /// Content-Range header is missing or malformed.
    BadContentRange,

    /// Response size conflicts with the expected size or range.
    SizeMismatch,

    /// Download exceeds the supported maximum size.
    TooLarge,

    /// The server reports that the requested partial file is already complete.
    AlreadyComplete,

    /// Insufficient free storage space.
    NoSpace {
        /// Required bytes, including reserve.
        needed: u64,
        /// Available free bytes.
        free: u64,
    },

    /// Downloaded byte count differs from the expected size.
    WrongSize,

    /// Downloaded SHA-256 does not match the expected digest.
    HashMismatch {
        /// Expected lowercase hexadecimal digest.
        expected: String,
        /// Actual lowercase hexadecimal digest.
        actual: String,
    },

    /// Expected SHA-256 digest has an invalid representation.
    InvalidExpectedHash,
}

impl fmt::Display for DownloadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::HttpStatus(status) => {
                write!(formatter, "unexpected HTTP status: {status}")
            }
            Self::BadContentRange => formatter.write_str("invalid or inconsistent Content-Range"),
            Self::SizeMismatch => formatter.write_str("download size mismatch"),
            Self::TooLarge => formatter.write_str("download exceeds 1 TiB limit"),
            Self::AlreadyComplete => formatter.write_str("download is already complete"),
            Self::NoSpace { needed, free } => {
                write!(
                    formatter,
                    "insufficient storage: need {needed} bytes, have {free}"
                )
            }
            Self::WrongSize => formatter.write_str("downloaded size does not match expected size"),
            Self::HashMismatch { .. } => {
                formatter.write_str("download SHA-256 verification failed")
            }
            Self::InvalidExpectedHash => formatter.write_str("invalid expected SHA-256 digest"),
        }
    }
}

impl Error for DownloadError {}

/// Generates an HTTP Range request header.
///
/// Returns `bytes=<already_have>-` when the download has existing bytes.
/// Returns `None` when a fresh download should be requested.
#[must_use]
pub fn range_header(plan: &Plan) -> Option<String> {
    if plan.already_have == 0 {
        None
    } else {
        Some(format!("bytes={}-", plan.already_have))
    }
}

/// Parsed HTTP Content-Range representation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ContentRange {
    start: u64,
    end: u64,
    total: Option<u64>,
}

/// Parses a satisfied Content-Range header.
///
/// Expected syntax: `bytes START-END/TOTAL` or `bytes START-END/*`.
fn parse_content_range(value: &str) -> Result<ContentRange, DownloadError> {
    let value = value.trim();
    let remainder = value
        .strip_prefix("bytes ")
        .ok_or(DownloadError::BadContentRange)?;

    let (range, total_text) = remainder
        .split_once('/')
        .ok_or(DownloadError::BadContentRange)?;

    let (start_text, end_text) = range
        .split_once('-')
        .ok_or(DownloadError::BadContentRange)?;

    let start = start_text
        .parse::<u64>()
        .map_err(|_| DownloadError::BadContentRange)?;

    let end = end_text
        .parse::<u64>()
        .map_err(|_| DownloadError::BadContentRange)?;

    if start > end {
        return Err(DownloadError::BadContentRange);
    }

    let total = if total_text == "*" {
        None
    } else {
        let parsed = total_text
            .parse::<u64>()
            .map_err(|_| DownloadError::BadContentRange)?;

        if parsed == 0 {
            return Err(DownloadError::BadContentRange);
        }

        Some(parsed)
    };

    if total.is_some_and(|total| end >= total) {
        return Err(DownloadError::BadContentRange);
    }

    Ok(ContentRange { start, end, total })
}

/// Parses an unsatisfied Content-Range header.
///
/// Expected syntax: `bytes */TOTAL`.
fn parse_unsatisfied_content_range(value: &str) -> Result<u64, DownloadError> {
    let remainder = value
        .trim()
        .strip_prefix("bytes */")
        .ok_or(DownloadError::BadContentRange)?;

    let total = remainder
        .parse::<u64>()
        .map_err(|_| DownloadError::BadContentRange)?;

    if total > MAX_DOWNLOAD_SIZE {
        return Err(DownloadError::TooLarge);
    }

    Ok(total)
}

/// Validates an HTTP response and determines how to handle its body.
///
/// Supported statuses:
///
/// - `206`: Validates Content-Range and appends the response.
/// - `200`: Restarts from zero because the server ignored the Range request.
/// - `416`: Detects already-complete downloads or requests a restart.
///
/// For `206`, Content-Length, when present, must equal the returned
/// byte-range length. A known total must match `expected_size`.
///
/// For `200`, Content-Length, when present, must match `expected_size`.
///
/// Known total sizes and response lengths exceeding 1 TiB are rejected.
pub fn check_response(
    plan: &Plan,
    status: u16,
    content_range: Option<&str>,
    content_length: Option<u64>,
) -> Result<ResumeAction, DownloadError> {
    match status {
        206 => {
            let header = content_range.ok_or(DownloadError::BadContentRange)?;
            let range = parse_content_range(header)?;

            if range.start != plan.already_have {
                return Err(DownloadError::BadContentRange);
            }

            let range_length = range
                .end
                .checked_sub(range.start)
                .and_then(|length| length.checked_add(1))
                .ok_or(DownloadError::BadContentRange)?;

            if let Some(total) = range.total {
                if total > MAX_DOWNLOAD_SIZE {
                    return Err(DownloadError::TooLarge);
                }

                if plan.expected_size.is_some_and(|expected| expected != total) {
                    return Err(DownloadError::BadContentRange);
                }
            } else if plan.expected_size.is_some() {
                return Err(DownloadError::BadContentRange);
            }

            if content_length.is_some_and(|length| length != range_length) {
                return Err(DownloadError::SizeMismatch);
            }

            if content_length.is_some_and(|length| length > MAX_DOWNLOAD_SIZE) {
                return Err(DownloadError::TooLarge);
            }

            let remaining = range
                .total
                .map(|total| total.saturating_sub(range.end.saturating_add(1)));

            Ok(ResumeAction::Append {
                from: range.start,
                remaining,
            })
        }

        200 => {
            if content_length.is_some_and(|length| length > MAX_DOWNLOAD_SIZE)
                || plan
                    .expected_size
                    .is_some_and(|size| size > MAX_DOWNLOAD_SIZE)
            {
                return Err(DownloadError::TooLarge);
            }

            if let (Some(expected), Some(actual)) = (plan.expected_size, content_length) {
                if expected != actual {
                    return Err(DownloadError::SizeMismatch);
                }
            }

            Ok(ResumeAction::Restart {
                remaining: plan.expected_size.or(content_length),
            })
        }

        416 => {
            let total = match content_range {
                Some(header) => Some(parse_unsatisfied_content_range(header)?),
                None => None,
            };

            if total.is_some_and(|size| size == plan.already_have) {
                return Err(DownloadError::AlreadyComplete);
            }

            if let Some(size) = total {
                if plan.expected_size.is_some_and(|expected| expected != size) {
                    return Err(DownloadError::SizeMismatch);
                }
            }

            if plan
                .expected_size
                .is_some_and(|size| size > MAX_DOWNLOAD_SIZE)
            {
                return Err(DownloadError::TooLarge);
            }

            Ok(ResumeAction::Restart {
                remaining: plan.expected_size.or(total),
            })
        }

        _ => Err(DownloadError::HttpStatus(status)),
    }
}

/// Checks whether sufficient storage space is available.
///
/// The required free space is `needed_bytes + reserve_bytes`.
/// If that sum overflows, no storage can satisfy it and the check fails
/// with `needed` reported as `u64::MAX`.
pub fn check_space(
    needed_bytes: u64,
    free_bytes: u64,
    reserve_bytes: u64,
) -> Result<(), DownloadError> {
    let Some(required) = needed_bytes.checked_add(reserve_bytes) else {
        return Err(DownloadError::NoSpace {
            needed: u64::MAX,
            free: free_bytes,
        });
    };

    if free_bytes < required {
        return Err(DownloadError::NoSpace {
            needed: required,
            free: free_bytes,
        });
    }

    Ok(())
}

/// Tracks downloaded bytes and incrementally verifies a hash.
pub struct Verifier {
    hasher: Box<dyn Hasher>,
    bytes_seen: u64,
}

impl fmt::Debug for Verifier {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Verifier")
            .field("bytes_seen", &self.bytes_seen)
            .finish_non_exhaustive()
    }
}

impl Verifier {
    /// Creates a verifier using a caller-provided hasher.
    #[must_use]
    pub fn new(hasher: Box<dyn Hasher>) -> Self {
        Self {
            hasher,
            bytes_seen: 0,
        }
    }

    /// Feeds downloaded bytes into the hash state.
    ///
    /// The byte count uses saturating arithmetic.
    pub fn feed(&mut self, bytes: &[u8]) {
        self.hasher.update(bytes);

        self.bytes_seen = self
            .bytes_seen
            .saturating_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
    }

    /// Returns the number of bytes processed.
    #[must_use]
    pub fn bytes_seen(&self) -> u64 {
        self.bytes_seen
    }

    /// Finalizes verification against the supplied download plan.
    ///
    /// Checks expected size first, then compares the SHA-256 digest
    /// when an expected digest is configured.
    ///
    /// Digest comparison is constant-time for equal-length strings.
    pub fn finish(self, plan: &Plan) -> Result<(), DownloadError> {
        if plan
            .expected_size
            .is_some_and(|size| size != self.bytes_seen)
        {
            return Err(DownloadError::WrongSize);
        }

        if let Some(expected) = &plan.expected_sha256 {
            let normalized =
                normalize_sha256_hex(expected).ok_or(DownloadError::InvalidExpectedHash)?;

            let actual = self.hasher.finish_hex();

            if !constant_time_equal(normalized.as_bytes(), actual.as_bytes()) {
                return Err(DownloadError::HashMismatch {
                    expected: normalized,
                    actual,
                });
            }
        }

        Ok(())
    }
}

/// Compares equal-length byte strings without early exit.
///
/// Returns false immediately when lengths differ.
fn constant_time_equal(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }

    let mut difference = 0_u8;

    for (left_byte, right_byte) in left.iter().zip(right.iter()) {
        difference |= left_byte ^ right_byte;
    }

    difference == 0
}

/// Validates a SHA-256 hexadecimal digest.
///
/// Accepts exactly 64 hexadecimal characters, uppercase or lowercase.
#[must_use]
pub fn validate_sha256_hex(value: &str) -> bool {
    value.len() == SHA256_HEX_LENGTH && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// Normalizes a SHA-256 hexadecimal digest to lowercase.
///
/// Returns `None` if the input is not exactly 64 hexadecimal characters.
#[must_use]
pub fn normalize_sha256_hex(value: &str) -> Option<String> {
    if !validate_sha256_hex(value) {
        return None;
    }

    Some(value.to_ascii_lowercase())
}

/// Decodes percent-encoded URL path bytes.
fn percent_decode(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());

    let mut position = 0_usize;

    while position < bytes.len() {
        let byte = bytes.get(position).copied()?;

        if byte == b'%' {
            let first = *bytes.get(position.checked_add(1)?)?;
            let second = *bytes.get(position.checked_add(2)?)?;

            // `from_str_radix` would also accept a leading `+`; require real hex digits.
            if !first.is_ascii_hexdigit() || !second.is_ascii_hexdigit() {
                return None;
            }

            let hex = [first, second];
            let hex = std::str::from_utf8(&hex).ok()?;

            decoded.push(u8::from_str_radix(hex, 16).ok()?);

            position = position.checked_add(3)?;
        } else {
            decoded.push(byte);
            position = position.checked_add(1)?;
        }
    }

    String::from_utf8(decoded).ok()
}

/// Returns a safe model filename from a URL.
///
/// Rules:
///
/// - Uses the final URL path segment.
/// - Removes query strings and fragments.
/// - Percent-decodes the filename.
/// - Accepts only ASCII letters, digits, `.`, `_`, and `-`.
/// - Requires a `.gguf` extension, case-insensitively.
/// - Rejects leading dots, `.` and `..`.
/// - Requires a decoded filename length between 1 and 100 bytes.
///
/// Encoded path separators and Unicode filenames are rejected.
#[must_use]
pub fn safe_file_name(url: &str) -> Option<String> {
    let without_fragment = url.split('#').next()?;
    let without_query = without_fragment.split('?').next()?;

    let path = if let Some(scheme_end) = without_query.find("://") {
        let authority_start = scheme_end.checked_add(3)?;
        let remainder = without_query.get(authority_start..)?;

        let path_start = remainder.find('/')?;

        remainder.get(path_start..)?
    } else {
        without_query
    };

    let filename = path.rsplit('/').next()?;

    if filename.is_empty() {
        return None;
    }

    let decoded = percent_decode(filename)?;

    if !(1..=100).contains(&decoded.len()) {
        return None;
    }

    if decoded == "." || decoded == ".." || decoded.starts_with('.') {
        return None;
    }

    if !decoded
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return None;
    }

    if !decoded.to_ascii_lowercase().ends_with(".gguf") {
        return None;
    }

    Some(decoded)
}

/// Download progress information.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Progress {
    /// Completion percentage, when total size is known.
    ///
    /// The result is between 0 and 100.
    pub percent: Option<u8>,

    /// Average download speed in bytes per second.
    pub bytes_per_sec: u64,

    /// Estimated remaining seconds, when total and speed are known.
    pub eta_secs: Option<u64>,
}

/// Calculates download progress using caller-provided elapsed time.
///
/// No clock is read. If elapsed time is zero, speed is reported as zero.
/// Unknown totals produce no percentage or ETA.
#[must_use]
pub fn progress(done: u64, total: Option<u64>, elapsed_ms: u64) -> Progress {
    let bytes_per_sec = if elapsed_ms == 0 {
        0
    } else {
        let speed = u128::from(done).saturating_mul(1000) / u128::from(elapsed_ms);

        u64::try_from(speed).unwrap_or(u64::MAX)
    };

    let percent = total.map(|total| {
        if total == 0 {
            return if done == 0 { 0 } else { 100 };
        }

        let value = u128::from(done).saturating_mul(100) / u128::from(total);

        u8::try_from(value.min(100)).unwrap_or(100)
    });

    let eta_secs = total.and_then(|total| {
        let remaining = total.saturating_sub(done);

        if remaining == 0 {
            return Some(0);
        }

        if bytes_per_sec == 0 {
            return None;
        }

        Some(remaining.div_ceil(bytes_per_sec))
    });

    Progress {
        percent,
        bytes_per_sec,
        eta_secs,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HASH_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    const HASH_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn plan(already_have: u64, expected_size: Option<u64>) -> Plan {
        Plan {
            url: "https://example.com/model.gguf".to_owned(),
            expected_size,
            expected_sha256: None,
            already_have,
        }
    }

    /// Test-only fake hasher returning a predetermined digest.
    struct FakeHasher {
        digest: String,
    }

    impl FakeHasher {
        fn new(digest: &str) -> Self {
            Self {
                digest: digest.to_owned(),
            }
        }
    }

    impl Hasher for FakeHasher {
        fn update(&mut self, _bytes: &[u8]) {}

        fn finish_hex(self: Box<Self>) -> String {
            self.digest
        }
    }

    #[test]
    fn range_header_for_fresh_download() {
        let download = plan(0, Some(1000));

        assert_eq!(range_header(&download), None);
    }

    #[test]
    fn range_header_for_resumed_download() {
        let download = plan(1024, Some(10000));

        assert_eq!(range_header(&download), Some("bytes=1024-".to_owned()));
    }

    #[test]
    fn valid_partial_response() {
        let download = plan(1000, Some(5000));

        let result = check_response(&download, 206, Some("bytes 1000-1999/5000"), Some(1000));

        assert_eq!(
            result,
            Ok(ResumeAction::Append {
                from: 1000,
                remaining: Some(3000),
            })
        );
    }

    #[test]
    fn partial_response_without_content_length() {
        let download = plan(1000, Some(5000));

        let result = check_response(&download, 206, Some("bytes 1000-1999/5000"), None);

        assert_eq!(
            result,
            Ok(ResumeAction::Append {
                from: 1000,
                remaining: Some(3000),
            })
        );
    }

    #[test]
    fn partial_response_with_unknown_total() {
        let download = plan(1000, None);

        let result = check_response(&download, 206, Some("bytes 1000-1999/*"), Some(1000));

        assert_eq!(
            result,
            Ok(ResumeAction::Append {
                from: 1000,
                remaining: None,
            })
        );
    }

    #[test]
    fn partial_response_rejects_unknown_total_when_size_expected() {
        let download = plan(1000, Some(5000));

        assert_eq!(
            check_response(&download, 206, Some("bytes 1000-1999/*"), Some(1000)),
            Err(DownloadError::BadContentRange)
        );
    }

    #[test]
    fn partial_response_rejects_wrong_start() {
        let download = plan(1000, Some(5000));

        assert_eq!(
            check_response(&download, 206, Some("bytes 999-1999/5000"), Some(1001)),
            Err(DownloadError::BadContentRange)
        );
    }

    #[test]
    fn partial_response_rejects_end_before_start() {
        let download = plan(1000, Some(5000));

        assert_eq!(
            check_response(&download, 206, Some("bytes 1000-999/5000"), None),
            Err(DownloadError::BadContentRange)
        );
    }

    #[test]
    fn partial_response_rejects_end_beyond_total() {
        let download = plan(1000, Some(5000));

        assert_eq!(
            check_response(&download, 206, Some("bytes 1000-5000/5000"), Some(4001)),
            Err(DownloadError::BadContentRange)
        );
    }

    #[test]
    fn partial_response_rejects_wrong_total() {
        let download = plan(1000, Some(5000));

        assert_eq!(
            check_response(&download, 206, Some("bytes 1000-1999/6000"), Some(1000)),
            Err(DownloadError::BadContentRange)
        );
    }

    #[test]
    fn partial_response_rejects_missing_content_range() {
        let download = plan(1000, Some(5000));

        assert_eq!(
            check_response(&download, 206, None, Some(1000)),
            Err(DownloadError::BadContentRange)
        );
    }

    #[test]
    fn partial_response_rejects_malformed_content_range() {
        let download = plan(1000, None);

        let cases = [
            "1000-1999/5000",
            "bytes 1000-1999",
            "bytes 1000/5000",
            "bytes abc-def/5000",
            "bytes 1000-1999/abc",
            "bytes 1000-1999/0",
            "bytes 1000-1999/5000 extra",
            "Bytes 1000-1999/5000",
            "bytes 1000--1999/5000",
        ];

        for header in cases {
            assert_eq!(
                check_response(&download, 206, Some(header), None),
                Err(DownloadError::BadContentRange),
                "{header}"
            );
        }
    }

    #[test]
    fn partial_response_rejects_content_length_mismatch() {
        let download = plan(1000, Some(5000));

        assert_eq!(
            check_response(&download, 206, Some("bytes 1000-1999/5000"), Some(999)),
            Err(DownloadError::SizeMismatch)
        );
    }

    #[test]
    fn partial_response_rejects_oversized_total() {
        let download = plan(1000, None);

        assert_eq!(
            check_response(
                &download,
                206,
                Some("bytes 1000-1999/1099511627777"),
                Some(1000),
            ),
            Err(DownloadError::TooLarge)
        );
    }

    #[test]
    fn full_response_restarts_resume() {
        let download = plan(1000, Some(5000));

        assert_eq!(
            check_response(&download, 200, None, Some(5000)),
            Ok(ResumeAction::Restart {
                remaining: Some(5000),
            })
        );
    }

    #[test]
    fn full_response_starts_fresh_download() {
        let download = plan(0, None);

        assert_eq!(
            check_response(&download, 200, None, Some(5000)),
            Ok(ResumeAction::Restart {
                remaining: Some(5000),
            })
        );
    }

    #[test]
    fn full_response_without_content_length() {
        let download = plan(1000, Some(5000));

        assert_eq!(
            check_response(&download, 200, None, None),
            Ok(ResumeAction::Restart {
                remaining: Some(5000),
            })
        );
    }

    #[test]
    fn full_response_rejects_size_mismatch() {
        let download = plan(1000, Some(5000));

        assert_eq!(
            check_response(&download, 200, None, Some(4000)),
            Err(DownloadError::SizeMismatch)
        );
    }

    #[test]
    fn full_response_rejects_oversized_content() {
        let download = plan(0, None);

        assert_eq!(
            check_response(&download, 200, None, Some(MAX_DOWNLOAD_SIZE + 1)),
            Err(DownloadError::TooLarge)
        );
    }

    #[test]
    fn range_not_satisfiable_already_complete() {
        let download = plan(5000, Some(5000));

        assert_eq!(
            check_response(&download, 416, Some("bytes */5000"), None),
            Err(DownloadError::AlreadyComplete)
        );
    }

    #[test]
    fn range_not_satisfiable_requires_restart_when_incomplete() {
        let download = plan(4000, Some(5000));

        assert_eq!(
            check_response(&download, 416, Some("bytes */5000"), None),
            Ok(ResumeAction::Restart {
                remaining: Some(5000),
            })
        );
    }

    #[test]
    fn range_not_satisfiable_without_content_range() {
        let download = plan(4000, Some(5000));

        assert_eq!(
            check_response(&download, 416, None, None),
            Ok(ResumeAction::Restart {
                remaining: Some(5000),
            })
        );
    }

    #[test]
    fn range_not_satisfiable_rejects_wrong_expected_size() {
        let download = plan(4000, Some(6000));

        assert_eq!(
            check_response(&download, 416, Some("bytes */5000"), None),
            Err(DownloadError::SizeMismatch)
        );
    }

    #[test]
    fn range_not_satisfiable_rejects_malformed_header() {
        let download = plan(4000, None);

        assert_eq!(
            check_response(&download, 416, Some("bytes 4000-5000/6000"), None),
            Err(DownloadError::BadContentRange)
        );
    }

    #[test]
    fn range_not_satisfiable_rejects_oversized_total() {
        let download = plan(4000, None);

        assert_eq!(
            check_response(&download, 416, Some("bytes */1099511627777"), None),
            Err(DownloadError::TooLarge)
        );
    }

    #[test]
    fn unsupported_http_statuses() {
        let download = plan(0, None);

        for status in [201, 301, 302, 400, 401, 403, 404, 408, 429, 500, 503, 504] {
            assert_eq!(
                check_response(&download, status, None, None),
                Err(DownloadError::HttpStatus(status))
            );
        }
    }

    #[test]
    fn check_space_sufficient() {
        assert_eq!(check_space(1000, 2000, 500), Ok(()));
        assert_eq!(check_space(1000, 1500, 500), Ok(()));
    }

    #[test]
    fn check_space_insufficient() {
        assert_eq!(
            check_space(1000, 1499, 500),
            Err(DownloadError::NoSpace {
                needed: 1500,
                free: 1499,
            })
        );
    }

    #[test]
    fn check_space_handles_overflow() {
        assert_eq!(
            check_space(u64::MAX, u64::MAX, 1),
            Err(DownloadError::NoSpace {
                needed: u64::MAX,
                free: u64::MAX,
            })
        );
    }

    #[test]
    fn verifier_tracks_bytes() {
        let hasher = Box::new(FakeHasher::new(HASH_A));
        let mut verifier = Verifier::new(hasher);

        assert_eq!(verifier.bytes_seen(), 0);

        verifier.feed(b"hello");
        assert_eq!(verifier.bytes_seen(), 5);

        verifier.feed(b" world");
        assert_eq!(verifier.bytes_seen(), 11);
    }

    #[test]
    fn verifier_accepts_matching_hash() {
        let mut download = plan(0, Some(11));
        download.expected_sha256 = Some(HASH_A.to_owned());

        let mut verifier = Verifier::new(Box::new(FakeHasher::new(HASH_A)));

        verifier.feed(b"hello");
        verifier.feed(b" world");

        assert_eq!(verifier.finish(&download), Ok(()));
    }

    #[test]
    fn verifier_rejects_wrong_size() {
        let download = plan(0, Some(100));

        let mut verifier = Verifier::new(Box::new(FakeHasher::new(HASH_A)));
        verifier.feed(b"hello");

        assert_eq!(verifier.finish(&download), Err(DownloadError::WrongSize));
    }

    #[test]
    fn verifier_rejects_hash_mismatch() {
        let mut download = plan(0, Some(5));
        download.expected_sha256 = Some(HASH_A.to_owned());

        let mut verifier = Verifier::new(Box::new(FakeHasher::new(HASH_B)));
        verifier.feed(b"hello");

        assert_eq!(
            verifier.finish(&download),
            Err(DownloadError::HashMismatch {
                expected: HASH_A.to_owned(),
                actual: HASH_B.to_owned(),
            })
        );
    }

    #[test]
    fn verifier_rejects_invalid_expected_hash() {
        let mut download = plan(0, None);
        download.expected_sha256 = Some("invalid".to_owned());

        let verifier = Verifier::new(Box::new(FakeHasher::new(HASH_A)));

        assert_eq!(
            verifier.finish(&download),
            Err(DownloadError::InvalidExpectedHash)
        );
    }

    #[test]
    fn verifier_without_expected_hash_checks_size_only() {
        let download = plan(0, Some(5));

        let mut verifier = Verifier::new(Box::new(FakeHasher::new(HASH_A)));
        verifier.feed(b"hello");

        assert_eq!(verifier.finish(&download), Ok(()));
    }

    #[test]
    fn sha256_validation() {
        assert!(validate_sha256_hex(HASH_A));
        assert!(validate_sha256_hex(HASH_B));
        assert!(validate_sha256_hex(&HASH_A.to_uppercase()));

        assert!(!validate_sha256_hex(""));
        assert!(!validate_sha256_hex("abc"));
        assert!(!validate_sha256_hex(&"a".repeat(63)));
        assert!(!validate_sha256_hex(&"a".repeat(65)));

        let invalid_character = format!("{}g", &HASH_A[..63]);
        assert!(!validate_sha256_hex(&invalid_character));
    }

    #[test]
    fn sha256_normalization() {
        assert_eq!(
            normalize_sha256_hex(&HASH_A.to_uppercase()),
            Some(HASH_A.to_owned())
        );

        assert_eq!(normalize_sha256_hex("invalid"), None);
    }

    #[test]
    fn constant_time_comparison_results() {
        assert!(constant_time_equal(b"abcdef", b"abcdef"));
        assert!(!constant_time_equal(b"abcdef", b"abcdeg"));
        assert!(!constant_time_equal(b"abcdef", b"abc"));
        assert!(constant_time_equal(b"", b""));
    }

    #[test]
    fn safe_filename_basic() {
        assert_eq!(
            safe_file_name("https://example.com/models/model.gguf"),
            Some("model.gguf".to_owned())
        );

        assert_eq!(
            safe_file_name("https://example.com/model.gguf?download=true"),
            Some("model.gguf".to_owned())
        );

        assert_eq!(
            safe_file_name("https://example.com/model.gguf#section"),
            Some("model.gguf".to_owned())
        );

        assert_eq!(
            safe_file_name("https://example.com/a/b/model.gguf?x=1#fragment"),
            Some("model.gguf".to_owned())
        );
    }

    #[test]
    fn safe_filename_percent_decoding() {
        assert_eq!(
            safe_file_name("https://example.com/model%2Egguf"),
            Some("model.gguf".to_owned())
        );

        assert_eq!(
            safe_file_name("https://example.com/my%2Dmodel.gguf"),
            Some("my-model.gguf".to_owned())
        );

        assert_eq!(
            safe_file_name("https://example.com/model%5F1.gguf"),
            Some("model_1.gguf".to_owned())
        );

        // A sign is not a hex digit, even though `from_str_radix` accepts it.
        assert_eq!(safe_file_name("https://example.com/model%+f.gguf"), None);
    }

    #[test]
    fn safe_filename_rejects_path_traversal() {
        // Only the final segment is used, so a traversal directory is harmless.
        assert_eq!(
            safe_file_name("https://example.com/%2e%2e/evil.gguf"),
            Some("evil.gguf".to_owned())
        );

        assert_eq!(safe_file_name("https://example.com/a/b/..%2Fx.gguf"), None);

        // Leading dots are rejected even when percent-encoded.
        assert_eq!(safe_file_name("https://example.com/%2e%2e.gguf"), None);
    }

    #[test]
    fn safe_filename_rejects_encoded_separators() {
        assert_eq!(
            safe_file_name("https://example.com/model%2Fsecret.gguf"),
            None
        );

        assert_eq!(
            safe_file_name("https://example.com/model%5Csecret.gguf"),
            None
        );
    }

    #[test]
    fn safe_filename_rejects_unicode() {
        assert_eq!(safe_file_name("https://example.com/模型.gguf"), None);

        assert_eq!(safe_file_name("https://example.com/caf%C3%A9.gguf"), None);
    }

    #[test]
    fn safe_filename_rejects_invalid_names() {
        let cases = [
            "",
            "https://example.com/",
            "https://example.com/model.bin",
            "https://example.com/.gguf",
            "https://example.com/..",
            "https://example.com/.",
            "https://example.com/.hidden.gguf",
            "https://example.com/model name.gguf",
            "https://example.com/model!.gguf",
            "https://example.com/model%ZZ.gguf",
            "https://example.com/%FF.gguf",
        ];

        for value in cases {
            assert_eq!(safe_file_name(value), None, "{value}");
        }
    }

    #[test]
    fn safe_filename_rejects_overlong_names() {
        let filename = format!("{}.gguf", "a".repeat(96));
        let url = format!("https://example.com/{filename}");

        assert_eq!(filename.len(), 101);
        assert_eq!(safe_file_name(&url), None);
    }

    #[test]
    fn safe_filename_accepts_exact_length_limit() {
        let filename = format!("{}.gguf", "a".repeat(95));
        let url = format!("https://example.com/{filename}");

        assert_eq!(filename.len(), 100);
        assert_eq!(safe_file_name(&url), Some(filename));
    }

    #[test]
    fn safe_filename_extension_is_case_insensitive() {
        assert_eq!(
            safe_file_name("https://example.com/model.GGUF"),
            Some("model.GGUF".to_owned())
        );

        assert_eq!(
            safe_file_name("https://example.com/model.GgUf"),
            Some("model.GgUf".to_owned())
        );
    }

    #[test]
    fn progress_with_known_total() {
        assert_eq!(
            progress(50, Some(100), 1000),
            Progress {
                percent: Some(50),
                bytes_per_sec: 50,
                eta_secs: Some(1),
            }
        );
    }

    #[test]
    fn progress_at_completion() {
        assert_eq!(
            progress(100, Some(100), 1000),
            Progress {
                percent: Some(100),
                bytes_per_sec: 100,
                eta_secs: Some(0),
            }
        );
    }

    #[test]
    fn progress_without_known_total() {
        assert_eq!(
            progress(1000, None, 2000),
            Progress {
                percent: None,
                bytes_per_sec: 500,
                eta_secs: None,
            }
        );
    }

    #[test]
    fn progress_zero_elapsed_time() {
        assert_eq!(
            progress(100, Some(200), 0),
            Progress {
                percent: Some(50),
                bytes_per_sec: 0,
                eta_secs: None,
            }
        );
    }

    #[test]
    fn progress_zero_total() {
        assert_eq!(
            progress(0, Some(0), 1000),
            Progress {
                percent: Some(0),
                bytes_per_sec: 0,
                eta_secs: Some(0),
            }
        );

        assert_eq!(
            progress(10, Some(0), 1000),
            Progress {
                percent: Some(100),
                bytes_per_sec: 10,
                eta_secs: Some(0),
            }
        );
    }

    #[test]
    fn progress_never_exceeds_one_hundred_percent() {
        let result = progress(200, Some(100), 1000);

        assert_eq!(result.percent, Some(100));
        assert_eq!(result.eta_secs, Some(0));
    }

    #[test]
    fn progress_rounds_eta_up() {
        assert_eq!(progress(1, Some(4), 1000).eta_secs, Some(3));
    }

    #[test]
    fn progress_handles_large_values() {
        let result = progress(u64::MAX, Some(u64::MAX), 1);

        assert_eq!(result.percent, Some(100));
        assert_eq!(result.bytes_per_sec, u64::MAX);
        assert_eq!(result.eta_secs, Some(0));
    }

    #[test]
    fn hostile_response_matrix() {
        let download = plan(1000, Some(5000));

        for status in [201, 301, 400, 401, 403, 404, 500, 502, 503, 504] {
            assert_eq!(
                check_response(&download, status, None, None),
                Err(DownloadError::HttpStatus(status))
            );
        }
    }

    #[test]
    fn malformed_content_ranges_never_panic() {
        let download = plan(100, None);

        let cases = [
            "",
            "bytes",
            "bytes ",
            "bytes */100",
            "bytes -/100",
            "bytes 100-/200",
            "bytes 100-200/",
            "bytes 100-200/abc",
            "bytes 100-200/100",
            "bytes 100-200/201",
            "bytes 100-200/*",
            "bytes 100-200/18446744073709551616",
            "bytes 18446744073709551615-18446744073709551615/*",
        ];

        for header in cases {
            let _ = check_response(&download, 206, Some(header), None);
        }
    }

    #[test]
    fn download_error_implements_display_and_error() {
        let error = DownloadError::TooLarge;

        assert_eq!(error.to_string(), "download exceeds 1 TiB limit");

        let _: &dyn Error = &error;
    }
}
