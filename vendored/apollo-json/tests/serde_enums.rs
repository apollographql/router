//! Differential enum coverage against `serde_json`: every variant shape
//! under every tag representation must agree on accepted input, produced
//! bytes, and rejected input.

use std::fmt::Debug;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

/// Both stacks must decode `json` to `expected` and serialize `expected`
/// to byte-identical output.
fn assert_agree<T>(json: &str, expected: &T)
where
    T: DeserializeOwned + Serialize + PartialEq + Debug,
{
    let ours: T = apollo_json::from_str(json).expect("apollo-json accepts the input");
    let reference: T = serde_json::from_str(json).expect("serde_json accepts the input");
    assert_eq!(ours, *expected, "apollo-json decoded {json} differently");
    assert_eq!(
        reference, *expected,
        "serde_json decoded {json} differently"
    );

    let ours = apollo_json::to_value(expected)
        .expect("apollo-json serializes the value")
        .to_vec();
    let reference = serde_json::to_vec(expected).expect("serde_json serializes the value");
    assert_eq!(
        String::from_utf8_lossy(&ours),
        String::from_utf8_lossy(&reference),
        "serialized bytes diverge for {expected:?}"
    );
}

fn assert_both_reject<T>(json: &str)
where
    T: DeserializeOwned + Debug,
{
    let ours = apollo_json::from_str::<T>(json);
    assert!(ours.is_err(), "apollo-json accepted {json}: {ours:?}");
    let reference = serde_json::from_str::<T>(json);
    assert!(
        reference.is_err(),
        "serde_json accepted {json}: {reference:?}"
    );
}

#[derive(Serialize, Deserialize, Debug, PartialEq)]
enum External {
    Unit,
    Newtype(u32),
    Tuple(u32, bool),
    Struct { x: u32, y: String },
}

// Tuple variants are a compile error under internal tagging, and a newtype
// variant's content must itself serialize as a map, so those are the shapes
// the representation supports.
#[derive(Serialize, Deserialize, Debug, PartialEq)]
#[serde(tag = "kind")]
enum Internal {
    Unit,
    Newtype(Inner),
    Struct { x: u32, y: String },
}

#[derive(Serialize, Deserialize, Debug, PartialEq)]
struct Inner {
    n: u32,
}

#[derive(Serialize, Deserialize, Debug, PartialEq)]
#[serde(tag = "t", content = "c")]
enum Adjacent {
    Unit,
    Newtype(u32),
    Tuple(u32, bool),
    Struct { x: u32, y: String },
}

#[derive(Serialize, Deserialize, Debug, PartialEq)]
#[serde(untagged)]
enum Untagged {
    Unit,
    Newtype(bool),
    Tuple(u32, bool),
    Struct { x: u32, y: String },
}

#[test]
fn externally_tagged_shapes_agree_with_serde_json() {
    assert_agree(r#""Unit""#, &External::Unit);
    assert_agree(r#"{"Unit":null}"#, &External::Unit);
    assert_agree(r#"{"Newtype":7}"#, &External::Newtype(7));
    assert_agree(r#"{"Tuple":[7,true]}"#, &External::Tuple(7, true));
    assert_agree(
        r#"{"Struct":{"x":7,"y":"hi"}}"#,
        &External::Struct {
            x: 7,
            y: "hi".into(),
        },
    );
}

#[test]
fn internally_tagged_shapes_agree_with_serde_json() {
    assert_agree(r#"{"kind":"Unit"}"#, &Internal::Unit);
    assert_agree(
        r#"{"kind":"Newtype","n":7}"#,
        &Internal::Newtype(Inner { n: 7 }),
    );
    assert_agree(
        r#"{"kind":"Struct","x":7,"y":"hi"}"#,
        &Internal::Struct {
            x: 7,
            y: "hi".into(),
        },
    );
    // The tag may appear after the payload fields.
    assert_agree(
        r#"{"x":7,"y":"hi","kind":"Struct"}"#,
        &Internal::Struct {
            x: 7,
            y: "hi".into(),
        },
    );
    // serde's tagged-content visitor also reads a `[tag]` array form.
    assert_agree(r#"["Unit"]"#, &Internal::Unit);
}

#[test]
fn adjacently_tagged_shapes_agree_with_serde_json() {
    assert_agree(r#"{"t":"Unit"}"#, &Adjacent::Unit);
    assert_agree(r#"{"t":"Unit","c":null}"#, &Adjacent::Unit);
    assert_agree(r#"{"t":"Newtype","c":7}"#, &Adjacent::Newtype(7));
    assert_agree(r#"{"t":"Tuple","c":[7,true]}"#, &Adjacent::Tuple(7, true));
    assert_agree(
        r#"{"t":"Struct","c":{"x":7,"y":"hi"}}"#,
        &Adjacent::Struct {
            x: 7,
            y: "hi".into(),
        },
    );
    // Content may precede the tag.
    assert_agree(r#"{"c":7,"t":"Newtype"}"#, &Adjacent::Newtype(7));
}

#[test]
fn untagged_shapes_agree_with_serde_json() {
    assert_agree("null", &Untagged::Unit);
    assert_agree("true", &Untagged::Newtype(true));
    assert_agree("[7,true]", &Untagged::Tuple(7, true));
    assert_agree(
        r#"{"x":7,"y":"hi"}"#,
        &Untagged::Struct {
            x: 7,
            y: "hi".into(),
        },
    );
}

#[test]
fn option_of_enum_agrees_with_serde_json() {
    assert_agree::<Option<External>>("null", &None);
    assert_agree(r#""Unit""#, &Some(External::Unit));
    assert_agree(r#"{"Newtype":7}"#, &Some(External::Newtype(7)));
    assert_agree(
        r#"{"t":"Tuple","c":[7,true]}"#,
        &Some(Adjacent::Tuple(7, true)),
    );
    // `null` matches the Option before the untagged unit variant in both
    // stacks.
    assert_agree::<Option<Untagged>>("null", &None);
}

#[test]
fn vec_of_enum_agrees_with_serde_json() {
    assert_agree(
        r#"["Unit",{"Newtype":7},{"Tuple":[7,true]},{"Struct":{"x":7,"y":"hi"}}]"#,
        &vec![
            External::Unit,
            External::Newtype(7),
            External::Tuple(7, true),
            External::Struct {
                x: 7,
                y: "hi".into(),
            },
        ],
    );
    assert_agree(
        r#"[{"t":"Unit"},{"t":"Newtype","c":7}]"#,
        &vec![Adjacent::Unit, Adjacent::Newtype(7)],
    );
    assert_agree(
        r#"[null,true,[7,true]]"#,
        &vec![
            Untagged::Unit,
            Untagged::Newtype(true),
            Untagged::Tuple(7, true),
        ],
    );
}

#[derive(Serialize, Deserialize, Debug, PartialEq)]
struct InternalEnvelope {
    id: u32,
    #[serde(flatten)]
    message: Internal,
}

#[derive(Serialize, Deserialize, Debug, PartialEq)]
struct ExternalEnvelope {
    id: u32,
    #[serde(flatten)]
    payload: External,
}

#[test]
fn internally_tagged_enum_flattened_into_a_struct_agrees_with_serde_json() {
    assert_agree(
        r#"{"id":1,"kind":"Struct","x":7,"y":"hi"}"#,
        &InternalEnvelope {
            id: 1,
            message: Internal::Struct {
                x: 7,
                y: "hi".into(),
            },
        },
    );
    assert_agree(
        r#"{"id":1,"kind":"Unit"}"#,
        &InternalEnvelope {
            id: 1,
            message: Internal::Unit,
        },
    );
}

#[test]
fn externally_tagged_enum_flattened_into_a_struct_agrees_with_serde_json() {
    assert_agree(
        r#"{"id":1,"Newtype":7}"#,
        &ExternalEnvelope {
            id: 1,
            payload: External::Newtype(7),
        },
    );
    assert_agree(
        r#"{"id":1,"Struct":{"x":7,"y":"hi"}}"#,
        &ExternalEnvelope {
            id: 1,
            payload: External::Struct {
                x: 7,
                y: "hi".into(),
            },
        },
    );
}

#[test]
fn unknown_variants_are_rejected_by_both_stacks() {
    assert_both_reject::<External>(r#""Bogus""#);
    assert_both_reject::<External>(r#"{"Bogus":1}"#);
    assert_both_reject::<Internal>(r#"{"kind":"Bogus"}"#);
    assert_both_reject::<Adjacent>(r#"{"t":"Bogus","c":1}"#);
}

#[test]
fn wrong_variant_shapes_are_rejected_by_both_stacks() {
    assert_both_reject::<External>(r#"{"Newtype":[1]}"#);
    assert_both_reject::<External>(r#"{"Tuple":7}"#);
    assert_both_reject::<External>(r#"{"Tuple":[7]}"#);
    assert_both_reject::<External>(r#"{"Tuple":[7,true,0]}"#);
    assert_both_reject::<External>(r#"{"Unit":1}"#);
    assert_both_reject::<External>("7");
    assert_both_reject::<Internal>(r#"{"kind":"Struct","x":true,"y":"hi"}"#);
    assert_both_reject::<Internal>(r#"["Struct",{"x":7,"y":"hi"}]"#);
    assert_both_reject::<Adjacent>(r#"{"t":"Newtype","c":[1]}"#);
    assert_both_reject::<Adjacent>(r#"{"t":"Struct","c":[7,"hi"]}"#);
    assert_both_reject::<Adjacent>(r#"{"t":"Unit","c":7}"#);
}

// serde_json accepts array content for a struct variant (fields taken in
// declaration order), the same relaxation deserialize_struct applies to
// top-level structs.
#[test]
fn externally_tagged_struct_variants_accept_array_content() {
    assert_agree(
        r#"{"Struct":[7,"hi"]}"#,
        &External::Struct {
            x: 7,
            y: "hi".into(),
        },
    );
}

#[test]
fn malformed_tagging_is_rejected_by_both_stacks() {
    assert_both_reject::<External>(r#"{"Newtype":7,"Tuple":[7,true]}"#);
    assert_both_reject::<Internal>(r#"{"x":7,"y":"hi"}"#);
    assert_both_reject::<Internal>(r#"{"kind":7}"#);
    assert_both_reject::<Adjacent>(r#"{"c":7}"#);
    assert_both_reject::<Untagged>("3");
    assert_both_reject::<Untagged>(r#"{"z":1}"#);
}
