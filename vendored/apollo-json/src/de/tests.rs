//! Unit tests for the deserialization surface. Behavior observable through
//! the public API alone belongs in the integration suites (`tests/serde_*`);
//! tests live here only when they assert internals — arena refcounts and
//! identity, or the `handoff` mechanism.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde::Deserialize;

use crate::{JsonError, Value, ValueBuilder, from_slice, from_str, from_value};

fn parse(input: &str) -> Value {
    Value::parse(input.as_bytes().to_vec()).expect("test input parses")
}

#[test]
fn scalars_deserialize() {
    assert!(from_str::<bool>("true").unwrap());
    assert_eq!(from_str::<u8>("7").unwrap(), 7);
    assert_eq!(from_str::<i32>("-42").unwrap(), -42);
    assert_eq!(from_str::<u64>("18446744073709551615").unwrap(), u64::MAX);
    assert_eq!(from_str::<i64>("-9223372036854775808").unwrap(), i64::MIN);
    assert_eq!(from_str::<f64>("1.5e3").unwrap(), 1500.0);
    assert_eq!(from_str::<f32>("0.25").unwrap(), 0.25);
    assert_eq!(from_str::<char>("\"x\"").unwrap(), 'x');
    assert_eq!(from_str::<String>("\"hi\"").unwrap(), "hi");
    from_str::<()>("null").unwrap();
}

#[test]
fn integers_narrow_through_the_visitor() {
    let error = from_str::<u8>("300").unwrap_err();
    assert!(error.to_string().contains("invalid value"), "{error}");
    let error = from_str::<u32>("-1").unwrap_err();
    assert!(error.to_string().contains("invalid value"), "{error}");
    // A float literal is an invalid type for an integer, not out of range.
    let error = from_str::<i64>("1.0").unwrap_err();
    assert!(error.to_string().contains("invalid type"), "{error}");
}

#[test]
fn oversized_integers_fall_back_to_f64() {
    // One past u64::MAX: deserialize_any and f64 both read it as a float.
    assert_eq!(
        from_str::<f64>("18446744073709551616").unwrap(),
        1.8446744073709552e19
    );
    let error = from_str::<u64>("18446744073709551616").unwrap_err();
    assert!(error.to_string().contains("invalid type"), "{error}");
}

#[test]
fn explicit_128_bit_integers_parse_the_full_literal() {
    assert_eq!(
        from_str::<u128>("340282366920938463463374607431768211455").unwrap(),
        u128::MAX
    );
    assert_eq!(
        from_str::<i128>("-170141183460469231731687303715884105728").unwrap(),
        i128::MIN
    );
    let error = from_str::<u128>("-1").unwrap_err();
    assert!(error.to_string().contains("number out of range"), "{error}");
    // `-0` errors for i64 (the float -0.0 would drop its sign, see
    // tests/serde_properties.rs), but an explicit i128 request parses the
    // digits, as in serde_json.
    assert_eq!(from_str::<i128>("-0").unwrap(), 0);
}

#[test]
fn overflowing_float_literals_error() {
    let error = from_str::<f64>("1e999").unwrap_err();
    assert!(error.to_string().contains("number out of range"), "{error}");
    let error = from_str::<f32>("1e39").unwrap_err();
    assert!(error.to_string().contains("number out of range"), "{error}");
}

#[test]
fn escape_free_strings_borrow_the_arena() {
    #[derive(Deserialize)]
    struct Borrowing<'a> {
        name: &'a str,
    }

    let doc = parse(r#"{"name":"ada"}"#);
    let borrowing: Borrowing<'_> = from_value(&doc).unwrap();
    assert_eq!(borrowing.name, "ada");
    let input_range = doc.arena.input().as_ptr_range();
    assert!(
        input_range.contains(&borrowing.name.as_ptr()),
        "escape-free string should point into the document input"
    );
}

#[test]
fn escaped_strings_unescape_into_owned_text() {
    let doc = parse(r#"{"name":"a\nda"}"#);
    let map: BTreeMap<String, String> = from_value(&doc).unwrap();
    assert_eq!(map["name"], "a\nda");
}

#[test]
fn structs_deserialize_with_nesting_options_and_ignored_fields() {
    #[derive(Deserialize, Debug, PartialEq)]
    struct User {
        id: u64,
        name: String,
        nickname: Option<String>,
        tags: Vec<String>,
        address: Address,
    }

    #[derive(Deserialize, Debug, PartialEq)]
    struct Address {
        city: String,
    }

    let user: User = from_str(
        r#"{
            "id": 7,
            "name": "ada",
            "nickname": null,
            "tags": ["a", "b"],
            "ignored": {"deep": [1, 2, {"x": null}]},
            "address": {"city": "London"}
        }"#,
    )
    .unwrap();
    assert_eq!(
        user,
        User {
            id: 7,
            name: "ada".into(),
            nickname: None,
            tags: vec!["a".into(), "b".into()],
            address: Address {
                city: "London".into()
            },
        }
    );
}

#[test]
fn missing_field_reports_the_field_name() {
    #[derive(Deserialize, Debug)]
    struct User {
        #[expect(dead_code)]
        id: u64,
    }

    let error = from_str::<User>("{}").unwrap_err();
    assert!(error.to_string().contains("missing field `id`"), "{error}");
}

#[test]
fn tuples_enforce_length() {
    assert_eq!(from_str::<(u8, bool)>("[1,true]").unwrap(), (1, true));
    let error = from_str::<(u8, bool)>("[1,true,3]").unwrap_err();
    assert!(
        error.to_string().contains("fewer elements in array"),
        "{error}"
    );
    assert!(from_str::<(u8, bool)>("[1]").is_err());
}

#[test]
fn map_keys_coerce_numbers_and_bools() {
    let by_int: BTreeMap<i32, String> = from_str(r#"{"-3":"a","10":"b"}"#).unwrap();
    assert_eq!(by_int[&-3], "a");
    assert_eq!(by_int[&10], "b");

    let by_u128: BTreeMap<u128, bool> =
        from_str(r#"{"340282366920938463463374607431768211455":true}"#).unwrap();
    assert!(by_u128[&u128::MAX]);

    let by_bool: BTreeMap<bool, u8> = from_str(r#"{"true":1,"false":0}"#).unwrap();
    assert_eq!(by_bool[&true], 1);

    let error = from_str::<BTreeMap<u32, String>>(r#"{"nope":"a"}"#).unwrap_err();
    assert!(
        error.to_string().contains("expected numeric key"),
        "{error}"
    );
    // Keys must spell a complete JSON number, exactly as serde_json requires.
    assert!(from_str::<BTreeMap<u32, String>>(r#"{"1 ":"a"}"#).is_err());
    assert!(from_str::<BTreeMap<u32, String>>(r#"{"+1":"a"}"#).is_err());
}

// Enum representations and serde attributes (flatten, rename, ...) are
// covered differentially against serde_json in tests/serde_enums.rs and
// tests/serde_attributes.rs.

#[test]
fn invalid_type_errors_carry_expected_found_and_offset() {
    let error = from_str::<u32>(r#""seven""#).unwrap_err();
    let message = error.to_string();
    assert!(
        message.contains(r#"invalid type: string "seven", expected u32"#),
        "{message}"
    );
    assert!(message.contains("at byte offset 1"), "{message}");

    #[derive(Deserialize, Debug)]
    struct User {
        #[expect(dead_code)]
        id: u64,
    }
    let error = from_str::<User>(r#"{"id":true}"#).unwrap_err();
    assert!(
        error.to_string().contains("invalid type: boolean `true`"),
        "{error}"
    );

    // The document path reports the offset from the leaf's span, so it
    // matches what the streaming path reports for the same input.
    let json = r#"{"pad":true,"id":"seven"}"#;
    let error = from_value::<User>(&parse(json)).unwrap_err();
    let message = error.to_string();
    assert!(
        message.contains(r#"invalid type: string "seven", expected u64"#),
        "{message}"
    );
    let offset = json.find("seven").unwrap();
    assert!(
        message.contains(&format!("at byte offset {offset}")),
        "{message}"
    );

    #[derive(Deserialize, Debug)]
    struct Named {
        #[expect(dead_code)]
        name: String,
    }
    let json = r#"{"name":1.5}"#;
    let error = from_value::<Named>(&parse(json)).unwrap_err();
    let message = error.to_string();
    let offset = json.find("1.5").unwrap();
    assert!(
        message.contains(&format!("at byte offset {offset}")),
        "{message}"
    );
}

#[test]
fn deserialization_errors_are_the_json_error_type() {
    let error = from_str::<u32>("true").unwrap_err();
    assert!(
        matches!(error, JsonError::Deserialization { .. }),
        "{error}"
    );
}

#[test]
fn compositions_deeper_than_the_parse_cap_error_instead_of_recursing() {
    // Parsing already rejects deep nesting, so build the depth by composing
    // documents: each round wraps the previous root one level deeper.
    let mut doc = parse("[]");
    for _ in 0..200 {
        let mut builder = ValueBuilder::new();
        builder.set("inner", doc.clone()).unwrap();
        doc = builder.seal();
    }
    let error = from_value::<serde_json::Value>(&doc).unwrap_err();
    assert!(
        matches!(error, JsonError::DepthLimitExceeded { .. }),
        "{error}"
    );
}

#[test]
fn from_value_deserializes_a_subtree_handle() {
    let doc = parse(r#"{"user":{"id":7}}"#);
    let user = doc.get("user").unwrap();
    let by_key: BTreeMap<String, u64> = crate::from_value(&user).unwrap();
    assert_eq!(by_key["id"], 7);
}

#[test]
fn from_slice_and_from_str_stream_without_a_document() {
    assert_eq!(from_slice::<Vec<u8>>(b"[1,2,3]").unwrap(), vec![1, 2, 3]);
    let error = from_str::<Vec<u8>>("[1,").unwrap_err();
    assert!(matches!(error, JsonError::Syntax { .. }), "{error}");
    // Invalid UTF-8 fails up front with the offending offset.
    let error = from_slice::<String>(b"\"a\xFF\"").unwrap_err();
    assert!(
        matches!(error, JsonError::Syntax { offset: 2, .. }),
        "{error}"
    );
}

#[test]
fn duplicate_keys_error_when_streaming_and_collapse_in_documents() {
    #[derive(Deserialize, Debug)]
    struct S {
        a: i32,
    }

    // The streaming entry points see both entries and reject the duplicate
    // field exactly as serde_json does.
    let error = from_str::<S>(r#"{"a":1,"a":2}"#).unwrap_err();
    assert!(error.to_string().contains("duplicate field `a`"), "{error}");
    assert!(serde_json::from_str::<S>(r#"{"a":1,"a":2}"#).is_err());

    // Parsing collapses duplicates (first position, last value) before the
    // document deserializer runs, so the same input reads as `{"a":2}`.
    let s: S = from_value(&parse(r#"{"a":1,"a":2}"#)).unwrap();
    assert_eq!(s.a, 2);
}

/// Pins the two documented margins where the depth budget diverges from
/// serde_json: this crate deserializes exactly 128 nested containers where
/// serde_json's recursion limit stops at 127, and it budgets ignored
/// content that serde_json skips without any depth bound.
#[test]
fn depth_cap_margins_diverge_from_serde_json() {
    let nested = |depth: usize| "[".repeat(depth) + &"]".repeat(depth);

    // At the cap: both typed paths accept, serde_json rejects.
    let at_cap = nested(128);
    from_str::<serde_json::Value>(&at_cap).expect("128 levels are within the budget");
    from_value::<serde_json::Value>(&parse(&at_cap)).expect("128 levels are within the budget");
    serde_json::from_str::<serde_json::Value>(&at_cap)
        .expect_err("serde_json's recursion limit stops at 127");

    // One past the cap: rejected on this side too.
    let error = from_str::<serde_json::Value>(&nested(129)).unwrap_err();
    assert!(
        matches!(error, JsonError::DepthLimitExceeded { limit: 128 }),
        "{error}"
    );

    // Deep ignored content: serde_json skips it unboundedly, this crate
    // spends the same budget it would deserializing it.
    #[derive(Deserialize, Debug)]
    struct Empty {}
    let deep_ignored = format!(r#"{{"skipped":{}}}"#, nested(200));
    serde_json::from_str::<Empty>(&deep_ignored)
        .expect("serde_json does not depth-limit ignored content");
    let error = from_str::<Empty>(&deep_ignored).unwrap_err();
    assert!(
        matches!(error, JsonError::DepthLimitExceeded { limit: 128 }),
        "{error}"
    );
}

#[test]
fn ignored_values_are_still_validated() {
    #[derive(Deserialize, Debug)]
    struct Only {
        id: u64,
    }

    let error = from_str::<Only>(r#"{"junk":{"a":tru},"id":1}"#).unwrap_err();
    assert!(matches!(error, JsonError::Syntax { .. }), "{error}");
    let ok: Only = from_str(r#"{"junk":{"a":[true,"x\n",1e3]},"id":1}"#).unwrap();
    assert_eq!(ok.id, 1);
}

#[test]
fn shared_subtrees_deserialize_across_arenas() {
    // A composition whose children live in two other arenas deserializes
    // transparently, including borrowed strings from the foreign inputs.
    let a = parse(r#"{"user":{"id":7,"name":"ada"}}"#);
    let b = parse(r#"{"labels":["x","y"]}"#);
    let mut builder = ValueBuilder::new();
    builder.set("user", a.get("user").unwrap()).unwrap();
    builder.set("labels", b.get("labels").unwrap()).unwrap();
    let composed = builder.seal();
    drop((a, b));

    #[derive(Deserialize, Debug, PartialEq)]
    struct Composed<'a> {
        #[serde(borrow)]
        user: User<'a>,
        #[serde(borrow)]
        labels: Vec<&'a str>,
    }
    #[derive(Deserialize, Debug, PartialEq)]
    struct User<'a> {
        id: u64,
        name: &'a str,
    }

    let composed_value: Composed<'_> = from_value(&composed).unwrap();
    assert_eq!(
        composed_value,
        Composed {
            user: User { id: 7, name: "ada" },
            labels: vec!["x", "y"],
        }
    );
}

#[test]
fn value_fields_capture_the_shared_subtree() {
    #[derive(Deserialize)]
    struct Envelope {
        id: u64,
        payload: Value,
    }

    let doc = parse(r#"{"id":7,"payload":{"n":1.50e2,"s":"A","list":[1,2]}}"#);
    let before = Arc::strong_count(&doc.arena);
    let envelope: Envelope = from_value(&doc).unwrap();
    assert_eq!(envelope.id, 7);

    // One arena Arc bump, no rebuild: the handle shares the parsed arena.
    // The public observables (byte-identical reserialization, raw literal
    // spellings) are asserted in tests/serde_capture.rs.
    assert!(Arc::ptr_eq(&doc.arena, &envelope.payload.arena));
    assert_eq!(Arc::strong_count(&doc.arena), before + 1);
}

#[test]
fn streaming_captures_own_only_their_subtree_bytes() {
    #[derive(Deserialize)]
    struct Envelope {
        payload: Value,
        detail: Value,
    }

    let envelope: Envelope =
        from_str(r#"{"id":7,"payload":[1,"two"],"detail":{"n":1.50e2},"tail":"x"}"#).unwrap();

    // Byte-identical reserialization, raw literal spellings intact.
    assert_eq!(envelope.payload.to_vec(), br#"[1,"two"]"#);
    assert_eq!(envelope.detail.to_vec(), br#"{"n":1.50e2}"#);
    // Each capture's arena holds a copy of just the delimited bytes, so
    // pinning a capture retains nothing beyond its own subtree.
    assert_eq!(envelope.payload.arena.input(), br#"[1,"two"]"#);
    assert!(envelope.detail.is_self_contained());
}

#[test]
fn each_capture_bumps_the_arena_refcount_once() {
    let doc = parse(r#"{"values":[{"a":1},"text",42],"missing":null}"#);

    #[derive(Deserialize)]
    struct Mixed {
        values: Vec<Value>,
        missing: Option<Value>,
    }

    let before = Arc::strong_count(&doc.arena);
    let mixed: Mixed = from_value(&doc).unwrap();
    assert_eq!(mixed.values.len(), 3);
    assert!(mixed.missing.is_none());
    assert_eq!(Arc::strong_count(&doc.arena), before + 3);
}

#[test]
fn the_root_value_captures_itself() {
    let doc = parse(r#"[1,{"a":2}]"#);
    let root: Value = from_value(&doc).unwrap();
    assert!(Arc::ptr_eq(&doc.arena, &root.arena));
    assert_eq!(root.to_vec(), doc.to_vec());
}

#[test]
fn captures_resolve_foreign_subtrees_to_their_owning_arena() {
    let source = parse(r#"{"tags":["x","y"]}"#);
    let mut builder = ValueBuilder::new();
    builder.set("tags", source.get("tags").unwrap()).unwrap();
    let composed = builder.seal();

    #[derive(Deserialize)]
    struct Tagged {
        tags: Value,
    }

    let tagged: Tagged = from_value(&composed).unwrap();
    // The capture points into the arena that owns the subtree, not the
    // composition, so it does not pin the composition's arena.
    assert!(Arc::ptr_eq(&source.arena, &tagged.tags.arena));
    assert_eq!(tagged.tags.to_vec(), br#"["x","y"]"#);
}

#[test]
#[should_panic(expected = "apollo-json deserializers")]
fn foreign_deserializers_panic_for_captured_values() {
    // A foreign deserializer can never produce an arena value; no input
    // makes this call succeed, so it is a defect in the calling code. An
    // error would be swallowed -- deserialization errors are used as
    // control flow, and a cache treats a failed read as a miss.
    let _ = serde_json::from_str::<Value>(r#"{"a":1}"#);
}

#[test]
#[should_panic(expected = "apollo-json deserializers")]
fn captures_inside_serde_buffering_panic() {
    // flatten and untagged enums replay values through serde's internal
    // content deserializer, which cannot hand over an arena. This must be a
    // panic, never an error and never a silent rebuild: the compiled code
    // can never succeed, and an error disappears into fallbacks and
    // cache-miss handling.
    #[derive(Deserialize, Debug)]
    struct Flattened {
        #[expect(dead_code)]
        id: u64,
        #[serde(flatten)]
        #[expect(dead_code)]
        rest: BTreeMap<String, Value>,
    }

    let _ = from_str::<Flattened>(r#"{"id":1,"a":{"b":2}}"#);
}

#[test]
fn adjacently_tagged_captures_depend_on_key_order() {
    // serde streams adjacently tagged content in band when the tag comes
    // first, but buffers it through its content deserializer when the
    // content precedes the tag — and a capture cannot cross that buffering.
    #[derive(Deserialize, Debug)]
    #[serde(tag = "t", content = "c")]
    enum Tagged {
        A(Value),
    }

    let Tagged::A(captured) = from_str(r#"{"t":"A","c":{"n":1}}"#).unwrap();
    assert_eq!(captured.to_vec(), br#"{"n":1}"#);
}

#[test]
#[should_panic(expected = "apollo-json deserializers")]
fn adjacently_tagged_capture_panics_when_content_precedes_the_tag() {
    // Whether serde buffers depends on the input's key order, so the same
    // compiled code works or panics per message -- worse than never
    // working. Treat adjacently tagged captures as unsupported.
    #[derive(Deserialize, Debug)]
    #[serde(tag = "t", content = "c")]
    enum Tagged {
        A(#[expect(dead_code)] Value),
    }

    let _ = from_str::<Tagged>(r#"{"c":{"n":1},"t":"A"}"#);
}

#[test]
fn from_slice_with_buffers_reuses_storage_across_calls() {
    #[derive(Deserialize, Debug, PartialEq)]
    struct Item {
        id: u64,
        name: String,
    }

    let options = crate::ParseOptions::default();
    let mut buffers = crate::ParseBuffers::new();
    for id in 0..3u64 {
        let json = format!(r#"{{"id":{id},"name":"item"}}"#);
        let item: Item =
            crate::from_slice_with_buffers(json.as_bytes(), &options, &mut buffers).unwrap();
        assert_eq!(
            item,
            Item {
                id,
                name: "item".into()
            }
        );
    }
}

#[test]
fn from_slice_with_buffers_supports_captures() {
    // A captured Value keeps the arena alive past the call, so the arena
    // cannot be recycled — the capture must stay valid regardless.
    #[derive(Deserialize)]
    struct Payload {
        id: u64,
        raw: Value,
    }

    let options = crate::ParseOptions::default();
    let mut buffers = crate::ParseBuffers::new();
    let first: Payload =
        crate::from_slice_with_buffers(br#"{"id":1,"raw":{"a":1}}"#, &options, &mut buffers)
            .unwrap();
    let second: Payload =
        crate::from_slice_with_buffers(br#"{"id":2,"raw":{"b":2}}"#, &options, &mut buffers)
            .unwrap();
    assert_eq!((first.id, second.id), (1, 2));
    assert_eq!(first.raw.to_vec(), br#"{"a":1}"#);
    assert_eq!(second.raw.to_vec(), br#"{"b":2}"#);
}
