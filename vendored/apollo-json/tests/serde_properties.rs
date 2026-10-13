//! Property-based tests for the typed serde surface: agreement with
//! `serde_json` on generated derive types and arbitrary documents,
//! `to_value`/`from_value` round-trips, numeric boundaries, and
//! error-class parity with `serde_json`.

// Each property runs dozens of generated cases; under Miri that multiplies
// interpreter overhead into hours. The typed serde paths are covered under
// Miri by the crate's example-based serde suites, so this suite is
// native-only.
#![cfg(not(miri))]

use std::collections::BTreeMap;

use apollo_json::{JsonError, from_slice, from_value, to_value};
use proptest::prelude::*;
use serde::{Deserialize, Serialize};

mod common;
use common::{arb_json_with, arb_text};

// --- the derive menagerie ---------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Point {
    x: i32,
    y: i32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Wrapped(String);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
enum Shape {
    Empty,
    Circle(u32),
    Segment(i64, i64),
    Rect { w: u32, h: u32 },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
enum Event {
    Ping { seq: u64 },
    Note { text: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
enum Loose {
    Num(i64),
    Text(String),
    List(Vec<bool>),
}

/// One struct exercising the breadth of the derive data model: every integer
/// width class, floats, chars, options, sequences, maps, tuples, nested
/// structs, newtypes, and all four enum variant shapes across externally
/// tagged, internally tagged, and untagged representations.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Menagerie {
    id: u64,
    delta: i64,
    tiny: u8,
    ratio: f64,
    flag: bool,
    letter: char,
    name: String,
    nickname: Option<String>,
    tags: Vec<String>,
    scores: BTreeMap<String, i64>,
    pair: (bool, String),
    point: Point,
    wrapped: Wrapped,
    shape: Shape,
    event: Event,
    loose: Loose,
}

// --- generators -------------------------------------------------------------

/// Finite floats with the representation edges pinned: signed zeros, the
/// largest and smallest normals, and the smallest subnormal.
fn arb_finite_f64() -> impl Strategy<Value = f64> {
    prop_oneof![
        any::<f64>().prop_filter("JSON has no non-finite numbers", |f| f.is_finite()),
        Just(0.0),
        Just(-0.0),
        Just(f64::MAX),
        Just(f64::MIN),
        Just(f64::MIN_POSITIVE),
        Just(5e-324),
    ]
}

fn arb_i64() -> impl Strategy<Value = i64> {
    prop_oneof![any::<i64>(), Just(i64::MIN), Just(i64::MAX)]
}

/// Unsigned values weighted toward the region above `i64::MAX`, where the
/// u64/i64 dispatch split lives.
fn arb_u64() -> impl Strategy<Value = u64> {
    prop_oneof![
        any::<u64>(),
        Just(u64::MAX),
        Just(i64::MAX as u64 + 1),
        Just(0),
    ]
}

fn arb_shape() -> impl Strategy<Value = Shape> {
    prop_oneof![
        Just(Shape::Empty),
        any::<u32>().prop_map(Shape::Circle),
        (arb_i64(), arb_i64()).prop_map(|(a, b)| Shape::Segment(a, b)),
        (any::<u32>(), any::<u32>()).prop_map(|(w, h)| Shape::Rect { w, h }),
    ]
}

fn arb_event() -> impl Strategy<Value = Event> {
    prop_oneof![
        arb_u64().prop_map(|seq| Event::Ping { seq }),
        arb_text().prop_map(|text| Event::Note { text }),
    ]
}

fn arb_loose() -> impl Strategy<Value = Loose> {
    prop_oneof![
        arb_i64().prop_map(Loose::Num),
        arb_text().prop_map(Loose::Text),
        prop::collection::vec(any::<bool>(), 0..4).prop_map(Loose::List),
    ]
}

fn arb_menagerie() -> impl Strategy<Value = Menagerie> {
    (
        (
            arb_u64(),
            arb_i64(),
            any::<u8>(),
            arb_finite_f64(),
            any::<bool>(),
            any::<char>(),
        ),
        (
            arb_text(),
            prop::option::of(arb_text()),
            prop::collection::vec(arb_text(), 0..4),
            prop::collection::btree_map(arb_text(), arb_i64(), 0..4),
            (any::<bool>(), arb_text()),
        ),
        (
            (any::<i32>(), any::<i32>()),
            arb_text(),
            arb_shape(),
            arb_event(),
            arb_loose(),
        ),
    )
        .prop_map(
            |(
                (id, delta, tiny, ratio, flag, letter),
                (name, nickname, tags, scores, pair),
                ((x, y), wrapped, shape, event, loose),
            )| Menagerie {
                id,
                delta,
                tiny,
                ratio,
                flag,
                letter,
                name,
                nickname,
                tags,
                scores,
                pair,
                point: Point { x, y },
                wrapped: Wrapped(wrapped),
                shape,
                event,
                loose,
            },
        )
}

/// Arbitrary JSON documents in the same shape space as the sharing-algebra
/// property suite: full-char-space strings, i64s, bounded floats, nesting.
fn arb_json() -> impl Strategy<Value = serde_json::Value> {
    arb_json_with(prop_oneof![
        Just(serde_json::Value::Null),
        any::<bool>().prop_map(serde_json::Value::Bool),
        arb_i64().prop_map(|i| serde_json::Value::Number(i.into())),
        arb_u64().prop_map(|u| serde_json::Value::Number(u.into())),
        (-1e12f64..1e12f64).prop_map(|f| serde_json::json!(f)),
        arb_text().prop_map(serde_json::Value::String),
    ])
}

// --- properties -------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    /// Both stacks deserialize the serde_json spelling of a derive type to
    /// the same value, and that value is the one that was serialized.
    #[test]
    fn from_slice_agrees_with_serde_json_on_derive_types(input in arb_menagerie()) {
        let bytes = serde_json::to_vec(&input).expect("derive types serialize");

        let ours: Menagerie = from_slice(&bytes).expect("apollo-json deserializes");
        let reference: Menagerie =
            serde_json::from_slice(&bytes).expect("serde_json reparses its own output");

        prop_assert_eq!(&ours, &reference);
        prop_assert_eq!(&ours, &input);
    }

    #[test]
    fn derive_types_round_trip_through_a_document(input in arb_menagerie()) {
        let doc = to_value(&input).expect("derive types build a document");
        let back: Menagerie = from_value(&doc).expect("built document deserializes");
        prop_assert_eq!(back, input);
    }

    /// On arbitrary documents, `from_slice::<serde_json::Value>` through the
    /// arena deserializer produces exactly what serde_json's own parser does.
    #[test]
    fn arbitrary_documents_parse_identically_in_both_stacks(model in arb_json()) {
        let bytes = serde_json::to_vec(&model).expect("model serializes");

        let ours: serde_json::Value = from_slice(&bytes).expect("apollo-json deserializes");
        let reference: serde_json::Value =
            serde_json::from_slice(&bytes).expect("serde_json reparses its own output");

        prop_assert_eq!(&ours, &reference);
        prop_assert_eq!(&ours, &model);
    }

    /// Arbitrary JSON values survive `to_value` → `from_value`
    /// unchanged, covering deep container nesting beyond the derive shapes.
    #[test]
    fn arbitrary_values_round_trip_through_a_document(model in arb_json()) {
        let doc = to_value(&model).expect("values build a document");
        let back: serde_json::Value = from_value(&doc).expect("built document deserializes");
        prop_assert_eq!(back, model);
    }

    /// Floats round-trip bit-exactly through both directions: JSON text
    /// produced by serde_json, and a document built by `to_value`.
    #[test]
    fn floats_round_trip_bit_exact(f in arb_finite_f64()) {
        let bytes = serde_json::to_vec(&f).expect("finite floats serialize");
        let from_text: f64 = from_slice(&bytes).expect("float literal deserializes");
        prop_assert_eq!(from_text.to_bits(), f.to_bits());

        let doc = to_value(&f).expect("floats build a document");
        let from_doc: f64 = from_value(&doc).expect("built document deserializes");
        prop_assert_eq!(from_doc.to_bits(), f.to_bits());
    }
}

// --- boundaries -------------------------------------------------------------

#[test]
fn integer_boundaries_deserialize_exactly_in_both_stacks() {
    let cases: &[(&str, i128)] = &[
        ("-9223372036854775808", i64::MIN as i128),
        ("9223372036854775807", i64::MAX as i128),
        ("18446744073709551615", u64::MAX as i128),
    ];
    for (literal, expected) in cases {
        let ours: serde_json::Value = from_slice(literal.as_bytes()).expect("literal parses");
        let reference: serde_json::Value =
            serde_json::from_str(literal).expect("literal parses in serde_json");
        assert_eq!(ours, reference, "{literal}");
        assert_eq!(
            from_slice::<i128>(literal.as_bytes()).expect("fits i128"),
            *expected
        );
    }

    assert_eq!(
        from_slice::<i64>(b"-9223372036854775808").expect("fits i64"),
        i64::MIN
    );
    assert_eq!(
        from_slice::<u64>(b"18446744073709551615").expect("fits u64"),
        u64::MAX
    );
}

#[test]
fn negative_zero_reads_as_a_float_in_both_stacks() {
    // serde_json reads `-0` as the float -0.0 — an integer reading would
    // silently drop the sign — so untagged selection, typed requests, and
    // reserialization must all see a float.
    let ours: serde_json::Value = from_slice(b"-0").expect("parses");
    let reference: serde_json::Value = serde_json::from_str("-0").expect("parses in serde_json");
    assert_eq!(ours, reference);
    assert!(ours.as_f64().expect("is a float").is_sign_negative());

    assert!(from_slice::<i64>(b"-0").is_err());
    assert!(serde_json::from_str::<i64>("-0").is_err());
}

#[test]
fn integer_boundaries_round_trip_through_a_document() {
    let doc = to_value(&i64::MIN).expect("i64 builds");
    assert_eq!(from_value::<i64>(&doc).expect("i64 reads back"), i64::MIN);

    let doc = to_value(&u64::MAX).expect("u64 builds");
    assert_eq!(from_value::<u64>(&doc).expect("u64 reads back"), u64::MAX);

    let doc = to_value(&i128::MIN).expect("i128 builds");
    assert_eq!(
        from_value::<i128>(&doc).expect("i128 reads back"),
        i128::MIN
    );

    let doc = to_value(&u128::MAX).expect("u128 builds");
    assert_eq!(
        from_value::<u128>(&doc).expect("u128 reads back"),
        u128::MAX
    );
}

#[test]
fn float_literals_parse_identically_to_serde_json() {
    // Correct-rounding stress literals: values near the subnormal boundary,
    // the extremes, and shortest-representation decimals.
    let literals = [
        "0.1",
        "2.2250738585072014e-308",
        "2.2250738585072011e-308",
        "5e-324",
        "1.7976931348623157e308",
        "-1.7976931348623157e308",
        "122.41646246623264",
        "1e-323",
        "0.30000000000000004",
        "2.225073858507201e-308",
        "-0",
        "-0.0",
    ];
    for literal in literals {
        let ours: f64 = from_slice(literal.as_bytes()).expect("literal deserializes");
        let reference: f64 = serde_json::from_str(literal).expect("literal parses in serde_json");
        assert_eq!(ours.to_bits(), reference.to_bits(), "{literal}");
    }
}

// --- error-class parity -----------------------------------------------------

#[derive(Debug, Deserialize)]
struct Strict {
    count: u32,
    label: String,
}

#[test]
fn well_formed_input_deserializes_in_both_stacks() {
    let input = br#"{"count":3,"label":"a"}"#;

    let ours: Strict = from_slice(input).expect("well-formed input deserializes");
    let reference: Strict = serde_json::from_slice(input).expect("serde_json deserializes");

    assert_eq!((ours.count, ours.label.as_str()), (3, "a"));
    assert_eq!((reference.count, reference.label.as_str()), (3, "a"));
}

#[test]
fn missing_field_is_a_data_error_in_both_stacks() {
    let input = br#"{"count":3}"#;

    let ours = from_slice::<Strict>(input).expect_err("missing field must fail");
    let reference = serde_json::from_slice::<Strict>(input).expect_err("missing field must fail");

    assert!(matches!(ours, JsonError::Deserialization { .. }), "{ours}");
    assert!(reference.is_data(), "{reference}");
}

#[test]
fn type_mismatch_is_a_data_error_in_both_stacks() {
    let input = br#"{"count":"three","label":"a"}"#;

    let ours = from_slice::<Strict>(input).expect_err("type mismatch must fail");
    let reference = serde_json::from_slice::<Strict>(input).expect_err("type mismatch must fail");

    assert!(matches!(ours, JsonError::Deserialization { .. }), "{ours}");
    assert!(reference.is_data(), "{reference}");
}

#[test]
fn trailing_garbage_is_a_syntax_error_in_both_stacks() {
    let input = br#"{"count":3,"label":"a"} x"#;

    let ours = from_slice::<Strict>(input).expect_err("trailing garbage must fail");
    let reference =
        serde_json::from_slice::<Strict>(input).expect_err("trailing garbage must fail");

    assert!(matches!(ours, JsonError::Syntax { .. }), "{ours}");
    assert!(reference.is_syntax(), "{reference}");
}
