//! Capture and adoption semantics: struct fields typed [`Value`] or
//! [`Value`] cross serde by reference — shared subtrees on deserialize,
//! foreign references on serialize — and reserialize byte-identically.
//! Sharing is asserted through public observables: borrowed leaves pointing
//! into the source arena's input bytes, and raw literal spellings that
//! cannot survive a structural copy. Arena retention and release live in
//! tests/memory.rs.

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};

use apollo_json::{JsonError, Value, ValueBuilder, ValueRef, from_value, to_value};
use serde::{Deserialize, Serialize};

mod common;
use common::parse;

/// Address of an escape-free string leaf inside its arena's input buffer;
/// two equal addresses prove two handles share one arena.
fn borrowed_str_ptr(value: ValueRef<'_>) -> *const u8 {
    match value.as_str().expect("value is a string") {
        Cow::Borrowed(s) => s.as_ptr(),
        Cow::Owned(_) => panic!("expected an escape-free string borrowing the arena input"),
    }
}

/// Address of a number leaf's raw literal inside its arena's input buffer.
fn raw_number_ptr(value: ValueRef<'_>) -> *const u8 {
    value.raw_number().expect("value is a number").as_ptr()
}

#[test]
fn captured_value_fields_share_the_source_arena() {
    #[derive(Deserialize)]
    struct Envelope {
        id: u64,
        payload: Value,
    }

    let doc = parse(r#"{"id":7,"payload":{"n":1e2,"s":"plain"}}"#);
    let source = doc.value().get("payload").unwrap();

    let envelope: Envelope = from_value(&doc).unwrap();

    assert_eq!(envelope.id, 7);
    let captured = envelope.payload.value();
    assert_eq!(
        borrowed_str_ptr(captured.get("s").unwrap()),
        borrowed_str_ptr(source.get("s").unwrap()),
        "the captured string leaf must borrow the same input bytes as the source"
    );
    assert_eq!(
        raw_number_ptr(captured.get("n").unwrap()),
        raw_number_ptr(source.get("n").unwrap()),
        "the captured number literal must be the source's span, not a copy"
    );
}

#[test]
fn captured_fields_reserialize_byte_identically() {
    #[derive(Deserialize)]
    struct Envelope {
        payload: Value,
    }

    let payload_json = r#"{"n":1e2,"f":1.50,"big":9007199254740993,"s":"A\n","list":[1E-005,"x"]}"#;
    let doc = parse(format!(r#"{{"payload":{payload_json}}}"#));

    let envelope: Envelope = from_value(&doc).unwrap();

    assert_eq!(envelope.payload.to_vec(), payload_json.as_bytes());
    let payload = envelope.payload.value();
    assert_eq!(payload.get("n").unwrap().raw_number(), Some("1e2"));
    assert_eq!(payload.get("s").unwrap().as_str().as_deref(), Some("A\n"));
}

#[test]
fn captured_fields_are_views_into_the_parse_arena() {
    #[derive(Deserialize)]
    struct Envelope {
        payload: Value,
    }

    let doc = parse(r#"{"payload":{"a":[true,null],"n":1.50e2,"s":"plain"}}"#);

    let envelope: Envelope = from_value(&doc).unwrap();

    assert_eq!(
        envelope.payload.to_vec(),
        br#"{"a":[true,null],"n":1.50e2,"s":"plain"}"#
    );
    assert_eq!(
        borrowed_str_ptr(envelope.payload.value().get("s").unwrap()),
        borrowed_str_ptr(doc.value().get("payload").unwrap().get("s").unwrap()),
        "the captured field must be a view into the parse arena"
    );
}

#[test]
fn captures_nest_inside_vec_option_and_map_values() {
    #[derive(Deserialize)]
    struct Containers {
        list: Vec<Value>,
        present: Option<Value>,
        absent: Option<Value>,
        by_key: HashMap<String, Value>,
    }

    let doc = parse(
        r#"{"list":[{"n":1e2},"txt"],"present":{"deep":[1,2]},"absent":null,"by_key":{"a":[true],"b":"esc\n"}}"#,
    );

    let containers: Containers = from_value(&doc).unwrap();

    assert_eq!(containers.list[0].to_vec(), br#"{"n":1e2}"#);
    assert_eq!(containers.list[1].to_vec(), br#""txt""#);
    assert_eq!(
        containers.present.as_ref().unwrap().to_vec(),
        br#"{"deep":[1,2]}"#
    );
    assert!(containers.absent.is_none());
    assert_eq!(containers.by_key["a"].to_vec(), b"[true]");
    assert_eq!(containers.by_key["b"].to_vec(), br#""esc\n""#);
    assert_eq!(
        raw_number_ptr(containers.list[0].value().get("n").unwrap()),
        raw_number_ptr(
            doc.value()
                .get("list")
                .and_then(|l| l.index(0))
                .and_then(|e| e.get("n"))
                .unwrap()
        ),
        "elements captured through a Vec must still share the parse arena"
    );
}

#[test]
fn captures_nest_inside_nested_structs() {
    #[derive(Deserialize)]
    struct Outer {
        meta: Meta,
        tail: u64,
    }

    #[derive(Deserialize)]
    struct Meta {
        label: String,
        payload: Value,
    }

    let doc = parse(r#"{"meta":{"label":"m","payload":{"n":1.50e2}},"tail":9}"#);

    let outer: Outer = from_value(&doc).unwrap();

    assert_eq!(outer.meta.label, "m");
    assert_eq!(outer.tail, 9);
    assert_eq!(outer.meta.payload.to_vec(), br#"{"n":1.50e2}"#);
    assert_eq!(
        raw_number_ptr(outer.meta.payload.value().get("n").unwrap()),
        raw_number_ptr(
            doc.value()
                .get("meta")
                .and_then(|m| m.get("payload"))
                .and_then(|p| p.get("n"))
                .unwrap()
        ),
    );
}

#[test]
fn captures_work_alongside_a_flattened_sibling_field() {
    // Only the flattened remainder goes through serde's content buffering;
    // named siblings deserialize directly off this crate's deserializer, so
    // a Value field next to a flatten must still capture by reference.
    #[derive(Deserialize)]
    struct WithFlatten {
        payload: Value,
        #[serde(flatten)]
        rest: BTreeMap<String, u64>,
    }

    let doc = parse(r#"{"payload":{"n":1.50e2},"x":1,"y":2}"#);

    let with_flatten: WithFlatten = from_value(&doc).unwrap();

    assert_eq!(with_flatten.payload.to_vec(), br#"{"n":1.50e2}"#);
    assert_eq!(with_flatten.rest["x"], 1);
    assert_eq!(with_flatten.rest["y"], 2);
    assert_eq!(
        raw_number_ptr(with_flatten.payload.value().get("n").unwrap()),
        raw_number_ptr(doc.value().get("payload").and_then(|p| p.get("n")).unwrap()),
    );
}

#[test]
fn to_value_adopts_value_fields_by_reference() {
    #[derive(Serialize)]
    struct Envelope {
        id: u64,
        payload: Value,
    }

    let source = parse(r#"{"payload":{"n":1.50e2,"s":"plain"}}"#);
    let source_str = borrowed_str_ptr(
        source
            .value()
            .get("payload")
            .and_then(|p| p.get("s"))
            .unwrap(),
    );
    let payload = source.get("payload").unwrap();

    let doc = to_value(&Envelope { id: 7, payload }).unwrap();

    // A foreign reference, not a copy: the built document does not own the
    // subtree, and the raw literal spelling — which cannot cross serde's
    // data model structurally — survives.
    assert!(!doc.is_self_contained());
    assert_eq!(
        doc.to_vec(),
        br#"{"id":7,"payload":{"n":1.50e2,"s":"plain"}}"#
    );
    let adopted = doc.get("payload").unwrap();
    assert_eq!(
        borrowed_str_ptr(adopted.value().get("s").unwrap()),
        source_str,
        "the adopted subtree must resolve into the source arena"
    );
}

/// Chained objects deep enough to reach the parse depth cap, ending in an
/// optional captured payload.
#[derive(Deserialize, Debug)]
struct Nest {
    c: Option<Box<Nest>>,
    p: Option<Value>,
}

#[test]
fn a_capturing_type_deserializes_at_the_parse_depth_cap() {
    // 127 wrappers around the innermost object: depth 128, the default cap.
    let mut json = String::from(r#"{"p":1e2}"#);
    for _ in 1..128 {
        json = format!(r#"{{"c":{json}}}"#);
    }
    let doc = parse(&json);

    let nest: Nest = from_value(&doc).unwrap();

    let mut cursor = &nest;
    let mut levels = 1;
    while let Some(child) = cursor.c.as_deref() {
        cursor = child;
        levels += 1;
    }
    assert_eq!(levels, 128);
    let payload = cursor.p.as_ref().expect("payload at the deepest level");
    assert_eq!(payload.to_vec(), b"1e2");
    assert_eq!(payload.value().raw_number(), Some("1e2"));
}

#[test]
fn a_capturing_type_past_the_depth_cap_errors_instead_of_recursing() {
    // Parsing rejects deep inputs, so the only way past the cap is composing
    // documents; the deserializer must spend its budget and stop.
    let mut doc = parse(r#"{"p":1e2}"#);
    for _ in 0..140 {
        let mut builder = ValueBuilder::new();
        builder.set("c", doc).unwrap();
        doc = builder.seal();
    }

    let error = from_value::<Nest>(&doc).unwrap_err();

    assert!(
        matches!(error, JsonError::DepthLimitExceeded { .. }),
        "{error}"
    );
}

#[test]
fn a_capture_syntax_error_reports_the_whole_input_offset() {
    #[derive(Deserialize, Debug)]
    #[allow(dead_code)]
    struct Envelope {
        id: u64,
        payload: Value,
    }

    // The capture parses its subtree bytes on their own; the offset it
    // reports must be rebased to the whole input, not the subtree.
    let json = r#"{"id":1,"payload":[1,!]}"#;

    let error = apollo_json::from_str::<Envelope>(json).unwrap_err();

    match error {
        JsonError::Syntax { offset, .. } => {
            assert_eq!(offset, json.find('!').unwrap(), "{error}");
        }
        other => panic!("expected a syntax error, got {other}"),
    }
}

#[test]
fn a_capture_depth_error_reports_the_overall_cap() {
    // Spend most of the nesting budget before the capture starts, then nest
    // the captured subtree past what remains. The capture's sub-parse runs
    // under the remaining budget, but the error must report the overall cap
    // the caller configured, not the leftover.
    let mut json = format!(r#"{{"p":{}{}}}"#, "[".repeat(50), "]".repeat(50));
    for _ in 0..100 {
        json = format!(r#"{{"c":{json}}}"#);
    }

    let error = apollo_json::from_str::<Nest>(&json).unwrap_err();

    match error {
        JsonError::DepthLimitExceeded { limit } => assert_eq!(limit, 128),
        other => panic!("expected a depth error, got {other}"),
    }
}
