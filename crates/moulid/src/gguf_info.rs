//! `gguf_info` — a minimal, dependency-free reader for the header of a
//! GGUF model file (format versions 2 and 3, little endian).
//!
//! No file I/O is performed: the caller supplies the leading bytes of the
//! file and receives a [`GgufInfo`] summary. If parsing fails with
//! [`GgufError::NeedMoreBytes`], the caller should read at least the
//! indicated total number of bytes and call [`parse`] again. Strings and
//! arrays that are not of interest are skipped by length arithmetic without
//! allocating; array nesting is limited to `MAX_ARRAY_DEPTH` (4).

use std::error::Error;
use std::fmt;

/// Maximum accepted length (in bytes) for any string in the file, including keys.
const MAX_STRING_LEN: u64 = 1024 * 1024;
/// Maximum number of metadata key/value entries accepted per header.
const MAX_METADATA_ENTRIES: u64 = 10_000;
/// Maximum number of tensors accepted per header.
const MAX_TENSORS: u64 = 1_000_000;
/// Maximum nesting depth for metadata arrays (top-level value is depth 0).
const MAX_ARRAY_DEPTH: u32 = 4;
/// Maximum number of elements accepted in a single metadata array.
const MAX_ARRAY_ELEMENTS: u64 = 16_777_216;
/// Fixed 256 MiB overhead added by [`estimate_ram`].
/// Working memory the runtime needs besides the weights and the attention cache.
pub const OVERHEAD_BYTES: u64 = 256 * 1024 * 1024;

/// A summary of the parts of a GGUF header that callers usually care about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GgufInfo {
    /// GGUF format version (2 or 3).
    pub version: u32,
    /// Number of tensors declared by the file.
    pub tensor_count: u64,
    /// Value of `general.architecture`, if present and a string.
    pub architecture: Option<String>,
    /// Value of `general.name`, if present and a string.
    pub name: Option<String>,
    /// Value of `general.file_type`, if present and an integer.
    pub file_type: Option<u32>,
    /// Value of `<arch>.context_length`, if present and an integer.
    pub context_length: Option<u64>,
    /// Value of `<arch>.embedding_length`, if present and an integer.
    pub embedding_length: Option<u64>,
    /// Value of `<arch>.block_count`, if present and an integer.
    pub block_count: Option<u64>,
    /// Number of bytes of the input buffer consumed by the header.
    pub header_bytes_read: usize,
}

/// Errors returned while parsing a GGUF header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GgufError {
    /// The magic bytes `GGUF` were not present at the start of the buffer.
    BadMagic,
    /// The version field was not 2 or 3.
    UnsupportedVersion(u32),
    /// The buffer ended before the header was complete. Carries the minimum
    /// total number of bytes known to be required; read at least that many
    /// bytes and retry.
    NeedMoreBytes(usize),
    /// The header declares more than 1,000,000 tensors.
    TooManyTensors,
    /// The header declares more than 10,000 metadata entries.
    TooManyEntries,
    /// A string longer than 1 MiB was encountered.
    StringTooLong,
    /// An unknown metadata value type was encountered.
    BadValueType(u32),
    /// An array was too large, too deeply nested, or otherwise unreasonable.
    ArrayTooLarge,
    /// A checked arithmetic operation overflowed.
    Overflow,
    /// A string in the header was not valid UTF-8.
    InvalidUtf8,
}

impl fmt::Display for GgufError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GgufError::BadMagic => write!(f, "bad magic: expected \"GGUF\""),
            GgufError::UnsupportedVersion(v) => {
                write!(f, "unsupported GGUF version {v} (expected 2 or 3)")
            }
            GgufError::NeedMoreBytes(n) => {
                write!(
                    f,
                    "need more bytes: read at least {n} total bytes and retry"
                )
            }
            GgufError::TooManyTensors => write!(f, "too many tensors (max 1,000,000)"),
            GgufError::TooManyEntries => write!(f, "too many metadata entries (max 10,000)"),
            GgufError::StringTooLong => write!(f, "string exceeds the 1 MiB limit"),
            GgufError::BadValueType(t) => write!(f, "bad metadata value type {t}"),
            GgufError::ArrayTooLarge => {
                write!(
                    f,
                    "array element count, size, or nesting depth exceeds limits"
                )
            }
            GgufError::Overflow => write!(f, "arithmetic overflow while parsing header"),
            GgufError::InvalidUtf8 => write!(f, "string is not valid UTF-8"),
        }
    }
}

impl Error for GgufError {}

/// Parse the header of a GGUF file from the leading bytes of the file.
///
/// `buf` must contain at least the beginning of the file. On success returns
/// a [`GgufInfo`] with [`GgufInfo::header_bytes_read`] set to the number of
/// bytes consumed.
///
/// # Errors
/// A [`GgufError`]; [`GgufError::NeedMoreBytes`] says how many total bytes to
/// read before retrying.
pub fn parse(buf: &[u8]) -> Result<GgufInfo, GgufError> {
    let mut p = Parser::new(buf);

    let magic = p.take(4)?;
    if magic != b"GGUF" {
        return Err(GgufError::BadMagic);
    }

    let version = p.read_u32()?;
    if version != 2 && version != 3 {
        return Err(GgufError::UnsupportedVersion(version));
    }

    let tensor_count = p.read_u64()?;
    if tensor_count > MAX_TENSORS {
        return Err(GgufError::TooManyTensors);
    }

    let kv_count = p.read_u64()?;
    if kv_count > MAX_METADATA_ENTRIES {
        return Err(GgufError::TooManyEntries);
    }

    let mut architecture: Option<String> = None;
    let mut name: Option<String> = None;
    let mut file_type: Option<u32> = None;
    let mut ctx_candidates: Vec<(String, u64)> = Vec::new();
    let mut emb_candidates: Vec<(String, u64)> = Vec::new();
    let mut blk_candidates: Vec<(String, u64)> = Vec::new();

    for _ in 0..kv_count {
        let key = p.read_string()?;
        let value_type = p.read_u32()?;
        match key.as_str() {
            "general.architecture" => {
                if value_type == 8 {
                    architecture = Some(p.read_string()?);
                } else {
                    p.skip_value(value_type, 0)?;
                }
            }
            "general.name" => {
                if value_type == 8 {
                    name = Some(p.read_string()?);
                } else {
                    p.skip_value(value_type, 0)?;
                }
            }
            "general.file_type" => {
                file_type = p
                    .read_int_as_u64(value_type)?
                    .and_then(|v| u32::try_from(v).ok());
            }
            _ => {
                if let Some(prefix) = key.strip_suffix(".context_length") {
                    collect_candidate(&mut ctx_candidates, prefix, value_type, &mut p)?;
                } else if let Some(prefix) = key.strip_suffix(".embedding_length") {
                    collect_candidate(&mut emb_candidates, prefix, value_type, &mut p)?;
                } else if let Some(prefix) = key.strip_suffix(".block_count") {
                    collect_candidate(&mut blk_candidates, prefix, value_type, &mut p)?;
                } else {
                    p.skip_value(value_type, 0)?;
                }
            }
        }
    }

    let context_length = resolve_candidate(architecture.as_deref(), &ctx_candidates);
    let embedding_length = resolve_candidate(architecture.as_deref(), &emb_candidates);
    let block_count = resolve_candidate(architecture.as_deref(), &blk_candidates);

    Ok(GgufInfo {
        version,
        tensor_count,
        architecture,
        name,
        file_type,
        context_length,
        embedding_length,
        block_count,
        header_bytes_read: p.pos,
    })
}

/// Estimate the RAM required to run a model: the file size plus a 16-bit
/// KV cache (2 tensors × 2 bytes × `block_count` × `ctx` ×
/// `embedding_length`) plus 256 MiB of runtime overhead.
///
/// Unknown fields contribute 0 to the KV-cache term, and all arithmetic is
/// saturating.
#[must_use]
pub fn estimate_ram(info: &GgufInfo, file_size: u64, ctx: u64) -> u64 {
    file_size
        .saturating_add(cache_bytes(info, ctx))
        .saturating_add(OVERHEAD_BYTES)
}

/// Bytes of attention cache for `ctx` tokens: 2 (K and V) x 2 bytes (f16) x layers x width x
/// tokens. Zero when the header does not say the layers or width.
#[must_use]
pub fn cache_bytes(info: &GgufInfo, ctx: u64) -> u64 {
    info.block_count
        .unwrap_or(0)
        .saturating_mul(info.embedding_length.unwrap_or(0))
        .saturating_mul(ctx)
        .saturating_mul(4)
}

/// Return the llama.cpp file-type name for the common `general.file_type`
/// values, or `"unknown"` for anything not listed.
#[must_use]
pub fn file_type_name(t: u32) -> &'static str {
    match t {
        0 => "F32",
        1 => "F16",
        2 => "Q4_0",
        3 => "Q4_1",
        7 => "Q8_0",
        8 => "Q5_0",
        9 => "Q5_1",
        10 => "Q2_K",
        11 => "Q3_K_S",
        12 => "Q3_K_M",
        13 => "Q3_K_L",
        14 => "Q4_K_S",
        15 => "Q4_K_M",
        16 => "Q5_K_S",
        17 => "Q5_K_M",
        18 => "Q6_K",
        _ => "unknown",
    }
}

/// Byte size of a value of the given scalar GGUF value type, or `None` for
/// strings, arrays, and unknown types.
fn fixed_size(value_type: u32) -> Option<u64> {
    match value_type {
        0 | 1 | 7 => Some(1),
        2 | 3 => Some(2),
        4..=6 => Some(4),
        10..=12 => Some(8),
        _ => None,
    }
}

/// Record an integer value for a `<arch>.<suffix>`-style key, consuming the
/// value bytes regardless of its type.
fn collect_candidate(
    candidates: &mut Vec<(String, u64)>,
    prefix: &str,
    value_type: u32,
    parser: &mut Parser<'_>,
) -> Result<(), GgufError> {
    if let Some(value) = parser.read_int_as_u64(value_type)? {
        candidates.push((prefix.to_owned(), value));
    }
    Ok(())
}

/// Pick the best candidate for a dimension key: the one whose prefix matches
/// the architecture, or the sole candidate if there is no architecture and no
/// ambiguity.
fn resolve_candidate(arch: Option<&str>, candidates: &[(String, u64)]) -> Option<u64> {
    arch.and_then(|a| {
        candidates
            .iter()
            .find(|(prefix, _)| prefix.as_str() == a)
            .map(|c| c.1)
    })
    .or_else(|| {
        if candidates.len() == 1 {
            candidates.first().map(|c| c.1)
        } else {
            None
        }
    })
}

/// Internal cursor over the input buffer.
struct Parser<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    fn remaining(&self) -> u64 {
        u64::try_from(self.buf.len().saturating_sub(self.pos)).unwrap_or(u64::MAX)
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], GgufError> {
        let end = self.pos.checked_add(n).ok_or(GgufError::Overflow)?;
        let bytes = self
            .buf
            .get(self.pos..end)
            .ok_or(GgufError::NeedMoreBytes(end))?;
        self.pos = end;
        Ok(bytes)
    }

    fn take_u64(&mut self, n: u64) -> Result<&'a [u8], GgufError> {
        let n = usize::try_from(n).map_err(|_| GgufError::Overflow)?;
        self.take(n)
    }

    fn read_array<const N: usize>(&mut self) -> Result<[u8; N], GgufError> {
        let bytes = self.take(N)?;
        bytes.try_into().map_err(|_| GgufError::Overflow)
    }

    fn read_u8(&mut self) -> Result<u8, GgufError> {
        Ok(u8::from_le_bytes(self.read_array::<1>()?))
    }

    fn read_i8(&mut self) -> Result<i8, GgufError> {
        Ok(i8::from_le_bytes(self.read_array::<1>()?))
    }

    fn read_u16(&mut self) -> Result<u16, GgufError> {
        Ok(u16::from_le_bytes(self.read_array::<2>()?))
    }

    fn read_i16(&mut self) -> Result<i16, GgufError> {
        Ok(i16::from_le_bytes(self.read_array::<2>()?))
    }

    fn read_u32(&mut self) -> Result<u32, GgufError> {
        Ok(u32::from_le_bytes(self.read_array::<4>()?))
    }

    fn read_i32(&mut self) -> Result<i32, GgufError> {
        Ok(i32::from_le_bytes(self.read_array::<4>()?))
    }

    fn read_u64(&mut self) -> Result<u64, GgufError> {
        Ok(u64::from_le_bytes(self.read_array::<8>()?))
    }

    fn read_i64(&mut self) -> Result<i64, GgufError> {
        Ok(i64::from_le_bytes(self.read_array::<8>()?))
    }

    fn read_string_len(&mut self) -> Result<u64, GgufError> {
        let len = self.read_u64()?;
        if len > MAX_STRING_LEN {
            return Err(GgufError::StringTooLong);
        }
        Ok(len)
    }

    /// Read a string, allocating. Validates UTF-8.
    fn read_string(&mut self) -> Result<String, GgufError> {
        let len = self.read_string_len()?;
        let bytes = self.take_u64(len)?;
        String::from_utf8(bytes.to_vec()).map_err(|_| GgufError::InvalidUtf8)
    }

    /// Skip a string without allocating. UTF-8 is not validated.
    fn skip_string(&mut self) -> Result<(), GgufError> {
        let len = self.read_string_len()?;
        self.take_u64(len)?;
        Ok(())
    }

    /// Read an integer value of any integer GGUF value type as `u64`, skipping
    /// other value types in place. Negative values and out-of-range `u64`
    /// conversions yield `Ok(None)` (the value is still consumed).
    fn read_int_as_u64(&mut self, value_type: u32) -> Result<Option<u64>, GgufError> {
        match value_type {
            0 => Ok(Some(u64::from(self.read_u8()?))),
            1 => Ok(u64::try_from(i64::from(self.read_i8()?)).ok()),
            2 => Ok(Some(u64::from(self.read_u16()?))),
            3 => Ok(u64::try_from(i64::from(self.read_i16()?)).ok()),
            4 => Ok(Some(u64::from(self.read_u32()?))),
            5 => Ok(u64::try_from(i64::from(self.read_i32()?)).ok()),
            10 => Ok(Some(self.read_u64()?)),
            11 => Ok(u64::try_from(self.read_i64()?).ok()),
            _ => {
                self.skip_value(value_type, 0)?;
                Ok(None)
            }
        }
    }

    /// Skip a value of the given type without allocating.
    fn skip_value(&mut self, value_type: u32, depth: u32) -> Result<(), GgufError> {
        match value_type {
            0 | 1 | 7 => {
                self.take(1)?;
            }
            2 | 3 => {
                self.take(2)?;
            }
            4..=6 => {
                self.take(4)?;
            }
            8 => self.skip_string()?,
            9 => self.skip_array(depth)?,
            10..=12 => {
                self.take(8)?;
            }
            other => return Err(GgufError::BadValueType(other)),
        }
        Ok(())
    }

    /// Skip an array value without allocating. Fixed-size elements are skipped
    /// in bulk; strings and nested arrays are skipped element-wise.
    fn skip_array(&mut self, depth: u32) -> Result<(), GgufError> {
        if depth >= MAX_ARRAY_DEPTH {
            return Err(GgufError::ArrayTooLarge);
        }
        let elem_type = self.read_u32()?;
        let count = self.read_u64()?;
        if count > MAX_ARRAY_ELEMENTS {
            return Err(GgufError::ArrayTooLarge);
        }
        let remaining = self.remaining();
        if let Some(size) = fixed_size(elem_type) {
            let need = count.checked_mul(size).ok_or(GgufError::Overflow)?;
            if need > remaining {
                let extra = usize::try_from(need).map_err(|_| GgufError::Overflow)?;
                let total = self.pos.checked_add(extra).ok_or(GgufError::Overflow)?;
                return Err(GgufError::NeedMoreBytes(total));
            }
            self.take_u64(need)?;
            Ok(())
        } else {
            if elem_type != 8 && elem_type != 9 {
                return Err(GgufError::BadValueType(elem_type));
            }
            if count > remaining {
                let extra = usize::try_from(count).map_err(|_| GgufError::Overflow)?;
                let total = self.pos.checked_add(extra).ok_or(GgufError::Overflow)?;
                return Err(GgufError::NeedMoreBytes(total));
            }
            for _ in 0..count {
                self.skip_value(elem_type, depth + 1)?;
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn begin(version: u32, tensor_count: u64, kv_count: u64) -> Vec<u8> {
        let mut buf = b"GGUF".to_vec();
        buf.extend_from_slice(&version.to_le_bytes());
        buf.extend_from_slice(&tensor_count.to_le_bytes());
        buf.extend_from_slice(&kv_count.to_le_bytes());
        buf
    }

    fn push_u32(buf: &mut Vec<u8>, v: u32) {
        buf.extend_from_slice(&v.to_le_bytes());
    }

    fn push_u64(buf: &mut Vec<u8>, v: u64) {
        buf.extend_from_slice(&v.to_le_bytes());
    }

    fn push_string(buf: &mut Vec<u8>, s: &str) {
        buf.extend_from_slice(&(s.len() as u64).to_le_bytes());
        buf.extend_from_slice(s.as_bytes());
    }

    fn entry_u32(buf: &mut Vec<u8>, key: &str, value: u32) {
        push_string(buf, key);
        push_u32(buf, 4);
        push_u32(buf, value);
    }

    fn entry_string(buf: &mut Vec<u8>, key: &str, value: &str) {
        push_string(buf, key);
        push_u32(buf, 8);
        push_string(buf, value);
    }

    fn entry_array_of_strings(buf: &mut Vec<u8>, key: &str, values: &[&str]) {
        push_string(buf, key);
        push_u32(buf, 9);
        push_u32(buf, 8);
        push_u64(buf, values.len() as u64);
        for v in values {
            push_string(buf, v);
        }
    }

    #[test]
    fn parses_v3_header_with_string_array_and_late_architecture() {
        let mut buf = begin(3, 2, 7);
        // Dimension keys deliberately come *before* the architecture key.
        entry_u32(&mut buf, "llama.context_length", 4096);
        entry_u32(&mut buf, "llama.embedding_length", 5120);
        entry_u32(&mut buf, "llama.block_count", 32);
        entry_array_of_strings(&mut buf, "tokenizer.ggml.tokens", &["a", "b", "c"]);
        entry_string(&mut buf, "general.name", "test-model");
        entry_u32(&mut buf, "general.file_type", 15);
        entry_string(&mut buf, "general.architecture", "llama");

        let info = parse(&buf).unwrap();
        assert_eq!(info.version, 3);
        assert_eq!(info.tensor_count, 2);
        assert_eq!(info.architecture.as_deref(), Some("llama"));
        assert_eq!(info.name.as_deref(), Some("test-model"));
        assert_eq!(info.file_type, Some(15));
        assert_eq!(info.context_length, Some(4096));
        assert_eq!(info.embedding_length, Some(5120));
        assert_eq!(info.block_count, Some(32));
        assert_eq!(info.header_bytes_read, buf.len());
    }

    #[test]
    fn parses_minimal_v2_header() {
        let buf = begin(2, 0, 0);
        let info = parse(&buf).unwrap();
        assert_eq!(info.version, 2);
        assert_eq!(info.tensor_count, 0);
        assert_eq!(info.architecture, None);
        assert_eq!(info.context_length, None);
        assert_eq!(info.header_bytes_read, buf.len());
    }

    #[test]
    fn every_truncation_is_an_error_not_a_panic() {
        let mut full = begin(3, 1, 1);
        entry_u32(&mut full, "llama.context_length", 4096);
        for i in 0..full.len() {
            let result = parse(&full[..i]);
            assert!(result.is_err(), "prefix of {i} bytes unexpectedly parsed");
        }
        assert!(parse(&full).is_ok());
    }

    #[test]
    fn rejects_absurd_counts() {
        assert_eq!(
            parse(&begin(3, u64::MAX, 0)),
            Err(GgufError::TooManyTensors)
        );
        assert_eq!(
            parse(&begin(3, 0, u64::MAX)),
            Err(GgufError::TooManyEntries)
        );

        let mut buf = begin(3, 0, 1);
        push_string(&mut buf, "some.array");
        push_u32(&mut buf, 9);
        push_u32(&mut buf, 0); // element type u8
        push_u64(&mut buf, u64::MAX);
        assert_eq!(parse(&buf), Err(GgufError::ArrayTooLarge));
    }

    #[test]
    fn rejects_string_length_2_to_the_63() {
        let mut buf = begin(3, 0, 1);
        push_string(&mut buf, "general.name");
        push_u32(&mut buf, 8);
        push_u64(&mut buf, 1u64 << 63);
        assert_eq!(parse(&buf), Err(GgufError::StringTooLong));

        // Also inside an array of strings.
        let mut buf = begin(3, 0, 1);
        push_string(&mut buf, "tokens");
        push_u32(&mut buf, 9);
        push_u32(&mut buf, 8);
        push_u64(&mut buf, 1);
        push_u64(&mut buf, 1u64 << 63);
        assert_eq!(parse(&buf), Err(GgufError::StringTooLong));
    }

    #[test]
    fn rejects_nesting_deeper_than_four() {
        fn nested_header(total_arrays: u32) -> Vec<u8> {
            let mut buf = begin(3, 0, 1);
            push_string(&mut buf, "nested");
            push_u32(&mut buf, 9); // outer value type: array
            for _ in 1..total_arrays {
                push_u32(&mut buf, 9); // element type: array
                push_u64(&mut buf, 1); // one element
            }
            push_u32(&mut buf, 0); // element type: u8
            push_u64(&mut buf, 1); // one element
            buf.push(7);
            buf
        }

        assert!(parse(&nested_header(4)).is_ok());
        assert_eq!(parse(&nested_header(5)), Err(GgufError::ArrayTooLarge));
    }

    #[test]
    fn rejects_bad_magic_and_version() {
        let buf = b"NOPE".to_vec();
        assert_eq!(parse(&buf), Err(GgufError::BadMagic));
        let buf = begin(99, 0, 0);
        assert_eq!(parse(&buf), Err(GgufError::UnsupportedVersion(99)));
        assert_eq!(parse(&[]), Err(GgufError::NeedMoreBytes(4)));
    }

    #[test]
    fn rejects_unknown_value_type() {
        let mut buf = begin(3, 0, 1);
        push_string(&mut buf, "something");
        push_u32(&mut buf, 200);
        assert_eq!(parse(&buf), Err(GgufError::BadValueType(200)));
    }

    #[test]
    fn rejects_invalid_utf8_in_string() {
        let mut buf = begin(3, 0, 1);
        push_string(&mut buf, "general.name");
        push_u32(&mut buf, 8);
        push_u64(&mut buf, 2);
        buf.extend_from_slice(&[0xff, 0xfe]);
        assert_eq!(parse(&buf), Err(GgufError::InvalidUtf8));
    }

    #[test]
    fn dimension_key_resolution_rules() {
        // Ambiguous candidates and no architecture: give up.
        let mut buf = begin(3, 0, 2);
        entry_u32(&mut buf, "a.block_count", 10);
        entry_u32(&mut buf, "b.block_count", 20);
        let info = parse(&buf).unwrap();
        assert_eq!(info.block_count, None);

        // Single candidate, no architecture: accept it.
        let mut buf = begin(3, 0, 1);
        entry_u32(&mut buf, "solo.block_count", 42);
        let info = parse(&buf).unwrap();
        assert_eq!(info.block_count, Some(42));
    }

    #[test]
    fn estimate_ram_sums_file_kv_and_overhead() {
        let info = GgufInfo {
            version: 3,
            tensor_count: 1,
            architecture: Some("llama".to_string()),
            name: None,
            file_type: None,
            context_length: None,
            embedding_length: Some(5120),
            block_count: Some(32),
            header_bytes_read: 128,
        };
        let kv = 4 * 32 * 4096 * 5120u64;
        assert_eq!(
            estimate_ram(&info, 1_000_000_000, 4096),
            1_000_000_000 + kv + OVERHEAD_BYTES
        );

        let bare = GgufInfo {
            architecture: None,
            embedding_length: None,
            block_count: None,
            ..info.clone()
        };
        assert_eq!(estimate_ram(&bare, 123, 4096), 123 + OVERHEAD_BYTES);

        let huge = GgufInfo {
            architecture: None,
            block_count: Some(u64::MAX),
            embedding_length: Some(u64::MAX),
            ..info
        };
        assert_eq!(estimate_ram(&huge, u64::MAX, u64::MAX), u64::MAX);
    }

    #[test]
    fn file_type_names() {
        assert_eq!(file_type_name(0), "F32");
        assert_eq!(file_type_name(1), "F16");
        assert_eq!(file_type_name(2), "Q4_0");
        assert_eq!(file_type_name(3), "Q4_1");
        assert_eq!(file_type_name(7), "Q8_0");
        assert_eq!(file_type_name(10), "Q2_K");
        assert_eq!(file_type_name(15), "Q4_K_M");
        assert_eq!(file_type_name(999), "unknown");
    }
}
