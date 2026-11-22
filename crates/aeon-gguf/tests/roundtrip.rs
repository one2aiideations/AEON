//! Round-trip, malformed-input and mutation tests for the public API.

use aeon_gguf::{parse, parse_header_only, Array, GgmlType, GgufError, GgufWriter, Value};

const ALL_TYPES: [GgmlType; 20] = [
    GgmlType::F32,
    GgmlType::F16,
    GgmlType::Q4_0,
    GgmlType::Q4_1,
    GgmlType::Q5_0,
    GgmlType::Q5_1,
    GgmlType::Q8_0,
    GgmlType::Q8_1,
    GgmlType::Q2_K,
    GgmlType::Q3_K,
    GgmlType::Q4_K,
    GgmlType::Q5_K,
    GgmlType::Q6_K,
    GgmlType::Q8_K,
    GgmlType::I8,
    GgmlType::I16,
    GgmlType::I32,
    GgmlType::I64,
    GgmlType::F64,
    GgmlType::BF16,
];

/// A small valid file used as the base for malformed variants.
fn small_file() -> Vec<u8> {
    let mut w = GgufWriter::new();
    w.add_metadata("general.architecture", Value::String("llama".into()));
    w.add_metadata("llama.block_count", Value::U32(2));
    w.add_tensor("t0", &[4, 2], GgmlType::F32, vec![1; 32])
        .unwrap();
    w.add_tensor("t1", &[32], GgmlType::Q8_0, vec![2; 34])
        .unwrap();
    w.finish().unwrap()
}

fn le32(v: u32) -> [u8; 4] {
    v.to_le_bytes()
}
fn le64(v: u64) -> [u8; 8] {
    v.to_le_bytes()
}
fn s(x: &str) -> Vec<u8> {
    let mut v = le64(x.len() as u64).to_vec();
    v.extend_from_slice(x.as_bytes());
    v
}

/// Hand-built file: lets tests produce structures the writer refuses to.
struct Raw {
    version: u32,
    kv: Vec<Vec<u8>>,
    tensors: Vec<Vec<u8>>,
    tensor_count_override: Option<u64>,
    data_len: usize,
}

impl Raw {
    fn new() -> Self {
        Raw {
            version: 3,
            kv: vec![],
            tensors: vec![],
            tensor_count_override: None,
            data_len: 256,
        }
    }
    fn kv(mut self, key: &str, ty: u32, value: &[u8]) -> Self {
        let mut e = s(key);
        e.extend_from_slice(&le32(ty));
        e.extend_from_slice(value);
        self.kv.push(e);
        self
    }
    fn tensor(mut self, name: &str, dims: &[u64], ty: u32, offset: u64) -> Self {
        let mut e = s(name);
        e.extend_from_slice(&le32(dims.len() as u32));
        for d in dims {
            e.extend_from_slice(&le64(*d));
        }
        e.extend_from_slice(&le32(ty));
        e.extend_from_slice(&le64(offset));
        self.tensors.push(e);
        self
    }
    fn build(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&le32(0x4655_4747));
        out.extend_from_slice(&le32(self.version));
        out.extend_from_slice(&le64(
            self.tensor_count_override
                .unwrap_or(self.tensors.len() as u64),
        ));
        out.extend_from_slice(&le64(self.kv.len() as u64));
        for k in &self.kv {
            out.extend_from_slice(k);
        }
        for t in &self.tensors {
            out.extend_from_slice(t);
        }
        while out.len() % 32 != 0 {
            out.push(0);
        }
        out.extend(std::iter::repeat_n(0u8, self.data_len));
        out
    }
}

// ---------------------------------------------------------------- round trips

#[test]
fn every_value_type_round_trips() {
    let values = vec![
        ("v.u8", Value::U8(200)),
        ("v.i8", Value::I8(-100)),
        ("v.u16", Value::U16(60000)),
        ("v.i16", Value::I16(-30000)),
        ("v.u32", Value::U32(4_000_000_000)),
        ("v.i32", Value::I32(-2_000_000_000)),
        ("v.f32", Value::F32(-1.25)),
        ("v.bool_t", Value::Bool(true)),
        ("v.bool_f", Value::Bool(false)),
        ("v.str", Value::String("héllo wörld".into())),
        ("v.empty_str", Value::String(String::new())),
        ("v.u64", Value::U64(u64::MAX)),
        ("v.i64", Value::I64(i64::MIN)),
        ("v.f64", Value::F64(std::f64::consts::PI)),
        ("a.u8", Value::Array(Array::U8(vec![1, 2, 3]))),
        ("a.i8", Value::Array(Array::I8(vec![-1, 0, 1]))),
        ("a.u16", Value::Array(Array::U16(vec![1, 65535]))),
        ("a.i16", Value::Array(Array::I16(vec![-1, 32767]))),
        ("a.u32", Value::Array(Array::U32(vec![7, 8]))),
        ("a.i32", Value::Array(Array::I32(vec![-7, 8]))),
        ("a.f32", Value::Array(Array::F32(vec![0.5, -0.5]))),
        ("a.bool", Value::Array(Array::Bool(vec![true, false, true]))),
        (
            "a.str",
            Value::Array(Array::String(vec!["a".into(), "".into(), "ccc".into()])),
        ),
        ("a.u64", Value::Array(Array::U64(vec![u64::MAX]))),
        ("a.i64", Value::Array(Array::I64(vec![i64::MIN]))),
        ("a.f64", Value::Array(Array::F64(vec![1e300]))),
        ("a.empty", Value::Array(Array::U32(vec![]))),
        (
            "a.nested",
            Value::Array(Array::Array(vec![
                Array::U32(vec![1, 2]),
                Array::String(vec!["x".into()]),
                Array::Array(vec![Array::U8(vec![9])]),
            ])),
        ),
    ];
    let mut w = GgufWriter::new();
    for (k, v) in &values {
        w.add_metadata(k, v.clone());
    }
    let bytes = w.finish().unwrap();
    let g = parse(&bytes).unwrap();
    let got: Vec<(&str, &Value)> = g.metadata().collect();
    assert_eq!(got.len(), values.len());
    for ((k, v), (gk, gv)) in values.iter().zip(got) {
        assert_eq!(*k, gk);
        assert_eq!(v, gv, "key {k}");
    }
}

#[test]
fn large_vocabulary_round_trips() {
    let vocab: Vec<String> = (0..150_000).map(|i| format!("tok{i}")).collect();
    let mut w = GgufWriter::new();
    w.add_metadata(
        "tokenizer.ggml.tokens",
        Value::Array(Array::String(vocab.clone())),
    );
    let bytes = w.finish().unwrap();
    let g = parse(&bytes).unwrap();
    let got = g
        .get_array("tokenizer.ggml.tokens")
        .unwrap()
        .as_strings()
        .unwrap();
    assert_eq!(got.len(), 150_000);
    assert_eq!(got[0], "tok0");
    assert_eq!(got[149_999], "tok149999");
    assert_eq!(got, &vocab[..]);
}

#[test]
fn every_tensor_type_round_trips() {
    let mut w = GgufWriter::new();
    let mut expected = Vec::new();
    for (i, ty) in ALL_TYPES.iter().enumerate() {
        let dims = [ty.block_size() * 2, 3];
        let n = dims[0] * dims[1];
        let data: Vec<u8> = (0..ty.row_bytes(n).unwrap())
            .map(|b| (b as u8).wrapping_add(i as u8))
            .collect();
        w.add_tensor(&format!("t.{}", ty.name()), &dims, *ty, data.clone())
            .unwrap();
        expected.push((format!("t.{}", ty.name()), dims, *ty, data));
    }
    let bytes = w.finish().unwrap();
    let g = parse(&bytes).unwrap();
    assert_eq!(g.tensors().len(), ALL_TYPES.len());
    for (name, dims, ty, data) in expected {
        let t = g.tensor(&name).unwrap();
        assert_eq!(t.dims, dims);
        assert_eq!(t.ty, ty);
        assert_eq!(t.n_elements, dims[0] * dims[1]);
        assert_eq!(g.tensor_data(&bytes, &name).unwrap(), &data[..], "{name}");
    }
}

#[test]
fn alignment_64_round_trips() {
    let mut w = GgufWriter::new();
    w.set_alignment(64);
    w.add_metadata("general.architecture", Value::String("x".into()));
    for i in 0..5u8 {
        w.add_tensor(&format!("t{i}"), &[7], GgmlType::I8, vec![i; 7])
            .unwrap();
    }
    let bytes = w.finish().unwrap();
    let g = parse(&bytes).unwrap();
    assert_eq!(g.alignment, 64);
    assert_eq!(g.data_start % 64, 0);
    for i in 0..5u8 {
        let t = g.tensor(&format!("t{i}")).unwrap();
        assert_eq!(t.offset % 64, 0);
        assert_eq!(
            g.tensor_data(&bytes, &format!("t{i}")).unwrap(),
            &[i; 7][..]
        );
    }
}

#[test]
fn four_dimensional_tensor() {
    let mut w = GgufWriter::new();
    w.add_tensor(
        "t",
        &[2, 3, 4, 5],
        GgmlType::F32,
        vec![0; 2 * 3 * 4 * 5 * 4],
    )
    .unwrap();
    let g = parse(&w.finish().unwrap()).unwrap();
    assert_eq!(g.tensor("t").unwrap().n_elements, 120);
}

// ------------------------------------------------------------------ malformed

#[test]
fn truncation_at_every_byte_is_an_error_never_a_panic() {
    let bytes = small_file();
    assert!(parse(&bytes).is_ok());
    for len in 0..bytes.len() {
        assert!(
            parse(&bytes[..len]).is_err(),
            "truncated to {len} bytes parsed"
        );
        let _ = parse_header_only(&bytes[..len]);
    }
}

#[test]
fn raw_builder_baseline_is_valid() {
    let b = Raw::new()
        .kv("general.architecture", 8, &s("llama"))
        .tensor("t", &[4], 0, 0)
        .build();
    assert!(parse(&b).is_ok());
}

#[test]
fn bad_magic_and_versions() {
    let mut b = small_file();
    b[0..4].copy_from_slice(b"GGML");
    assert!(matches!(parse(&b), Err(GgufError::BadMagic(_))));
    for v in [1u32, 4] {
        let mut r = Raw::new();
        r.version = v;
        assert!(matches!(parse(&r.build()), Err(GgufError::UnsupportedVersion(x)) if x == v));
    }
}

#[test]
fn unknown_types_and_bad_bool() {
    assert!(matches!(
        parse(&Raw::new().kv("k", 99, &[0]).build()),
        Err(GgufError::UnknownValueType(99))
    ));
    assert!(matches!(
        parse(&Raw::new().tensor("t", &[4], 99, 0).build()),
        Err(GgufError::UnknownGgmlType(99))
    ));
    assert!(matches!(
        parse(&Raw::new().kv("k", 7, &[2]).build()),
        Err(GgufError::BadBool(2))
    ));
}

#[test]
fn duplicates() {
    let b = Raw::new().kv("k", 0, &[1]).kv("k", 0, &[2]).build();
    assert!(matches!(parse(&b), Err(GgufError::DuplicateKey(k)) if k == "k"));
    let b = Raw::new()
        .tensor("t", &[4], 0, 0)
        .tensor("t", &[4], 0, 32)
        .build();
    assert!(matches!(parse(&b), Err(GgufError::DuplicateTensor(n)) if n == "t"));
}

#[test]
fn dimension_errors() {
    assert!(matches!(
        parse(&Raw::new().tensor("t", &[], 0, 0).build()),
        Err(GgufError::BadDims { .. })
    ));
    assert!(matches!(
        parse(&Raw::new().tensor("t", &[1, 1, 1, 1, 1], 0, 0).build()),
        Err(GgufError::BadDims { .. })
    ));
    assert!(matches!(
        parse(&Raw::new().tensor("t", &[4, 0], 0, 0).build()),
        Err(GgufError::BadDims { .. })
    ));
    assert!(matches!(
        parse(&Raw::new().tensor("t", &[u64::MAX, 2], 24, 0).build()),
        Err(GgufError::Overflow)
    ));
    // 32-element blocks: 33 is not a multiple.
    assert!(matches!(
        parse(&Raw::new().tensor("t", &[33], 8, 0).build()),
        Err(GgufError::NotBlockAligned { .. })
    ));
}

#[test]
fn offset_errors() {
    assert!(matches!(
        parse(&Raw::new().tensor("t", &[4], 0, 16).build()),
        Err(GgufError::MisalignedOffset { .. })
    ));
    // two 16-byte tensors, the second starting inside the first? needs a 32-aligned start: use size 64.
    let b = Raw::new()
        .tensor("a", &[16], 0, 0)
        .tensor("b", &[4], 0, 32)
        .build();
    assert!(matches!(parse(&b), Err(GgufError::TensorOverlap { .. })));
    // past the end of the file
    let mut r = Raw::new().tensor("t", &[4], 0, 4096);
    r.data_len = 64;
    assert!(matches!(
        parse(&r.build()),
        Err(GgufError::TensorOutOfBounds { .. })
    ));
    // offset that overflows when added to the data start
    let b = Raw::new().tensor("t", &[4], 0, u64::MAX - 31).build();
    assert!(matches!(parse(&b), Err(GgufError::Overflow)));
}

#[test]
fn string_and_array_lengths_larger_than_the_file() {
    let mut b = Raw::new().build();
    // metadata count 1 with a key whose length claims far more than the file holds
    b[16..24].copy_from_slice(&le64(1));
    let mut bad = b[..24].to_vec();
    bad.extend_from_slice(&le64(1 << 40));
    bad.extend_from_slice(&[b'k'; 32]);
    assert!(parse(&bad).is_err());

    let mut arr = le32(4).to_vec();
    arr.extend_from_slice(&le64(1 << 40));
    assert!(matches!(
        parse(&Raw::new().kv("k", 9, &arr).build()),
        Err(GgufError::CountTooLarge { .. })
    ));

    let mut strarr = le32(8).to_vec();
    strarr.extend_from_slice(&le64(u64::MAX));
    assert!(matches!(
        parse(&Raw::new().kv("k", 9, &strarr).build()),
        Err(GgufError::CountTooLarge { .. })
    ));
}

#[test]
fn array_nesting_depth() {
    fn nested(levels: u32) -> Vec<u8> {
        let mut inner = le32(0).to_vec();
        inner.extend_from_slice(&le64(1));
        inner.push(9);
        for _ in 1..levels {
            let mut outer = le32(9).to_vec();
            outer.extend_from_slice(&le64(1));
            outer.extend_from_slice(&inner);
            inner = outer;
        }
        inner
    }
    assert!(parse(&Raw::new().kv("k", 9, &nested(4)).build()).is_ok());
    assert!(matches!(
        parse(&Raw::new().kv("k", 9, &nested(5)).build()),
        Err(GgufError::ArrayTooDeep)
    ));
}

#[test]
fn alignment_values() {
    for a in [0u32, 3] {
        let b = Raw::new().kv("general.alignment", 4, &le32(a)).build();
        assert!(
            matches!(parse(&b), Err(GgufError::BadAlignment(x)) if x == u64::from(a)),
            "alignment {a}"
        );
    }
    let b = Raw::new().kv("general.alignment", 8, &s("32")).build();
    assert!(matches!(parse(&b), Err(GgufError::WrongType { .. })));
    // wrong integer type for alignment is also rejected: it must be U32
    let b = Raw::new().kv("general.alignment", 10, &le64(32)).build();
    assert!(matches!(parse(&b), Err(GgufError::WrongType { .. })));
}

#[test]
fn invalid_utf8_and_empty_names() {
    let mut e = le64(2).to_vec();
    e.extend_from_slice(&[0xff, 0xfe]);
    e.extend_from_slice(&le32(0));
    e.push(1);
    let mut b = Raw::new().build();
    b[16..24].copy_from_slice(&le64(1));
    let mut bad = b[..24].to_vec();
    bad.extend_from_slice(&e);
    assert!(matches!(parse(&bad), Err(GgufError::BadString { .. })));

    assert!(matches!(
        parse(&Raw::new().kv("", 0, &[1]).build()),
        Err(GgufError::BadString { .. })
    ));
    assert!(matches!(
        parse(&Raw::new().tensor("", &[4], 0, 0).build()),
        Err(GgufError::BadString { .. })
    ));
    assert!(matches!(
        parse(&Raw::new().kv("clé", 0, &[1]).build()),
        Err(GgufError::BadString { .. })
    ));
}

#[test]
fn huge_header_counts() {
    let mut r = Raw::new();
    r.tensor_count_override = Some(u64::MAX);
    assert!(matches!(
        parse(&r.build()),
        Err(GgufError::CountTooLarge { what: "tensor", .. })
    ));
}

#[test]
fn header_only_skips_data_bounds_but_not_structure() {
    let bytes = small_file();
    let g = parse(&bytes).unwrap();
    let head = &bytes[..g.data_start as usize];
    assert!(parse(head).is_err());
    let h = parse_header_only(head).unwrap();
    assert!(h.header_only);
    assert_eq!(h.tensors().len(), 2);
    // overlaps are still detected without data
    let mut r = Raw::new().tensor("a", &[16], 0, 0).tensor("b", &[4], 0, 32);
    r.data_len = 0;
    assert!(matches!(
        parse_header_only(&r.build()),
        Err(GgufError::TensorOverlap { .. })
    ));
}

// ------------------------------------------------------------------- mutation

struct XorShift(u64);
impl XorShift {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

#[test]
fn random_byte_flips_never_panic() {
    let mut w = GgufWriter::new();
    w.add_metadata("general.architecture", Value::String("llama".into()));
    w.add_metadata(
        "tokenizer.ggml.tokens",
        Value::Array(Array::String((0..50).map(|i| format!("t{i}")).collect())),
    );
    w.add_metadata(
        "a.nested",
        Value::Array(Array::Array(vec![
            Array::U32(vec![1, 2]),
            Array::F32(vec![0.5]),
        ])),
    );
    w.add_tensor("t0", &[4, 2], GgmlType::F32, vec![1; 32])
        .unwrap();
    w.add_tensor("t1", &[32], GgmlType::Q8_0, vec![2; 34])
        .unwrap();
    let base = w.finish().unwrap();
    let mut rng = XorShift(0x9E37_79B9_7F4A_7C15);
    let (mut ok, mut err) = (0u32, 0u32);
    for _ in 0..10_000 {
        let mut b = base.clone();
        let flips = 1 + (rng.next() % 4) as usize;
        for _ in 0..flips {
            let i = (rng.next() % b.len() as u64) as usize;
            b[i] = (rng.next() & 0xff) as u8;
        }
        match parse(&b) {
            Ok(g) => {
                ok += 1;
                // anything accepted must be internally consistent
                for t in g.tensors() {
                    assert!(g.tensor_data(&b, &t.name).is_ok());
                }
            }
            Err(_) => err += 1,
        }
        let _ = parse_header_only(&b);
    }
    assert!(err > 0, "mutations never produced an error ({ok} ok)");
}

#[test]
fn random_garbage_never_panics() {
    let mut rng = XorShift(0x1234_5678_9ABC_DEF1);
    for len in [0usize, 1, 7, 24, 100, 1000] {
        for _ in 0..200 {
            let mut b: Vec<u8> = (0..len).map(|_| (rng.next() & 0xff) as u8).collect();
            if len >= 8 && rng.next().is_multiple_of(2) {
                b[0..4].copy_from_slice(b"GGUF");
                b[4..8].copy_from_slice(&3u32.to_le_bytes());
            }
            let _ = parse(&b);
            let _ = parse_header_only(&b);
        }
    }
}
