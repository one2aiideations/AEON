//! Metadata values and typed arrays.
#![forbid(unsafe_code)]

use crate::cursor::Cursor;
use crate::error::GgufError;
use crate::types::ValueType;

/// Deepest allowed array nesting (an array of arrays of arrays of arrays is level 4).
pub const MAX_ARRAY_DEPTH: u32 = 4;

/// A metadata value.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// Unsigned 8-bit integer.
    U8(u8),
    /// Signed 8-bit integer.
    I8(i8),
    /// Unsigned 16-bit integer.
    U16(u16),
    /// Signed 16-bit integer.
    I16(i16),
    /// Unsigned 32-bit integer.
    U32(u32),
    /// Signed 32-bit integer.
    I32(i32),
    /// 32-bit float.
    F32(f32),
    /// Boolean.
    Bool(bool),
    /// UTF-8 string.
    String(String),
    /// Typed array.
    Array(Array),
    /// Unsigned 64-bit integer.
    U64(u64),
    /// Signed 64-bit integer.
    I64(i64),
    /// 64-bit float.
    F64(f64),
}

/// A homogeneous array, stored as a typed vector so large vocabularies stay compact.
#[derive(Debug, Clone, PartialEq)]
pub enum Array {
    /// `u8` elements.
    U8(Vec<u8>),
    /// `i8` elements.
    I8(Vec<i8>),
    /// `u16` elements.
    U16(Vec<u16>),
    /// `i16` elements.
    I16(Vec<i16>),
    /// `u32` elements.
    U32(Vec<u32>),
    /// `i32` elements.
    I32(Vec<i32>),
    /// `f32` elements.
    F32(Vec<f32>),
    /// Boolean elements.
    Bool(Vec<bool>),
    /// String elements.
    String(Vec<String>),
    /// Nested arrays.
    Array(Vec<Array>),
    /// `u64` elements.
    U64(Vec<u64>),
    /// `i64` elements.
    I64(Vec<i64>),
    /// `f64` elements.
    F64(Vec<f64>),
}

impl Value {
    /// Name of this value's type, for error messages.
    pub fn type_name(&self) -> &'static str {
        match self {
            Value::U8(_) => "U8",
            Value::I8(_) => "I8",
            Value::U16(_) => "U16",
            Value::I16(_) => "I16",
            Value::U32(_) => "U32",
            Value::I32(_) => "I32",
            Value::F32(_) => "F32",
            Value::Bool(_) => "BOOL",
            Value::String(_) => "STRING",
            Value::Array(_) => "ARRAY",
            Value::U64(_) => "U64",
            Value::I64(_) => "I64",
            Value::F64(_) => "F64",
        }
    }

    /// The wire type of this value.
    pub fn value_type(&self) -> ValueType {
        match self {
            Value::U8(_) => ValueType::U8,
            Value::I8(_) => ValueType::I8,
            Value::U16(_) => ValueType::U16,
            Value::I16(_) => ValueType::I16,
            Value::U32(_) => ValueType::U32,
            Value::I32(_) => ValueType::I32,
            Value::F32(_) => ValueType::F32,
            Value::Bool(_) => ValueType::BOOL,
            Value::String(_) => ValueType::STRING,
            Value::Array(_) => ValueType::ARRAY,
            Value::U64(_) => ValueType::U64,
            Value::I64(_) => ValueType::I64,
            Value::F64(_) => ValueType::F64,
        }
    }

    /// Any integer value that fits a `u32` losslessly (negative values do not fit).
    pub fn as_u32(&self) -> Option<u32> {
        self.as_u64().and_then(|v| u32::try_from(v).ok())
    }

    /// Any integer value that fits a `u64` losslessly (negative values do not fit).
    pub fn as_u64(&self) -> Option<u64> {
        match *self {
            Value::U8(v) => Some(u64::from(v)),
            Value::U16(v) => Some(u64::from(v)),
            Value::U32(v) => Some(u64::from(v)),
            Value::U64(v) => Some(v),
            Value::I8(v) => u64::try_from(v).ok(),
            Value::I16(v) => u64::try_from(v).ok(),
            Value::I32(v) => u64::try_from(v).ok(),
            Value::I64(v) => u64::try_from(v).ok(),
            _ => None,
        }
    }

    /// Any integer value that fits an `i64` losslessly.
    pub fn as_i64(&self) -> Option<i64> {
        match *self {
            Value::I8(v) => Some(i64::from(v)),
            Value::I16(v) => Some(i64::from(v)),
            Value::I32(v) => Some(i64::from(v)),
            Value::I64(v) => Some(v),
            Value::U8(v) => Some(i64::from(v)),
            Value::U16(v) => Some(i64::from(v)),
            Value::U32(v) => Some(i64::from(v)),
            Value::U64(v) => i64::try_from(v).ok(),
            _ => None,
        }
    }

    /// `F32` as is; `F64` narrowed to `f32` (may lose precision). Other types give `None`.
    pub fn as_f32(&self) -> Option<f32> {
        match *self {
            Value::F32(v) => Some(v),
            Value::F64(v) => Some(v as f32),
            _ => None,
        }
    }

    /// The boolean, if this is a `Bool`.
    pub fn as_bool(&self) -> Option<bool> {
        match *self {
            Value::Bool(v) => Some(v),
            _ => None,
        }
    }

    /// The string, if this is a `String`.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s),
            _ => None,
        }
    }

    /// The array, if this is an `Array`.
    pub fn as_array(&self) -> Option<&Array> {
        match self {
            Value::Array(a) => Some(a),
            _ => None,
        }
    }
}

impl Array {
    /// Number of elements.
    pub fn len(&self) -> usize {
        match self {
            Array::U8(v) => v.len(),
            Array::I8(v) => v.len(),
            Array::U16(v) => v.len(),
            Array::I16(v) => v.len(),
            Array::U32(v) => v.len(),
            Array::I32(v) => v.len(),
            Array::F32(v) => v.len(),
            Array::Bool(v) => v.len(),
            Array::String(v) => v.len(),
            Array::Array(v) => v.len(),
            Array::U64(v) => v.len(),
            Array::I64(v) => v.len(),
            Array::F64(v) => v.len(),
        }
    }

    /// `true` if there are no elements.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The element wire type.
    pub fn element_type(&self) -> ValueType {
        match self {
            Array::U8(_) => ValueType::U8,
            Array::I8(_) => ValueType::I8,
            Array::U16(_) => ValueType::U16,
            Array::I16(_) => ValueType::I16,
            Array::U32(_) => ValueType::U32,
            Array::I32(_) => ValueType::I32,
            Array::F32(_) => ValueType::F32,
            Array::Bool(_) => ValueType::BOOL,
            Array::String(_) => ValueType::STRING,
            Array::Array(_) => ValueType::ARRAY,
            Array::U64(_) => ValueType::U64,
            Array::I64(_) => ValueType::I64,
            Array::F64(_) => ValueType::F64,
        }
    }

    /// The elements, if this is a string array.
    pub fn as_strings(&self) -> Option<&[String]> {
        match self {
            Array::String(v) => Some(v),
            _ => None,
        }
    }

    /// The elements, if this is an `f32` array.
    pub fn as_f32s(&self) -> Option<&[f32]> {
        match self {
            Array::F32(v) => Some(v),
            _ => None,
        }
    }

    /// The elements, if this is an `i32` array.
    pub fn as_i32s(&self) -> Option<&[i32]> {
        match self {
            Array::I32(v) => Some(v),
            _ => None,
        }
    }

    /// The elements, if this is a `u32` array.
    pub fn as_u32s(&self) -> Option<&[u32]> {
        match self {
            Array::U32(v) => Some(v),
            _ => None,
        }
    }
}

/// Smallest number of bytes one element of `ty` can occupy on disk.
fn min_element_bytes(ty: ValueType) -> u64 {
    match ty {
        ValueType::STRING => 8,    // u64 length
        ValueType::ARRAY => 4 + 8, // element type + count
        other => other.fixed_size().unwrap_or(1),
    }
}

/// Parse one value of type `ty`. `depth` is the array nesting level already entered.
pub(crate) fn parse_value(
    c: &mut Cursor<'_>,
    ty: ValueType,
    depth: u32,
) -> Result<Value, GgufError> {
    Ok(match ty {
        ValueType::U8 => Value::U8(c.read_u8()?),
        ValueType::I8 => Value::I8(c.read_i8()?),
        ValueType::U16 => Value::U16(c.read_u16()?),
        ValueType::I16 => Value::I16(c.read_i16()?),
        ValueType::U32 => Value::U32(c.read_u32()?),
        ValueType::I32 => Value::I32(c.read_i32()?),
        ValueType::F32 => Value::F32(c.read_f32()?),
        ValueType::BOOL => Value::Bool(parse_bool(c)?),
        ValueType::STRING => Value::String(c.read_string()?),
        ValueType::ARRAY => Value::Array(parse_array(c, depth + 1)?),
        ValueType::U64 => Value::U64(c.read_u64()?),
        ValueType::I64 => Value::I64(c.read_i64()?),
        ValueType::F64 => Value::F64(c.read_f64()?),
    })
}

fn parse_bool(c: &mut Cursor<'_>) -> Result<bool, GgufError> {
    match c.read_u8()? {
        0 => Ok(false),
        1 => Ok(true),
        b => Err(GgufError::BadBool(b)),
    }
}

/// Parse an array body (element type, count, elements) at nesting `level` (1 = outermost).
fn parse_array(c: &mut Cursor<'_>, level: u32) -> Result<Array, GgufError> {
    if level > MAX_ARRAY_DEPTH {
        return Err(GgufError::ArrayTooDeep);
    }
    let elem = ValueType::from_u32(c.read_u32()?)?;
    let count = c.read_u64()?;
    // Cap the count by the bytes left before allocating anything.
    let min = min_element_bytes(elem);
    let needed = count.checked_mul(min).ok_or(GgufError::CountTooLarge {
        what: "array",
        count,
    })?;
    if needed > c.remaining() as u64 {
        return Err(GgufError::CountTooLarge {
            what: "array",
            count,
        });
    }
    let n = usize::try_from(count).map_err(|_| GgufError::CountTooLarge {
        what: "array",
        count,
    })?;

    macro_rules! fixed {
        ($variant:ident, $read:ident) => {{
            let mut v = Vec::with_capacity(n);
            for _ in 0..n {
                v.push(c.$read()?);
            }
            Array::$variant(v)
        }};
    }
    Ok(match elem {
        ValueType::U8 => fixed!(U8, read_u8),
        ValueType::I8 => fixed!(I8, read_i8),
        ValueType::U16 => fixed!(U16, read_u16),
        ValueType::I16 => fixed!(I16, read_i16),
        ValueType::U32 => fixed!(U32, read_u32),
        ValueType::I32 => fixed!(I32, read_i32),
        ValueType::F32 => fixed!(F32, read_f32),
        ValueType::U64 => fixed!(U64, read_u64),
        ValueType::I64 => fixed!(I64, read_i64),
        ValueType::F64 => fixed!(F64, read_f64),
        ValueType::BOOL => {
            let mut v = Vec::with_capacity(n);
            for _ in 0..n {
                v.push(parse_bool(c)?);
            }
            Array::Bool(v)
        }
        ValueType::STRING => {
            let mut v = Vec::with_capacity(n);
            for _ in 0..n {
                v.push(c.read_string()?);
            }
            Array::String(v)
        }
        ValueType::ARRAY => {
            let mut v = Vec::with_capacity(n);
            for _ in 0..n {
                v.push(parse_array(c, level + 1)?);
            }
            Array::Array(v)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arr_bytes(elem: u32, count: u64, body: &[u8]) -> Vec<u8> {
        let mut v = elem.to_le_bytes().to_vec();
        v.extend_from_slice(&count.to_le_bytes());
        v.extend_from_slice(body);
        v
    }

    #[test]
    fn integer_accessors_are_lossless_only() {
        assert_eq!(Value::U8(7).as_u32(), Some(7));
        assert_eq!(Value::I32(5).as_u32(), Some(5));
        assert_eq!(Value::I32(-5).as_u32(), None);
        assert_eq!(Value::U64(u64::from(u32::MAX) + 1).as_u32(), None);
        assert_eq!(Value::U64(u64::MAX).as_i64(), None);
        assert_eq!(Value::I8(-3).as_i64(), Some(-3));
        assert_eq!(Value::F32(1.0).as_u32(), None);
        assert_eq!(Value::String("x".into()).as_u64(), None);
    }

    #[test]
    fn float_bool_str_array_accessors() {
        assert_eq!(Value::F32(1.5).as_f32(), Some(1.5));
        assert_eq!(Value::F64(2.5).as_f32(), Some(2.5));
        assert_eq!(Value::U32(1).as_f32(), None);
        assert_eq!(Value::Bool(true).as_bool(), Some(true));
        assert_eq!(Value::U8(1).as_bool(), None);
        assert_eq!(Value::String("a".into()).as_str(), Some("a"));
        assert!(Value::Array(Array::U32(vec![1])).as_array().is_some());
        assert!(Value::U32(1).as_array().is_none());
    }

    #[test]
    fn array_helpers() {
        let a = Array::String(vec!["a".into(), "b".into()]);
        assert_eq!(a.len(), 2);
        assert!(!a.is_empty());
        assert_eq!(a.as_strings().unwrap().len(), 2);
        assert!(a.as_f32s().is_none());
        assert_eq!(Array::F32(vec![1.0]).as_f32s().unwrap(), &[1.0]);
        assert_eq!(Array::I32(vec![-1]).as_i32s().unwrap(), &[-1]);
        assert_eq!(Array::U32(vec![3]).as_u32s().unwrap(), &[3]);
        assert!(Array::U8(vec![]).is_empty());
        assert_eq!(Array::Array(vec![]).element_type(), ValueType::ARRAY);
    }

    #[test]
    fn parses_scalar_and_typed_arrays() {
        let mut c = Cursor::new(&[0xfe, 0xff]);
        assert_eq!(
            parse_value(&mut c, ValueType::I16, 0).unwrap(),
            Value::I16(-2)
        );

        let bytes = arr_bytes(4, 3, &[1, 0, 0, 0, 2, 0, 0, 0, 3, 0, 0, 0]);
        let mut c = Cursor::new(&bytes);
        let v = parse_value(&mut c, ValueType::ARRAY, 0).unwrap();
        assert_eq!(v, Value::Array(Array::U32(vec![1, 2, 3])));
    }

    #[test]
    fn bad_bool_scalar_and_array() {
        assert!(matches!(
            parse_value(&mut Cursor::new(&[2]), ValueType::BOOL, 0),
            Err(GgufError::BadBool(2))
        ));
        let bytes = arr_bytes(7, 2, &[1, 5]);
        assert!(matches!(
            parse_value(&mut Cursor::new(&bytes), ValueType::ARRAY, 0),
            Err(GgufError::BadBool(5))
        ));
    }

    #[test]
    fn count_larger_than_remaining_is_rejected_before_allocating() {
        let bytes = arr_bytes(4, u64::MAX, &[0; 8]);
        assert!(matches!(
            parse_value(&mut Cursor::new(&bytes), ValueType::ARRAY, 0),
            Err(GgufError::CountTooLarge { what: "array", .. })
        ));
        let bytes = arr_bytes(8, 1_000_000, &[0; 16]);
        assert!(matches!(
            parse_value(&mut Cursor::new(&bytes), ValueType::ARRAY, 0),
            Err(GgufError::CountTooLarge { .. })
        ));
    }

    #[test]
    fn unknown_element_type_is_an_error() {
        let bytes = arr_bytes(99, 0, &[]);
        assert!(matches!(
            parse_value(&mut Cursor::new(&bytes), ValueType::ARRAY, 0),
            Err(GgufError::UnknownValueType(99))
        ));
    }

    fn nested(levels: u32) -> Vec<u8> {
        // `levels` arrays deep; the innermost holds one u8.
        let mut inner = arr_bytes(0, 1, &[9]);
        for _ in 1..levels {
            let mut outer = 9u32.to_le_bytes().to_vec();
            outer.extend_from_slice(&1u64.to_le_bytes());
            outer.extend_from_slice(&inner);
            inner = outer;
        }
        inner
    }

    #[test]
    fn nesting_limit_is_four_levels() {
        for ok in 1..=4 {
            let b = nested(ok);
            assert!(
                parse_value(&mut Cursor::new(&b), ValueType::ARRAY, 0).is_ok(),
                "depth {ok}"
            );
        }
        let b = nested(5);
        assert!(matches!(
            parse_value(&mut Cursor::new(&b), ValueType::ARRAY, 0),
            Err(GgufError::ArrayTooDeep)
        ));
    }

    #[test]
    fn strings_in_arrays() {
        let mut body = Vec::new();
        for s in ["ab", "", "cde"] {
            body.extend_from_slice(&(s.len() as u64).to_le_bytes());
            body.extend_from_slice(s.as_bytes());
        }
        let bytes = arr_bytes(8, 3, &body);
        let v = parse_value(&mut Cursor::new(&bytes), ValueType::ARRAY, 0).unwrap();
        assert_eq!(
            v,
            Value::Array(Array::String(vec!["ab".into(), "".into(), "cde".into()]))
        );
    }
}
