//! Pure packet logic for SNTPv4 (RFC 4330).
//!
//! The caller owns UDP send/receive and supplies all times as parameters.
//! This module only builds request packets and validates/decodes replies.

use std::error::Error;
use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

const NTP_UNIX_EPOCH_DELTA: i128 = 2_208_988_800;
const MODULUS_2_32: i128 = 4_294_967_296;
const HIGH_BIT: i128 = 2_147_483_648;
const MAX_ROUND_TRIP_NS: i128 = 5_000_000_000;

/// Decoded SNTP correction for a client clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SntpFix {
    /// Server time corrected for half the round trip, expressed as whole Unix seconds.
    pub unix_seconds: i64,
    /// Estimated clock offset in milliseconds: `((t2 - t1) + (t3 - t4)) / 2`.
    pub offset_ms: i64,
    /// Measured round-trip time in milliseconds.
    pub round_trip_ms: u32,
}

/// Errors returned while validating an SNTP reply packet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SntpError {
    /// Reply buffer length was not at least 48 bytes.
    WrongLength,
    /// Reply mode was not server mode 4.
    InvalidMode(u8),
    /// Reply NTP version was below 3.
    UnsupportedVersion(u8),
    /// Leap indicator was 3, meaning the server is unsynchronised.
    LeapIndicatorUnsynchronized,
    /// Stratum was 0; the reference identifier contains the kiss-o'-death code.
    KissODeath(String),
    /// Stratum was greater than 15.
    StratumTooHigh(u8),
    /// Originate timestamp did not match the timestamp placed in the request.
    OriginateMismatch,
    /// Transmit timestamp was all zero.
    ZeroTransmitTimestamp,
    /// Round trip exceeded 5000 milliseconds.
    RoundTripTooLong,
    /// Round trip computed to a negative duration.
    NegativeRoundTrip,
}

impl fmt::Display for SntpError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WrongLength => formatter.write_str("SNTP reply shorter than 48 bytes"),
            Self::InvalidMode(mode) => {
                write!(formatter, "SNTP reply mode {mode} is not server mode 4")
            }
            Self::UnsupportedVersion(version) => {
                write!(formatter, "SNTP version {version} is below 3")
            }
            Self::LeapIndicatorUnsynchronized => {
                formatter.write_str("SNTP server reports it is unsynchronised")
            }
            Self::KissODeath(code) => write!(formatter, "SNTP kiss-o'-death code {code}"),
            Self::StratumTooHigh(stratum) => {
                write!(formatter, "SNTP stratum {stratum} is above 15")
            }
            Self::OriginateMismatch => {
                formatter.write_str("SNTP reply originate timestamp does not match request")
            }
            Self::ZeroTransmitTimestamp => {
                formatter.write_str("SNTP reply has a zero transmit timestamp")
            }
            Self::RoundTripTooLong => formatter.write_str("SNTP round trip exceeds 5000 ms"),
            Self::NegativeRoundTrip => formatter.write_str("SNTP round trip is negative"),
        }
    }
}

impl Error for SntpError {}

/// Builds a 48-byte SNTPv4 client request packet.
///
/// LI is 0, VN is 4, Mode is 3, every other field is zero except the
/// transmit timestamp, which is `transmit_time` encoded as an NTP 64-bit
/// timestamp.
#[must_use]
pub fn request(transmit_time: SystemTime) -> [u8; 48] {
    let mut packet = [0u8; 48];
    packet[0] = 0x23; // LI = 0, VN = 4, Mode = 3
    let timestamp = unix_nanos_to_ntp64(system_time_to_unix_nanos(transmit_time));
    packet[40..48].copy_from_slice(&timestamp.to_be_bytes());
    packet
}

/// Parses and validates a 48-byte (or larger) SNTPv4 server reply.
///
/// `sent` is the system time placed in the request's transmit timestamp and
/// `now` is the system time at which the reply was received. Timestamps whose
/// NTP seconds field has the high bit clear are interpreted as era 1, after
/// 2036, and timestamps with the high bit set are interpreted as era 0.
///
/// # Errors
/// An [`SntpError`] naming the first check the reply failed.
pub fn parse_reply(buf: &[u8], sent: SystemTime, now: SystemTime) -> Result<SntpFix, SntpError> {
    if buf.len() < 48 {
        return Err(SntpError::WrongLength);
    }

    let first = *buf.first().ok_or(SntpError::WrongLength)?;
    let leap = first >> 6;
    let version = (first >> 3) & 0x07;
    let mode = first & 0x07;
    let stratum = *buf.get(1).ok_or(SntpError::WrongLength)?;

    if mode != 4 {
        return Err(SntpError::InvalidMode(mode));
    }
    if version < 3 {
        return Err(SntpError::UnsupportedVersion(version));
    }
    if leap == 3 {
        return Err(SntpError::LeapIndicatorUnsynchronized);
    }
    if stratum == 0 {
        let code_bytes = buf.get(12..16).ok_or(SntpError::WrongLength)?;
        return Err(SntpError::KissODeath(
            String::from_utf8_lossy(code_bytes).into_owned(),
        ));
    }
    if stratum > 15 {
        return Err(SntpError::StratumTooHigh(stratum));
    }

    let originate = read_u64_be(buf, 24)?;
    let sent_ntp = unix_nanos_to_ntp64(system_time_to_unix_nanos(sent));
    if originate != sent_ntp {
        return Err(SntpError::OriginateMismatch);
    }

    let receive_ts = read_u64_be(buf, 32)?;
    let transmit_ts = read_u64_be(buf, 40)?;
    if transmit_ts == 0 {
        return Err(SntpError::ZeroTransmitTimestamp);
    }

    let t1 = system_time_to_unix_nanos(sent);
    let t4 = system_time_to_unix_nanos(now);
    let t2 = ntp64_to_unix_nanos(receive_ts);
    let t3 = ntp64_to_unix_nanos(transmit_ts);

    let round_trip_ns = (t4 - t1) - (t3 - t2);
    if round_trip_ns < 0 {
        return Err(SntpError::NegativeRoundTrip);
    }
    if round_trip_ns > MAX_ROUND_TRIP_NS {
        return Err(SntpError::RoundTripTooLong);
    }

    let server_now_ns = t3 + round_trip_ns / 2;
    let unix_seconds = floor_div(server_now_ns, 1_000_000_000) as i64;
    let offset_numerator = (t2 - t1) + (t3 - t4);
    let offset_ms = round_half_away(offset_numerator, 2_000_000) as i64;
    let round_trip_ms = u32::try_from(round_half_away(round_trip_ns, 1_000_000))
        .map_err(|_| SntpError::RoundTripTooLong)?;

    Ok(SntpFix {
        unix_seconds,
        offset_ms,
        round_trip_ms,
    })
}

/// Computes the median offset from at least three fixes after rejecting outliers.
///
/// The median of all input offsets is found first. Fixes whose offsets are
/// more than 1000 ms away from that median are dropped. If fewer than two
/// fixes remain, `None` is returned; otherwise the median of the remaining
/// offsets is returned.
#[must_use]
pub fn median_offset(fixes: &[SntpFix]) -> Option<i64> {
    if fixes.len() < 3 {
        return None;
    }

    let mut offsets: Vec<i64> = fixes.iter().map(|fix| fix.offset_ms).collect();
    offsets.sort_unstable();

    let median2 = two_times_median_sorted(&offsets)?;
    let kept: Vec<i64> = offsets
        .into_iter()
        .filter(|offset| (2_i128 * i128::from(*offset) - median2).abs() <= 2_000)
        .collect();

    if kept.len() < 2 {
        return None;
    }

    median_sorted(&kept)
}

fn system_time_to_unix_nanos(time: SystemTime) -> i128 {
    match time.duration_since(UNIX_EPOCH) {
        Ok(duration) => {
            i128::from(duration.as_secs()) * 1_000_000_000 + i128::from(duration.subsec_nanos())
        }
        Err(error) => {
            let duration = error.duration();
            -(i128::from(duration.as_secs()) * 1_000_000_000) - i128::from(duration.subsec_nanos())
        }
    }
}

fn unix_nanos_to_ntp64(nanos: i128) -> u64 {
    let seconds = nanos.div_euclid(1_000_000_000);
    let subsec = nanos.rem_euclid(1_000_000_000);
    let ntp_seconds = (seconds + NTP_UNIX_EPOCH_DELTA).rem_euclid(MODULUS_2_32) as u64;
    let fraction = ((subsec as u128 * MODULUS_2_32 as u128) + 500_000_000) / 1_000_000_000;
    (ntp_seconds << 32) | (fraction as u64)
}

fn ntp64_to_unix_nanos(timestamp: u64) -> i128 {
    let raw_seconds = (timestamp >> 32) as u32;
    let fraction = i128::from(timestamp & 0xffff_ffff);
    let seconds = ntp_to_unix_secs(raw_seconds);
    let nanos = (fraction * 1_000_000_000 + (1_i128 << 31)) >> 32;
    seconds * 1_000_000_000 + nanos
}

fn ntp_to_unix_secs(raw_seconds: u32) -> i128 {
    let raw = i128::from(raw_seconds);
    if raw < HIGH_BIT {
        raw + MODULUS_2_32 - NTP_UNIX_EPOCH_DELTA
    } else {
        raw - NTP_UNIX_EPOCH_DELTA
    }
}

fn read_u64_be(buf: &[u8], start: usize) -> Result<u64, SntpError> {
    let end = start.checked_add(8).ok_or(SntpError::WrongLength)?;
    let bytes = buf.get(start..end).ok_or(SntpError::WrongLength)?;
    let mut array = [0_u8; 8];
    array.copy_from_slice(bytes);
    Ok(u64::from_be_bytes(array))
}

fn floor_div(numerator: i128, denominator: i128) -> i128 {
    let quotient = numerator / denominator;
    let remainder = numerator % denominator;
    if remainder != 0 && ((remainder < 0) != (denominator < 0)) {
        quotient - 1
    } else {
        quotient
    }
}

fn round_half_away(numerator: i128, denominator: i128) -> i128 {
    if numerator >= 0 {
        (numerator + denominator / 2) / denominator
    } else {
        -((-numerator + denominator / 2) / denominator)
    }
}

fn two_times_median_sorted(values: &[i64]) -> Option<i128> {
    let len = values.len();
    if len == 0 {
        return None;
    }
    let mid = len / 2;
    if len.is_multiple_of(2) {
        let lower = i128::from(*values.get(mid.checked_sub(1)?)?);
        let upper = i128::from(*values.get(mid)?);
        Some(lower + upper)
    } else {
        Some(2 * i128::from(*values.get(mid)?))
    }
}

fn median_sorted(values: &[i64]) -> Option<i64> {
    let len = values.len();
    if len == 0 {
        return None;
    }
    let mid = len / 2;
    if len.is_multiple_of(2) {
        let lower = i128::from(*values.get(mid.checked_sub(1)?)?);
        let upper = i128::from(*values.get(mid)?);
        Some(round_half_away(lower + upper, 2) as i64)
    } else {
        values.get(mid).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    const NANOS_PER_MS: i128 = 1_000_000;

    fn fix(offset_ms: i64) -> SntpFix {
        SntpFix {
            unix_seconds: 0,
            offset_ms,
            round_trip_ms: 0,
        }
    }

    fn nanos(seconds: i64, subsec_nanos: u32) -> i128 {
        i128::from(seconds) * 1_000_000_000 + i128::from(subsec_nanos)
    }

    fn sent_for_case() -> SystemTime {
        UNIX_EPOCH + Duration::new(1_700_000_000, 250_000_000)
    }

    fn ntp_bytes(timestamp: u64) -> [u8; 8] {
        timestamp.to_be_bytes()
    }

    fn make_reply(
        sent: SystemTime,
        server_offset_ns: i128,
        receive_delay_ns: i128,
        processing_ns: i128,
    ) -> Vec<u8> {
        let sent_nanos = system_time_to_unix_nanos(sent);
        let t2 = sent_nanos + receive_delay_ns + server_offset_ns;
        let t3 = t2 + processing_ns;

        let mut packet = vec![0_u8; 48];
        packet[0] = 0x24; // LI = 0, VN = 4, Mode = 4
        packet[1] = 1; // stratum 1
        packet[24..32].copy_from_slice(&ntp_bytes(unix_nanos_to_ntp64(sent_nanos)));
        packet[32..40].copy_from_slice(&ntp_bytes(unix_nanos_to_ntp64(t2)));
        packet[40..48].copy_from_slice(&ntp_bytes(unix_nanos_to_ntp64(t3)));
        packet
    }

    fn base_case() -> (SystemTime, SystemTime, Vec<u8>) {
        let sent = sent_for_case();
        let now = sent + Duration::from_millis(100);
        let reply = make_reply(
            sent,
            50 * NANOS_PER_MS,
            20 * NANOS_PER_MS,
            10 * NANOS_PER_MS,
        );
        (sent, now, reply)
    }

    #[test]
    fn request_sets_only_expected_fields() {
        let sent = sent_for_case();
        let packet = request(sent);
        assert_eq!(packet[0], 0x23);
        assert!(packet[1..40].iter().all(|byte| *byte == 0));
        let expected = unix_nanos_to_ntp64(system_time_to_unix_nanos(sent));
        assert_eq!(
            u64::from_be_bytes(packet[40..48].try_into().unwrap()),
            expected
        );
    }

    #[test]
    fn parses_hand_built_valid_reply() {
        let (sent, now, reply) = base_case();
        let parsed = parse_reply(&reply, sent, now).unwrap();
        assert_eq!(parsed.unix_seconds, 1_700_000_000);
        assert_eq!(parsed.offset_ms, 25);
        assert_eq!(parsed.round_trip_ms, 90);
    }

    #[test]
    fn request_and_parse_wrap_into_ntp_era_one_after_2036() {
        let sent = UNIX_EPOCH + Duration::new(2_116_000_000, 123_000_000); // 2037
        let raw = (system_time_to_unix_nanos(sent).div_euclid(1_000_000_000)
            + NTP_UNIX_EPOCH_DELTA)
            .rem_euclid(MODULUS_2_32);
        assert!(raw < HIGH_BIT);

        let now = sent + Duration::from_millis(50);
        let reply = make_reply(
            sent,
            150 * NANOS_PER_MS,
            10 * NANOS_PER_MS,
            5 * NANOS_PER_MS,
        );
        let parsed = parse_reply(&reply, sent, now).unwrap();
        assert_eq!(parsed.unix_seconds, 2_116_000_000);
        assert_eq!(parsed.offset_ms, 138); // 137.5 rounds half away from zero
        assert_eq!(parsed.round_trip_ms, 45);
    }

    #[test]
    fn rejects_short_buffer() {
        let (sent, now, _) = base_case();
        let short = [0_u8; 47];
        assert_eq!(parse_reply(&short, sent, now), Err(SntpError::WrongLength));
    }

    #[test]
    fn rejects_mode_other_than_server() {
        let (sent, now, mut reply) = base_case();
        reply[0] = 0x23; // client mode
        assert_eq!(
            parse_reply(&reply, sent, now),
            Err(SntpError::InvalidMode(3))
        );
    }

    #[test]
    fn rejects_version_below_three() {
        let (sent, now, mut reply) = base_case();
        reply[0] = 0x14; // LI = 0, VN = 2, Mode = 4
        assert_eq!(
            parse_reply(&reply, sent, now),
            Err(SntpError::UnsupportedVersion(2))
        );
    }

    #[test]
    fn rejects_leap_indicator_three() {
        let (sent, now, mut reply) = base_case();
        reply[0] = 0xe4; // LI = 3, VN = 4, Mode = 4
        assert_eq!(
            parse_reply(&reply, sent, now),
            Err(SntpError::LeapIndicatorUnsynchronized)
        );
    }

    #[test]
    fn rejects_kiss_o_death_and_returns_code() {
        let (sent, now, mut reply) = base_case();
        reply[1] = 0;
        reply[12..16].copy_from_slice(b"RATE");
        assert_eq!(
            parse_reply(&reply, sent, now),
            Err(SntpError::KissODeath("RATE".to_string()))
        );

        reply[12..16].copy_from_slice(b"DENY");
        assert_eq!(
            parse_reply(&reply, sent, now),
            Err(SntpError::KissODeath("DENY".to_string()))
        );
    }

    #[test]
    fn rejects_stratum_above_fifteen() {
        let (sent, now, mut reply) = base_case();
        reply[1] = 16;
        assert_eq!(
            parse_reply(&reply, sent, now),
            Err(SntpError::StratumTooHigh(16))
        );
    }

    #[test]
    fn accepts_stratum_fifteen() {
        let (sent, now, mut reply) = base_case();
        reply[1] = 15;
        assert!(parse_reply(&reply, sent, now).is_ok());
    }

    #[test]
    fn rejects_originate_timestamp_mismatch_in_low_bit() {
        let (sent, now, mut reply) = base_case();
        reply[31] ^= 0x01; // same high 32 bits, different low 32 bits
        assert_eq!(
            parse_reply(&reply, sent, now),
            Err(SntpError::OriginateMismatch)
        );
    }

    #[test]
    fn rejects_zero_transmit_timestamp() {
        let (sent, now, mut reply) = base_case();
        reply[40..48].fill(0);
        assert_eq!(
            parse_reply(&reply, sent, now),
            Err(SntpError::ZeroTransmitTimestamp)
        );
    }

    #[test]
    fn rejects_round_trip_over_5000_ms_but_accepts_exactly_5000_ms() {
        let sent = sent_for_case();
        let reply = make_reply(sent, 0, 0, 0);

        let too_late = sent + Duration::from_millis(5_001);
        assert_eq!(
            parse_reply(&reply, sent, too_late),
            Err(SntpError::RoundTripTooLong)
        );

        let exactly = sent + Duration::from_millis(5_000);
        let parsed = parse_reply(&reply, sent, exactly).unwrap();
        assert_eq!(parsed.round_trip_ms, 5_000);
    }

    #[test]
    fn rejects_negative_round_trip() {
        let sent = sent_for_case();
        let reply = make_reply(sent, 0, 100 * NANOS_PER_MS, 100 * NANOS_PER_MS);
        assert_eq!(
            parse_reply(&reply, sent, sent),
            Err(SntpError::NegativeRoundTrip)
        );
    }

    #[test]
    fn transmit_before_receive_still_follows_the_rfc_formula() {
        let (sent, now, mut reply) = base_case();
        let sent_nanos = system_time_to_unix_nanos(sent);
        let receive = unix_nanos_to_ntp64(sent_nanos + 500 * NANOS_PER_MS);
        let transmit = unix_nanos_to_ntp64(sent_nanos + 100 * NANOS_PER_MS);
        reply[32..40].copy_from_slice(&ntp_bytes(receive));
        reply[40..48].copy_from_slice(&ntp_bytes(transmit));
        let fix = parse_reply(&reply, sent, now).unwrap();
        assert_eq!(fix.round_trip_ms, 500);
        assert_eq!(fix.offset_ms, 250);
    }

    #[test]
    fn rounding_is_half_away_from_zero() {
        assert_eq!(round_half_away(1_500, 1_000), 2);
        assert_eq!(round_half_away(-1_500, 1_000), -2);
        assert_eq!(round_half_away(1_499, 1_000), 1);
        assert_eq!(round_half_away(-1_499, 1_000), -1);
        assert_eq!(round_half_away(2_000_000_000, 4_000_000_000), 1);
        assert_eq!(round_half_away(-2_000_000_000, 4_000_000_000), -1);
    }

    #[test]
    fn ntp_fraction_rounding_and_unix_second_flooring() {
        let exact_half_ms = nanos(1_700_000_000, 500_000);
        let timestamp = unix_nanos_to_ntp64(exact_half_ms);
        assert_eq!(ntp64_to_unix_nanos(timestamp), exact_half_ms);
        assert_eq!(floor_div(-1, 1_000_000_000), -1);
        assert_eq!(floor_div(1, 1_000_000_000), 0);
        assert_eq!(floor_div(-1_500_000_000, 1_000_000_000), -2);
    }

    #[test]
    fn median_offset_requires_at_least_three_fixes() {
        assert_eq!(median_offset(&[]), None);
        assert_eq!(median_offset(&[fix(1), fix(2)]), None);
    }

    #[test]
    fn median_offset_drops_distant_outlier() {
        let fixes = [fix(10), fix(20), fix(30), fix(40), fix(5_000)];
        assert_eq!(median_offset(&fixes), Some(25));

        let low_outlier = [fix(-5_000), fix(10), fix(20), fix(30), fix(40)];
        assert_eq!(median_offset(&low_outlier), Some(25));
    }

    #[test]
    fn median_offset_returns_none_when_fewer_than_two_survive() {
        let fixes = [fix(0), fix(5_000), fix(10_000)];
        assert_eq!(median_offset(&fixes), None);
    }

    #[test]
    fn median_offset_handles_even_survivor_count_with_half_up_rounding() {
        let fixes = [fix(0), fix(10), fix(20), fix(1_000)];
        assert_eq!(median_offset(&fixes), Some(15));
    }

    #[test]
    fn parsed_error_display_is_descriptive() {
        let error = SntpError::KissODeath("RATE".to_string());
        assert_eq!(error.to_string(), "SNTP kiss-o'-death code RATE");
        let error = SntpError::NegativeRoundTrip;
        assert_eq!(error.to_string(), "SNTP round trip is negative");
    }
}
