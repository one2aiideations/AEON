//! Slices the SwiGLU feed-forward blocks of a dense llama or qwen2 GGUF model.
//!
//! Gate and up weights keep their quantised bytes (whole rows are copied, nothing is
//! re-quantised). Down weights are dequantised, the kept neurons selected, and the result stored
//! as F16 or Q8_0. Everything else is copied.
//!
//! Slicing does not show that the smaller model still answers well; measure that separately.

use mouli_llm::gguf::{parse, GgmlType, GgufWriter, Value};
use mouli_llm::quant::dequantize_vec;
use std::error::Error;
use std::fmt;

const BLOCK: usize = 32;
const Q8_BLOCK_BYTES: usize = 34;

/// How the kept columns of every `ffn_down` tensor are stored in the output.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DownType {
    /// IEEE half precision.
    F16,
    /// 8-bit blocks of 32 with an f16 scale.
    Q8_0,
}

/// The result of slicing a GGUF model.
///
/// `bytes_before` and `bytes_after` describe complete GGUF file sizes.
/// The result does not prove that the sliced model retains its original
/// capabilities, accuracy, or generation quality. `general.file_type`
/// is preserved but is no longer necessarily an exact description of
/// the tensors' quantisation formats.
#[derive(Debug)]
pub struct SliceOutcome {
    /// The newly serialized GGUF file.
    pub bytes: Vec<u8>,
    /// Number of transformer layers processed.
    pub layers: usize,
    /// Original feed-forward width.
    pub d_ff_before: usize,
    /// Retained feed-forward width.
    pub d_ff_after: usize,
    /// Original GGUF file size.
    pub bytes_before: usize,
    /// New GGUF file size.
    pub bytes_after: usize,
}

/// Errors encountered while reading, validating, slicing, or writing a GGUF.
///
/// An unreadable model means parsing or the output self-check failed.
/// Unsupported models include architectures and layouts that this
/// implementation cannot safely interpret. Bad plans describe invalid
/// neuron selections. Unsupported tensor types describe quantisation
/// formats that cannot be processed by the required operation.
/// Arithmetic overflow means a size or offset could not be represented.
#[derive(Debug)]
pub enum GgufSliceError {
    /// The input or generated GGUF could not be parsed or serialized.
    Unreadable(String),
    /// The architecture, model layout, metadata, or tensor is unsupported.
    UnsupportedModel(String),
    /// The neuron selection does not satisfy the slicing requirements.
    BadPlan(String),
    /// A tensor uses a format that cannot be processed.
    UnsupportedTensorType(String),
    /// A checked size or arithmetic operation overflowed.
    ArithmeticOverflow(String),
}

impl fmt::Display for GgufSliceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unreadable(s) => write!(f, "unreadable GGUF: {s}"),
            Self::UnsupportedModel(s) => write!(f, "unsupported model: {s}"),
            Self::BadPlan(s) => write!(f, "bad slicing plan: {s}"),
            Self::UnsupportedTensorType(s) => {
                write!(f, "unsupported tensor type: {s}")
            }
            Self::ArithmeticOverflow(s) => {
                write!(f, "arithmetic overflow: {s}")
            }
        }
    }
}

impl Error for GgufSliceError {}

/// One replacement MLP tensor: name, dims, type, data.
type Replacement = (String, Vec<u64>, GgmlType, Vec<u8>);

fn checked_usize(value: u64, description: &str) -> Result<usize, GgufSliceError> {
    usize::try_from(value).map_err(|_| GgufSliceError::ArithmeticOverflow(description.to_owned()))
}

fn checked_u32(value: u64, description: &str) -> Result<u32, GgufSliceError> {
    u32::try_from(value).map_err(|_| GgufSliceError::ArithmeticOverflow(description.to_owned()))
}

fn checked_mul(a: usize, b: usize, description: &str) -> Result<usize, GgufSliceError> {
    a.checked_mul(b)
        .ok_or_else(|| GgufSliceError::ArithmeticOverflow(description.to_owned()))
}

fn metadata_width(gguf: &mouli_llm::gguf::Gguf, key: &str) -> Result<usize, GgufSliceError> {
    let value = gguf.get_u32(key).map_err(|e| {
        GgufSliceError::UnsupportedModel(format!("missing or invalid metadata {key}: {e:?}"))
    })?;

    usize::try_from(value)
        .map_err(|_| GgufSliceError::ArithmeticOverflow(format!("metadata {key} cannot fit usize")))
}

fn is_moe(gguf: &mouli_llm::gguf::Gguf, architecture: &str) -> bool {
    let key = format!("{architecture}.expert_count");

    let nonzero = match gguf.get(&key) {
        Some(Value::U8(v)) => *v != 0,
        Some(Value::U16(v)) => *v != 0,
        Some(Value::U32(v)) => *v != 0,
        Some(Value::U64(v)) => *v != 0,
        Some(Value::I8(v)) => *v != 0,
        Some(Value::I16(v)) => *v != 0,
        Some(Value::I32(v)) => *v != 0,
        Some(Value::I64(v)) => *v != 0,
        Some(Value::F32(v)) => *v != 0.0,
        Some(Value::F64(v)) => *v != 0.0,
        Some(_) => true,
        None => false,
    };

    nonzero
        || gguf
            .tensors()
            .iter()
            .any(|tensor| tensor.name.contains("ffn_gate_exps"))
}

fn row_bytes(ty: GgmlType, elements: usize, name: &str) -> Result<usize, GgufSliceError> {
    let elements_u64 = u64::try_from(elements)
        .map_err(|_| GgufSliceError::ArithmeticOverflow(format!("{name}: row element count")))?;

    let bytes = ty.row_bytes(elements_u64).map_err(|e| {
        GgufSliceError::UnsupportedTensorType(format!(
            "{name}: row is incompatible with {ty:?}: {e:?}"
        ))
    })?;

    checked_usize(bytes, name)
}

fn tensor_data(
    gguf: &mouli_llm::gguf::Gguf,
    file: &[u8],
    name: &str,
    dims: &[u64],
) -> Result<(GgmlType, Vec<u8>), GgufSliceError> {
    let info = gguf.tensor(name).ok_or_else(|| {
        GgufSliceError::UnsupportedModel(format!("required tensor {name} is missing"))
    })?;

    if info.dims != dims {
        return Err(GgufSliceError::UnsupportedModel(format!(
            "{name}: expected dimensions {dims:?}, found {:?}",
            info.dims
        )));
    }

    let data = gguf
        .tensor_data(file, name)
        .map_err(|e| GgufSliceError::Unreadable(format!("cannot read tensor {name}: {e:?}")))?;

    let expected = checked_usize(info.n_bytes, name)?;

    if data.len() != expected {
        return Err(GgufSliceError::Unreadable(format!(
            "{name}: declared {expected} bytes but obtained {}",
            data.len()
        )));
    }

    Ok((info.ty, data.to_vec()))
}

fn validate_matrix(
    gguf: &mouli_llm::gguf::Gguf,
    file: &[u8],
    name: &str,
    dims: &[u64],
    rows: usize,
    columns: usize,
) -> Result<(GgmlType, Vec<u8>, usize), GgufSliceError> {
    let (ty, data) = tensor_data(gguf, file, name, dims)?;
    let bytes_per_row = row_bytes(ty, columns, name)?;
    let expected = checked_mul(rows, bytes_per_row, name)?;

    if data.len() != expected {
        return Err(GgufSliceError::UnsupportedModel(format!(
            "{name}: expected {expected} bytes, found {}",
            data.len()
        )));
    }

    Ok((ty, data, bytes_per_row))
}

fn select_rows(
    source: &[u8],
    row_size: usize,
    indices: &[usize],
    name: &str,
) -> Result<Vec<u8>, GgufSliceError> {
    let output_size = checked_mul(row_size, indices.len(), name)?;

    let mut output = Vec::with_capacity(output_size);

    for &index in indices {
        let start = checked_mul(index, row_size, name)?;
        let end = start
            .checked_add(row_size)
            .ok_or_else(|| GgufSliceError::ArithmeticOverflow(name.to_owned()))?;

        let row = source.get(start..end).ok_or_else(|| {
            GgufSliceError::Unreadable(format!("{name}: source row {index} is outside the tensor"))
        })?;

        output.extend_from_slice(row);
    }

    Ok(output)
}

/// Convert IEEE-754 single precision to IEEE-754 binary16.
///
/// Implements round-to-nearest, ties-to-even, including subnormals,
/// signed zero, infinities, overflow, and NaN payload preservation.
/// This conversion does not establish the numerical quality of a
/// model whose weights are converted to half precision.
fn f32_to_f16(value: f32) -> u16 {
    let bits = value.to_bits();
    let sign = (bits >> 16) & 0x8000;
    let exponent = ((bits >> 23) & 0xff) as i32;
    let mantissa = bits & 0x007f_ffff;

    if exponent == 0xff {
        if mantissa == 0 {
            return (sign | 0x7c00) as u16;
        }

        let payload = mantissa >> 13;
        return (sign | 0x7c00 | payload | 0x0200) as u16;
    }

    let half_exp = exponent - 127 + 15;

    if half_exp >= 31 {
        return (sign | 0x7c00) as u16;
    }

    if half_exp <= 0 {
        if half_exp < -10 {
            return sign as u16;
        }

        let significand = mantissa | 0x0080_0000;
        let shift = (14 - half_exp) as u32;
        let mut result = significand >> shift;

        let remainder_mask = (1u32 << shift) - 1;
        let remainder = significand & remainder_mask;
        let halfway = 1u32 << (shift - 1);

        if remainder > halfway || (remainder == halfway && result & 1 != 0) {
            result += 1;
        }

        return (sign | result) as u16;
    }

    let mut result = sign | ((half_exp as u32) << 10) | (mantissa >> 13);

    let remainder = mantissa & 0x1fff;

    if remainder > 0x1000 || (remainder == 0x1000 && result & 1 != 0) {
        result += 1;
    }

    if result & 0x7c00 == 0x7c00 {
        result = sign | 0x7c00;
    }

    result as u16
}

#[cfg(test)]
fn f16_to_f32(value: u16) -> f32 {
    let sign = u32::from(value & 0x8000) << 16;
    let exponent = u32::from((value >> 10) & 0x1f);
    let mantissa = u32::from(value & 0x03ff);

    let bits = match exponent {
        0 if mantissa == 0 => sign,
        0 => {
            let mut m = mantissa;
            let mut e = -14i32;

            while m & 0x400 == 0 {
                m <<= 1;
                e -= 1;
            }

            m &= 0x3ff;

            sign | (((e + 127) as u32) << 23) | (m << 13)
        }
        31 => sign | 0x7f80_0000 | (mantissa << 13),
        _ => sign | ((exponent + 112) << 23) | (mantissa << 13),
    };

    f32::from_bits(bits)
}

fn encode_f16(values: &[f32]) -> Result<Vec<u8>, GgufSliceError> {
    let capacity = checked_mul(values.len(), 2, "F16 output size")?;
    let mut output = Vec::with_capacity(capacity);

    for &value in values {
        let bits = f32_to_f16(value);
        output.extend_from_slice(&bits.to_le_bytes());
    }

    Ok(output)
}

fn encode_q8_0(values: &[f32]) -> Result<Vec<u8>, GgufSliceError> {
    if !values.len().is_multiple_of(BLOCK) {
        return Err(GgufSliceError::BadPlan(format!(
            "Q8_0 output width {} is not a multiple of {BLOCK}",
            values.len()
        )));
    }

    let blocks = values.len() / BLOCK;
    let capacity = checked_mul(blocks, Q8_BLOCK_BYTES, "Q8_0 output size")?;

    let mut output = Vec::with_capacity(capacity);

    for block in values.as_chunks::<BLOCK>().0 {
        let max_abs = block
            .iter()
            .fold(0.0f32, |maximum, value| maximum.max(value.abs()));

        let scale = max_abs / 127.0;
        let scale_bits = f32_to_f16(scale);

        output.extend_from_slice(&scale_bits.to_le_bytes());

        for &value in block {
            let quantized = if scale == 0.0 {
                0
            } else {
                (value / scale).round().clamp(-127.0, 127.0) as i8
            };

            output.push(quantized as u8);
        }
    }

    Ok(output)
}

fn slice_down(
    ty: GgmlType,
    source: &[u8],
    d_model: usize,
    d_ff: usize,
    keep: &[usize],
    down: DownType,
    name: &str,
) -> Result<(GgmlType, Vec<u8>), GgufSliceError> {
    let source_row_size = row_bytes(ty, d_ff, name)?;

    let expected = checked_mul(d_model, source_row_size, name)?;

    if source.len() != expected {
        return Err(GgufSliceError::UnsupportedModel(format!(
            "{name}: expected {expected} bytes, found {}",
            source.len()
        )));
    }

    let mut selected = Vec::with_capacity(checked_mul(d_model, keep.len(), name)?);

    for row_index in 0..d_model {
        let start = checked_mul(row_index, source_row_size, name)?;
        let end = start
            .checked_add(source_row_size)
            .ok_or_else(|| GgufSliceError::ArithmeticOverflow(name.to_owned()))?;

        let row = source.get(start..end).ok_or_else(|| {
            GgufSliceError::Unreadable(format!("{name}: invalid source row {row_index}"))
        })?;

        let values = dequantize_vec(ty, row, d_ff).map_err(|e| {
            GgufSliceError::UnsupportedTensorType(format!("{name}: dequantisation failed: {e:?}"))
        })?;

        if values.len() != d_ff {
            return Err(GgufSliceError::Unreadable(format!(
                "{name}: dequantiser returned {} elements, expected {d_ff}",
                values.len()
            )));
        }

        for &index in keep {
            let value = *values.get(index).ok_or_else(|| {
                GgufSliceError::BadPlan(format!("{name}: neuron index {index} is invalid"))
            })?;

            if !value.is_finite() {
                return Err(GgufSliceError::UnsupportedTensorType(format!(
                    "{name}: non-finite dequantised value"
                )));
            }

            selected.push(value);
        }
    }

    match down {
        DownType::F16 => Ok((GgmlType::F16, encode_f16(&selected)?)),
        DownType::Q8_0 => Ok((GgmlType::Q8_0, encode_q8_0(&selected)?)),
    }
}

fn validate_plan(keep: &[Vec<usize>], layers: usize, d_ff: usize) -> Result<usize, GgufSliceError> {
    if keep.len() != layers {
        return Err(GgufSliceError::BadPlan(format!(
            "expected {layers} layer selections, received {}",
            keep.len()
        )));
    }

    let mut expected_width = None;

    for (layer, indices) in keep.iter().enumerate() {
        if indices.is_empty() {
            return Err(GgufSliceError::BadPlan(format!(
                "layer {layer}: selection cannot be empty"
            )));
        }

        if let Some(width) = expected_width {
            if indices.len() != width {
                return Err(GgufSliceError::BadPlan(format!(
                    "unequal widths: layer 0 keeps {width} neurons, \
                     layer {layer} keeps {}",
                    indices.len()
                )));
            }
        } else {
            expected_width = Some(indices.len());
        }

        for (position, &index) in indices.iter().enumerate() {
            if index >= d_ff {
                return Err(GgufSliceError::BadPlan(format!(
                    "layer {layer}: neuron index {index} is outside 0..{d_ff}"
                )));
            }

            if position > 0 {
                let previous = indices.get(position - 1).copied().ok_or_else(|| {
                    GgufSliceError::BadPlan("invalid selection ordering".to_owned())
                })?;

                if index <= previous {
                    return Err(GgufSliceError::BadPlan(format!(
                        "layer {layer}: indices must be strictly ascending; \
                         found {previous} followed by {index}"
                    )));
                }
            }
        }
    }

    let width = expected_width
        .ok_or_else(|| GgufSliceError::BadPlan("no layer selections supplied".to_owned()))?;

    if !width.is_multiple_of(BLOCK) {
        let lower = (width / BLOCK) * BLOCK;
        let upper = lower.checked_add(BLOCK);

        let lower_valid = (lower > 0).then_some(lower);
        let upper_valid = upper.filter(|&value| value <= d_ff);

        return Err(GgufSliceError::BadPlan(format!(
            "kept width {width} must be a multiple of {BLOCK}; \
             nearest valid counts: lower={lower_valid:?}, \
             upper={upper_valid:?}"
        )));
    }

    if width > d_ff {
        return Err(GgufSliceError::BadPlan(format!(
            "kept width {width} exceeds original width {d_ff}"
        )));
    }

    Ok(width)
}

/// Return the smallest neuron count greater than or equal to `wanted`
/// that is a multiple of 32 and no greater than `d_ff`.
///
/// Returns `None` when no positive valid width exists. This helper
/// performs no model validation and does not establish model quality.
#[must_use]
pub fn round_up_to_block(wanted: usize, d_ff: usize) -> Option<usize> {
    if wanted == 0 {
        return None;
    }

    let quotient = wanted / BLOCK;
    let remainder = wanted % BLOCK;

    let blocks = if remainder == 0 {
        quotient
    } else {
        quotient.checked_add(1)?
    };

    let result = blocks.checked_mul(BLOCK)?;

    if result <= d_ff {
        Some(result)
    } else {
        None
    }
}

/// Slice the SwiGLU feed-forward blocks of a llama or qwen2 GGUF model.
///
/// Gate and up weights retain their original quantised bytes and tensor
/// types. Down weights are dequantised, selected by neuron, and stored
/// using the requested output type. Other tensors and metadata are
/// preserved, except for the documented feed-forward metadata changes.
///
/// Only dense llama and qwen2 layouts described by this module are
/// accepted. Models containing expert tensors are refused rather than
/// guessed at. The operation does not prove that the resulting model
/// retains its original behaviour or quality. The original
/// `general.file_type` metadata is retained but is no longer exact.
///
/// A successful output parse is a structural self-check, not a proof
/// that every tensor or model-level invariant is semantically correct.
///
/// # Errors
/// A [`GgufSliceError`] for an unreadable or unsupported model, a bad plan, an unsupported
/// tensor type, or an arithmetic overflow.
pub fn slice_gguf(
    file: &[u8],
    keep: &[Vec<usize>],
    down: DownType,
) -> Result<SliceOutcome, GgufSliceError> {
    let gguf = parse(file)
        .map_err(|e| GgufSliceError::Unreadable(format!("input parse failed: {e:?}")))?;

    let architecture = gguf.architecture().map_err(|e| {
        GgufSliceError::UnsupportedModel(format!("architecture metadata missing: {e:?}"))
    })?;

    if architecture != "llama" && architecture != "qwen2" {
        return Err(GgufSliceError::UnsupportedModel(format!(
            "architecture {architecture:?} is not supported; \
             expected llama or qwen2"
        )));
    }

    if is_moe(&gguf, architecture) {
        return Err(GgufSliceError::UnsupportedModel(
            "mixture-of-experts models are not supported".to_owned(),
        ));
    }

    let layers = metadata_width(&gguf, &format!("{architecture}.block_count"))?;

    let d_model = metadata_width(&gguf, &format!("{architecture}.embedding_length"))?;

    let ff_key = format!("{architecture}.feed_forward_length");
    let d_ff = metadata_width(&gguf, &ff_key)?;

    if layers == 0 || d_model == 0 || d_ff == 0 {
        return Err(GgufSliceError::UnsupportedModel(
            "layer count and model dimensions must be non-zero".to_owned(),
        ));
    }

    let k = validate_plan(keep, layers, d_ff)?;

    let d_model_u64 = u64::try_from(d_model)
        .map_err(|_| GgufSliceError::ArithmeticOverflow("d_model metadata".to_owned()))?;

    let d_ff_u64 = u64::try_from(d_ff)
        .map_err(|_| GgufSliceError::ArithmeticOverflow("d_ff metadata".to_owned()))?;

    let k_u64 = u64::try_from(k)
        .map_err(|_| GgufSliceError::ArithmeticOverflow("kept width metadata".to_owned()))?;

    let mut replacements: Vec<Replacement> =
        Vec::with_capacity(checked_mul(layers, 3, "replacement tensor count")?);

    for layer in 0..layers {
        let indices = keep.get(layer).ok_or_else(|| {
            GgufSliceError::BadPlan(format!("missing selection for layer {layer}"))
        })?;

        let gate_name = format!("blk.{layer}.ffn_gate.weight");
        let up_name = format!("blk.{layer}.ffn_up.weight");
        let down_name = format!("blk.{layer}.ffn_down.weight");

        let (gate_ty, gate_data, gate_row_size) = validate_matrix(
            &gguf,
            file,
            &gate_name,
            &[d_model_u64, d_ff_u64],
            d_ff,
            d_model,
        )?;

        let (up_ty, up_data, up_row_size) = validate_matrix(
            &gguf,
            file,
            &up_name,
            &[d_model_u64, d_ff_u64],
            d_ff,
            d_model,
        )?;

        let (down_ty, down_data, _) = validate_matrix(
            &gguf,
            file,
            &down_name,
            &[d_ff_u64, d_model_u64],
            d_model,
            d_ff,
        )?;

        let new_gate = select_rows(&gate_data, gate_row_size, indices, &gate_name)?;

        let new_up = select_rows(&up_data, up_row_size, indices, &up_name)?;

        let (new_down_ty, new_down) = slice_down(
            down_ty, &down_data, d_model, d_ff, indices, down, &down_name,
        )?;

        replacements.push((gate_name, vec![d_model_u64, k_u64], gate_ty, new_gate));

        replacements.push((up_name, vec![d_model_u64, k_u64], up_ty, new_up));

        replacements.push((down_name, vec![k_u64, d_model_u64], new_down_ty, new_down));
    }

    let mut replacement_iter = replacements.into_iter();

    let mut writer = GgufWriter::new();

    let alignment = checked_u32(gguf.alignment, "GGUF alignment")?;

    if alignment == 0 {
        return Err(GgufSliceError::UnsupportedModel(
            "GGUF alignment must be non-zero".to_owned(),
        ));
    }

    writer.set_alignment(alignment);

    let k_u32 = u32::try_from(k).map_err(|_| {
        GgufSliceError::ArithmeticOverflow("feed_forward_length metadata".to_owned())
    })?;

    for (key, value) in gguf.metadata() {
        if key == ff_key {
            writer.add_metadata(key, Value::U32(k_u32));
        } else {
            writer.add_metadata(key, value.clone());
        }
    }

    writer.add_metadata(
        "mouli.slice.source_feed_forward_length",
        Value::U32(u32::try_from(d_ff).map_err(|_| {
            GgufSliceError::ArithmeticOverflow("source feed-forward metadata".to_owned())
        })?),
    );

    let note = match down {
        DownType::F16 => "sliced by MOULI; ffn_down re-stored as F16; quality not verified",
        DownType::Q8_0 => "sliced by MOULI; ffn_down re-stored as Q8_0; quality not verified",
    };

    writer.add_metadata("mouli.slice.note", Value::String(note.to_owned()));

    for tensor in gguf.tensors() {
        let is_mlp = tensor.name.starts_with("blk.")
            && (tensor.name.ends_with(".ffn_gate.weight")
                || tensor.name.ends_with(".ffn_up.weight")
                || tensor.name.ends_with(".ffn_down.weight"));

        if is_mlp {
            let (name, dims, ty, data) = replacement_iter.next().ok_or_else(|| {
                GgufSliceError::UnsupportedModel(format!(
                    "missing replacement for tensor {}",
                    tensor.name
                ))
            })?;

            if name != tensor.name {
                return Err(GgufSliceError::UnsupportedModel(format!(
                    "replacement order mismatch: expected {}, got {name}",
                    tensor.name
                )));
            }

            writer.add_tensor(&name, &dims, ty, data).map_err(|e| {
                GgufSliceError::Unreadable(format!("cannot write tensor {name}: {e:?}"))
            })?;
        } else {
            let data = gguf
                .tensor_data(file, &tensor.name)
                .map_err(|e| {
                    GgufSliceError::Unreadable(format!(
                        "cannot read unchanged tensor {}: {e:?}",
                        tensor.name
                    ))
                })?
                .to_vec();

            writer
                .add_tensor(&tensor.name, &tensor.dims, tensor.ty, data)
                .map_err(|e| {
                    GgufSliceError::Unreadable(format!(
                        "cannot write unchanged tensor {}: {e:?}",
                        tensor.name
                    ))
                })?;
        }
    }

    if replacement_iter.next().is_some() {
        return Err(GgufSliceError::UnsupportedModel(
            "unused replacement tensors remain".to_owned(),
        ));
    }

    let output = writer
        .finish()
        .map_err(|e| GgufSliceError::Unreadable(format!("GGUF serialization failed: {e:?}")))?;

    parse(&output)
        .map_err(|e| GgufSliceError::Unreadable(format!("output self-check failed: {e:?}")))?;

    Ok(SliceOutcome {
        bytes_before: file.len(),
        bytes_after: output.len(),
        bytes: output,
        layers,
        d_ff_before: d_ff,
        d_ff_after: k,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use mouli_llm::gguf::parse as parse_gguf;

    fn rng(state: &mut u64) -> u32 {
        *state ^= *state << 13;
        *state ^= *state >> 7;
        *state ^= *state << 17;
        (*state >> 16) as u32
    }

    fn random_values(state: &mut u64, count: usize) -> Vec<f32> {
        (0..count)
            .map(|_| {
                let value = rng(state) % 20001;
                (value as f32 - 10000.0) / 10000.0
            })
            .collect()
    }

    fn f32_bytes(values: &[f32]) -> Vec<u8> {
        values.iter().flat_map(|v| v.to_le_bytes()).collect()
    }

    fn q8_bytes(values: &[f32]) -> Vec<u8> {
        let mut output = Vec::new();

        for block in values.as_chunks::<32>().0 {
            let max_abs = block.iter().fold(0.0f32, |a, b| a.max(b.abs()));

            let scale = max_abs / 127.0;
            output.extend_from_slice(&f32_to_f16(scale).to_le_bytes());

            for &value in block {
                let q = if scale == 0.0 {
                    0
                } else {
                    (value / scale).round().clamp(-127.0, 127.0) as i8
                };

                output.push(q as u8);
            }
        }

        output
    }

    fn build_model(quantized: bool) -> Vec<u8> {
        let mut writer = GgufWriter::new();
        writer.set_alignment(32);

        writer.add_metadata("general.architecture", Value::String("llama".to_owned()));
        writer.add_metadata("llama.block_count", Value::U32(2));
        writer.add_metadata("llama.embedding_length", Value::U32(32));
        writer.add_metadata("llama.feed_forward_length", Value::U32(64));
        writer.add_metadata("llama.context_length", Value::U32(128));
        writer.add_metadata("general.name", Value::String("test-model".to_owned()));

        let mut state = 0x1234_5678_9abc_def0;

        let ty = if quantized {
            GgmlType::Q8_0
        } else {
            GgmlType::F32
        };

        for name in ["token_embd.weight", "output_norm.weight"] {
            let values = random_values(&mut state, 32);

            writer
                .add_tensor(name, &[32], GgmlType::F32, f32_bytes(&values))
                .unwrap();
        }

        for layer in 0..2 {
            for suffix in ["ffn_gate.weight", "ffn_up.weight", "ffn_down.weight"] {
                let dimensions = if suffix == "ffn_down.weight" {
                    vec![64, 32]
                } else {
                    vec![32, 64]
                };

                let values = random_values(&mut state, 64 * 32);

                let data = if quantized {
                    q8_bytes(&values)
                } else {
                    f32_bytes(&values)
                };

                writer
                    .add_tensor(&format!("blk.{layer}.{suffix}"), &dimensions, ty, data)
                    .unwrap();
            }
        }

        writer.finish().unwrap()
    }

    fn selection() -> Vec<Vec<usize>> {
        vec![(0..32).collect(), (32..64).collect()]
    }

    fn decode_tensor(file: &[u8], name: &str) -> Vec<f32> {
        let gguf = parse_gguf(file).unwrap();
        let tensor = gguf.tensor(name).unwrap();
        let data = gguf.tensor_data(file, name).unwrap();

        dequantize_vec(tensor.ty, data, tensor.n_elements as usize).unwrap()
    }

    #[test]
    fn round_up_edges() {
        assert_eq!(round_up_to_block(1, 64), Some(32));
        assert_eq!(round_up_to_block(32, 64), Some(32));
        assert_eq!(round_up_to_block(33, 64), Some(64));
        assert_eq!(round_up_to_block(64, 64), Some(64));
        assert_eq!(round_up_to_block(65, 64), None);
        assert_eq!(round_up_to_block(0, 64), None);
        assert_eq!(round_up_to_block(usize::MAX, usize::MAX), None);
        assert_eq!(round_up_to_block(1, 31), None);
    }

    #[test]
    fn f16_conversion() {
        assert_eq!(f32_to_f16(0.0), 0x0000);
        assert_eq!(f32_to_f16(-0.0), 0x8000);
        assert_eq!(f32_to_f16(1.0), 0x3c00);
        assert_eq!(f32_to_f16(65504.0), 0x7bff);
        assert_eq!(f32_to_f16(70000.0), 0x7c00);
        assert_eq!(f32_to_f16(2f32.powi(-24)), 0x0001);

        assert!(f16_to_f32(f32_to_f16(f32::NAN)).is_nan());

        assert!((f16_to_f32(f32_to_f16(1.0)) - 1.0).abs() < f32::EPSILON);

        assert_eq!(f16_to_f32(f32_to_f16(-0.0)).to_bits(), (-0.0f32).to_bits());
    }

    #[test]
    fn f16_rounding_ties_to_even() {
        let even = 1.0f32 + 2f32.powi(-11);
        let odd = 1.0f32 + 3.0 * 2f32.powi(-11);

        assert_eq!(f32_to_f16(even), 0x3c00);
        assert_eq!(f32_to_f16(odd), 0x3c02);
    }

    #[test]
    fn basic_f32_slice_and_metadata() {
        let file = build_model(false);
        let keep = selection();

        let outcome = slice_gguf(&file, &keep, DownType::F16).unwrap();

        assert_eq!(outcome.layers, 2);
        assert_eq!(outcome.d_ff_before, 64);
        assert_eq!(outcome.d_ff_after, 32);
        assert_eq!(outcome.bytes_before, file.len());
        assert_eq!(outcome.bytes_after, outcome.bytes.len());

        let output = parse_gguf(&outcome.bytes).unwrap();

        assert_eq!(output.get_u32("llama.feed_forward_length").unwrap(), 32);

        assert_eq!(
            output
                .get_u32("mouli.slice.source_feed_forward_length")
                .unwrap(),
            64
        );

        assert_eq!(
            output.get("mouli.slice.note"),
            Some(&Value::String(
                "sliced by MOULI; ffn_down re-stored as F16; quality not verified".to_owned()
            ))
        );

        assert_eq!(output.get_u32("llama.context_length").unwrap(), 128);

        assert_eq!(
            output.get("general.name"),
            Some(&Value::String("test-model".to_owned()))
        );
    }

    #[test]
    fn q8_input_runs_dequantization_path() {
        let file = build_model(true);
        let outcome = slice_gguf(&file, &selection(), DownType::F16).unwrap();

        let output = parse_gguf(&outcome.bytes).unwrap();

        assert_eq!(
            output.tensor("blk.0.ffn_down.weight").unwrap().ty,
            GgmlType::F16
        );

        assert_eq!(
            output.tensor("blk.0.ffn_gate.weight").unwrap().ty,
            GgmlType::Q8_0
        );
    }

    #[test]
    fn gate_and_up_rows_are_copied_exactly() {
        for quantized in [false, true] {
            let file = build_model(quantized);
            let source = parse_gguf(&file).unwrap();

            let outcome = slice_gguf(&file, &selection(), DownType::F16).unwrap();

            let output = parse_gguf(&outcome.bytes).unwrap();

            let row_size = if quantized { 34 } else { 32 * 4 };

            for layer in 0..2 {
                for suffix in ["ffn_gate.weight", "ffn_up.weight"] {
                    let name = format!("blk.{layer}.{suffix}");

                    let original = source.tensor_data(&file, &name).unwrap();

                    let sliced = output.tensor_data(&outcome.bytes, &name).unwrap();

                    let expected = if layer == 0 {
                        &original[..32 * row_size]
                    } else {
                        &original[32 * row_size..64 * row_size]
                    };

                    assert_eq!(sliced, expected);
                }
            }
        }
    }

    #[test]
    fn down_f16_values_match_selected_source() {
        let file = build_model(false);
        let source = parse_gguf(&file).unwrap();

        let outcome = slice_gguf(&file, &selection(), DownType::F16).unwrap();

        for layer in 0..2 {
            let name = format!("blk.{layer}.ffn_down.weight");

            let original = decode_tensor(&file, &name);
            let actual = decode_tensor(&outcome.bytes, &name);

            assert_eq!(actual.len(), 32 * 32);

            for row in 0..32 {
                for column in 0..32 {
                    let source_index = row * 64 + if layer == 0 { column } else { column + 32 };

                    let actual_index = row * 32 + column;

                    let expected = original[source_index];
                    let obtained = actual[actual_index];

                    let tolerance = expected.abs() / 1024.0 + 0.0001;

                    assert!(
                        (expected - obtained).abs() <= tolerance,
                        "source={expected}, output={obtained}"
                    );
                }
            }

            assert_eq!(source.tensor(&name).unwrap().dims, vec![64, 32]);
        }
    }

    #[test]
    fn down_q8_output_is_valid() {
        let file = build_model(false);

        let outcome = slice_gguf(&file, &selection(), DownType::Q8_0).unwrap();

        let output = parse_gguf(&outcome.bytes).unwrap();

        for layer in 0..2 {
            let name = format!("blk.{layer}.ffn_down.weight");
            let tensor = output.tensor(&name).unwrap();

            assert_eq!(tensor.ty, GgmlType::Q8_0);
            assert_eq!(tensor.dims, vec![32, 32]);
            assert_eq!(tensor.n_bytes, 32 * 34);
        }

        let values = decode_tensor(&outcome.bytes, "blk.0.ffn_down.weight");

        assert_eq!(values.len(), 1024);
        assert!(values.iter().all(|v| v.is_finite()));
    }

    #[test]
    fn down_q8_values_are_within_one_step_of_the_source() {
        let file = build_model(false);
        let outcome = slice_gguf(&file, &selection(), DownType::Q8_0).unwrap();

        let original = decode_tensor(&file, "blk.0.ffn_down.weight");
        let actual = decode_tensor(&outcome.bytes, "blk.0.ffn_down.weight");

        for row in 0..32 {
            let selected: Vec<f32> = (0..32).map(|c| original[row * 64 + c]).collect();
            let step = selected.iter().fold(0.0f32, |m, v| m.max(v.abs())) / 127.0;
            for (column, expected) in selected.iter().enumerate() {
                let obtained = actual[row * 32 + column];
                assert!(
                    (expected - obtained).abs() <= step * 0.51 + 0.001,
                    "row {row} col {column}: {expected} vs {obtained}"
                );
            }
        }
    }

    #[test]
    fn unrelated_tensors_and_order_are_preserved() {
        let file = build_model(false);
        let source = parse_gguf(&file).unwrap();

        let outcome = slice_gguf(&file, &selection(), DownType::F16).unwrap();

        let output = parse_gguf(&outcome.bytes).unwrap();

        assert_eq!(
            source.tensors().iter().map(|t| &t.name).collect::<Vec<_>>(),
            output.tensors().iter().map(|t| &t.name).collect::<Vec<_>>()
        );

        for name in ["token_embd.weight", "output_norm.weight"] {
            assert_eq!(
                source.tensor_data(&file, name).unwrap(),
                output.tensor_data(&outcome.bytes, name).unwrap()
            );
        }
    }

    #[test]
    fn keeping_all_neurons() {
        let file = build_model(false);
        let keep = vec![(0..64).collect(), (0..64).collect()];

        let outcome = slice_gguf(&file, &keep, DownType::F16).unwrap();

        let original = decode_tensor(&file, "blk.0.ffn_down.weight");

        let actual = decode_tensor(&outcome.bytes, "blk.0.ffn_down.weight");

        for (a, b) in original.iter().zip(actual.iter()) {
            assert!((a - b).abs() < 0.001);
        }
    }

    #[test]
    fn rejects_unequal_widths() {
        let file = build_model(false);

        let keep = vec![(0..32).collect(), (0..64).collect()];

        let error = slice_gguf(&file, &keep, DownType::F16).unwrap_err();

        assert!(error.to_string().contains("unequal widths"));
    }

    #[test]
    fn rejects_invalid_width() {
        let file = build_model(false);

        let keep = vec![(0..33).collect(), (0..33).collect()];

        let error = slice_gguf(&file, &keep, DownType::F16).unwrap_err();

        let message = error.to_string();

        assert!(message.contains("multiple of 32"));
        assert!(message.contains("lower"));
        assert!(message.contains("upper"));
    }

    #[test]
    fn rejects_unsorted_duplicate_and_out_of_range() {
        let file = build_model(false);

        let mut unsorted: Vec<usize> = (0..32).collect();
        unsorted.swap(0, 1);
        let mut duplicate: Vec<usize> = (0..32).collect();
        duplicate[2] = 1;
        let mut out_of_range: Vec<usize> = (0..32).collect();
        out_of_range[31] = 64;

        for first in [unsorted, duplicate, out_of_range] {
            let keep = vec![first, (0..32).collect()];
            assert!(slice_gguf(&file, &keep, DownType::F16).is_err());
        }
    }

    #[test]
    fn rejects_wrong_layer_count() {
        let file = build_model(false);

        let error = slice_gguf(&file, &[(0..32).collect()], DownType::F16).unwrap_err();

        assert!(error.to_string().contains("expected 2"));
    }

    #[test]
    fn rejects_moe() {
        let mut writer = GgufWriter::new();

        writer.add_metadata("general.architecture", Value::String("llama".to_owned()));
        writer.add_metadata("llama.block_count", Value::U32(0));
        writer.add_metadata("llama.embedding_length", Value::U32(32));
        writer.add_metadata("llama.feed_forward_length", Value::U32(64));
        writer.add_metadata("llama.expert_count", Value::U32(8));

        let file = writer.finish().unwrap();

        let error = slice_gguf(&file, &[], DownType::F16).unwrap_err();

        assert!(error.to_string().contains("expert"));
    }

    #[test]
    fn rejects_unknown_architecture() {
        let mut writer = GgufWriter::new();

        writer.add_metadata("general.architecture", Value::String("unknown".to_owned()));

        let file = writer.finish().unwrap();

        let error = slice_gguf(&file, &[], DownType::F16).unwrap_err();

        assert!(error.to_string().contains("architecture"));
    }

    #[test]
    fn rejects_missing_tensor() {
        let mut writer = GgufWriter::new();

        writer.add_metadata("general.architecture", Value::String("llama".to_owned()));
        writer.add_metadata("llama.block_count", Value::U32(1));
        writer.add_metadata("llama.embedding_length", Value::U32(32));
        writer.add_metadata("llama.feed_forward_length", Value::U32(64));

        let file = writer.finish().unwrap();

        let error = slice_gguf(&file, &[(0..32).collect()], DownType::F16).unwrap_err();

        assert!(error.to_string().contains("missing"));
    }

    #[test]
    fn truncated_and_garbage_files_are_errors_not_panics() {
        let file = build_model(false);

        for length in [0, 1, 4, 8, file.len() / 2] {
            assert!(slice_gguf(&file[..length], &selection(), DownType::F16).is_err());
        }

        assert!(slice_gguf(b"not a GGUF model", &selection(), DownType::F16).is_err());
    }

    #[test]
    fn deterministic_output() {
        let file = build_model(false);
        let keep = selection();

        let first = slice_gguf(&file, &keep, DownType::Q8_0).unwrap();

        let second = slice_gguf(&file, &keep, DownType::Q8_0).unwrap();

        assert_eq!(first.bytes, second.bytes);
    }

    #[test]
    fn fuzz_plans_and_shapes() {
        let mut state = 0xfeed_cafe_dead_beef;
        let file = build_model(false);

        for _case in 0..120 {
            // The fixture has d_ff 64, so the valid widths are 32 and 64.
            let k = 32 * (1 + (rng(&mut state) as usize % 2));

            let mut keep = Vec::new();

            for _layer in 0..2 {
                let mut indices: Vec<usize> = (0..64).collect();

                for i in (1..indices.len()).rev() {
                    let j = rng(&mut state) as usize % (i + 1);
                    indices.swap(i, j);
                }

                indices.truncate(k);
                indices.sort_unstable();
                keep.push(indices);
            }

            let down = if rng(&mut state).is_multiple_of(2) {
                DownType::F16
            } else {
                DownType::Q8_0
            };

            let outcome = slice_gguf(&file, &keep, down).unwrap();

            let output = parse_gguf(&outcome.bytes).unwrap();

            assert_eq!(
                output.get_u32("llama.feed_forward_length").unwrap() as usize,
                k
            );

            for layer in 0..2 {
                let dims = |suffix: &str| {
                    output
                        .tensor(&format!("blk.{layer}.{suffix}"))
                        .unwrap()
                        .dims
                        .clone()
                };

                assert_eq!(dims("ffn_gate.weight"), vec![32, k as u64]);
                assert_eq!(dims("ffn_up.weight"), vec![32, k as u64]);
                assert_eq!(dims("ffn_down.weight"), vec![k as u64, 32]);
            }
        }
    }
}
