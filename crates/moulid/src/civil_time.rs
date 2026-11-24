//! Date/time helpers for UTC proleptic Gregorian civil time.
//!
//! This module intentionally does no I/O and never reads the clock. It provides
//! a small `DateTime` value, conversions to and from Unix seconds, strict
//! RFC 3339 / RTC file parsing, X.509 time decoding, and simple formatting.

/// A UTC civil date and time in the proleptic Gregorian calendar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DateTime {
    /// Year in the proleptic Gregorian calendar.
    pub year: i32,
    /// Month of year, 1 through 12.
    pub month: u8,
    /// Day of month, 1 through the length of the month.
    pub day: u8,
    /// Hour of day, 0 through 23.
    pub hour: u8,
    /// Minute of hour, 0 through 59.
    pub minute: u8,
    /// Second of minute, 0 through 59; leap seconds are not supported.
    pub second: u8,
}

/// Errors returned by parsing and decoding helpers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeError {
    /// The input has the wrong length for the expected format.
    InvalidLength,
    /// The input contains characters outside the grammar at a position.
    InvalidFormat,
    /// The input is structurally well formed but not a valid date/time.
    InvalidValue,
    /// The DER tag is not UTCTime or GeneralizedTime.
    UnsupportedTag,
    /// The time uses a form other than uppercase `Z` Zulu form.
    UnsupportedForm,
    /// The value cannot be represented on this platform or in Unix seconds.
    OutOfRange,
}

impl std::fmt::Display for TimeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidLength => write!(f, "invalid length"),
            Self::InvalidFormat => write!(f, "invalid format"),
            Self::InvalidValue => write!(f, "invalid date or time"),
            Self::UnsupportedTag => write!(f, "unsupported DER tag"),
            Self::UnsupportedForm => write!(f, "unsupported time form"),
            Self::OutOfRange => write!(f, "value out of range"),
        }
    }
}

impl std::error::Error for TimeError {}

const PLAUSIBLE_START: i64 = 1_735_689_600;
const PLAUSIBLE_END: i64 = 4_102_444_800;

impl DateTime {
    /// Creates a `DateTime` from Unix seconds.
    ///
    /// Returns `None` outside the inclusive year range 1970 through 9999.
    #[must_use]
    pub fn from_unix(secs: i64) -> Option<Self> {
        let days = secs.div_euclid(86_400);
        let rem = secs.rem_euclid(86_400);
        let (year, month, day) = civil_from_days(days)?;
        if !(1970..=9999).contains(&year) {
            return None;
        }
        let hour = u8::try_from(rem / 3_600).ok()?;
        let minute = u8::try_from((rem / 60) % 60).ok()?;
        let second = u8::try_from(rem % 60).ok()?;
        Some(Self {
            year: i32::try_from(year).ok()?,
            month,
            day,
            hour,
            minute,
            second,
        })
    }

    /// Converts this value to Unix seconds.
    ///
    /// Returns `None` unless the value is valid and inside the inclusive year
    /// range 1970 through 9999.
    #[must_use]
    pub fn to_unix(&self) -> Option<i64> {
        if !self.is_valid() || !(1970..=9999).contains(&self.year) {
            return None;
        }
        unix_seconds(self)
    }

    /// Checks month lengths, leap years, and time-of-day ranges.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        let month_length = match self.month {
            1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
            4 | 6 | 9 | 11 => 30,
            2 => {
                if is_leap_year(self.year) {
                    29
                } else {
                    28
                }
            }
            _ => return false,
        };
        (1..=month_length).contains(&self.day)
            && self.hour < 24
            && self.minute < 60
            && self.second <= 59
    }

    /// Parses exactly `YYYY-MM-DDTHH:MM:SSZ`; `t` and `z` may be lowercase.
    ///
    /// Offsets, fractional seconds, missing fields, and any other trailing or
    /// leading data are rejected.
    ///
    /// # Errors
    /// A [`TimeError`] describing why the text is not an exact UTC timestamp.
    pub fn parse_rfc3339_utc(s: &str) -> Result<Self, TimeError> {
        let bytes = s.as_bytes();
        if bytes.len() != 20 {
            return Err(TimeError::InvalidLength);
        }
        if !matches!(bytes.get(4), Some(&b'-'))
            || !matches!(bytes.get(7), Some(&b'-'))
            || !matches!(bytes.get(13), Some(&b':'))
            || !matches!(bytes.get(16), Some(&b':'))
        {
            return Err(TimeError::InvalidFormat);
        }
        let marker = *bytes.get(10).ok_or(TimeError::InvalidLength)?;
        if marker != b'T' && marker != b't' {
            return Err(TimeError::InvalidFormat);
        }
        let zed = *bytes.get(19).ok_or(TimeError::InvalidLength)?;
        if zed != b'Z' && zed != b'z' {
            return Err(TimeError::UnsupportedForm);
        }
        let (year, month, day) = parse_date(bytes.get(0..10).ok_or(TimeError::InvalidLength)?)
            .ok_or(TimeError::InvalidFormat)?;
        let (hour, minute, second) = parse_time(bytes.get(11..19).ok_or(TimeError::InvalidLength)?)
            .ok_or(TimeError::InvalidFormat)?;
        let value = Self {
            year,
            month,
            day,
            hour,
            minute,
            second,
        };
        if value.is_valid() {
            Ok(value)
        } else {
            Err(TimeError::InvalidValue)
        }
    }

    /// Parses the separate contents of RTC `date` and `time` files.
    ///
    /// Trailing carriage returns and line feeds are trimmed from both inputs.
    /// The date must be `YYYY-MM-DD` and the time must be `HH:MM:SS`.
    ///
    /// # Errors
    /// A [`TimeError`] when either file's text is malformed or not a real time.
    pub fn from_rtc_files(date: &str, time: &str) -> Result<Self, TimeError> {
        let date = date.trim_end_matches(['\n', '\r']);
        let time = time.trim_end_matches(['\n', '\r']);
        if date.len() != 10 || time.len() != 8 {
            return Err(TimeError::InvalidLength);
        }
        let (year, month, day) = parse_date(date.as_bytes()).ok_or(TimeError::InvalidFormat)?;
        let (hour, minute, second) = parse_time(time.as_bytes()).ok_or(TimeError::InvalidFormat)?;
        let value = Self {
            year,
            month,
            day,
            hour,
            minute,
            second,
        };
        if value.is_valid() {
            Ok(value)
        } else {
            Err(TimeError::InvalidValue)
        }
    }

    /// Formats as `YYYY-MM-DDTHH:MM:SSZ`.
    #[must_use]
    pub fn format_rfc3339(&self) -> String {
        format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
            self.year, self.month, self.day, self.hour, self.minute, self.second
        )
    }

    /// Formats as `DD Mon YYYY HH:MM:SS UTC`, for example `24 Nov 2026 12:34:56 UTC`.
    #[must_use]
    pub fn format_human(&self) -> String {
        format!(
            "{:02} {} {:04} {:02}:{:02}:{:02} UTC",
            self.day,
            month_abbrev(self.month),
            self.year,
            self.hour,
            self.minute,
            self.second
        )
    }
}

/// Decodes an X.509 UTCTime (`0x17`) or GeneralizedTime (`0x18`) value.
///
/// Only the uppercase `Z` form is accepted. UTCTime years below 50 are decoded
/// as 20YY and years 50 through 99 are decoded as 19YY.
///
/// # Errors
/// A [`TimeError`] for an unknown tag, wrong length or form, bad digits, or an
/// impossible date.
pub fn der_time_to_unix(tag: u8, content: &[u8]) -> Result<i64, TimeError> {
    let year_digits = match tag {
        0x17 => 2_usize,
        0x18 => 4_usize,
        _ => return Err(TimeError::UnsupportedTag),
    };
    let expected_len = year_digits + 11;
    if content.len() != expected_len {
        return Err(TimeError::InvalidLength);
    }
    if *content
        .get(expected_len - 1)
        .ok_or(TimeError::InvalidLength)?
        != b'Z'
    {
        return Err(TimeError::UnsupportedForm);
    }
    let year_raw = parse_digits(content, 0, year_digits).ok_or(TimeError::InvalidFormat)?;
    let year_i32 = i32::try_from(year_raw).map_err(|_| TimeError::OutOfRange)?;
    let year = if year_digits == 2 {
        if year_i32 >= 50 {
            1900 + year_i32
        } else {
            2000 + year_i32
        }
    } else {
        year_i32
    };
    let field = |offset: usize| -> Result<u8, TimeError> {
        let value =
            parse_digits(content, year_digits + offset, 2).ok_or(TimeError::InvalidFormat)?;
        u8::try_from(value).map_err(|_| TimeError::OutOfRange)
    };
    let value = DateTime {
        year,
        month: field(0)?,
        day: field(2)?,
        hour: field(4)?,
        minute: field(6)?,
        second: field(8)?,
    };
    if !value.is_valid() {
        return Err(TimeError::InvalidValue);
    }
    unix_seconds(&value).ok_or(TimeError::OutOfRange)
}

/// Returns true only after 2025-01-01T00:00:00Z and before 2100-01-01T00:00:00Z.
#[must_use]
pub fn plausible(unix: i64) -> bool {
    unix > PLAUSIBLE_START && unix < PLAUSIBLE_END
}

fn is_leap_year(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

fn unix_seconds(dt: &DateTime) -> Option<i64> {
    let days = days_from_civil(i64::from(dt.year), i64::from(dt.month), i64::from(dt.day));
    let seconds = i64::from(dt.hour) * 3_600 + i64::from(dt.minute) * 60 + i64::from(dt.second);
    days.checked_mul(86_400)?.checked_add(seconds)
}

fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let adjusted_year = if month <= 2 { year - 1 } else { year };
    let era = (if adjusted_year >= 0 {
        adjusted_year
    } else {
        adjusted_year - 399
    }) / 400;
    let year_of_era = adjusted_year - era * 400;
    let month_prime = if month > 2 { month - 3 } else { month + 9 };
    let day_of_year = (153 * month_prime + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

fn civil_from_days(z: i64) -> Option<(i64, u8, u8)> {
    let shifted = z.checked_add(719_468)?;
    let era = (if shifted >= 0 {
        shifted
    } else {
        shifted.checked_sub(146_096)?
    }) / 146_097;
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    };
    let output_year = year + i64::from(month <= 2);
    Some((
        output_year,
        u8::try_from(month).ok()?,
        u8::try_from(day).ok()?,
    ))
}

fn parse_digits(bytes: &[u8], start: usize, len: usize) -> Option<u32> {
    let mut value = 0_u32;
    for offset in 0..len {
        let index = start.checked_add(offset)?;
        let byte = *bytes.get(index)?;
        if !byte.is_ascii_digit() {
            return None;
        }
        value = value.checked_mul(10)?.checked_add(u32::from(byte - b'0'))?;
    }
    Some(value)
}

fn parse_date(bytes: &[u8]) -> Option<(i32, u8, u8)> {
    if bytes.len() != 10 {
        return None;
    }
    if !matches!(bytes.get(4), Some(&b'-')) || !matches!(bytes.get(7), Some(&b'-')) {
        return None;
    }
    let year = i32::try_from(parse_digits(bytes, 0, 4)?).ok()?;
    let month = u8::try_from(parse_digits(bytes, 5, 2)?).ok()?;
    let day = u8::try_from(parse_digits(bytes, 8, 2)?).ok()?;
    Some((year, month, day))
}

fn parse_time(bytes: &[u8]) -> Option<(u8, u8, u8)> {
    if bytes.len() != 8 {
        return None;
    }
    if !matches!(bytes.get(2), Some(&b':')) || !matches!(bytes.get(5), Some(&b':')) {
        return None;
    }
    let hour = u8::try_from(parse_digits(bytes, 0, 2)?).ok()?;
    let minute = u8::try_from(parse_digits(bytes, 3, 2)?).ok()?;
    let second = u8::try_from(parse_digits(bytes, 6, 2)?).ok()?;
    Some((hour, minute, second))
}

fn month_abbrev(month: u8) -> &'static str {
    match month {
        1 => "Jan",
        2 => "Feb",
        3 => "Mar",
        4 => "Apr",
        5 => "May",
        6 => "Jun",
        7 => "Jul",
        8 => "Aug",
        9 => "Sep",
        10 => "Oct",
        11 => "Nov",
        12 => "Dec",
        _ => "???",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unix_epoch_boundaries() {
        let epoch = DateTime {
            year: 1970,
            month: 1,
            day: 1,
            hour: 0,
            minute: 0,
            second: 0,
        };
        assert_eq!(DateTime::from_unix(0), Some(epoch));
        assert_eq!(epoch.to_unix(), Some(0));
        assert_eq!(DateTime::from_unix(-1), None);
        assert_eq!(
            DateTime {
                year: 1969,
                month: 12,
                day: 31,
                hour: 23,
                minute: 59,
                second: 59,
            }
            .to_unix(),
            None
        );
    }

    #[test]
    fn known_dates() {
        let y2k = DateTime {
            year: 2000,
            month: 2,
            day: 29,
            hour: 0,
            minute: 0,
            second: 0,
        };
        assert!(y2k.is_valid());
        assert_eq!(
            DateTime::parse_rfc3339_utc("2000-02-29T00:00:00Z").unwrap(),
            y2k
        );
        assert!(!DateTime {
            year: 2100,
            month: 2,
            day: 29,
            hour: 0,
            minute: 0,
            second: 0,
        }
        .is_valid());
        assert!(DateTime::parse_rfc3339_utc("2100-02-29T00:00:00Z").is_err());

        let end_2038 = DateTime::parse_rfc3339_utc("2038-01-19T03:14:08Z").unwrap();
        assert_eq!(end_2038.to_unix(), Some(2_147_483_648));
        assert_eq!(
            DateTime::from_unix(2_147_483_647).unwrap().format_rfc3339(),
            "2038-01-19T03:14:07Z"
        );

        let max = DateTime {
            year: 9999,
            month: 12,
            day: 31,
            hour: 23,
            minute: 59,
            second: 59,
        };
        let max_unix = max.to_unix().unwrap();
        assert_eq!(DateTime::from_unix(max_unix), Some(max));
        assert_eq!(DateTime::from_unix(max_unix + 1), None);
    }

    #[test]
    fn round_trip_pseudo_random_values() {
        let max_secs = DateTime {
            year: 9999,
            month: 12,
            day: 31,
            hour: 23,
            minute: 59,
            second: 59,
        }
        .to_unix()
        .unwrap();
        let span = u64::try_from(max_secs).unwrap() + 1;
        let mut x = 0x243f_6a88_85a3_08d3_u64;
        for _ in 0..1000 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            let secs = i64::try_from(x % span).unwrap();
            let dt = DateTime::from_unix(secs).unwrap();
            assert!(dt.is_valid());
            assert_eq!(dt.to_unix(), Some(secs));
        }
    }

    #[test]
    fn rfc3339_accepts_expected_forms_only() {
        assert_eq!(
            DateTime::parse_rfc3339_utc("2026-11-24T12:34:56Z")
                .unwrap()
                .format_rfc3339(),
            "2026-11-24T12:34:56Z"
        );
        assert!(DateTime::parse_rfc3339_utc("2026-11-24t12:34:56z").is_ok());
        for bad in [
            "",
            "2026-11-24 12:34:56Z",
            "2026/11/24T12:34:56Z",
            "2026-11-24T12:34:56+00:00",
            "2026-11-24T12:34:56-01:00",
            "2026-11-24T12:34:56.000Z",
            "2026-11-24T12:34:56",
            "2026-11-24T12:34:56ZZ",
            " 2026-11-24T12:34:56Z",
            "2026-11-24T12:34:56Z ",
            "2026-13-24T00:00:00Z",
            "2026-00-24T00:00:00Z",
            "2026-11-00T00:00:00Z",
            "2026-11-24T24:00:00Z",
            "2026-11-24T12:60:00Z",
            "2026-11-24T12:34:60Z",
            "2026-02-30T00:00:00Z",
            "2026-11-24x12:34:56Z",
            "abcd-11-24T12:34:56Z",
        ] {
            assert!(DateTime::parse_rfc3339_utc(bad).is_err(), "case: {bad:?}");
        }
    }

    #[test]
    fn rtc_file_parsing() {
        let expected = DateTime {
            year: 2026,
            month: 11,
            day: 24,
            hour: 12,
            minute: 34,
            second: 56,
        };
        assert_eq!(
            DateTime::from_rtc_files("2026-11-24\n", "12:34:56\n").unwrap(),
            expected
        );
        assert_eq!(
            DateTime::from_rtc_files("2026-11-24\r\n", "12:34:56\r\n").unwrap(),
            expected
        );
        assert_eq!(
            DateTime::from_rtc_files("2026-11-24", "12:34:56").unwrap(),
            expected
        );
        assert!(DateTime::from_rtc_files("2026-11-24\n\n", "12:34:56\n\n").is_ok());
        for (date, time) in [
            ("", "12:34:56\n"),
            ("2026-11-24\n", ""),
            (" 2026-11-24\n", "12:34:56\n"),
            ("2026-11-24\n", " 12:34:56\n"),
            ("2026-11-24\n", "12:34:56Z\n"),
            ("2026-11-24T00:00:00Z\n", "12:34:56\n"),
            ("2026-13-24\n", "12:34:56\n"),
            ("2026-11-24\n", "24:00:00\n"),
            ("2026-11-24\n", "12:34:60\n"),
            ("20261124\n", "12:34:56\n"),
            ("2026-11-24\n", "123456\n"),
        ] {
            assert!(DateTime::from_rtc_files(date, time).is_err());
        }
    }

    #[test]
    fn formats_values() {
        let dt = DateTime {
            year: 2026,
            month: 11,
            day: 24,
            hour: 12,
            minute: 34,
            second: 56,
        };
        assert_eq!(dt.format_rfc3339(), "2026-11-24T12:34:56Z");
        assert_eq!(dt.format_human(), "24 Nov 2026 12:34:56 UTC");
        assert_eq!(
            DateTime {
                year: 1970,
                month: 1,
                day: 1,
                hour: 0,
                minute: 0,
                second: 0,
            }
            .format_human(),
            "01 Jan 1970 00:00:00 UTC"
        );
    }

    #[test]
    fn der_utc_time_pivot() {
        let y49 = der_time_to_unix(0x17, b"490101000000Z").unwrap();
        let y50 = der_time_to_unix(0x17, b"500101000000Z").unwrap();
        let y99 = der_time_to_unix(0x17, b"990101000000Z").unwrap();
        let y00 = der_time_to_unix(0x17, b"000101000000Z").unwrap();
        assert!(y49 > 0);
        assert!(y50 < 0);
        assert!(y00 > y99);
        assert_eq!(y49, der_time_to_unix(0x18, b"20490101000000Z").unwrap());
        assert_eq!(y50, der_time_to_unix(0x18, b"19500101000000Z").unwrap());
    }

    #[test]
    fn der_rejects_malformed_inputs() {
        for (tag, content) in [
            (0x16, b"490101000000Z".as_slice()),
            (0x17, b"49010100000Z".as_slice()),
            (0x17, b"4901010000000Z".as_slice()),
            (0x17, b"490101000000z".as_slice()),
            (0x17, b"49A101000000Z".as_slice()),
            (0x17, b"49010100000+Z".as_slice()),
            (0x17, b"491301000000Z".as_slice()),
            (0x17, b"490230000000Z".as_slice()),
            (0x17, b"490101240000Z".as_slice()),
            (0x17, b"490101006000Z".as_slice()),
            (0x17, b"490101000060Z".as_slice()),
            (0x18, b"2049010100000Z".as_slice()),
            (0x18, b"204901010000000Z".as_slice()),
            (0x18, b"20490101000000z".as_slice()),
            (0x18, b"20491301000000Z".as_slice()),
            (0x18, b"21000229000000Z".as_slice()),
        ] {
            assert!(der_time_to_unix(tag, content).is_err());
        }
    }

    #[test]
    fn plausible_boundaries() {
        let start = 1_735_689_600;
        let end = 4_102_444_800;
        assert!(!plausible(0));
        assert!(!plausible(start));
        assert!(plausible(start + 1));
        assert!(plausible(end - 1));
        assert!(!plausible(end));
        assert!(!plausible(i64::MIN));
        assert!(!plausible(i64::MAX));
    }
}
