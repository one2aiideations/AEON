//! M026 — tenant-isolated time-series metrics engine.
//!
//! Uses block compression with timestamp delta-of-delta varints and XOR
//! encoding of IEEE-754 value bits. This is a compact Gorilla-inspired
//! encoding, not a byte-for-byte implementation of the original Gorilla
//! wire format. The engine is in-memory and exposes a small line protocol.
//!
//! Tenant identity is part of every series key. A production deployment must
//! authenticate tenant identity before calling this library.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;

const DEFAULT_BLOCK_SAMPLES: usize = 256;
const MAX_BLOCK_SAMPLES: usize = 4096;
const DEFAULT_MAX_SERIES: usize = 100_000;
const DEFAULT_MAX_SAMPLES_PER_SERIES: usize = 1_000_000;
const DEFAULT_MAX_METRIC_BYTES: usize = 128;
const MAX_LABELS: usize = 32;
const MAX_LABEL_BYTES: usize = 128;
const MAX_QUERY_RESULTS: usize = 100_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TsError {
    InvalidTenant,
    InvalidMetric,
    InvalidLabels,
    InvalidTimestamp,
    InvalidValue,
    InvalidConfig(String),
    CapacityExceeded,
    CorruptBlock(String),
    NotFound,
}

impl fmt::Display for TsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidTenant => write!(f, "tenant ID must be non-zero"),
            Self::InvalidMetric => write!(f, "invalid metric name"),
            Self::InvalidLabels => write!(f, "invalid or excessive labels"),
            Self::InvalidTimestamp => write!(f, "timestamps must be non-decreasing"),
            Self::InvalidValue => write!(f, "metric value must be finite"),
            Self::InvalidConfig(message) => write!(f, "invalid configuration: {message}"),
            Self::CapacityExceeded => write!(f, "configured time-series capacity exceeded"),
            Self::CorruptBlock(message) => write!(f, "corrupt compressed block: {message}"),
            Self::NotFound => write!(f, "time series not found"),
        }
    }
}
impl Error for TsError {}
type TsResult<T> = Result<T, TsError>;

#[derive(Clone, Debug)]
pub struct TsConfig {
    pub block_samples: usize,
    pub max_series: usize,
    pub max_samples_per_series: usize,
    pub max_metric_bytes: usize,
}

impl Default for TsConfig {
    fn default() -> Self {
        Self {
            block_samples: DEFAULT_BLOCK_SAMPLES,
            max_series: DEFAULT_MAX_SERIES,
            max_samples_per_series: DEFAULT_MAX_SAMPLES_PER_SERIES,
            max_metric_bytes: DEFAULT_MAX_METRIC_BYTES,
        }
    }
}

impl TsConfig {
    fn validate(&self) -> TsResult<()> {
        if self.block_samples == 0 || self.block_samples > MAX_BLOCK_SAMPLES {
            return Err(TsError::InvalidConfig(format!(
                "block_samples must be 1..={MAX_BLOCK_SAMPLES}"
            )));
        }
        if self.max_series == 0 || self.max_series > 1_000_000 {
            return Err(TsError::InvalidConfig(
                "max_series must be 1..=1000000".into(),
            ));
        }
        if self.max_samples_per_series == 0 {
            return Err(TsError::InvalidConfig(
                "max_samples_per_series must be non-zero".into(),
            ));
        }
        if self.max_metric_bytes == 0 || self.max_metric_bytes > 1024 {
            return Err(TsError::InvalidConfig(
                "max_metric_bytes must be 1..=1024".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Sample {
    pub timestamp_ms: u64,
    pub value: f64,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct SeriesKey {
    tenant_id: u64,
    metric: String,
    labels: Vec<(String, String)>,
}

#[derive(Clone, Debug)]
struct CompressedBlock {
    first_timestamp_ms: u64,
    last_timestamp_ms: u64,
    sample_count: usize,
    bytes: Vec<u8>,
}

#[derive(Clone, Debug, Default)]
struct Series {
    blocks: Vec<CompressedBlock>,
    pending: Vec<Sample>,
    sample_count: usize,
    last_timestamp_ms: Option<u64>,
}

pub struct TimeSeriesDb {
    config: TsConfig,
    series: BTreeMap<SeriesKey, Series>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct QueryResult {
    pub metric: String,
    pub labels: Vec<(String, String)>,
    pub samples: Vec<Sample>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DbStats {
    pub series_count: usize,
    pub sample_count: usize,
    pub compressed_bytes: usize,
}

fn validate_key(
    tenant_id: u64,
    metric: &str,
    labels: &[(String, String)],
    config: &TsConfig,
) -> TsResult<SeriesKey> {
    if tenant_id == 0 {
        return Err(TsError::InvalidTenant);
    }
    if metric.is_empty()
        || metric.len() > config.max_metric_bytes
        || !metric
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b':')
    {
        return Err(TsError::InvalidMetric);
    }
    if labels.len() > MAX_LABELS {
        return Err(TsError::InvalidLabels);
    }
    let mut sorted = labels.to_vec();
    sorted.sort();
    let mut previous: Option<&str> = None;
    for (name, value) in &sorted {
        if name.is_empty()
            || name.len() > MAX_LABEL_BYTES
            || value.len() > MAX_LABEL_BYTES
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
            || name.contains('\0')
            || value.contains('\0')
        {
            return Err(TsError::InvalidLabels);
        }
        if previous == Some(name.as_str()) {
            return Err(TsError::InvalidLabels);
        }
        previous = Some(name);
    }
    Ok(SeriesKey {
        tenant_id,
        metric: metric.to_owned(),
        labels: sorted,
    })
}

fn put_varint(mut value: u64, output: &mut Vec<u8>) {
    while value >= 0x80 {
        output.push((value as u8) | 0x80);
        value >>= 7;
    }
    output.push(value as u8);
}

fn get_varint(input: &[u8], cursor: &mut usize) -> TsResult<u64> {
    let mut value = 0u64;
    let mut shift = 0u32;
    loop {
        let byte = *input
            .get(*cursor)
            .ok_or_else(|| TsError::CorruptBlock("truncated varint".into()))?;
        *cursor = cursor.checked_add(1).ok_or(TsError::CapacityExceeded)?;
        let part = u64::from(byte & 0x7f);
        if shift >= 64 || (shift == 63 && part > 1) {
            return Err(TsError::CorruptBlock("varint overflow".into()));
        }
        value |= part
            .checked_shl(shift)
            .ok_or_else(|| TsError::CorruptBlock("varint overflow".into()))?;
        if byte & 0x80 == 0 {
            return Ok(value);
        }
        shift = shift.checked_add(7).ok_or(TsError::CapacityExceeded)?;
    }
}

fn zigzag_encode(value: i64) -> u64 {
    ((value << 1) ^ (value >> 63)) as u64
}

fn zigzag_decode(value: u64) -> i64 {
    ((value >> 1) as i64) ^ (-((value & 1) as i64))
}

fn encode_block(samples: &[Sample]) -> TsResult<CompressedBlock> {
    let first = samples
        .first()
        .ok_or_else(|| TsError::InvalidConfig("cannot encode empty block".into()))?;
    let last = samples
        .last()
        .ok_or_else(|| TsError::InvalidConfig("cannot encode empty block".into()))?;
    let mut bytes = Vec::with_capacity(samples.len().saturating_mul(4).saturating_add(16));
    bytes.extend_from_slice(&first.timestamp_ms.to_le_bytes());
    bytes.extend_from_slice(&first.value.to_bits().to_le_bytes());

    let mut previous_timestamp = first.timestamp_ms;
    let mut previous_delta: i64 = 0;
    let mut previous_value_bits = first.value.to_bits();

    for sample in samples.iter().skip(1) {
        let delta_u64 = sample
            .timestamp_ms
            .checked_sub(previous_timestamp)
            .ok_or(TsError::InvalidTimestamp)?;
        let delta = i64::try_from(delta_u64).map_err(|_| TsError::InvalidTimestamp)?;
        let delta_of_delta = delta
            .checked_sub(previous_delta)
            .ok_or(TsError::InvalidTimestamp)?;
        put_varint(zigzag_encode(delta_of_delta), &mut bytes);

        let value_bits = sample.value.to_bits();
        let xor = value_bits ^ previous_value_bits;
        put_varint(xor, &mut bytes);

        previous_timestamp = sample.timestamp_ms;
        previous_delta = delta;
        previous_value_bits = value_bits;
    }

    Ok(CompressedBlock {
        first_timestamp_ms: first.timestamp_ms,
        last_timestamp_ms: last.timestamp_ms,
        sample_count: samples.len(),
        bytes,
    })
}

fn decode_block(block: &CompressedBlock) -> TsResult<Vec<Sample>> {
    if block.sample_count == 0 || block.sample_count > MAX_BLOCK_SAMPLES || block.bytes.len() < 16 {
        return Err(TsError::CorruptBlock("invalid block header".into()));
    }
    let timestamp_bytes: [u8; 8] = block
        .bytes
        .get(0..8)
        .ok_or_else(|| TsError::CorruptBlock("missing first timestamp".into()))?
        .try_into()
        .map_err(|_| TsError::CorruptBlock("invalid first timestamp".into()))?;
    let value_bytes: [u8; 8] = block
        .bytes
        .get(8..16)
        .ok_or_else(|| TsError::CorruptBlock("missing first value".into()))?
        .try_into()
        .map_err(|_| TsError::CorruptBlock("invalid first value".into()))?;
    let mut timestamp = u64::from_le_bytes(timestamp_bytes);
    let mut value_bits = u64::from_le_bytes(value_bytes);
    let first_value = f64::from_bits(value_bits);
    if !first_value.is_finite() {
        return Err(TsError::CorruptBlock("non-finite first value".into()));
    }

    let mut output = Vec::with_capacity(block.sample_count);
    output.push(Sample {
        timestamp_ms: timestamp,
        value: first_value,
    });
    let mut previous_delta: i64 = 0;
    let mut cursor = 16usize;

    for _ in 1..block.sample_count {
        let encoded_delta = get_varint(&block.bytes, &mut cursor)?;
        let delta_of_delta = zigzag_decode(encoded_delta);
        let delta = previous_delta
            .checked_add(delta_of_delta)
            .ok_or_else(|| TsError::CorruptBlock("timestamp delta overflow".into()))?;
        if delta < 0 {
            return Err(TsError::CorruptBlock("negative timestamp delta".into()));
        }
        timestamp = timestamp
            .checked_add(delta as u64)
            .ok_or_else(|| TsError::CorruptBlock("timestamp overflow".into()))?;

        let xor = get_varint(&block.bytes, &mut cursor)?;
        value_bits ^= xor;
        let value = f64::from_bits(value_bits);
        if !value.is_finite() {
            return Err(TsError::CorruptBlock("non-finite value".into()));
        }
        output.push(Sample {
            timestamp_ms: timestamp,
            value,
        });
        previous_delta = delta;
    }
    if cursor != block.bytes.len() {
        return Err(TsError::CorruptBlock(
            "trailing bytes in compressed block".into(),
        ));
    }
    if output.first().map(|sample| sample.timestamp_ms) != Some(block.first_timestamp_ms)
        || output.last().map(|sample| sample.timestamp_ms) != Some(block.last_timestamp_ms)
    {
        return Err(TsError::CorruptBlock(
            "block timestamp bounds mismatch".into(),
        ));
    }
    Ok(output)
}

impl TimeSeriesDb {
    pub fn new(config: TsConfig) -> TsResult<Self> {
        config.validate()?;
        Ok(Self {
            config,
            series: BTreeMap::new(),
        })
    }

    pub fn append(
        &mut self,
        tenant_id: u64,
        metric: &str,
        labels: &[(String, String)],
        sample: Sample,
    ) -> TsResult<()> {
        let key = validate_key(tenant_id, metric, labels, &self.config)?;
        if !sample.value.is_finite() {
            return Err(TsError::InvalidValue);
        }

        if !self.series.contains_key(&key) && self.series.len() >= self.config.max_series {
            return Err(TsError::CapacityExceeded);
        }
        let series = self.series.entry(key).or_default();
        if series.sample_count >= self.config.max_samples_per_series {
            return Err(TsError::CapacityExceeded);
        }
        if series
            .last_timestamp_ms
            .map(|last| sample.timestamp_ms < last)
            .unwrap_or(false)
        {
            return Err(TsError::InvalidTimestamp);
        }
        series.last_timestamp_ms = Some(sample.timestamp_ms);
        series.sample_count = series
            .sample_count
            .checked_add(1)
            .ok_or(TsError::CapacityExceeded)?;
        series.pending.push(sample);

        if series.pending.len() >= self.config.block_samples {
            let block = encode_block(&series.pending)?;
            series.blocks.push(block);
            series.pending.clear();
        }
        Ok(())
    }

    pub fn flush(&mut self) -> TsResult<()> {
        for series in self.series.values_mut() {
            if !series.pending.is_empty() {
                let block = encode_block(&series.pending)?;
                series.blocks.push(block);
                series.pending.clear();
            }
        }
        Ok(())
    }

    pub fn query(
        &self,
        tenant_id: u64,
        metric: &str,
        labels: &[(String, String)],
        start_ms: u64,
        end_ms: u64,
        limit: usize,
    ) -> TsResult<QueryResult> {
        if start_ms > end_ms || limit == 0 || limit > MAX_QUERY_RESULTS {
            return Err(TsError::InvalidConfig(
                "invalid query range or result limit".into(),
            ));
        }
        let key = validate_key(tenant_id, metric, labels, &self.config)?;
        let series = self.series.get(&key).ok_or(TsError::NotFound)?;
        let mut samples = Vec::new();

        for block in &series.blocks {
            if block.last_timestamp_ms < start_ms || block.first_timestamp_ms > end_ms {
                continue;
            }
            for sample in decode_block(block)? {
                if sample.timestamp_ms >= start_ms && sample.timestamp_ms <= end_ms {
                    if samples.len() >= limit {
                        return Err(TsError::CapacityExceeded);
                    }
                    samples.push(sample);
                }
            }
        }
        for sample in &series.pending {
            if sample.timestamp_ms >= start_ms && sample.timestamp_ms <= end_ms {
                if samples.len() >= limit {
                    return Err(TsError::CapacityExceeded);
                }
                samples.push(sample.clone());
            }
        }
        Ok(QueryResult {
            metric: key.metric,
            labels: key.labels,
            samples,
        })
    }

    pub fn stats(&self) -> DbStats {
        let mut sample_count = 0usize;
        let mut compressed_bytes = 0usize;
        for series in self.series.values() {
            sample_count = sample_count.saturating_add(series.sample_count);
            for block in &series.blocks {
                compressed_bytes = compressed_bytes.saturating_add(block.bytes.len());
            }
            compressed_bytes = compressed_bytes.saturating_add(
                series
                    .pending
                    .len()
                    .saturating_mul(std::mem::size_of::<Sample>()),
            );
        }
        DbStats {
            series_count: self.series.len(),
            sample_count,
            compressed_bytes,
        }
    }

    pub fn delete_series(
        &mut self,
        tenant_id: u64,
        metric: &str,
        labels: &[(String, String)],
    ) -> TsResult<()> {
        let key = validate_key(tenant_id, metric, labels, &self.config)?;
        self.series
            .remove(&key)
            .map(|_| ())
            .ok_or(TsError::NotFound)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> TsConfig {
        TsConfig {
            block_samples: 3,
            max_series: 8,
            max_samples_per_series: 32,
            ..TsConfig::default()
        }
    }

    fn labels() -> Vec<(String, String)> {
        vec![
            ("host".into(), "node-a".into()),
            ("region".into(), "ap-south".into()),
        ]
    }

    #[test]
    fn block_codec_round_trips_delta_of_delta_and_xor_values() {
        let samples = vec![
            Sample {
                timestamp_ms: 1000,
                value: 10.25,
            },
            Sample {
                timestamp_ms: 1010,
                value: 10.5,
            },
            Sample {
                timestamp_ms: 1025,
                value: 10.5,
            },
            Sample {
                timestamp_ms: 1045,
                value: -2.75,
            },
        ];
        let block = encode_block(&samples).expect("encode");
        assert_eq!(decode_block(&block).expect("decode"), samples);
    }

    #[test]
    fn tenant_series_are_isolated() {
        let mut db = TimeSeriesDb::new(config()).expect("db");
        let labels = labels();
        db.append(
            1,
            "cpu_usage",
            &labels,
            Sample {
                timestamp_ms: 1,
                value: 0.5,
            },
        )
        .expect("append");
        db.append(
            2,
            "cpu_usage",
            &labels,
            Sample {
                timestamp_ms: 1,
                value: 0.9,
            },
        )
        .expect("append");
        let first = db.query(1, "cpu_usage", &labels, 0, 10, 10).expect("query");
        let second = db.query(2, "cpu_usage", &labels, 0, 10, 10).expect("query");
        assert_eq!(first.samples.first().map(|sample| sample.value), Some(0.5));
        assert_eq!(second.samples.first().map(|sample| sample.value), Some(0.9));
    }

    #[test]
    fn flush_and_range_query_return_ordered_samples() {
        let mut db = TimeSeriesDb::new(config()).expect("db");
        for timestamp in 0..8u64 {
            db.append(
                7,
                "requests_total",
                &[],
                Sample {
                    timestamp_ms: timestamp * 1000,
                    value: timestamp as f64,
                },
            )
            .expect("append");
        }
        db.flush().expect("flush");
        let result = db
            .query(7, "requests_total", &[], 2000, 5000, 10)
            .expect("query");
        assert_eq!(result.samples.len(), 4);
        assert_eq!(
            result.samples.first().map(|sample| sample.timestamp_ms),
            Some(2000)
        );
        assert_eq!(
            result.samples.last().map(|sample| sample.timestamp_ms),
            Some(5000)
        );
    }

    #[test]
    fn rejects_out_of_order_and_non_finite_samples() {
        let mut db = TimeSeriesDb::new(config()).expect("db");
        assert!(db
            .append(
                1,
                "metric",
                &[],
                Sample {
                    timestamp_ms: 10,
                    value: 1.0
                }
            )
            .is_ok());
        assert_eq!(
            db.append(
                1,
                "metric",
                &[],
                Sample {
                    timestamp_ms: 9,
                    value: 2.0
                }
            ),
            Err(TsError::InvalidTimestamp)
        );
        assert_eq!(
            db.append(
                1,
                "metric",
                &[],
                Sample {
                    timestamp_ms: 11,
                    value: f64::NAN
                }
            ),
            Err(TsError::InvalidValue)
        );
    }

    #[test]
    fn validates_labels_and_capacity() {
        let mut db = TimeSeriesDb::new(config()).expect("db");
        let duplicate = vec![("host".into(), "a".into()), ("host".into(), "b".into())];
        assert_eq!(
            db.append(
                1,
                "metric",
                &duplicate,
                Sample {
                    timestamp_ms: 1,
                    value: 1.0
                }
            ),
            Err(TsError::InvalidLabels)
        );
        for index in 0..8 {
            db.append(
                index + 1,
                "metric",
                &[],
                Sample {
                    timestamp_ms: 1,
                    value: 1.0,
                },
            )
            .expect("append");
        }
        assert_eq!(
            db.append(
                9,
                "metric",
                &[],
                Sample {
                    timestamp_ms: 1,
                    value: 1.0
                }
            ),
            Err(TsError::CapacityExceeded)
        );
    }
}
