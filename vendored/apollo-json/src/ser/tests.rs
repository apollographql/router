use std::collections::BTreeMap;
use std::sync::Arc;

use serde::Serialize;

use crate::{JsonError, Value, ValueBuilder, to_value};

fn parse(input: &str) -> Value {
    Value::parse(input.as_bytes().to_vec()).expect("test input parses")
}

#[test]
fn documents_serialize_through_serde_preserving_key_order() {
    let doc = parse(r#"{"b":1,"a":[true,null,"x"],"nested":{"k":"v"}}"#);
    assert_eq!(
        serde_json::to_string(&doc).unwrap(),
        r#"{"b":1,"a":[true,null,"x"],"nested":{"k":"v"}}"#
    );
}

#[test]
fn numbers_normalize_at_serde_json_widths() {
    // Raw literal spellings cannot cross serde's data model: integers stay
    // integers at 64 bits, everything else re-formats as f64 — including
    // `-0`, which serde_json reads as the float -0.0 to keep its sign.
    let doc = parse(r#"[7,-3,1.50e2,18446744073709551615,18446744073709551616,-0]"#);
    assert_eq!(
        serde_json::to_string(&doc).unwrap(),
        "[7,-3,150.0,18446744073709551615,1.8446744073709552e+19,-0.0]"
    );
}

#[test]
fn strings_serialize_unescaped() {
    // The original escape spelling is decoded; the target serializer applies
    // its own escaping rules.
    let doc = parse(r#"{"s":"AA","t":"line\nbreak"}"#);
    assert_eq!(
        serde_json::to_string(&doc).unwrap(),
        r#"{"s":"AA","t":"line\nbreak"}"#
    );
}

#[test]
fn value_and_value_ref_serialize_the_subtree() {
    let doc = parse(r#"{"user":{"id":7}}"#);
    let user = doc.get("user").unwrap();
    assert_eq!(serde_json::to_string(&user).unwrap(), r#"{"id":7}"#);
    assert_eq!(
        serde_json::to_string(&doc.value().get("user").unwrap()).unwrap(),
        r#"{"id":7}"#
    );
}

#[test]
fn shared_subtrees_serialize_across_arenas() {
    let a = parse(r#"{"user":{"id":7}}"#);
    let b = parse(r#"{"labels":["x","y"]}"#);
    let mut builder = ValueBuilder::new();
    builder.set("user", a.get("user").unwrap()).unwrap();
    builder.set("labels", b.get("labels").unwrap()).unwrap();
    let composed = builder.seal();
    drop((a, b));

    assert_eq!(
        serde_json::to_value(&composed).unwrap(),
        serde_json::json!({"user":{"id":7},"labels":["x","y"]})
    );
}

#[test]
fn overflowing_number_literals_error() {
    let doc = parse("[1e999]");
    let error = serde_json::to_string(&doc).unwrap_err();
    assert!(error.to_string().contains("number out of range"), "{error}");
}

#[test]
fn serializing_compositions_deeper_than_the_parse_cap_errors_instead_of_recursing() {
    let mut doc = parse("[]");
    for _ in 0..200 {
        let mut builder = ValueBuilder::new();
        builder.set("inner", doc.clone()).unwrap();
        doc = builder.seal();
    }
    let error = serde_json::to_string(&doc).unwrap_err();
    assert!(
        error.to_string().contains("nesting depth exceeds"),
        "{error}"
    );
}

#[test]
fn to_value_maps_the_full_serde_data_model() {
    #[derive(Serialize)]
    struct Unit;

    #[derive(Serialize)]
    struct Wrapper(u8);

    #[derive(Serialize)]
    enum Shape {
        Unit,
        Newtype(u8),
        Tuple(u8, bool),
        Struct { x: u8 },
    }

    #[derive(Serialize)]
    struct Everything {
        text: String,
        character: char,
        none: Option<u8>,
        some: Option<u8>,
        unit: (),
        unit_struct: Unit,
        newtype: Wrapper,
        tuple: (u8, bool),
        list: Vec<i32>,
        variants: Vec<Shape>,
        big: i128,
        float: f64,
    }

    let doc = to_value(&Everything {
        text: "hi".into(),
        character: 'x',
        none: None,
        some: Some(1),
        unit: (),
        unit_struct: Unit,
        newtype: Wrapper(2),
        tuple: (3, true),
        list: vec![-1, 0, 1],
        variants: vec![
            Shape::Unit,
            Shape::Newtype(4),
            Shape::Tuple(5, false),
            Shape::Struct { x: 6 },
        ],
        big: i128::MAX,
        float: 1.5,
    })
    .unwrap();

    assert_eq!(
        doc.to_string(),
        r#"{"text":"hi","character":"x","none":null,"some":1,"unit":null,"unit_struct":null,"newtype":2,"tuple":[3,true],"list":[-1,0,1],"variants":["Unit",{"Newtype":4},{"Tuple":[5,false]},{"Struct":{"x":6}}],"big":170141183460469231731687303715884105727,"float":1.5}"#
    );
}

#[test]
fn to_value_coerces_map_keys_to_strings() {
    let mut by_int = BTreeMap::new();
    by_int.insert(-3i32, "a");
    by_int.insert(10, "b");
    let doc = to_value(&by_int).unwrap();
    assert_eq!(doc.to_vec(), br#"{"-3":"a","10":"b"}"#);

    let mut by_bool = BTreeMap::new();
    by_bool.insert(true, 1);
    let doc = to_value(&by_bool).unwrap();
    assert_eq!(doc.to_vec(), br#"{"true":1}"#);

    let mut by_unit = BTreeMap::new();
    by_unit.insert((), 1);
    let error = to_value(&by_unit).unwrap_err();
    assert!(
        error.to_string().contains("key must be a string"),
        "{error}"
    );
}

#[test]
fn to_value_collapses_duplicate_keys_like_parsing() {
    // First position, last value — the parser's duplicate-key semantics.
    struct Duplicates;

    impl Serialize for Duplicates {
        fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            use serde::ser::SerializeMap;
            let mut map = serializer.serialize_map(Some(3))?;
            map.serialize_entry("a", &1)?;
            map.serialize_entry("b", &2)?;
            map.serialize_entry("a", &3)?;
            map.end()
        }
    }

    let doc = to_value(&Duplicates).unwrap();
    assert_eq!(doc.to_vec(), br#"{"a":3,"b":2}"#);

    // Wide objects switch duplicate detection to the hashed index; the
    // semantics must not change across the threshold.
    struct WideDuplicates;

    impl Serialize for WideDuplicates {
        fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            use serde::ser::SerializeMap;
            let mut map = serializer.serialize_map(None)?;
            for i in 0..40 {
                map.serialize_entry(&format!("k{i}"), &i)?;
            }
            // One duplicate from each side of the index threshold.
            map.serialize_entry("k1", &101)?;
            map.serialize_entry("k39", &139)?;
            map.end()
        }
    }

    let doc = to_value(&WideDuplicates).unwrap();
    let root = doc.value();
    assert_eq!(root.len(), Some(40));
    assert_eq!(root.get("k1").and_then(|v| v.as_i64()), Some(101));
    assert_eq!(root.get("k39").and_then(|v| v.as_i64()), Some(139));
    assert_eq!(
        root.member_at(1)
            .map(|(key, _)| key.into_owned())
            .as_deref(),
        Some("k1"),
        "the first occurrence keeps its position"
    );
}

#[test]
fn to_value_writes_null_for_non_finite_floats() {
    // serde_json serializes NaN and the infinities as null — JSON has no
    // representation for them — and to_value matches.
    assert_eq!(to_value(&f64::NAN).unwrap().to_vec(), b"null");
    assert_eq!(to_value(&f64::NEG_INFINITY).unwrap().to_vec(), b"null");
    assert_eq!(
        to_value(&[1.0f32, f32::INFINITY]).unwrap().to_vec(),
        b"[1.0,null]"
    );
}

#[test]
fn to_value_rejects_non_finite_float_map_keys() {
    // A key has no null to fall back to; serde_json errors here too.
    struct NanKey;

    impl Serialize for NanKey {
        fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            use serde::ser::SerializeMap;
            let mut map = serializer.serialize_map(Some(1))?;
            map.serialize_entry(&f64::NAN, &1)?;
            map.end()
        }
    }

    let error = to_value(&NanKey).unwrap_err();
    assert!(matches!(error, JsonError::NonFiniteNumber), "{error}");
}

#[test]
fn to_value_serializes_bytes_as_number_arrays() {
    struct Blob;

    impl Serialize for Blob {
        fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            serializer.serialize_bytes(&[1, 2, 255])
        }
    }

    let doc = to_value(&Blob).unwrap();
    assert_eq!(doc.to_vec(), b"[1,2,255]");
}

#[test]
fn to_value_accepts_serde_json_raw_number_structs() {
    // With serde_json's arbitrary_precision feature enabled, its Number
    // serializes as a struct with a private marker name carrying the
    // literal; that literal must land as the document's raw number.
    struct ArbitraryPrecisionNumber(&'static str);

    impl Serialize for ArbitraryPrecisionNumber {
        fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            use serde::ser::SerializeStruct;
            let mut s = serializer.serialize_struct("$serde_json::private::Number", 1)?;
            s.serialize_field("$serde_json::private::Number", self.0)?;
            s.end()
        }
    }

    let doc = to_value(&ArbitraryPrecisionNumber("1.50e2")).unwrap();
    assert_eq!(doc.to_vec(), b"1.50e2");

    let error = to_value(&ArbitraryPrecisionNumber("not-a-number")).unwrap_err();
    assert!(
        error.to_string().contains("invalid JSON number literal"),
        "{error}"
    );
}

#[test]
fn value_fields_are_adopted_by_reference() {
    #[derive(Serialize)]
    struct Envelope {
        id: u64,
        payload: crate::Value,
    }

    let source = parse(r#"{"payload":{"n":1.50e2,"s":"A","list":[1,2]}}"#);
    let payload = source.get("payload").unwrap();
    let envelope = Envelope { id: 7, payload };
    let before = Arc::strong_count(&source.arena);

    let doc = to_value(&envelope).unwrap();

    // Adoption is one refcount bump on the source arena, never a copy. The
    // public observables (shared input bytes, raw literal spellings) are
    // asserted in tests/serde_capture.rs.
    assert_eq!(Arc::strong_count(&source.arena), before + 1);
    assert!(Arc::ptr_eq(
        &doc.get("payload").unwrap().arena,
        &source.arena
    ));
}

#[test]
fn root_value_fields_are_adopted_by_reference() {
    #[derive(Serialize)]
    struct Envelope {
        payload: crate::Value,
    }

    let payload = parse(r#"{"a":[true,null]}"#);
    let doc = to_value(&Envelope {
        payload: payload.clone(),
    })
    .unwrap();
    assert!(Arc::ptr_eq(
        &doc.get("payload").unwrap().arena,
        &payload.arena
    ));
    assert_eq!(doc.to_vec(), br#"{"payload":{"a":[true,null]}}"#);
}

#[test]
fn adopting_at_the_root_shares_the_source_document() {
    let source = parse(r#"{"n":1.50e2}"#);
    let doc = to_value(&source).unwrap();
    assert!(Arc::ptr_eq(&doc.arena, &source.arena));

    let handle = source.get("n").unwrap();
    let doc = to_value(&handle).unwrap();
    assert!(Arc::ptr_eq(&doc.arena, &source.arena));
    assert_eq!(doc.to_vec(), b"1.50e2");
}

#[test]
fn flattened_value_fields_still_adopt_by_reference() {
    // On the serialize side flatten forwards fields into the outer map
    // without buffering, so the hand-off crosses it intact.
    #[derive(Serialize)]
    struct Flattened {
        id: u64,
        #[serde(flatten)]
        rest: BTreeMap<String, crate::Value>,
    }

    let source = parse(r#"{"a":{"n":1.50e2}}"#);
    let mut rest = BTreeMap::new();
    rest.insert("a".to_owned(), source.get("a").unwrap());

    let doc = to_value(&Flattened { id: 1, rest }).unwrap();
    assert!(!doc.is_self_contained());
    assert_eq!(doc.to_vec(), br#"{"id":1,"a":{"n":1.50e2}}"#);
}

#[test]
fn enum_representations_do_not_break_adoption() {
    // Tagged enum representations forward their content on the serialize
    // side (serde only buffers when deserializing), so handles inside any
    // variant shape still adopt.
    #[derive(Serialize)]
    #[serde(tag = "t", content = "c")]
    enum Tagged {
        Payload { value: crate::Value },
    }

    let source = parse(r#"{"n":1.50e2}"#);
    let doc = to_value(&Tagged::Payload {
        value: source.clone(),
    })
    .unwrap();
    assert!(!doc.is_self_contained());
    assert_eq!(
        doc.to_vec(),
        br#"{"t":"Payload","c":{"value":{"n":1.50e2}}}"#
    );
}

#[test]
fn a_marker_request_without_a_stash_copies_the_content() {
    // A serializer wrapper that replays recorded output can hand the marker
    // name over without the thread-local stash; the replayed content must
    // land as a plain copy, never an error.
    struct Replayed;

    impl Serialize for Replayed {
        fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            serializer.serialize_newtype_struct(crate::handoff::TOKEN, &42u8)
        }
    }

    let doc = to_value(&Replayed).unwrap();
    assert!(doc.is_self_contained());
    assert_eq!(doc.to_vec(), b"42");
}

#[test]
fn value_refs_copy_structurally_into_to_value() {
    let source = parse(r#"{"n":1.50e2}"#);
    let doc = to_value(&source.value()).unwrap();
    // A borrowed view has no owning handle to adopt, so the copy normalizes
    // numbers like any structural serialization.
    assert!(doc.is_self_contained());
    assert_eq!(doc.to_vec(), br#"{"n":150.0}"#);
}

#[test]
fn typed_values_round_trip_through_a_document() {
    #[derive(Serialize, serde::Deserialize, PartialEq, Debug)]
    enum Shape {
        Unit,
        Newtype(u8),
        Tuple(u8, bool),
        Struct { x: u8 },
    }

    #[derive(Serialize, serde::Deserialize, PartialEq, Debug)]
    struct User {
        id: u64,
        name: String,
        nickname: Option<String>,
        tags: Vec<String>,
        shapes: Vec<Shape>,
        by_int: BTreeMap<i32, f64>,
        big: i128,
    }

    let user = User {
        id: 7,
        name: "ada".into(),
        nickname: None,
        tags: vec!["a".into(), "b".into()],
        shapes: vec![
            Shape::Unit,
            Shape::Newtype(1),
            Shape::Tuple(2, true),
            Shape::Struct { x: 3 },
        ],
        by_int: BTreeMap::from([(-3, 0.5), (10, 1.5)]),
        big: i128::MAX,
    };

    let doc = to_value(&user).unwrap();
    let back: User = crate::from_value(&doc).unwrap();
    assert_eq!(back, user);
}

#[test]
fn value_fields_round_trip_sharing_the_source_arena() {
    #[derive(Serialize, serde::Deserialize)]
    struct Envelope {
        id: u64,
        payload: crate::Value,
    }

    let source = parse(r#"{"payload":{"n":1.50e2,"list":[1,2]}}"#);
    let envelope = Envelope {
        id: 7,
        payload: source.get("payload").unwrap(),
    };

    let doc = to_value(&envelope).unwrap();
    let back: Envelope = crate::from_value(&doc).unwrap();

    assert_eq!(back.id, envelope.id);
    // The subtree crossed both directions by reference: the round-tripped
    // handle points into the original source arena, not a rebuilt copy.
    assert!(Arc::ptr_eq(&back.payload.arena, &source.arena));
    assert_eq!(back.payload.to_vec(), br#"{"n":1.50e2,"list":[1,2]}"#);
}

#[test]
fn captured_value_fields_round_trip_sharing_the_source_arena() {
    #[derive(Serialize, serde::Deserialize)]
    struct Envelope {
        payload: crate::Value,
    }

    let payload = parse(r#"{"a":[true,null]}"#);
    let doc = to_value(&Envelope {
        payload: payload.clone(),
    })
    .unwrap();
    let back: Envelope = crate::from_value(&doc).unwrap();
    assert!(Arc::ptr_eq(&back.payload.arena, &payload.arena));
    assert_eq!(back.payload.to_vec(), payload.to_vec());
}
