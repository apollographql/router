//! Differential coverage of the serde attribute matrix: every attribute is
//! exercised through apollo-json and serde_json on identical input, and the
//! results are asserted equal.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::fmt::Debug;

use apollo_json::{from_str, from_value, to_value};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

mod common;
use common::parse;

/// Deserializes `input` through both crates and asserts they agree.
fn de_both<T>(input: &str) -> T
where
    T: DeserializeOwned + PartialEq + Debug,
{
    let ours: T = from_str(input).expect("apollo-json deserializes");
    let reference: T = serde_json::from_str(input).expect("serde_json deserializes");
    assert_eq!(ours, reference, "deserialization diverged on {input}");
    ours
}

/// Serializes `value` through both crates and asserts identical output.
fn ser_both<T: Serialize>(value: &T) -> String {
    let ours = to_value(value).expect("to_value succeeds").to_string();
    let reference = serde_json::to_string(value).expect("serde_json serializes");
    assert_eq!(ours, reference, "serialization diverged");
    ours
}

#[test]
fn rename_maps_the_field_in_both_directions() {
    #[derive(Serialize, Deserialize, Debug, PartialEq)]
    struct Renamed {
        #[serde(rename = "userName")]
        user_name: String,
    }

    let input = r#"{"userName":"ada"}"#;
    let value: Renamed = de_both(input);
    assert_eq!(value.user_name, "ada");
    assert_eq!(ser_both(&value), input);
}

#[test]
fn rename_all_camel_case_maps_struct_fields() {
    #[derive(Serialize, Deserialize, Debug, PartialEq)]
    #[serde(rename_all = "camelCase")]
    struct Profile {
        first_name: String,
        last_login_at: u64,
    }

    let input = r#"{"firstName":"ada","lastLoginAt":7}"#;
    let value: Profile = de_both(input);
    assert_eq!(value.first_name, "ada");
    assert_eq!(value.last_login_at, 7);
    assert_eq!(ser_both(&value), input);
}

#[test]
fn rename_all_snake_case_maps_enum_variants() {
    #[derive(Serialize, Deserialize, Debug, PartialEq)]
    #[serde(rename_all = "snake_case")]
    enum Mode {
        ReadOnly,
        ReadWrite { max_writers: u8 },
    }

    let unit: Mode = de_both(r#""read_only""#);
    assert_eq!(unit, Mode::ReadOnly);
    assert_eq!(ser_both(&unit), r#""read_only""#);

    let input = r#"{"read_write":{"max_writers":3}}"#;
    let structured: Mode = de_both(input);
    assert_eq!(structured, Mode::ReadWrite { max_writers: 3 });
    assert_eq!(ser_both(&structured), input);
}

#[test]
fn alias_accepts_every_spelling() {
    #[derive(Deserialize, Debug, PartialEq)]
    struct Aliased {
        #[serde(alias = "id", alias = "identifier")]
        user_id: u64,
    }

    for input in [r#"{"user_id":7}"#, r#"{"id":7}"#, r#"{"identifier":7}"#] {
        let value: Aliased = de_both(input);
        assert_eq!(value.user_id, 7);
    }
}

#[test]
fn default_fills_missing_fields_and_yields_to_present_ones() {
    fn default_retries() -> u32 {
        3
    }

    #[derive(Deserialize, Debug, PartialEq)]
    struct WithDefaults {
        #[serde(default)]
        enabled: bool,
        #[serde(default = "default_retries")]
        retries: u32,
    }

    let defaulted: WithDefaults = de_both("{}");
    assert_eq!(
        defaulted,
        WithDefaults {
            enabled: false,
            retries: 3,
        }
    );

    let explicit: WithDefaults = de_both(r#"{"enabled":true,"retries":9}"#);
    assert_eq!(
        explicit,
        WithDefaults {
            enabled: true,
            retries: 9,
        }
    );
}

#[test]
fn skip_excludes_the_field_from_both_directions() {
    #[derive(Serialize, Deserialize, Debug, PartialEq)]
    struct WithSkip {
        id: u64,
        #[serde(skip)]
        cached: bool,
    }

    let absent: WithSkip = de_both(r#"{"id":1}"#);
    assert!(!absent.cached);

    // A skipped field's key in the input is an unknown field, ignored by both.
    let present: WithSkip = de_both(r#"{"id":1,"cached":true}"#);
    assert!(!present.cached);

    let output = ser_both(&WithSkip {
        id: 1,
        cached: true,
    });
    assert_eq!(output, r#"{"id":1}"#);
}

#[test]
fn skip_deserializing_defaults_on_read_but_still_writes() {
    #[derive(Serialize, Deserialize, Debug, PartialEq)]
    struct WithSkipDe {
        id: u64,
        #[serde(skip_deserializing)]
        revision: u32,
    }

    let value: WithSkipDe = de_both(r#"{"id":1,"revision":9}"#);
    assert_eq!(value.revision, 0);

    let output = ser_both(&WithSkipDe { id: 1, revision: 9 });
    assert_eq!(output, r#"{"id":1,"revision":9}"#);
}

#[test]
fn skip_serializing_if_omits_only_matching_values() {
    #[derive(Serialize)]
    struct WithSkipIf {
        id: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        note: Option<String>,
    }

    let omitted = ser_both(&WithSkipIf { id: 1, note: None });
    assert_eq!(omitted, r#"{"id":1}"#);

    let kept = ser_both(&WithSkipIf {
        id: 1,
        note: Some("hi".into()),
    });
    assert_eq!(kept, r#"{"id":1,"note":"hi"}"#);
}

#[test]
fn flatten_collects_unmatched_fields_in_both_directions() {
    #[derive(Serialize, Deserialize, Debug, PartialEq)]
    struct Flat {
        id: u64,
        #[serde(flatten)]
        rest: BTreeMap<String, u32>,
    }

    let input = r#"{"id":1,"a":2,"b":3}"#;
    let value: Flat = de_both(input);
    assert_eq!(value.id, 1);
    assert_eq!(
        value.rest,
        BTreeMap::from([("a".into(), 2), ("b".into(), 3)])
    );
    assert_eq!(ser_both(&value), input);
}

#[test]
fn nested_flatten_distributes_fields_across_levels() {
    #[derive(Serialize, Deserialize, Debug, PartialEq)]
    struct Outer {
        id: u64,
        #[serde(flatten)]
        mid: Middle,
    }

    #[derive(Serialize, Deserialize, Debug, PartialEq)]
    struct Middle {
        name: String,
        #[serde(flatten)]
        rest: BTreeMap<String, bool>,
    }

    let input = r#"{"id":1,"name":"ada","x":true,"y":false}"#;
    let value: Outer = de_both(input);
    assert_eq!(value.id, 1);
    assert_eq!(value.mid.name, "ada");
    assert_eq!(
        value.mid.rest,
        BTreeMap::from([("x".into(), true), ("y".into(), false)])
    );
    assert_eq!(ser_both(&value), input);
}

#[test]
fn flatten_alongside_a_captured_value_field() {
    // Named siblings of a flattened field deserialize in band, so a Value
    // sibling still captures the shared subtree; only the flattened remainder
    // goes through serde's content buffering.
    #[derive(Deserialize)]
    struct Envelope {
        payload: apollo_json::Value,
        #[serde(flatten)]
        rest: BTreeMap<String, u32>,
    }

    #[derive(Deserialize)]
    struct ReferenceEnvelope {
        payload: serde_json::Value,
        #[serde(flatten)]
        rest: BTreeMap<String, u32>,
    }

    let input = r#"{"payload":{"n":1.50e2,"s":"A"},"a":2}"#;
    let doc = parse(input);
    let ours: Envelope = from_value(&doc).expect("apollo-json deserializes");
    let reference: ReferenceEnvelope =
        serde_json::from_str(input).expect("serde_json deserializes");

    assert_eq!(ours.rest, reference.rest);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&ours.payload.to_string())
            .expect("captured payload reparses"),
        reference.payload
    );
    // Raw literal and escape spellings survive only when the subtree is
    // shared by reference, never through a rebuild.
    assert_eq!(ours.payload.to_vec(), br#"{"n":1.50e2,"s":"A"}"#);
}

#[test]
fn deny_unknown_fields_rejects_extra_keys_identically() {
    #[derive(Deserialize, Debug, PartialEq)]
    #[serde(deny_unknown_fields)]
    struct Strict {
        id: u64,
        name: String,
    }

    let value: Strict = de_both(r#"{"id":7,"name":"ada"}"#);
    assert_eq!(value.name, "ada");

    let bad = r#"{"id":7,"name":"ada","extra":true}"#;
    let ours = from_str::<Strict>(bad).unwrap_err();
    let reference = serde_json::from_str::<Strict>(bad).unwrap_err();
    let expected = "unknown field `extra`, expected `id` or `name`";
    assert!(ours.to_string().contains(expected), "{ours}");
    assert!(reference.to_string().contains(expected), "{reference}");
}

#[test]
fn transparent_wrappers_delegate_to_the_inner_type() {
    #[derive(Serialize, Deserialize, Debug, PartialEq)]
    #[serde(transparent)]
    struct UserId {
        value: u64,
    }

    #[derive(Serialize, Deserialize, Debug, PartialEq)]
    struct Record {
        id: UserId,
    }

    let root: UserId = de_both("7");
    assert_eq!(root, UserId { value: 7 });
    assert_eq!(ser_both(&root), "7");

    let input = r#"{"id":7}"#;
    let nested: Record = de_both(input);
    assert_eq!(nested.id, UserId { value: 7 });
    assert_eq!(ser_both(&nested), input);
}

#[test]
fn borrowed_str_fields_point_into_the_document_input() {
    #[derive(Deserialize, Debug, PartialEq)]
    struct Session<'a> {
        #[serde(borrow)]
        token: &'a str,
        #[serde(borrow)]
        name: Cow<'a, str>,
        id: u64,
    }

    let input = r#"{"token":"abc123","name":"ada","id":7}"#;
    let doc = parse(input);
    let ours: Session<'_> = from_value(&doc).expect("apollo-json deserializes");
    let reference: Session<'_> = serde_json::from_str(input).expect("serde_json deserializes");
    assert_eq!(ours, reference);

    // The document's own view of an escape-free string borrows the arena
    // input; the deserialized &str must be that exact span, not a copy.
    let Cow::Borrowed(token_span) = doc.value().get("token").unwrap().as_str().unwrap() else {
        panic!("escape-free string should borrow the input");
    };
    assert_eq!(ours.token.as_ptr(), token_span.as_ptr());
    assert_eq!(ours.token.len(), token_span.len());
    assert!(matches!(ours.name, Cow::Borrowed(_)));
    let Cow::Borrowed(name_span) = doc.value().get("name").unwrap().as_str().unwrap() else {
        panic!("escape-free string should borrow the input");
    };
    assert_eq!(ours.name.as_ptr(), name_span.as_ptr());
}
