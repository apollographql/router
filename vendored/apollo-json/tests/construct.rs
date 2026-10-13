//! Standalone value construction and the owned read accessors.

use apollo_json::{JsonKind, Value, ValueBuilder};

mod common;
use common::parse;

#[test]
fn scalar_constructors_serialize_as_json() {
    assert_eq!(Value::null().to_string(), "null");
    assert_eq!(Value::from("a\"b").to_string(), r#""a\"b""#);
    assert_eq!(Value::from(String::from("ada")).to_string(), r#""ada""#);
    assert_eq!(Value::from(true).to_string(), "true");
    assert_eq!(Value::from(-7i64).to_string(), "-7");
    assert_eq!(Value::from(1.5f64).to_string(), "1.5");
}

#[test]
fn scalar_constructors_read_back_at_their_own_type() {
    assert!(Value::null().is_null());
    assert_eq!(Value::from("ada").as_string().as_deref(), Some("ada"));
    assert_eq!(Value::from(true).as_bool(), Some(true));
    assert_eq!(Value::from(-7i64).as_i64(), Some(-7));
    assert_eq!(Value::from(1.5f64).as_f64(), Some(1.5));
}

#[test]
fn unsigned_constructor_keeps_digits_above_i64_max() {
    let value = Value::from(u64::MAX);

    assert_eq!(value.to_string(), "18446744073709551615");
    assert_eq!(value.as_u64(), Some(u64::MAX));
    assert_eq!(value.as_i64(), None);
}

#[test]
fn non_finite_floats_become_null() {
    assert!(Value::from(f64::NAN).is_null());
    assert!(Value::from(f64::INFINITY).is_null());
    assert!(Value::from(f64::NEG_INFINITY).is_null());
    assert_eq!(
        Value::array([f64::INFINITY, 1.0]).to_string(),
        "[null,1.0]",
        "a non-finite element lands as null rather than failing the array"
    );
    assert_eq!(
        Value::object([("x", f64::NAN)]).to_string(),
        r#"{"x":null}"#
    );
}

#[test]
fn empty_array_constructor_produces_an_array_not_an_object() {
    let empty = Value::array(Vec::<Value>::new());

    assert_eq!(empty.kind(), JsonKind::Array);
    assert_eq!(empty.to_string(), "[]");
    assert_eq!(empty.len(), Some(0));
}

#[test]
fn array_and_object_constructors_nest_and_adopt_values() {
    let doc = parse(r#"{"tags":["x","y"]}"#);
    let tags = doc.get("tags").expect("tags is present");

    let value = Value::object([("tags", tags), ("counts", Value::array([1i64, 2]))]);

    assert_eq!(value.to_string(), r#"{"tags":["x","y"],"counts":[1,2]}"#);
}

#[test]
fn object_constructor_collapses_a_repeated_key_to_its_last_value() {
    let value = Value::object([("a", 1i64), ("b", 2i64), ("a", 3i64)]);

    assert_eq!(
        value.to_string(),
        r#"{"a":3,"b":2}"#,
        "the repeated key keeps its first position and its last value"
    );
}

#[test]
fn array_builder_root_accepts_elements() {
    let mut builder = ValueBuilder::new_array();
    builder.push("x").expect("an array root accepts elements");
    builder.push(2i64).expect("an array root accepts elements");

    assert_eq!(builder.seal().to_string(), r#"["x",2]"#);
}

#[test]
fn owned_string_accessor_reads_through_a_temporary_handle() {
    let doc = parse(r#"{"outer":{"name":"ada","escaped":"a\nb"}}"#);
    let root = doc;

    // The Cow that `as_str` returns borrows its receiver, so this chain only
    // compiles against the owned accessor.
    let name = root
        .get("outer")
        .and_then(|outer| outer.get("name"))
        .and_then(|name| name.as_string());
    assert_eq!(name.as_deref(), Some("ada"));

    let escaped = root
        .get("outer")
        .and_then(|outer| outer.get("escaped"))
        .and_then(|escaped| escaped.as_string());
    assert_eq!(escaped.as_deref(), Some("a\nb"));

    assert_eq!(root.get("outer").and_then(|outer| outer.as_string()), None);
}

#[test]
fn owned_container_accessors_materialize_members() {
    let doc = parse(r#"{"a":[1,"two"],"b":{"c":true}}"#);
    let root = doc;

    let elements = root
        .get("a")
        .and_then(|a| a.as_array())
        .expect("a is an array");
    assert_eq!(elements.len(), 2);
    assert_eq!(elements[0].as_i64(), Some(1));
    assert_eq!(elements[1].as_string().as_deref(), Some("two"));

    let members = root
        .get("b")
        .and_then(|b| b.as_object())
        .expect("b is an object");
    assert_eq!(members.len(), 1);
    assert_eq!(members[0].0, "c");
    assert_eq!(members[0].1.as_bool(), Some(true));

    assert!(root.get("a").and_then(|a| a.as_object()).is_none());
    assert!(root.get("b").and_then(|b| b.as_array()).is_none());
}

#[test]
fn shape_predicates_answer_for_one_shape_each() {
    let doc = parse(r#"{"object":{},"array":[],"string":"s","number":1,"bool":true,"null":null}"#);
    let root = doc;
    let shapes = ["object", "array", "string", "number", "bool", "null"];

    for shape in shapes {
        let value = root.get(shape).expect("every key is present");
        let matched: Vec<&str> = [
            ("object", value.is_object()),
            ("array", value.is_array()),
            ("string", value.is_string()),
            ("number", value.is_number()),
            ("bool", value.is_boolean()),
            ("null", value.is_null()),
        ]
        .into_iter()
        .filter(|(_, held)| *held)
        .map(|(name, _)| name)
        .collect();
        assert_eq!(matched, vec![shape]);

        let borrowed = value.value();
        assert_eq!(borrowed.is_object(), value.is_object());
        assert_eq!(borrowed.is_array(), value.is_array());
        assert_eq!(borrowed.is_string(), value.is_string());
        assert_eq!(borrowed.is_number(), value.is_number());
        assert_eq!(borrowed.is_boolean(), value.is_boolean());
    }
}
