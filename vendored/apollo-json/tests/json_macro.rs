//! `json!` macro coverage: literal shapes, interpolation, and adoption.

use apollo_json::{Value, json};

fn text(value: &Value) -> String {
    value.to_string()
}

#[test]
fn scalar_roots() {
    assert_eq!(text(&json!(null)), "null");
    assert_eq!(text(&json!(true)), "true");
    assert_eq!(text(&json!(false)), "false");
    assert_eq!(text(&json!(1)), "1");
    assert_eq!(text(&json!(-7)), "-7");
    assert_eq!(text(&json!(2.5)), "2.5");
    assert_eq!(text(&json!("hi")), r#""hi""#);
    assert_eq!(
        text(&json!(18446744073709551615u64)),
        "18446744073709551615"
    );
}

#[test]
fn container_roots_and_nesting() {
    assert_eq!(text(&json!([])), "[]");
    assert_eq!(text(&json!({})), "{}");
    assert_eq!(
        text(&json!([1, "two", null, true, [3], {"four": 4}])),
        r#"[1,"two",null,true,[3],{"four":4}]"#
    );
    assert_eq!(
        text(&json!({
            "a": {"deep": {"deeper": [null, {}]}},
            "b": [[], [[false]]]
        })),
        r#"{"a":{"deep":{"deeper":[null,{}]}},"b":[[],[[false]]]}"#
    );
}

#[test]
fn trailing_commas_are_accepted() {
    assert_eq!(text(&json!([1, 2,])), "[1,2]");
    assert_eq!(text(&json!({"a": 1, "b": 2,})), r#"{"a":1,"b":2}"#);
    assert_eq!(
        text(&json!({"a": [1,], "b": {"c": null,},})),
        r#"{"a":[1],"b":{"c":null}}"#
    );
}

#[test]
fn expressions_interpolate_in_value_position() {
    let n = 20i64;
    let name = String::from("ada");
    let borrowed: &str = "borrowed";
    let maybe: Option<i64> = None;
    assert_eq!(
        text(&json!({
            "sum": n + 22,
            "name": name.clone(),
            "borrowed": borrowed,
            "call": format!("{name}!"),
            "opt": maybe,
            "some": Some(borrowed),
            "arm": if n > 0 { "pos" } else { "neg" },
        })),
        r#"{"sum":42,"name":"ada","borrowed":"borrowed","call":"ada!","opt":null,"some":"borrowed","arm":"pos"}"#
    );
    assert_eq!(text(&json!([n, n])), "[20,20]");
    assert_eq!(text(&json!(n * 2)), "40");
}

#[test]
fn keys_are_string_expressions() {
    let owned = String::from("owned");
    let borrowed = String::from("borrowed");
    let index = 3;
    assert_eq!(
        text(&json!({
            "literal": 1,
            (borrowed.as_str()): 2,
            (owned): 3,
            (format!("k{index}")): 4,
        })),
        r#"{"literal":1,"borrowed":2,"owned":3,"k3":4}"#
    );
}

#[test]
fn duplicate_keys_collapse_like_parsing() {
    assert_eq!(
        text(&json!({"a": 1, "b": 2, "a": 3})),
        r#"{"a":3,"b":2}"#,
        "first position, last value — what parsing the same text produces"
    );
}

#[test]
fn value_handles_are_adopted_by_reference() {
    let source = apollo_json::Value::parse(br#"{"payload":{"n":1.50}}"#.to_vec()).unwrap();
    let payload = source.get("payload").unwrap();
    let composed = json!({"data": payload, "ok": true});
    drop(source);
    // The raw literal spelling survives: the subtree was adopted, not copied.
    assert_eq!(text(&composed), r#"{"data":{"n":1.50},"ok":true}"#);

    // A handle at the root comes back as-is.
    let root = composed.clone();
    assert_eq!(text(&json!(root)), text(&composed));
}

#[test]
fn macro_invocations_nest() {
    let inner = json!([1, 2]);
    assert_eq!(
        text(&json!({"outer": json!({"inner": inner})})),
        r#"{"outer":{"inner":[1,2]}}"#
    );
}

#[test]
fn non_finite_floats_write_null() {
    assert_eq!(
        text(&json!({"nan": f64::NAN, "inf": [f64::INFINITY, f64::NEG_INFINITY]})),
        r#"{"nan":null,"inf":[null,null]}"#,
        "matches what serde_json::json! writes"
    );
}

#[test]
fn output_matches_parsing_the_same_text() {
    let built = json!({
        "id": 7,
        "tags": ["a", "b"],
        "meta": {"empty": {}, "none": null, "xs": [0.5, false]}
    });
    let parsed = apollo_json::Value::parse(built.to_vec()).unwrap();
    assert_eq!(built.value(), parsed.value(), "round-trips structurally");
    assert_eq!(built.to_vec(), parsed.to_vec());
}

/// The result is a plain self-contained document unless a handle was adopted.
#[test]
fn literal_only_results_are_self_contained() {
    assert!(json!({"a": [1]}).is_self_contained());

    let adopted = json!({"a": Value::from("x")});
    assert!(!adopted.is_self_contained());
}
