use insta::assert_debug_snapshot;
use serde_json_bytes::json;

use super::*;
use crate::selection;

#[test]
fn test_echo_method() {
    assert_eq!(
        selection!("$->echo('oyez')").apply_to(&json!(null)),
        (Some(json!("oyez")), vec![]),
    );

    assert_eq!(
        selection!("$->echo('oyez')").apply_to(&json!([1, 2, 3])),
        (Some(json!("oyez")), vec![]),
    );

    assert_eq!(
        selection!("$->echo([1, 2, 3]) { id: $ }").apply_to(&json!(null)),
        (Some(json!([{ "id": 1 }, { "id": 2 }, { "id": 3 }])), vec![]),
    );

    assert_eq!(
        selection!("$->echo([1, 2, 3])->last { id: $ }").apply_to(&json!(null)),
        (Some(json!({ "id": 3 })), vec![]),
    );

    assert_eq!(
        selection!("$->echo([1.1, 0.2, -3.3]) { id: $ }").apply_to(&json!(null)),
        (
            Some(json!([{ "id": 1.1 }, { "id": 0.2 }, { "id": -3.3 }])),
            vec![]
        ),
    );

    assert_eq!(
        selection!("$.nested.value->echo(['before', @, 'after'])").apply_to(&json!({
            "nested": {
                "value": 123,
            },
        })),
        (Some(json!(["before", 123, "after"])), vec![]),
    );

    assert_eq!(
        selection!("$.nested.value->echo(['before', $, 'after'])").apply_to(&json!({
            "nested": {
                "value": 123,
            },
        })),
        (
            Some(json!(["before", {
            "nested": {
                "value": 123,
            },
        }, "after"])),
            vec![]
        ),
    );

    assert_eq!(
        selection!("data->echo(@.results->last)").apply_to(&json!({
            "data": {
                "results": [1, 2, 3],
            },
        })),
        (Some(json!(3)), vec![]),
    );

    assert_eq!(
        selection!("results->echo(@->first)").apply_to(&json!({
            "results": [
                [1, 2, 3],
                "ignored",
            ],
        })),
        (Some(json!([1, 2, 3])), vec![]),
    );

    assert_eq!(
        selection!("results->echo(@->first)->last").apply_to(&json!({
            "results": [
                [1, 2, 3],
                "ignored",
            ],
        })),
        (Some(json!(3)), vec![]),
    );

    {
        let nested_value_data = json!({
            "nested": {
                "value": 123,
            },
        });

        let expected = (Some(json!({ "wrapped": 123 })), vec![]);

        let check = |selection: &str| {
            assert_eq!(selection!(selection).apply_to(&nested_value_data), expected,);
        };

        check("nested.value->echo({ wrapped: @ })");
        check("nested.value->echo({ wrapped: @,})");
        check("nested.value->echo({ wrapped: @,},)");
        check("nested.value->echo({ wrapped: @},)");
        check("nested.value->echo({ wrapped: @ , } , )");
    }

    // Turn a list of { name, hobby } objects into a single { names: [...],
    // hobbies: [...] } object.
    assert_eq!(
        selection!(
            r#"
            people->echo({
                names: @.name,
                hobbies: @.hobby,
            })
            "#
        )
        .apply_to(&json!({
            "people": [
                { "name": "Alice", "hobby": "reading" },
                { "name": "Bob", "hobby": "fishing" },
                { "hobby": "painting", "name": "Charlie" },
            ],
        })),
        (
            Some(json!({
                "names": ["Alice", "Bob", "Charlie"],
                "hobbies": ["reading", "fishing", "painting"],
            })),
            vec![],
        ),
    );
}

#[test]
fn test_typeof_method() {
    fn check(selection: &str, data: &JSON, expected_type: &str) {
        assert_eq!(
            selection!(selection).apply_to(data),
            (Some(json!(expected_type)), vec![]),
        );
    }

    check("$->typeof", &json!(null), "null");
    check("$->typeof", &json!(true), "boolean");
    check("@->typeof", &json!(false), "boolean");
    check("$->typeof", &json!(123), "number");
    check("$->typeof", &json!(123.45), "number");
    check("$->typeof", &json!("hello"), "string");
    check("$->typeof", &json!([1, 2, 3]), "array");
    check("$->typeof", &json!({ "key": "value" }), "object");
}

#[test]
fn test_map_method() {
    assert_eq!(
        selection!("$->map(@->add(10))").apply_to(&json!([1, 2, 3])),
        (Some(json!(vec![11, 12, 13])), vec![]),
    );

    assert_eq!(
        selection!("messages->map(@.role)").apply_to(&json!({
            "messages": [
                { "role": "admin" },
                { "role": "user" },
                { "role": "guest" },
            ],
        })),
        (Some(json!(["admin", "user", "guest"])), vec![]),
    );

    assert_eq!(
        selection!("messages->map(@.roles)").apply_to(&json!({
            "messages": [
                { "roles": ["admin"] },
                { "roles": ["user", "guest"] },
            ],
        })),
        (Some(json!([["admin"], ["user", "guest"]])), vec![]),
    );

    assert_eq!(
        selection!("values->map(@->typeof)").apply_to(&json!({
            "values": [1, 2.5, "hello", true, null, [], {}],
        })),
        (
            Some(json!([
                "number", "number", "string", "boolean", "null", "array", "object"
            ])),
            vec![],
        ),
    );

    assert_eq!(
        selection!("singleValue->map(@->mul(10))").apply_to(&json!({
            "singleValue": 123,
        })),
        (Some(json!(1230)), vec![]),
    );
}

#[test]
fn test_missing_method() {
    assert_eq!(
        selection!("nested.path->bogus").apply_to(&json!({
            "nested": {
                "path": 123,
            },
        })),
        (
            None,
            vec![ApplyToError::from_json(&json!({
                "message": "Method ->bogus not found",
                "path": ["nested", "path", "->bogus"],
                "range": [13, 18],
            }))],
        ),
    );
}

#[test]
fn test_match_methods() {
    assert_eq!(
        selection!(
            r#"
            name
            __typename: kind->match(
                ['dog', 'Canine'],
                ['cat', 'Feline']
            )
            "#
        )
        .apply_to(&json!({
            "kind": "cat",
            "name": "Whiskers",
        })),
        (
            Some(json!({
                "__typename": "Feline",
                "name": "Whiskers",
            })),
            vec![],
        ),
    );

    assert_eq!(
        selection!(
            r#"
            name
            __typename: kind->match(
                ['dog', 'Canine'],
                ['cat', 'Feline'],
                [@, 'Exotic'],
            )
            "#
        )
        .apply_to(&json!({
            "kind": "axlotl",
            "name": "Gulpy",
        })),
        (
            Some(json!({
                "__typename": "Exotic",
                "name": "Gulpy",
            })),
            vec![],
        ),
    );

    assert_eq!(
        selection!(
            r#"
            name
            __typename: kind->match(
                ['dog', 'Canine'],
                ['cat', 'Feline'],
                [@, 'Exotic'],
            )
            "#
        )
        .apply_to(&json!({
            "kind": "dog",
            "name": "Laika",
        })),
        (
            Some(json!({
                "__typename": "Canine",
                "name": "Laika",
            })),
            vec![],
        ),
    );

    assert_eq!(
        selection!(
            r#"
            num: value->matchIf(
                [@->typeof->eq('number'), @],
                [true, 'not a number']
            )
            "#
        )
        .apply_to(&json!({ "value": 123 })),
        (
            Some(json!({
                "num": 123,
            })),
            vec![],
        ),
    );

    assert_eq!(
        selection!(
            r#"
            num: value->matchIf(
                [@->typeof->eq('number'), @],
                [true, 'not a number']
            )
            "#
        )
        .apply_to(&json!({ "value": true })),
        (
            Some(json!({
                "num": "not a number",
            })),
            vec![],
        ),
    );

    assert_eq!(
        selection!(
            r#"
            result->matchIf(
                [@->typeof->eq('boolean'), @],
                [true, 'not boolean']
            )
            "#
        )
        .apply_to(&json!({
            "result": true,
        })),
        (Some(json!(true)), vec![]),
    );

    assert_eq!(
        selection!(
            r#"
            result->match_if(
                [@->typeof->eq('boolean'), @],
                [true, 'not boolean']
            )
            "#
        )
        .apply_to(&json!({
            "result": 321,
        })),
        (Some(json!("not boolean")), vec![]),
    );
}

#[test]
fn test_arithmetic_methods() {
    assert_eq!(
        selection!("$->add(1)").apply_to(&json!(2)),
        (Some(json!(3)), vec![]),
    );
    assert_eq!(
        selection!("$->add(1.5)").apply_to(&json!(2)),
        (Some(json!(3.5)), vec![]),
    );
    assert_eq!(
        selection!("$->add(1)").apply_to(&json!(2.5)),
        (Some(json!(3.5)), vec![]),
    );
    assert_eq!(
        selection!("$->add(1, 2, 3, 5, 8)").apply_to(&json!(1)),
        (Some(json!(20)), vec![]),
    );

    assert_eq!(
        selection!("$->sub(1)").apply_to(&json!(2)),
        (Some(json!(1)), vec![]),
    );
    assert_eq!(
        selection!("$->sub(1.5)").apply_to(&json!(2)),
        (Some(json!(0.5)), vec![]),
    );
    assert_eq!(
        selection!("$->sub(10)").apply_to(&json!(2.5)),
        (Some(json!(-7.5)), vec![]),
    );
    assert_eq!(
        selection!("$->sub(10, 2.5)").apply_to(&json!(2.5)),
        (Some(json!(-10.0)), vec![]),
    );

    assert_eq!(
        selection!("$->mul(2)").apply_to(&json!(3)),
        (Some(json!(6)), vec![]),
    );
    assert_eq!(
        selection!("$->mul(2.5)").apply_to(&json!(3)),
        (Some(json!(7.5)), vec![]),
    );
    assert_eq!(
        selection!("$->mul(2)").apply_to(&json!(3.5)),
        (Some(json!(7.0)), vec![]),
    );
    assert_eq!(
        selection!("$->mul(-2.5)").apply_to(&json!(3.5)),
        (Some(json!(-8.75)), vec![]),
    );
    assert_eq!(
        selection!("$->mul(2, 3, 5, 7)").apply_to(&json!(10)),
        (Some(json!(2100)), vec![]),
    );

    assert_eq!(
        selection!("$->div(2)").apply_to(&json!(6)),
        (Some(json!(3)), vec![]),
    );
    assert_eq!(
        selection!("$->div(2.5)").apply_to(&json!(7.5)),
        (Some(json!(3.0)), vec![]),
    );
    assert_eq!(
        selection!("$->div(2)").apply_to(&json!(7)),
        (Some(json!(3)), vec![]),
    );
    assert_eq!(
        selection!("$->div(2.5)").apply_to(&json!(7)),
        (Some(json!(2.8)), vec![]),
    );
    assert_eq!(
        selection!("$->div(2, 3, 5, 7)").apply_to(&json!(2100)),
        (Some(json!(10)), vec![]),
    );

    assert_eq!(
        selection!("$->mod(2)").apply_to(&json!(6)),
        (Some(json!(0)), vec![]),
    );
    assert_eq!(
        selection!("$->mod(2.5)").apply_to(&json!(7.5)),
        (Some(json!(0.0)), vec![]),
    );
    assert_eq!(
        selection!("$->mod(2)").apply_to(&json!(7)),
        (Some(json!(1)), vec![]),
    );
    assert_eq!(
        selection!("$->mod(4)").apply_to(&json!(7)),
        (Some(json!(3)), vec![]),
    );
    assert_eq!(
        selection!("$->mod(2.5)").apply_to(&json!(7)),
        (Some(json!(2.0)), vec![]),
    );
    assert_eq!(
        selection!("$->mod(2, 3, 5, 7)").apply_to(&json!(2100)),
        (Some(json!(0)), vec![]),
    );
}

#[test]
fn test_array_methods() {
    assert_eq!(
        selection!("$->first").apply_to(&json!([1, 2, 3])),
        (Some(json!(1)), vec![]),
    );

    assert_eq!(selection!("$->first").apply_to(&json!([])), (None, vec![]),);

    assert_eq!(
        selection!("$->last").apply_to(&json!([1, 2, 3])),
        (Some(json!(3)), vec![]),
    );

    assert_eq!(selection!("$->last").apply_to(&json!([])), (None, vec![]),);

    assert_eq!(
        selection!("$->get(1)").apply_to(&json!([1, 2, 3])),
        (Some(json!(2)), vec![]),
    );

    assert_eq!(
        selection!("$->get(-1)").apply_to(&json!([1, 2, 3])),
        (Some(json!(3)), vec![]),
    );

    assert_eq!(
        selection!("numbers->map(@->get(-2))").apply_to(&json!({
            "numbers": [
                [1, 2, 3],
                [5, 6],
            ],
        })),
        (Some(json!([2, 5])), vec![]),
    );

    assert_eq!(
        selection!("$->get(3)").apply_to(&json!([1, 2, 3])),
        (
            None,
            vec![ApplyToError::from_json(&json!({
                "message": "Method ->get(3) index out of bounds",
                "path": ["->get"],
                "range": [7, 8],
            }))]
        ),
    );

    assert_eq!(
        selection!("$->get(-4)").apply_to(&json!([1, 2, 3])),
        (
            None,
            vec![ApplyToError::from_json(&json!({
                "message": "Method ->get(-4) index out of bounds",
                "path": ["->get"],
                "range": [7, 9],
            }))]
        ),
    );

    assert_eq!(
        selection!("$->get").apply_to(&json!([1, 2, 3])),
        (
            None,
            vec![ApplyToError::from_json(&json!({
                "message": "Method ->get requires an argument",
                "path": ["->get"],
                "range": [3, 6],
            }))]
        ),
    );

    assert_eq!(
        selection!("$->get('bogus')").apply_to(&json!([1, 2, 3])),
        (
            None,
            vec![ApplyToError::from_json(&json!({
                "message": "Method ->get(\"bogus\") requires an object input",
                "path": ["->get"],
                "range": [3, 15],
            }))]
        ),
    );

    assert_eq!(
        selection!("$->has(1)").apply_to(&json!([1, 2, 3])),
        (Some(json!(true)), vec![]),
    );

    assert_eq!(
        selection!("$->has(5)").apply_to(&json!([1, 2, 3])),
        (Some(json!(false)), vec![]),
    );

    assert_eq!(
        selection!("$->slice(1, 3)").apply_to(&json!([1, 2, 3, 4, 5])),
        (Some(json!([2, 3])), vec![]),
    );

    assert_eq!(
        selection!("$->slice(1, 3)").apply_to(&json!([1, 2])),
        (Some(json!([2])), vec![]),
    );

    assert_eq!(
        selection!("$->slice(1, 3)").apply_to(&json!([1])),
        (Some(json!([])), vec![]),
    );

    assert_eq!(
        selection!("$->slice(1, 3)").apply_to(&json!([])),
        (Some(json!([])), vec![]),
    );

    assert_eq!(
        selection!("$->size").apply_to(&json!([])),
        (Some(json!(0)), vec![]),
    );

    assert_eq!(
        selection!("$->size").apply_to(&json!([1, 2, 3])),
        (Some(json!(3)), vec![]),
    );
}

#[test]
fn test_size_method_errors() {
    assert_eq!(
        selection!("$->size").apply_to(&json!(null)),
        (
            None,
            vec![ApplyToError::from_json(&json!({
                "message": "Method ->size requires an array, string, or object input, not null",
                "path": ["->size"],
                "range": [3, 7],
            }))]
        ),
    );

    assert_eq!(
        selection!("$->size").apply_to(&json!(true)),
        (
            None,
            vec![ApplyToError::from_json(&json!({
                "message": "Method ->size requires an array, string, or object input, not boolean",
                "path": ["->size"],
                "range": [3, 7],
            }))]
        ),
    );

    assert_eq!(
        selection!("count->size").apply_to(&json!({
            "count": 123,
        })),
        (
            None,
            vec![ApplyToError::from_json(&json!({
                "message": "Method ->size requires an array, string, or object input, not number",
                "path": ["count", "->size"],
                "range": [7, 11],
            }))]
        ),
    );
}

#[test]
fn test_string_methods() {
    assert_eq!(
        selection!("$->has(2)").apply_to(&json!("oyez")),
        (Some(json!(true)), vec![]),
    );

    assert_eq!(
        selection!("$->has(-2)").apply_to(&json!("oyez")),
        (Some(json!(true)), vec![]),
    );

    assert_eq!(
        selection!("$->has(10)").apply_to(&json!("oyez")),
        (Some(json!(false)), vec![]),
    );

    assert_eq!(
        selection!("$->has(-10)").apply_to(&json!("oyez")),
        (Some(json!(false)), vec![]),
    );

    assert_eq!(
        selection!("$->first").apply_to(&json!("hello")),
        (Some(json!("h")), vec![]),
    );

    assert_eq!(
        selection!("$->last").apply_to(&json!("hello")),
        (Some(json!("o")), vec![]),
    );

    assert_eq!(
        selection!("$->get(2)").apply_to(&json!("oyez")),
        (Some(json!("e")), vec![]),
    );

    assert_eq!(
        selection!("$->get(-1)").apply_to(&json!("oyez")),
        (Some(json!("z")), vec![]),
    );

    assert_eq!(
        selection!("$->get(3)").apply_to(&json!("oyez")),
        (Some(json!("z")), vec![]),
    );

    assert_eq!(
        selection!("$->get(4)").apply_to(&json!("oyez")),
        (
            None,
            vec![ApplyToError::from_json(&json!({
                "message": "Method ->get(4) index out of bounds",
                "path": ["->get"],
                "range": [7, 8],
            }))]
        ),
    );

    {
        let expected = (
            None,
            vec![ApplyToError::from_json(&json!({
                "message": "Method ->get(-10) index out of bounds",
                "path": ["->get"],
                "range": [7, 26],
            }))],
        );
        assert_eq!(
            selection!("$->get($->echo(-5)->mul(2))").apply_to(&json!("oyez")),
            expected.clone(),
        );
        assert_eq!(
            // The extra spaces here should not affect the error.range, as long
            // as we don't accidentally capture trailing spaces in the range.
            selection!("$->get($->echo(-5)->mul(2)  )").apply_to(&json!("oyez")),
            expected.clone(),
        );
        // All these extra spaces certainly do affect the error.range, but it's
        // worth testing that we get all the ranges right, even with so much
        // space that could be accidentally captured.
        let selection_with_spaces = selection!(" $ -> get ( $ -> echo ( - 5 ) -> mul ( 2 ) ) ");
        assert_eq!(
            selection_with_spaces.apply_to(&json!("oyez")),
            (
                None,
                vec![ApplyToError::from_json(&json!({
                    "message": "Method ->get(-10) index out of bounds",
                    "path": ["->get"],
                    "range": [12, 42],
                }))]
            )
        );
        assert_debug_snapshot!(selection_with_spaces);
    }

    assert_eq!(
        selection!("$->get(true)").apply_to(&json!("input")),
        (
            None,
            vec![ApplyToError::from_json(&json!({
                "message": "Method ->get(true) requires an integer or string argument",
                "path": ["->get"],
                "range": [7, 11],
            }))]
        ),
    );

    assert_eq!(
        selection!("$->slice(1, 3)").apply_to(&json!("")),
        (Some(json!("")), vec![]),
    );

    assert_eq!(
        selection!("$->slice(1, 3)").apply_to(&json!("hello")),
        (Some(json!("el")), vec![]),
    );

    assert_eq!(
        selection!("$->slice(1, 3)").apply_to(&json!("he")),
        (Some(json!("e")), vec![]),
    );

    assert_eq!(
        selection!("$->slice(1, 3)").apply_to(&json!("h")),
        (Some(json!("")), vec![]),
    );

    assert_eq!(
        selection!("$->size").apply_to(&json!("hello")),
        (Some(json!(5)), vec![]),
    );

    assert_eq!(
        selection!("$->size").apply_to(&json!("")),
        (Some(json!(0)), vec![]),
    );
}

#[test]
fn test_object_methods() {
    assert_eq!(
        selection!("object->has('a')").apply_to(&json!({
            "object": {
                "a": 123,
                "b": 456,
            },
        })),
        (Some(json!(true)), vec![]),
    );

    assert_eq!(
        selection!("object->has('c')").apply_to(&json!({
            "object": {
                "a": 123,
                "b": 456,
            },
        })),
        (Some(json!(false)), vec![]),
    );

    assert_eq!(
        selection!("object->has(true)").apply_to(&json!({
            "object": {
                "a": 123,
                "b": 456,
            },
        })),
        (Some(json!(false)), vec![]),
    );

    assert_eq!(
        selection!("object->has(null)").apply_to(&json!({
            "object": {
                "a": 123,
                "b": 456,
            },
        })),
        (Some(json!(false)), vec![]),
    );

    assert_eq!(
        selection!("object->has('a')->and(object->has('b'))").apply_to(&json!({
            "object": {
                "a": 123,
                "b": 456,
            },
        })),
        (Some(json!(true)), vec![]),
    );

    assert_eq!(
        selection!("object->has('b')->and(object->has('c'))").apply_to(&json!({
            "object": {
                "a": 123,
                "b": 456,
            },
        })),
        (Some(json!(false)), vec![]),
    );

    assert_eq!(
        selection!("object->has('xxx')->typeof").apply_to(&json!({
            "object": {
                "a": 123,
                "b": 456,
            },
        })),
        (Some(json!("boolean")), vec![]),
    );

    assert_eq!(
        selection!("$->size").apply_to(&json!({ "a": 1, "b": 2, "c": 3 })),
        (Some(json!(3)), vec![]),
    );

    assert_eq!(
        selection!("$->size").apply_to(&json!({})),
        (Some(json!(0)), vec![]),
    );

    assert_eq!(
        selection!("$->get('a')").apply_to(&json!({
            "a": 1,
            "b": 2,
            "c": 3,
        })),
        (Some(json!(1)), vec![]),
    );

    assert_eq!(
        selection!("$->get('b')").apply_to(&json!({
            "a": 1,
            "b": 2,
            "c": 3,
        })),
        (Some(json!(2)), vec![]),
    );

    assert_eq!(
        selection!("$->get('c')").apply_to(&json!({
            "a": 1,
            "b": 2,
            "c": 3,
        })),
        (Some(json!(3)), vec![]),
    );

    assert_eq!(
        selection!("$->get('d')").apply_to(&json!({
            "a": 1,
            "b": 2,
            "c": 3,
        })),
        (
            None,
            vec![ApplyToError::from_json(&json!({
                "message": "Method ->get(\"d\") object key not found",
                "path": ["->get"],
                "range": [7, 10],
            }))]
        ),
    );

    assert_eq!(
        selection!("$->get('a')->add(10)").apply_to(&json!({
            "a": 1,
            "b": 2,
            "c": 3,
        })),
        (Some(json!(11)), vec![]),
    );

    assert_eq!(
        selection!("$->get('b')->add(10)").apply_to(&json!({
            "a": 1,
            "b": 2,
            "c": 3,
        })),
        (Some(json!(12)), vec![]),
    );

    assert_eq!(
        selection!("$->keys").apply_to(&json!({
            "a": 1,
            "b": 2,
            "c": 3,
        })),
        (Some(json!(["a", "b", "c"])), vec![]),
    );

    assert_eq!(
        selection!("$->keys").apply_to(&json!({})),
        (Some(json!([])), vec![]),
    );

    assert_eq!(
        selection!("notAnObject->keys").apply_to(&json!({
            "notAnObject": 123,
        })),
        (
            None,
            vec![ApplyToError::from_json(&json!({
                "message": "Method ->keys requires an object input, not number",
                "path": ["notAnObject", "->keys"],
                "range": [13, 17],
            }))]
        ),
    );

    assert_eq!(
        selection!("$->values").apply_to(&json!({
            "a": 1,
            "b": "two",
            "c": false,
        })),
        (Some(json!([1, "two", false])), vec![]),
    );

    assert_eq!(
        selection!("$->values").apply_to(&json!({})),
        (Some(json!([])), vec![]),
    );

    assert_eq!(
        selection!("notAnObject->values").apply_to(&json!({
            "notAnObject": null,
        })),
        (
            None,
            vec![ApplyToError::from_json(&json!({
                "message": "Method ->values requires an object input, not null",
                "path": ["notAnObject", "->values"],
                "range": [13, 19],
            }))]
        ),
    );

    assert_eq!(
        selection!("$->entries").apply_to(&json!({
            "a": 1,
            "b": "two",
            "c": false,
        })),
        (
            Some(json!([
                { "key": "a", "value": 1 },
                { "key": "b", "value": "two" },
                { "key": "c", "value": false },
            ])),
            vec![],
        ),
    );

    assert_eq!(
        // This is just like $->keys, given the automatic array mapping of
        // .key, though you probably want to use ->keys directly because it
        // avoids cloning all the values unnecessarily.
        selection!("$->entries.key").apply_to(&json!({
            "one": 1,
            "two": 2,
            "three": 3,
        })),
        (Some(json!(["one", "two", "three"])), vec![]),
    );

    assert_eq!(
        // This is just like $->values, given the automatic array mapping of
        // .value, though you probably want to use ->values directly because
        // it avoids cloning all the keys unnecessarily.
        selection!("$->entries.value").apply_to(&json!({
            "one": 1,
            "two": 2,
            "three": 3,
        })),
        (Some(json!([1, 2, 3])), vec![]),
    );

    assert_eq!(
        selection!("$->entries").apply_to(&json!({})),
        (Some(json!([])), vec![]),
    );

    assert_eq!(
        selection!("notAnObject->entries").apply_to(&json!({
            "notAnObject": true,
        })),
        (
            None,
            vec![ApplyToError::from_json(&json!({
                "message": "Method ->entries requires an object input, not boolean",
                "path": ["notAnObject", "->entries"],
                "range": [13, 20],
            }))]
        ),
    );
}

#[test]
fn test_logical_methods() {
    assert_eq!(
        selection!("$->map(@->not)").apply_to(&json!([
            true,
            false,
            0,
            1,
            -123,
            null,
            "hello",
            {},
            [],
        ])),
        (
            Some(json!([
                false, true, true, false, false, true, false, false, false,
            ])),
            vec![],
        ),
    );

    assert_eq!(
        selection!("$->map(@->not->not)").apply_to(&json!([
            true,
            false,
            0,
            1,
            -123,
            null,
            "hello",
            {},
            [],
        ])),
        (
            Some(json!([
                true, false, false, true, true, false, true, true, true,
            ])),
            vec![],
        ),
    );

    assert_eq!(
        selection!("$.a->and($.b, $.c)").apply_to(&json!({
            "a": true,
            "b": null,
            "c": true,
        })),
        (Some(json!(false)), vec![]),
    );
    assert_eq!(
        selection!("$.b->and($.c, $.a)").apply_to(&json!({
            "a": "hello",
            "b": true,
            "c": 123,
        })),
        (Some(json!(true)), vec![]),
    );
    assert_eq!(
        selection!("$.both->and($.and)").apply_to(&json!({
            "both": true,
            "and": true,
        })),
        (Some(json!(true)), vec![]),
    );
    assert_eq!(
        selection!("data.x->and($.data.y)").apply_to(&json!({
            "data": {
                "x": true,
                "y": false,
            },
        })),
        (Some(json!(false)), vec![]),
    );

    assert_eq!(
        selection!("$.a->or($.b, $.c)").apply_to(&json!({
            "a": true,
            "b": null,
            "c": true,
        })),
        (Some(json!(true)), vec![]),
    );
    assert_eq!(
        selection!("$.b->or($.a, $.c)").apply_to(&json!({
            "a": false,
            "b": null,
            "c": 0,
        })),
        (Some(json!(false)), vec![]),
    );
    assert_eq!(
        selection!("$.both->or($.and)").apply_to(&json!({
            "both": true,
            "and": false,
        })),
        (Some(json!(true)), vec![]),
    );
    assert_eq!(
        selection!("data.x->or($.data.y)").apply_to(&json!({
            "data": {
                "x": false,
                "y": false,
            },
        })),
        (Some(json!(false)), vec![]),
    );
}
