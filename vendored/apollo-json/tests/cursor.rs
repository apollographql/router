//! The cursor mutation API: chaining, conversions, and copy-on-write
//! isolation when editing shared documents through cursors.

use apollo_json::{JsonError, Value};

mod common;
use common::parse;

#[test]
fn cursors_chain_and_edit_locally() {
    let doc = parse(r#"{"name":"alice","person":{"age":18,"legacy_id":7,"tags":["a"]}}"#);
    let mut builder = doc.edit();

    builder.set("name", "bob").unwrap();
    let mut person = builder.get_mut("person").unwrap();
    person.set("age", 19).unwrap();
    person.set("email", "bob@example.com").unwrap();
    assert!(person.remove("legacy_id"));
    let mut tags = person.get_mut("tags").unwrap();
    tags.push("verified").unwrap();

    assert_eq!(
        builder.seal().to_string(),
        r#"{"name":"bob","person":{"age":19,"tags":["a","verified"],"email":"bob@example.com"}}"#
    );
}

#[test]
fn index_segments_and_array_cursors() {
    let doc = parse(r#"{"rows":[[1,2],[3,4]]}"#);
    let mut builder = doc.edit();

    let mut first = builder.get_mut("rows").unwrap().get_mut(0usize).unwrap();
    first.set(1usize, 9).unwrap();
    first.push(()).unwrap();
    // Setting one past the end appends; further is an error.
    first.set(3usize, true).unwrap();
    assert!(matches!(
        first.set(9usize, 0i64),
        Err(JsonError::PathNotFound { .. })
    ));
    // Key segments do not apply to arrays.
    assert!(matches!(
        first.set("k", 0i64),
        Err(JsonError::PathNotFound { .. })
    ));

    assert_eq!(
        builder.seal().to_string(),
        r#"{"rows":[[1,9,null,true],[3,4]]}"#
    );
}

#[test]
fn get_mut_creates_missing_objects_and_rejects_bad_indexes() {
    let mut builder = Value::parse(b"{}".to_vec()).unwrap().edit();
    let mut fresh = builder.get_mut("created").unwrap();
    fresh.set("inner", 1).unwrap();
    assert!(matches!(
        builder.get_mut(3usize),
        Err(JsonError::PathNotFound { .. })
    ));
    assert_eq!(builder.seal().to_string(), r#"{"created":{"inner":1}}"#);
}

#[test]
fn conversions_cover_scalars_handles_and_null() {
    let fragment = parse(r#"{"sub":{"k":"v"}}"#);
    let mut builder = Value::parse(b"{}".to_vec()).unwrap().edit();
    builder.set("s", "text").unwrap();
    builder.set("owned", String::from("owned")).unwrap();
    builder.set("i", -3i64).unwrap();
    builder.set("f", 1.5f64).unwrap();
    builder.set("b", true).unwrap();
    builder.set("n", ()).unwrap();
    builder
        .set("adopted", fragment.get("sub").unwrap())
        .unwrap();
    assert!(matches!(
        builder.set("bad", f64::NAN),
        Err(JsonError::NonFiniteNumber)
    ));
    assert_eq!(
        builder.seal().to_string(),
        r#"{"s":"text","owned":"owned","i":-3,"f":1.5,"b":true,"n":null,"adopted":{"k":"v"}}"#
    );
}

/// Editing a shared document through cursors copies on write: the original
/// never observes the edits, and mutating the original afterwards never
/// leaks into the sealed copy.
#[test]
fn cursor_edits_isolate_shared_documents_in_both_directions() {
    let original = parse(r#"{"shared":{"x":1,"list":[1]}}"#);
    let before = original.to_string();

    let mut builder = original.clone().edit();
    let mut shared = builder.get_mut("shared").unwrap();
    shared.set("x", 99).unwrap();
    let mut list = shared.get_mut("list").unwrap();
    list.push(2).unwrap();
    let edited = builder.seal();

    assert_eq!(edited.to_string(), r#"{"shared":{"x":99,"list":[1,2]}}"#);
    assert_eq!(original.to_string(), before, "original unchanged");

    // Other direction: edit the original while `edited` is shared out.
    let mut builder = original.edit();
    let mut shared = builder.get_mut("shared").unwrap();
    shared.set("x", 41).unwrap();
    let reedited = builder.seal();
    assert_eq!(reedited.to_string(), r#"{"shared":{"x":41,"list":[1]}}"#);
    assert_eq!(
        edited.to_string(),
        r#"{"shared":{"x":99,"list":[1,2]}}"#,
        "the earlier edit is unaffected"
    );
}

/// Cursors work on merged state: edits apply to the merge result and the
/// merged-in source stays untouched.
#[test]
fn cursors_edit_merge_results_without_touching_sources() {
    let base = parse(r#"{"data":{"a":1}}"#);
    let patch = parse(r#"{"data":{"b":{"x":1}}}"#);
    let before = patch.to_string();

    let mut builder = base.edit();
    builder.merge(&patch);
    let mut b = builder.get_mut("data").unwrap().get_mut("b").unwrap();
    b.set("x", 2).unwrap();
    b.set("y", 3).unwrap();
    let merged = builder.seal();

    assert_eq!(merged.to_string(), r#"{"data":{"a":1,"b":{"x":2,"y":3}}}"#);
    assert_eq!(patch.to_string(), before, "merged-in source untouched");
}
