//! Released Rhai JSON compatibility suite.
//!
//! Rhai scripts see JSON-bearing fields (`variables`, `extensions`, `data`, `errors`) as native
//! Rhai `Map`, `Array` and scalar values. Scripts rely on everything those values do: every
//! method and operator of Rhai's standard map and array packages, return values, debug output,
//! error text, key order and when a change is written back to the request or response.
//!
//! The expectations below were captured by running this suite against the source of Router
//! v2.17.0 (`c68f255cab723c27a156b73070a25f753a529bb1`, Rhai 1.23.6), the released baseline.
//! Any change to how Router represents or converts these fields, including a Rhai upgrade, must
//! keep this suite passing. A failing row is a scripting compatibility change, not a fixture to
//! refresh: either fix the regression or, for undocumented details only, record the new
//! behavior and its reason in [`ACCEPTED_CHANGES`].
//!
//! [`operation_surface`] and [`value_semantics`] run each case directly against Router's Rhai
//! engine and property accessors. The pipeline tests in [`callbacks`] run representative
//! scripts through the real plugin services at every stage that exposes JSON, including
//! deferred chunks, errors, cancellation and conversion accounting.

use rhai::Dynamic;
use rhai::Scope;

use super::new_rhai_test_engine;
use crate::graphql;

mod callbacks;

/// Response data used by collection cases. Keys are deliberately not in sorted order: Rhai maps
/// are sorted, so writing a value back re-orders keys.
const COLLECTIONS: &str = r#"{"users":[{"name":"ann","id":1},{"name":"bob","id":2}],"meta":{"keep":"yes","gone":true},"nums":[3,1,2]}"#;

/// Response data used by scalar conversion cases.
const SCALARS: &str =
    r#"{"null":null,"flag":true,"int":-7,"float":1.5,"big":18446744073709551615,"text":"t"}"#;

/// Request used by request field cases.
const REQUEST: &str =
    r#"{"query":"{ me }","variables":{"b":1,"a":{"y":2,"x":1}},"extensions":{"z":true,"c":[1]}}"#;

/// The object a case's script runs against, named in the script's scope.
#[derive(Clone, Copy, Debug)]
enum Subject {
    /// `response`: a response whose `data` is [`COLLECTIONS`].
    Collections,
    /// `response`: a response whose `data` is [`SCALARS`].
    Scalars,
    /// `request`: the request [`REQUEST`].
    Request,
}

struct Case {
    name: &'static str,
    subject: Subject,
    script: &'static str,
    /// Debug output of the script's value, or the error's display text.
    result: Result<&'static str, &'static str>,
    /// The subject serialized after the script ran.
    after: &'static str,
}

/// A case which applies one expression to a local copy of `response.data` and writes it back:
///
/// ```rhai
/// let json = response.data;
/// let result = <expression>;
/// response.data = json;
/// result
/// ```
struct Operation {
    name: &'static str,
    expression: &'static str,
    result: Result<&'static str, &'static str>,
    after: &'static str,
}

/// Every operation family of Rhai 1.23.6's `BasicMapPackage` and `BasicArrayPackage`, with
/// each overload, applied to JSON response data.
const OPERATIONS: &[Operation] = &[
    Operation {
        name: "map_len",
        expression: r#"json.meta.len()"#,
        result: Ok(r#"2"#),
        after: r#"{"data":{"meta":{"gone":true,"keep":"yes"},"nums":[3,1,2],"users":[{"id":1,"name":"ann"},{"id":2,"name":"bob"}]}}"#,
    },
    Operation {
        name: "map_is_empty",
        expression: r#"json.meta.is_empty()"#,
        result: Ok(r#"false"#),
        after: r#"{"data":{"meta":{"gone":true,"keep":"yes"},"nums":[3,1,2],"users":[{"id":1,"name":"ann"},{"id":2,"name":"bob"}]}}"#,
    },
    Operation {
        name: "map_contains",
        expression: r#"json.meta.contains("keep")"#,
        result: Ok(r#"true"#),
        after: r#"{"data":{"meta":{"gone":true,"keep":"yes"},"nums":[3,1,2],"users":[{"id":1,"name":"ann"},{"id":2,"name":"bob"}]}}"#,
    },
    Operation {
        name: "map_in_operator",
        expression: r#""keep" in json.meta"#,
        result: Ok(r#"true"#),
        after: r#"{"data":{"meta":{"gone":true,"keep":"yes"},"nums":[3,1,2],"users":[{"id":1,"name":"ann"},{"id":2,"name":"bob"}]}}"#,
    },
    Operation {
        name: "map_get",
        expression: r#"json.meta.get("keep")"#,
        result: Ok(r#""yes""#),
        after: r#"{"data":{"meta":{"gone":true,"keep":"yes"},"nums":[3,1,2],"users":[{"id":1,"name":"ann"},{"id":2,"name":"bob"}]}}"#,
    },
    Operation {
        name: "map_get_missing",
        expression: r#"json.meta.get("absent")"#,
        result: Ok(r#"()"#),
        after: r#"{"data":{"meta":{"gone":true,"keep":"yes"},"nums":[3,1,2],"users":[{"id":1,"name":"ann"},{"id":2,"name":"bob"}]}}"#,
    },
    Operation {
        name: "map_set",
        expression: r#"json.meta.set("added", 1)"#,
        result: Ok(r#"()"#),
        after: r#"{"data":{"meta":{"added":1,"gone":true,"keep":"yes"},"nums":[3,1,2],"users":[{"id":1,"name":"ann"},{"id":2,"name":"bob"}]}}"#,
    },
    Operation {
        name: "map_clear",
        expression: r#"json.meta.clear()"#,
        result: Ok(r#"()"#),
        after: r#"{"data":{"meta":{},"nums":[3,1,2],"users":[{"id":1,"name":"ann"},{"id":2,"name":"bob"}]}}"#,
    },
    Operation {
        name: "map_remove",
        expression: r#"json.meta.remove("gone")"#,
        result: Ok(r#"true"#),
        after: r#"{"data":{"meta":{"keep":"yes"},"nums":[3,1,2],"users":[{"id":1,"name":"ann"},{"id":2,"name":"bob"}]}}"#,
    },
    Operation {
        name: "map_remove_missing",
        expression: r#"json.meta.remove("absent")"#,
        result: Ok(r#"()"#),
        after: r#"{"data":{"meta":{"gone":true,"keep":"yes"},"nums":[3,1,2],"users":[{"id":1,"name":"ann"},{"id":2,"name":"bob"}]}}"#,
    },
    Operation {
        name: "map_mixin",
        expression: r#"json.meta.mixin(#{ added: 1, keep: "no" })"#,
        result: Ok(r#"()"#),
        after: r#"{"data":{"meta":{"added":1,"gone":true,"keep":"no"},"nums":[3,1,2],"users":[{"id":1,"name":"ann"},{"id":2,"name":"bob"}]}}"#,
    },
    Operation {
        name: "map_add_assign",
        expression: r#"{ json.meta += #{ added: 1 }; json.meta }"#,
        result: Ok(r#"#{"added": 1, "gone": true, "keep": "yes"}"#),
        after: r#"{"data":{"meta":{"added":1,"gone":true,"keep":"yes"},"nums":[3,1,2],"users":[{"id":1,"name":"ann"},{"id":2,"name":"bob"}]}}"#,
    },
    Operation {
        name: "map_merge",
        expression: r#"json.meta + #{ added: 1 }"#,
        result: Ok(r#"#{"added": 1, "gone": true, "keep": "yes"}"#),
        after: r#"{"data":{"meta":{"gone":true,"keep":"yes"},"nums":[3,1,2],"users":[{"id":1,"name":"ann"},{"id":2,"name":"bob"}]}}"#,
    },
    Operation {
        name: "map_fill_with",
        expression: r#"json.meta.fill_with(#{ keep: "no", added: 1 })"#,
        result: Ok(r#"()"#),
        after: r#"{"data":{"meta":{"added":1,"gone":true,"keep":"yes"},"nums":[3,1,2],"users":[{"id":1,"name":"ann"},{"id":2,"name":"bob"}]}}"#,
    },
    Operation {
        name: "map_equals",
        expression: r#"json.meta == #{ gone: true, keep: "yes" }"#,
        result: Ok(r#"true"#),
        after: r#"{"data":{"meta":{"gone":true,"keep":"yes"},"nums":[3,1,2],"users":[{"id":1,"name":"ann"},{"id":2,"name":"bob"}]}}"#,
    },
    Operation {
        name: "map_not_equals",
        expression: r#"json.meta != #{ gone: true, keep: "yes" }"#,
        result: Ok(r#"false"#),
        after: r#"{"data":{"meta":{"gone":true,"keep":"yes"},"nums":[3,1,2],"users":[{"id":1,"name":"ann"},{"id":2,"name":"bob"}]}}"#,
    },
    Operation {
        name: "map_keys",
        expression: r#"json.meta.keys()"#,
        result: Ok(r#"["gone", "keep"]"#),
        after: r#"{"data":{"meta":{"gone":true,"keep":"yes"},"nums":[3,1,2],"users":[{"id":1,"name":"ann"},{"id":2,"name":"bob"}]}}"#,
    },
    Operation {
        name: "map_values",
        expression: r#"json.meta.values()"#,
        result: Ok(r#"[true, "yes"]"#),
        after: r#"{"data":{"meta":{"gone":true,"keep":"yes"},"nums":[3,1,2],"users":[{"id":1,"name":"ann"},{"id":2,"name":"bob"}]}}"#,
    },
    Operation {
        name: "map_map",
        expression: r#"json.meta.map(|key, value| key + "=" + value)"#,
        result: Ok(r#"#{"gone": "gone=true", "keep": "keep=yes"}"#),
        after: r#"{"data":{"meta":{"gone":true,"keep":"yes"},"nums":[3,1,2],"users":[{"id":1,"name":"ann"},{"id":2,"name":"bob"}]}}"#,
    },
    Operation {
        name: "map_filter",
        expression: r#"json.meta.filter(|key, value| key == "keep")"#,
        result: Ok(r#"#{"keep": "yes"}"#),
        after: r#"{"data":{"meta":{"gone":true,"keep":"yes"},"nums":[3,1,2],"users":[{"id":1,"name":"ann"},{"id":2,"name":"bob"}]}}"#,
    },
    Operation {
        name: "map_drain",
        expression: r#"json.meta.drain(|key, value| key == "gone")"#,
        result: Ok(r#"#{"gone": true}"#),
        after: r#"{"data":{"meta":{"keep":"yes"},"nums":[3,1,2],"users":[{"id":1,"name":"ann"},{"id":2,"name":"bob"}]}}"#,
    },
    Operation {
        name: "map_retain",
        expression: r#"json.meta.retain(|key, value| key == "keep")"#,
        result: Ok(r#"#{"gone": true}"#),
        after: r#"{"data":{"meta":{"keep":"yes"},"nums":[3,1,2],"users":[{"id":1,"name":"ann"},{"id":2,"name":"bob"}]}}"#,
    },
    Operation {
        name: "map_to_json",
        expression: r#"json.meta.to_json()"#,
        result: Ok(r#""{\"gone\":true,\"keep\":\"yes\"}""#),
        after: r#"{"data":{"meta":{"gone":true,"keep":"yes"},"nums":[3,1,2],"users":[{"id":1,"name":"ann"},{"id":2,"name":"bob"}]}}"#,
    },
    Operation {
        name: "map_for_keys",
        expression: r#"{ let keys = []; for key in json.meta.keys() { keys.push(key) } keys }"#,
        result: Ok(r#"["gone", "keep"]"#),
        after: r#"{"data":{"meta":{"gone":true,"keep":"yes"},"nums":[3,1,2],"users":[{"id":1,"name":"ann"},{"id":2,"name":"bob"}]}}"#,
    },
    Operation {
        name: "array_len",
        expression: r#"json.nums.len()"#,
        result: Ok(r#"3"#),
        after: r#"{"data":{"meta":{"gone":true,"keep":"yes"},"nums":[3,1,2],"users":[{"id":1,"name":"ann"},{"id":2,"name":"bob"}]}}"#,
    },
    Operation {
        name: "array_len_property",
        expression: r#"json.nums.len"#,
        result: Ok(r#"3"#),
        after: r#"{"data":{"meta":{"gone":true,"keep":"yes"},"nums":[3,1,2],"users":[{"id":1,"name":"ann"},{"id":2,"name":"bob"}]}}"#,
    },
    Operation {
        name: "array_is_empty",
        expression: r#"json.nums.is_empty()"#,
        result: Ok(r#"false"#),
        after: r#"{"data":{"meta":{"gone":true,"keep":"yes"},"nums":[3,1,2],"users":[{"id":1,"name":"ann"},{"id":2,"name":"bob"}]}}"#,
    },
    Operation {
        name: "array_get",
        expression: r#"json.nums.get(0)"#,
        result: Ok(r#"3"#),
        after: r#"{"data":{"meta":{"gone":true,"keep":"yes"},"nums":[3,1,2],"users":[{"id":1,"name":"ann"},{"id":2,"name":"bob"}]}}"#,
    },
    Operation {
        name: "array_get_negative",
        expression: r#"json.nums.get(-1)"#,
        result: Ok(r#"2"#),
        after: r#"{"data":{"meta":{"gone":true,"keep":"yes"},"nums":[3,1,2],"users":[{"id":1,"name":"ann"},{"id":2,"name":"bob"}]}}"#,
    },
    Operation {
        name: "array_get_out_of_bounds",
        expression: r#"json.nums.get(9)"#,
        result: Ok(r#"()"#),
        after: r#"{"data":{"meta":{"gone":true,"keep":"yes"},"nums":[3,1,2],"users":[{"id":1,"name":"ann"},{"id":2,"name":"bob"}]}}"#,
    },
    Operation {
        name: "array_set",
        expression: r#"json.nums.set(0, 9)"#,
        result: Ok(r#"()"#),
        after: r#"{"data":{"meta":{"gone":true,"keep":"yes"},"nums":[9,1,2],"users":[{"id":1,"name":"ann"},{"id":2,"name":"bob"}]}}"#,
    },
    Operation {
        name: "array_push",
        expression: r#"json.nums.push(4)"#,
        result: Ok(r#"()"#),
        after: r#"{"data":{"meta":{"gone":true,"keep":"yes"},"nums":[3,1,2,4],"users":[{"id":1,"name":"ann"},{"id":2,"name":"bob"}]}}"#,
    },
    Operation {
        name: "array_append",
        expression: r#"json.nums.append([4, 5])"#,
        result: Ok(r#"()"#),
        after: r#"{"data":{"meta":{"gone":true,"keep":"yes"},"nums":[3,1,2,4,5],"users":[{"id":1,"name":"ann"},{"id":2,"name":"bob"}]}}"#,
    },
    Operation {
        name: "array_add_assign",
        expression: r#"{ json.nums += [4]; json.nums }"#,
        result: Ok(r#"[3, 1, 2, 4]"#),
        after: r#"{"data":{"meta":{"gone":true,"keep":"yes"},"nums":[3,1,2,4],"users":[{"id":1,"name":"ann"},{"id":2,"name":"bob"}]}}"#,
    },
    Operation {
        name: "array_concat",
        expression: r#"json.nums + [4]"#,
        result: Ok(r#"[3, 1, 2, 4]"#),
        after: r#"{"data":{"meta":{"gone":true,"keep":"yes"},"nums":[3,1,2],"users":[{"id":1,"name":"ann"},{"id":2,"name":"bob"}]}}"#,
    },
    Operation {
        name: "array_insert",
        expression: r#"json.nums.insert(1, 9)"#,
        result: Ok(r#"()"#),
        after: r#"{"data":{"meta":{"gone":true,"keep":"yes"},"nums":[3,9,1,2],"users":[{"id":1,"name":"ann"},{"id":2,"name":"bob"}]}}"#,
    },
    Operation {
        name: "array_pad",
        expression: r#"json.nums.pad(5, 0)"#,
        result: Ok(r#"()"#),
        after: r#"{"data":{"meta":{"gone":true,"keep":"yes"},"nums":[3,1,2,0,0],"users":[{"id":1,"name":"ann"},{"id":2,"name":"bob"}]}}"#,
    },
    Operation {
        name: "array_pop",
        expression: r#"json.nums.pop()"#,
        result: Ok(r#"2"#),
        after: r#"{"data":{"meta":{"gone":true,"keep":"yes"},"nums":[3,1],"users":[{"id":1,"name":"ann"},{"id":2,"name":"bob"}]}}"#,
    },
    Operation {
        name: "array_shift",
        expression: r#"json.nums.shift()"#,
        result: Ok(r#"3"#),
        after: r#"{"data":{"meta":{"gone":true,"keep":"yes"},"nums":[1,2],"users":[{"id":1,"name":"ann"},{"id":2,"name":"bob"}]}}"#,
    },
    Operation {
        name: "array_remove",
        expression: r#"json.users.remove(0)"#,
        result: Ok(r#"#{"id": 1, "name": "ann"}"#),
        after: r#"{"data":{"meta":{"gone":true,"keep":"yes"},"nums":[3,1,2],"users":[{"id":2,"name":"bob"}]}}"#,
    },
    Operation {
        name: "array_clear",
        expression: r#"json.nums.clear()"#,
        result: Ok(r#"()"#),
        after: r#"{"data":{"meta":{"gone":true,"keep":"yes"},"nums":[],"users":[{"id":1,"name":"ann"},{"id":2,"name":"bob"}]}}"#,
    },
    Operation {
        name: "array_truncate",
        expression: r#"json.nums.truncate(1)"#,
        result: Ok(r#"()"#),
        after: r#"{"data":{"meta":{"gone":true,"keep":"yes"},"nums":[3],"users":[{"id":1,"name":"ann"},{"id":2,"name":"bob"}]}}"#,
    },
    Operation {
        name: "array_chop",
        expression: r#"json.nums.chop(1)"#,
        result: Ok(r#"()"#),
        after: r#"{"data":{"meta":{"gone":true,"keep":"yes"},"nums":[2],"users":[{"id":1,"name":"ann"},{"id":2,"name":"bob"}]}}"#,
    },
    Operation {
        name: "array_reverse",
        expression: r#"json.nums.reverse()"#,
        result: Ok(r#"()"#),
        after: r#"{"data":{"meta":{"gone":true,"keep":"yes"},"nums":[2,1,3],"users":[{"id":1,"name":"ann"},{"id":2,"name":"bob"}]}}"#,
    },
    Operation {
        name: "array_splice_range",
        expression: r#"json.nums.splice(0..1, [8, 9])"#,
        result: Ok(r#"()"#),
        after: r#"{"data":{"meta":{"gone":true,"keep":"yes"},"nums":[8,9,1,2],"users":[{"id":1,"name":"ann"},{"id":2,"name":"bob"}]}}"#,
    },
    Operation {
        name: "array_splice_inclusive_range",
        expression: r#"json.nums.splice(0..=1, [])"#,
        result: Ok(r#"()"#),
        after: r#"{"data":{"meta":{"gone":true,"keep":"yes"},"nums":[2],"users":[{"id":1,"name":"ann"},{"id":2,"name":"bob"}]}}"#,
    },
    Operation {
        name: "array_splice_start_len",
        expression: r#"json.nums.splice(1, 1, [7])"#,
        result: Ok(r#"()"#),
        after: r#"{"data":{"meta":{"gone":true,"keep":"yes"},"nums":[3,7,2],"users":[{"id":1,"name":"ann"},{"id":2,"name":"bob"}]}}"#,
    },
    Operation {
        name: "array_extract_range",
        expression: r#"json.nums.extract(0..2)"#,
        result: Ok(r#"[3, 1]"#),
        after: r#"{"data":{"meta":{"gone":true,"keep":"yes"},"nums":[3,1,2],"users":[{"id":1,"name":"ann"},{"id":2,"name":"bob"}]}}"#,
    },
    Operation {
        name: "array_extract_inclusive_range",
        expression: r#"json.nums.extract(0..=1)"#,
        result: Ok(r#"[3, 1]"#),
        after: r#"{"data":{"meta":{"gone":true,"keep":"yes"},"nums":[3,1,2],"users":[{"id":1,"name":"ann"},{"id":2,"name":"bob"}]}}"#,
    },
    Operation {
        name: "array_extract_start_len",
        expression: r#"json.nums.extract(1, 2)"#,
        result: Ok(r#"[1, 2]"#),
        after: r#"{"data":{"meta":{"gone":true,"keep":"yes"},"nums":[3,1,2],"users":[{"id":1,"name":"ann"},{"id":2,"name":"bob"}]}}"#,
    },
    Operation {
        name: "array_extract_tail",
        expression: r#"json.nums.extract(1)"#,
        result: Ok(r#"[1, 2]"#),
        after: r#"{"data":{"meta":{"gone":true,"keep":"yes"},"nums":[3,1,2],"users":[{"id":1,"name":"ann"},{"id":2,"name":"bob"}]}}"#,
    },
    Operation {
        name: "array_split",
        expression: r#"json.nums.split(1)"#,
        result: Ok(r#"[1, 2]"#),
        after: r#"{"data":{"meta":{"gone":true,"keep":"yes"},"nums":[3],"users":[{"id":1,"name":"ann"},{"id":2,"name":"bob"}]}}"#,
    },
    Operation {
        name: "array_for_each",
        expression: r#"json.users.for_each(|| this.id += 10)"#,
        result: Ok(r#"()"#),
        after: r#"{"data":{"meta":{"gone":true,"keep":"yes"},"nums":[3,1,2],"users":[{"id":11,"name":"ann"},{"id":12,"name":"bob"}]}}"#,
    },
    Operation {
        name: "array_map",
        expression: r#"json.users.map(|user| user.name)"#,
        result: Ok(r#"["ann", "bob"]"#),
        after: r#"{"data":{"meta":{"gone":true,"keep":"yes"},"nums":[3,1,2],"users":[{"id":1,"name":"ann"},{"id":2,"name":"bob"}]}}"#,
    },
    Operation {
        name: "array_map_with_index",
        expression: r#"json.users.map(|user, index| index)"#,
        result: Ok(r#"[0, 1]"#),
        after: r#"{"data":{"meta":{"gone":true,"keep":"yes"},"nums":[3,1,2],"users":[{"id":1,"name":"ann"},{"id":2,"name":"bob"}]}}"#,
    },
    Operation {
        name: "array_filter",
        expression: r#"json.users.filter(|user| user.id > 1)"#,
        result: Ok(r#"[#{"id": 2, "name": "bob"}]"#),
        after: r#"{"data":{"meta":{"gone":true,"keep":"yes"},"nums":[3,1,2],"users":[{"id":1,"name":"ann"},{"id":2,"name":"bob"}]}}"#,
    },
    Operation {
        name: "array_contains",
        expression: r#"json.nums.contains(1)"#,
        result: Ok(r#"true"#),
        after: r#"{"data":{"meta":{"gone":true,"keep":"yes"},"nums":[3,1,2],"users":[{"id":1,"name":"ann"},{"id":2,"name":"bob"}]}}"#,
    },
    Operation {
        name: "array_in_operator",
        expression: r#"2 in json.nums"#,
        result: Ok(r#"true"#),
        after: r#"{"data":{"meta":{"gone":true,"keep":"yes"},"nums":[3,1,2],"users":[{"id":1,"name":"ann"},{"id":2,"name":"bob"}]}}"#,
    },
    Operation {
        name: "array_index_of",
        expression: r#"json.nums.index_of(1)"#,
        result: Ok(r#"1"#),
        after: r#"{"data":{"meta":{"gone":true,"keep":"yes"},"nums":[3,1,2],"users":[{"id":1,"name":"ann"},{"id":2,"name":"bob"}]}}"#,
    },
    Operation {
        name: "array_index_of_start",
        expression: r#"json.nums.index_of(3, 1)"#,
        result: Ok(r#"-1"#),
        after: r#"{"data":{"meta":{"gone":true,"keep":"yes"},"nums":[3,1,2],"users":[{"id":1,"name":"ann"},{"id":2,"name":"bob"}]}}"#,
    },
    Operation {
        name: "array_index_of_predicate",
        expression: r#"json.users.index_of(|user| user.name == "bob")"#,
        result: Ok(r#"1"#),
        after: r#"{"data":{"meta":{"gone":true,"keep":"yes"},"nums":[3,1,2],"users":[{"id":1,"name":"ann"},{"id":2,"name":"bob"}]}}"#,
    },
    Operation {
        name: "array_index_of_predicate_start",
        expression: r#"json.users.index_of(|user| user.id > 0, 1)"#,
        result: Ok(r#"1"#),
        after: r#"{"data":{"meta":{"gone":true,"keep":"yes"},"nums":[3,1,2],"users":[{"id":1,"name":"ann"},{"id":2,"name":"bob"}]}}"#,
    },
    Operation {
        name: "array_find",
        expression: r#"json.users.find(|user| user.id == 2)"#,
        result: Ok(r#"#{"id": 2, "name": "bob"}"#),
        after: r#"{"data":{"meta":{"gone":true,"keep":"yes"},"nums":[3,1,2],"users":[{"id":1,"name":"ann"},{"id":2,"name":"bob"}]}}"#,
    },
    Operation {
        name: "array_find_start",
        expression: r#"json.users.find(|user| user.id > 0, 1)"#,
        result: Ok(r#"#{"id": 2, "name": "bob"}"#),
        after: r#"{"data":{"meta":{"gone":true,"keep":"yes"},"nums":[3,1,2],"users":[{"id":1,"name":"ann"},{"id":2,"name":"bob"}]}}"#,
    },
    Operation {
        name: "array_find_map",
        expression: r#"json.users.find_map(|user| user.name)"#,
        result: Ok(r#""ann""#),
        after: r#"{"data":{"meta":{"gone":true,"keep":"yes"},"nums":[3,1,2],"users":[{"id":1,"name":"ann"},{"id":2,"name":"bob"}]}}"#,
    },
    Operation {
        name: "array_find_map_start",
        expression: r#"json.users.find_map(|user| user.name, 1)"#,
        result: Ok(r#""bob""#),
        after: r#"{"data":{"meta":{"gone":true,"keep":"yes"},"nums":[3,1,2],"users":[{"id":1,"name":"ann"},{"id":2,"name":"bob"}]}}"#,
    },
    Operation {
        name: "array_some",
        expression: r#"json.users.some(|user| user.id == 2)"#,
        result: Ok(r#"true"#),
        after: r#"{"data":{"meta":{"gone":true,"keep":"yes"},"nums":[3,1,2],"users":[{"id":1,"name":"ann"},{"id":2,"name":"bob"}]}}"#,
    },
    Operation {
        name: "array_all",
        expression: r#"json.users.all(|user| user.id > 0)"#,
        result: Ok(r#"true"#),
        after: r#"{"data":{"meta":{"gone":true,"keep":"yes"},"nums":[3,1,2],"users":[{"id":1,"name":"ann"},{"id":2,"name":"bob"}]}}"#,
    },
    Operation {
        name: "array_dedup",
        expression: r#"{ json.nums.push(2); json.nums.dedup(); json.nums }"#,
        result: Ok(r#"[3, 1, 2]"#),
        after: r#"{"data":{"meta":{"gone":true,"keep":"yes"},"nums":[3,1,2],"users":[{"id":1,"name":"ann"},{"id":2,"name":"bob"}]}}"#,
    },
    Operation {
        name: "array_dedup_comparer",
        expression: r#"json.users.dedup(|a, b| true)"#,
        result: Ok(r#"()"#),
        after: r#"{"data":{"meta":{"gone":true,"keep":"yes"},"nums":[3,1,2],"users":[{"id":1,"name":"ann"}]}}"#,
    },
    Operation {
        name: "array_reduce",
        expression: r#"json.nums.reduce(|sum, n| sum + n)"#,
        result: Err(r#"Function not found: + ((), i64) (line 2, position 44)
in closure call (line 2, position 24)"#),
        after: r#"{"data":{"users":[{"name":"ann","id":1},{"name":"bob","id":2}],"meta":{"keep":"yes","gone":true},"nums":[3,1,2]}}"#,
    },
    Operation {
        name: "array_reduce_initial",
        expression: r#"json.nums.reduce(|sum, n| sum + n, 10)"#,
        result: Ok(r#"16"#),
        after: r#"{"data":{"meta":{"gone":true,"keep":"yes"},"nums":[3,1,2],"users":[{"id":1,"name":"ann"},{"id":2,"name":"bob"}]}}"#,
    },
    Operation {
        name: "array_reduce_rev",
        expression: r#"json.users.reduce_rev(|names, user| names + user.name, "")"#,
        result: Ok(r#""bobann""#),
        after: r#"{"data":{"meta":{"gone":true,"keep":"yes"},"nums":[3,1,2],"users":[{"id":1,"name":"ann"},{"id":2,"name":"bob"}]}}"#,
    },
    Operation {
        name: "array_reduce_rev_no_initial",
        expression: r#"json.nums.reduce_rev(|sum, n| sum + n)"#,
        result: Err(r#"Function not found: + ((), i64) (line 2, position 48)
in closure call (line 2, position 24)"#),
        after: r#"{"data":{"users":[{"name":"ann","id":1},{"name":"bob","id":2}],"meta":{"keep":"yes","gone":true},"nums":[3,1,2]}}"#,
    },
    Operation {
        name: "array_zip",
        expression: r#"json.nums.zip(["a", "b"], |n, s| s + n)"#,
        result: Ok(r#"["a3", "b1"]"#),
        after: r#"{"data":{"meta":{"gone":true,"keep":"yes"},"nums":[3,1,2],"users":[{"id":1,"name":"ann"},{"id":2,"name":"bob"}]}}"#,
    },
    Operation {
        name: "array_sort",
        expression: r#"json.nums.sort()"#,
        result: Ok(r#"()"#),
        after: r#"{"data":{"meta":{"gone":true,"keep":"yes"},"nums":[1,2,3],"users":[{"id":1,"name":"ann"},{"id":2,"name":"bob"}]}}"#,
    },
    Operation {
        name: "array_sort_comparer",
        expression: r#"json.users.sort(|a, b| b.id - a.id)"#,
        result: Ok(r#"()"#),
        after: r#"{"data":{"meta":{"gone":true,"keep":"yes"},"nums":[3,1,2],"users":[{"id":2,"name":"bob"},{"id":1,"name":"ann"}]}}"#,
    },
    Operation {
        name: "array_drain_predicate",
        expression: r#"json.nums.drain(|n| n > 1)"#,
        result: Ok(r#"[3, 2]"#),
        after: r#"{"data":{"meta":{"gone":true,"keep":"yes"},"nums":[1],"users":[{"id":1,"name":"ann"},{"id":2,"name":"bob"}]}}"#,
    },
    Operation {
        name: "array_drain_range",
        expression: r#"json.nums.drain(0..1)"#,
        result: Ok(r#"[3]"#),
        after: r#"{"data":{"meta":{"gone":true,"keep":"yes"},"nums":[1,2],"users":[{"id":1,"name":"ann"},{"id":2,"name":"bob"}]}}"#,
    },
    Operation {
        name: "array_drain_inclusive_range",
        expression: r#"json.nums.drain(0..=1)"#,
        result: Ok(r#"[3, 1]"#),
        after: r#"{"data":{"meta":{"gone":true,"keep":"yes"},"nums":[2],"users":[{"id":1,"name":"ann"},{"id":2,"name":"bob"}]}}"#,
    },
    Operation {
        name: "array_drain_start_len",
        expression: r#"json.nums.drain(1, 1)"#,
        result: Ok(r#"[1]"#),
        after: r#"{"data":{"meta":{"gone":true,"keep":"yes"},"nums":[3,2],"users":[{"id":1,"name":"ann"},{"id":2,"name":"bob"}]}}"#,
    },
    Operation {
        name: "array_retain_predicate",
        expression: r#"json.nums.retain(|n| n > 1)"#,
        result: Ok(r#"[1]"#),
        after: r#"{"data":{"meta":{"gone":true,"keep":"yes"},"nums":[3,2],"users":[{"id":1,"name":"ann"},{"id":2,"name":"bob"}]}}"#,
    },
    Operation {
        name: "array_retain_range",
        expression: r#"json.nums.retain(0..1)"#,
        result: Ok(r#"[1, 2]"#),
        after: r#"{"data":{"meta":{"gone":true,"keep":"yes"},"nums":[3],"users":[{"id":1,"name":"ann"},{"id":2,"name":"bob"}]}}"#,
    },
    Operation {
        name: "array_retain_inclusive_range",
        expression: r#"json.nums.retain(0..=1)"#,
        result: Ok(r#"[2]"#),
        after: r#"{"data":{"meta":{"gone":true,"keep":"yes"},"nums":[3,1],"users":[{"id":1,"name":"ann"},{"id":2,"name":"bob"}]}}"#,
    },
    Operation {
        name: "array_retain_start_len",
        expression: r#"json.nums.retain(1, 1)"#,
        result: Ok(r#"[3, 2]"#),
        after: r#"{"data":{"meta":{"gone":true,"keep":"yes"},"nums":[1],"users":[{"id":1,"name":"ann"},{"id":2,"name":"bob"}]}}"#,
    },
    Operation {
        name: "array_equals",
        expression: r#"json.nums == [3, 1, 2]"#,
        result: Ok(r#"true"#),
        after: r#"{"data":{"meta":{"gone":true,"keep":"yes"},"nums":[3,1,2],"users":[{"id":1,"name":"ann"},{"id":2,"name":"bob"}]}}"#,
    },
    Operation {
        name: "array_not_equals",
        expression: r#"json.nums != [3, 1, 2]"#,
        result: Ok(r#"false"#),
        after: r#"{"data":{"meta":{"gone":true,"keep":"yes"},"nums":[3,1,2],"users":[{"id":1,"name":"ann"},{"id":2,"name":"bob"}]}}"#,
    },
    Operation {
        name: "array_for_loop",
        expression: r#"{ let seen = []; for (n, i) in json.nums { seen.push(i * 10 + n) } seen }"#,
        result: Ok(r#"[3, 11, 22]"#),
        after: r#"{"data":{"meta":{"gone":true,"keep":"yes"},"nums":[3,1,2],"users":[{"id":1,"name":"ann"},{"id":2,"name":"bob"}]}}"#,
    },
    Operation {
        name: "array_negative_index",
        expression: r#"json.nums[-1]"#,
        result: Ok(r#"2"#),
        after: r#"{"data":{"meta":{"gone":true,"keep":"yes"},"nums":[3,1,2],"users":[{"id":1,"name":"ann"},{"id":2,"name":"bob"}]}}"#,
    },
    Operation {
        name: "array_index_assign",
        expression: r#"{ json.nums[0] = 10; json.nums[0] }"#,
        result: Ok(r#"10"#),
        after: r#"{"data":{"meta":{"gone":true,"keep":"yes"},"nums":[10,1,2],"users":[{"id":1,"name":"ann"},{"id":2,"name":"bob"}]}}"#,
    },
    Operation {
        name: "array_index_out_of_bounds",
        expression: r#"json.nums[9]"#,
        result: Err(
            r#"Array index 9 out of bounds: only 3 elements in array (line 2, position 24)"#,
        ),
        after: r#"{"data":{"users":[{"name":"ann","id":1},{"name":"bob","id":2}],"meta":{"keep":"yes","gone":true},"nums":[3,1,2]}}"#,
    },
    Operation {
        name: "array_index_assign_out_of_bounds",
        expression: r#"{ json.nums[9] = 0 }"#,
        result: Err(
            r#"Array index 9 out of bounds: only 3 elements in array (line 2, position 26)"#,
        ),
        after: r#"{"data":{"users":[{"name":"ann","id":1},{"name":"bob","id":2}],"meta":{"keep":"yes","gone":true},"nums":[3,1,2]}}"#,
    },
    Operation {
        name: "array_sort_mixed_types",
        expression: r#"{ json.nums.push("x"); json.nums.sort() }"#,
        result: Err(
            r#"Function not found: sort() cannot be called with elements of different types (line 2, position 47)"#,
        ),
        after: r#"{"data":{"users":[{"name":"ann","id":1},{"name":"bob","id":2}],"meta":{"keep":"yes","gone":true},"nums":[3,1,2]}}"#,
    },
];

/// Conversion, alias, write-back, ordering and error semantics of JSON fields.
const CASES: &[Case] = &[
    Case {
        name: "read_only_local_copy",
        subject: Subject::Collections,
        script: r#"let json = response.data; json.meta.keep"#,
        result: Ok(r#""yes""#),
        after: r#"{"data":{"users":[{"name":"ann","id":1},{"name":"bob","id":2}],"meta":{"keep":"yes","gone":true},"nums":[3,1,2]}}"#,
    },
    Case {
        name: "local_edit_without_write_back",
        subject: Subject::Collections,
        script: r#"let json = response.data; json.meta.keep = "no"; json.meta.keep"#,
        result: Ok(r#""no""#),
        after: r#"{"data":{"users":[{"name":"ann","id":1},{"name":"bob","id":2}],"meta":{"keep":"yes","gone":true},"nums":[3,1,2]}}"#,
    },
    Case {
        name: "write_back_unchanged_value",
        subject: Subject::Collections,
        script: r#"let json = response.data; response.data = json;"#,
        result: Ok(r#"()"#),
        after: r#"{"data":{"meta":{"gone":true,"keep":"yes"},"nums":[3,1,2],"users":[{"id":1,"name":"ann"},{"id":2,"name":"bob"}]}}"#,
    },
    Case {
        name: "property_chain_assignment",
        subject: Subject::Collections,
        script: r#"response.data.meta.keep = "no";"#,
        result: Ok(r#"()"#),
        after: r#"{"data":{"meta":{"gone":true,"keep":"no"},"nums":[3,1,2],"users":[{"id":1,"name":"ann"},{"id":2,"name":"bob"}]}}"#,
    },
    Case {
        name: "index_chain_assignment",
        subject: Subject::Collections,
        script: r#"response.data["users"][0]["name"] = "amy";"#,
        result: Ok(r#"()"#),
        after: r#"{"data":{"meta":{"gone":true,"keep":"yes"},"nums":[3,1,2],"users":[{"id":1,"name":"amy"},{"id":2,"name":"bob"}]}}"#,
    },
    Case {
        name: "property_chain_read",
        subject: Subject::Collections,
        script: r#"response.data.meta.keep"#,
        result: Ok(r#""yes""#),
        after: r#"{"data":{"users":[{"name":"ann","id":1},{"name":"bob","id":2}],"meta":{"keep":"yes","gone":true},"nums":[3,1,2]}}"#,
    },
    Case {
        name: "index_chain_read",
        subject: Subject::Collections,
        script: r#"response.data["users"][1]["name"]"#,
        result: Ok(r#""bob""#),
        after: r#"{"data":{"users":[{"name":"ann","id":1},{"name":"bob","id":2}],"meta":{"keep":"yes","gone":true},"nums":[3,1,2]}}"#,
    },
    Case {
        name: "property_chain_pure_method",
        subject: Subject::Collections,
        script: r#"response.data.nums.len()"#,
        result: Ok(r#"3"#),
        after: r#"{"data":{"meta":{"gone":true,"keep":"yes"},"nums":[3,1,2],"users":[{"id":1,"name":"ann"},{"id":2,"name":"bob"}]}}"#,
    },
    Case {
        name: "property_chain_mutating_method",
        subject: Subject::Collections,
        script: r#"response.data.nums.push(4);"#,
        result: Ok(r#"()"#),
        after: r#"{"data":{"meta":{"gone":true,"keep":"yes"},"nums":[3,1,2,4],"users":[{"id":1,"name":"ann"},{"id":2,"name":"bob"}]}}"#,
    },
    Case {
        name: "property_chain_compound_assignment",
        subject: Subject::Collections,
        script: r#"response.data.nums[0] += 1;"#,
        result: Ok(r#"()"#),
        after: r#"{"data":{"meta":{"gone":true,"keep":"yes"},"nums":[4,1,2],"users":[{"id":1,"name":"ann"},{"id":2,"name":"bob"}]}}"#,
    },
    Case {
        name: "missing_property_read",
        subject: Subject::Collections,
        script: r#"response.data.meta.absent"#,
        result: Ok(r#"()"#),
        after: r#"{"data":{"users":[{"name":"ann","id":1},{"name":"bob","id":2}],"meta":{"keep":"yes","gone":true},"nums":[3,1,2]}}"#,
    },
    Case {
        name: "missing_property_assignment",
        subject: Subject::Collections,
        script: r#"response.data.meta.added = 1;"#,
        result: Ok(r#"()"#),
        after: r#"{"data":{"meta":{"added":1,"gone":true,"keep":"yes"},"nums":[3,1,2],"users":[{"id":1,"name":"ann"},{"id":2,"name":"bob"}]}}"#,
    },
    Case {
        name: "new_keys_are_sorted",
        subject: Subject::Collections,
        script: r#"let json = response.data; json.meta.zzz = 1; json.meta.aaa = 2; response.data = json; json.meta.keys()"#,
        result: Ok(r#"["aaa", "gone", "keep", "zzz"]"#),
        after: r#"{"data":{"meta":{"aaa":2,"gone":true,"keep":"yes","zzz":1},"nums":[3,1,2],"users":[{"id":1,"name":"ann"},{"id":2,"name":"bob"}]}}"#,
    },
    Case {
        name: "detached_child_mutation",
        subject: Subject::Collections,
        script: r#"let json = response.data; let user = json.users[0]; user.name = "alias"; response.data = json; user.name"#,
        result: Ok(r#""alias""#),
        after: r#"{"data":{"meta":{"gone":true,"keep":"yes"},"nums":[3,1,2],"users":[{"id":1,"name":"ann"},{"id":2,"name":"bob"}]}}"#,
    },
    Case {
        name: "detached_clone_mutation",
        subject: Subject::Collections,
        script: r#"let json = response.data; let copy = json; copy.meta.keep = "no"; response.data = json; copy.meta.keep"#,
        result: Ok(r#""no""#),
        after: r#"{"data":{"meta":{"gone":true,"keep":"yes"},"nums":[3,1,2],"users":[{"id":1,"name":"ann"},{"id":2,"name":"bob"}]}}"#,
    },
    Case {
        name: "reattach_child_same_path",
        subject: Subject::Collections,
        script: r#"let json = response.data; let user = json.users[0]; user.name = "alias"; json.users[0] = user; response.data = json;"#,
        result: Ok(r#"()"#),
        after: r#"{"data":{"meta":{"gone":true,"keep":"yes"},"nums":[3,1,2],"users":[{"id":1,"name":"alias"},{"id":2,"name":"bob"}]}}"#,
    },
    Case {
        name: "reattach_child_other_path",
        subject: Subject::Collections,
        script: r#"let json = response.data; let user = json.users[0]; user.name = "alias"; json.meta.owner = user; response.data = json;"#,
        result: Ok(r#"()"#),
        after: r#"{"data":{"meta":{"gone":true,"keep":"yes","owner":{"id":1,"name":"alias"}},"nums":[3,1,2],"users":[{"id":1,"name":"ann"},{"id":2,"name":"bob"}]}}"#,
    },
    Case {
        name: "nested_object_propagation",
        subject: Subject::Collections,
        script: r#"let json = response.data; json.meta.nested = #{}; json.meta.nested.deep = #{ value: 1 }; json.meta.nested.deep.value += 1; response.data = json;"#,
        result: Ok(r#"()"#),
        after: r#"{"data":{"meta":{"gone":true,"keep":"yes","nested":{"deep":{"value":2}}},"nums":[3,1,2],"users":[{"id":1,"name":"ann"},{"id":2,"name":"bob"}]}}"#,
    },
    Case {
        name: "nested_array_propagation",
        subject: Subject::Collections,
        script: r#"let json = response.data; json.users[1].tags = []; json.users[1].tags.push("new"); json.users[1].id *= 10; response.data = json;"#,
        result: Ok(r#"()"#),
        after: r#"{"data":{"meta":{"gone":true,"keep":"yes"},"nums":[3,1,2],"users":[{"id":1,"name":"ann"},{"id":20,"name":"bob","tags":["new"]}]}}"#,
    },
    Case {
        name: "root_replacement",
        subject: Subject::Collections,
        script: r#"response.data = #{ replaced: true };"#,
        result: Ok(r#"()"#),
        after: r#"{"data":{"replaced":true}}"#,
    },
    Case {
        name: "object_subtree_replacement",
        subject: Subject::Collections,
        script: r#"let json = response.data; json.meta = #{ b: 2, a: 1 }; response.data = json;"#,
        result: Ok(r#"()"#),
        after: r#"{"data":{"meta":{"a":1,"b":2},"nums":[3,1,2],"users":[{"id":1,"name":"ann"},{"id":2,"name":"bob"}]}}"#,
    },
    Case {
        name: "array_subtree_replacement",
        subject: Subject::Collections,
        script: r#"let json = response.data; json.users = [1, "two", #{ three: 3 }, [4]]; response.data = json;"#,
        result: Ok(r#"()"#),
        after: r#"{"data":{"meta":{"gone":true,"keep":"yes"},"nums":[3,1,2],"users":[1,"two",{"three":3},[4]]}}"#,
    },
    Case {
        name: "repeated_assignment",
        subject: Subject::Collections,
        script: r#"response.data.meta.keep = "first"; response.data.meta.keep = "second"; let json = response.data; json.meta.keep = "third"; json.meta.keep = "fourth"; response.data = json;"#,
        result: Ok(r#"()"#),
        after: r#"{"data":{"meta":{"gone":true,"keep":"fourth"},"nums":[3,1,2],"users":[{"id":1,"name":"ann"},{"id":2,"name":"bob"}]}}"#,
    },
    Case {
        name: "remove_then_reinsert",
        subject: Subject::Collections,
        script: r#"let json = response.data; json.meta.remove("keep"); json.meta.keep = "again"; response.data = json;"#,
        result: Ok(r#"()"#),
        after: r#"{"data":{"meta":{"gone":true,"keep":"again"},"nums":[3,1,2],"users":[{"id":1,"name":"ann"},{"id":2,"name":"bob"}]}}"#,
    },
    Case {
        name: "edit_then_throw",
        subject: Subject::Collections,
        script: r#"response.data.meta.keep = "no"; throw "boom";"#,
        result: Err(r#"Runtime error: boom (line 1, position 33)"#),
        after: r#"{"data":{"meta":{"gone":true,"keep":"no"},"nums":[3,1,2],"users":[{"id":1,"name":"ann"},{"id":2,"name":"bob"}]}}"#,
    },
    Case {
        name: "local_edit_then_throw",
        subject: Subject::Collections,
        script: r#"let json = response.data; json.meta.keep = "no"; throw "boom";"#,
        result: Err(r#"Runtime error: boom (line 1, position 50)"#),
        after: r#"{"data":{"users":[{"name":"ann","id":1},{"name":"bob","id":2}],"meta":{"keep":"yes","gone":true},"nums":[3,1,2]}}"#,
    },
    Case {
        name: "structured_throw",
        subject: Subject::Collections,
        script: r#"throw #{ status: 400, message: "bad request" };"#,
        result: Err(
            r#"Runtime error: #{"message": "bad request", "status": 400} (line 1, position 1)"#,
        ),
        after: r#"{"data":{"users":[{"name":"ann","id":1},{"name":"bob","id":2}],"meta":{"keep":"yes","gone":true},"nums":[3,1,2]}}"#,
    },
    Case {
        name: "type_names",
        subject: Subject::Collections,
        script: r#"let json = response.data; [type_of(json), type_of(json.users), type_of(json.users[0]), type_of(json.nums[0]), type_of(json.meta.gone), type_of(json.meta.keep)]"#,
        result: Ok(r#"["map", "array", "map", "i64", "bool", "string"]"#),
        after: r#"{"data":{"users":[{"name":"ann","id":1},{"name":"bob","id":2}],"meta":{"keep":"yes","gone":true},"nums":[3,1,2]}}"#,
    },
    Case {
        name: "debug_output",
        subject: Subject::Collections,
        script: r#"response.data"#,
        result: Ok(
            r#"#{"meta": #{"gone": true, "keep": "yes"}, "nums": [3, 1, 2], "users": [#{"id": 1, "name": "ann"}, #{"id": 2, "name": "bob"}]}"#,
        ),
        after: r#"{"data":{"users":[{"name":"ann","id":1},{"name":"bob","id":2}],"meta":{"keep":"yes","gone":true},"nums":[3,1,2]}}"#,
    },
    Case {
        name: "to_string_output",
        subject: Subject::Collections,
        script: r#"response.data.meta.to_string()"#,
        result: Ok(r##""#{\"gone\": true, \"keep\": \"yes\"}""##),
        after: r#"{"data":{"meta":{"gone":true,"keep":"yes"},"nums":[3,1,2],"users":[{"id":1,"name":"ann"},{"id":2,"name":"bob"}]}}"#,
    },
    Case {
        name: "for_over_map",
        subject: Subject::Collections,
        script: r#"for entry in response.data.meta { }"#,
        result: Err(r#"For loop expects iterable type (line 1, position 14)"#),
        after: r#"{"data":{"users":[{"name":"ann","id":1},{"name":"bob","id":2}],"meta":{"keep":"yes","gone":true},"nums":[3,1,2]}}"#,
    },
    Case {
        name: "data_setter_rejects_integer",
        subject: Subject::Collections,
        script: r#"response.data = 5;"#,
        result: Err(
            r#"No writable property 'data' - a setter is not registered for type 'apollo_router::graphql::response::Response' to handle 'i64' (line 1, position 10)"#,
        ),
        after: r#"{"data":{"users":[{"name":"ann","id":1},{"name":"bob","id":2}],"meta":{"keep":"yes","gone":true},"nums":[3,1,2]}}"#,
    },
    Case {
        name: "data_setter_rejects_array",
        subject: Subject::Collections,
        script: r#"response.data = [1];"#,
        result: Err(
            r#"No writable property 'data' - a setter is not registered for type 'apollo_router::graphql::response::Response' to handle 'array' (line 1, position 10)"#,
        ),
        after: r#"{"data":{"users":[{"name":"ann","id":1},{"name":"bob","id":2}],"meta":{"keep":"yes","gone":true},"nums":[3,1,2]}}"#,
    },
    Case {
        name: "data_setter_rejects_function",
        subject: Subject::Collections,
        script: r#"response.data = #{ f: || 1 };"#,
        result: Err(
            r#"Output type incorrect: Fn (expecting serde_json_bytes::value::Value) (line 1, position 10)"#,
        ),
        after: r#"{"data":{"users":[{"name":"ann","id":1},{"name":"bob","id":2}],"meta":{"keep":"yes","gone":true},"nums":[3,1,2]}}"#,
    },
    Case {
        name: "data_setter_accepts_unit_value",
        subject: Subject::Collections,
        script: r#"response.data = #{ u: () };"#,
        result: Ok(r#"()"#),
        after: r#"{"data":{"u":null}}"#,
    },
    Case {
        name: "data_read_when_absent",
        subject: Subject::Collections,
        script: r#"response.data = #{}; let json = response.data; json.len()"#,
        result: Ok(r#"0"#),
        after: r#"{"data":{}}"#,
    },
    Case {
        name: "scalar_types",
        subject: Subject::Scalars,
        script: r#"let json = response.data; [type_of(json["null"]), type_of(json.flag), type_of(json.int), type_of(json.float), type_of(json.big), type_of(json.text)]"#,
        result: Ok(r#"["()", "bool", "i64", "f64", "f64", "string"]"#),
        after: r#"{"data":{"null":null,"flag":true,"int":-7,"float":1.5,"big":18446744073709551615,"text":"t"}}"#,
    },
    Case {
        name: "scalar_values",
        subject: Subject::Scalars,
        script: r#"let json = response.data; [json["null"], json.flag, json.int, json.float, json.big, json.text]"#,
        result: Ok(r#"[(), true, -7, 1.5, 1.8446744073709552e19, "t"]"#),
        after: r#"{"data":{"null":null,"flag":true,"int":-7,"float":1.5,"big":18446744073709551615,"text":"t"}}"#,
    },
    Case {
        name: "scalar_write_back",
        subject: Subject::Scalars,
        script: r#"let json = response.data; response.data = json;"#,
        result: Ok(r#"()"#),
        after: r#"{"data":{"big":1.8446744073709552e+19,"flag":true,"float":1.5,"int":-7,"null":null,"text":"t"}}"#,
    },
    Case {
        name: "scalar_arithmetic",
        subject: Subject::Scalars,
        script: r#"let json = response.data; [json.int + 1, json.float * 2.0, json.int / 2]"#,
        result: Ok(r#"[-6, 3.0, -3]"#),
        after: r#"{"data":{"null":null,"flag":true,"int":-7,"float":1.5,"big":18446744073709551615,"text":"t"}}"#,
    },
    Case {
        name: "big_integer_arithmetic",
        subject: Subject::Scalars,
        script: r#"response.data.big + 1"#,
        result: Ok(r#"1.8446744073709552e19"#),
        after: r#"{"data":{"null":null,"flag":true,"int":-7,"float":1.5,"big":18446744073709551615,"text":"t"}}"#,
    },
    Case {
        name: "new_scalar_values",
        subject: Subject::Scalars,
        script: r#"let json = response.data; json.char = 'c'; json.max = 9223372036854775807; json.min = -9223372036854775808; json.small = 0.1; json.whole = 2.0; json.unit = (); response.data = json;"#,
        result: Ok(r#"()"#),
        after: r#"{"data":{"big":1.8446744073709552e+19,"char":"c","flag":true,"float":1.5,"int":-7,"max":9223372036854775807,"min":-9223372036854775808,"null":null,"small":0.1,"text":"t","unit":null,"whole":2.0}}"#,
    },
    Case {
        name: "string_mutation",
        subject: Subject::Scalars,
        script: r#"let json = response.data; json.text += "ext"; json.text.pad(5, '!'); response.data = json;"#,
        result: Ok(r#"()"#),
        after: r#"{"data":{"big":1.8446744073709552e+19,"flag":true,"float":1.5,"int":-7,"null":null,"text":"text!"}}"#,
    },
    Case {
        name: "errors_read_empty",
        subject: Subject::Collections,
        script: r#"response.errors"#,
        result: Ok(r#"[]"#),
        after: r#"{"data":{"users":[{"name":"ann","id":1},{"name":"bob","id":2}],"meta":{"keep":"yes","gone":true},"nums":[3,1,2]}}"#,
    },
    Case {
        name: "errors_assignment",
        subject: Subject::Collections,
        script: r#"response.errors = [#{ message: "first", extensions: #{ code: "E1" } }, #{ message: "second", path: ["users", 0], locations: [#{ line: 1, column: 2 }] }];"#,
        result: Ok(r#"()"#),
        after: r#"{"data":{"users":[{"name":"ann","id":1},{"name":"bob","id":2}],"meta":{"keep":"yes","gone":true},"nums":[3,1,2]},"errors":[{"message":"first","extensions":{"code":"E1"}},{"message":"second","locations":[{"line":1,"column":2}],"path":["users",0]}]}"#,
    },
    Case {
        name: "errors_chain_edit",
        subject: Subject::Collections,
        script: r#"response.errors = [#{ message: "first" }]; response.errors[0].message = "edited"; response.errors.push(#{ message: "pushed" });"#,
        result: Ok(r#"()"#),
        after: r#"{"data":{"users":[{"name":"ann","id":1},{"name":"bob","id":2}],"meta":{"keep":"yes","gone":true},"nums":[3,1,2]},"errors":[{"message":"edited"},{"message":"pushed"}]}"#,
    },
    Case {
        name: "errors_read_back",
        subject: Subject::Collections,
        script: r#"response.errors = [#{ message: "first", extensions: #{ code: "E1" } }]; response.errors"#,
        result: Ok(r#"[#{"extensions": #{"code": "E1"}, "message": "first"}]"#),
        after: r#"{"data":{"users":[{"name":"ann","id":1},{"name":"bob","id":2}],"meta":{"keep":"yes","gone":true},"nums":[3,1,2]},"errors":[{"message":"first","extensions":{"code":"E1"}}]}"#,
    },
    Case {
        name: "errors_setter_defaults_missing_message",
        subject: Subject::Collections,
        script: r#"response.errors = [#{ extensions: #{} }];"#,
        result: Ok(r#"()"#),
        after: r#"{"data":{"users":[{"name":"ann","id":1},{"name":"bob","id":2}],"meta":{"keep":"yes","gone":true},"nums":[3,1,2]},"errors":[{"message":""}]}"#,
    },
    Case {
        name: "errors_setter_rejects_map",
        subject: Subject::Collections,
        script: r#"response.errors = #{ message: "not an array" };"#,
        result: Err(
            r#"Output type incorrect: map (expecting alloc::vec::Vec<apollo_router::graphql::Error>) (line 1, position 10)"#,
        ),
        after: r#"{"data":{"users":[{"name":"ann","id":1},{"name":"bob","id":2}],"meta":{"keep":"yes","gone":true},"nums":[3,1,2]}}"#,
    },
    Case {
        name: "response_extensions_chain_edit",
        subject: Subject::Collections,
        script: r#"response.extensions.b = 1; response.extensions.a = #{ nested: [true] };"#,
        result: Ok(r#"()"#),
        after: r#"{"data":{"users":[{"name":"ann","id":1},{"name":"bob","id":2}],"meta":{"keep":"yes","gone":true},"nums":[3,1,2]},"extensions":{"a":{"nested":[true]},"b":1}}"#,
    },
    Case {
        name: "response_extensions_read",
        subject: Subject::Collections,
        script: r#"response.extensions"#,
        result: Ok(r#"#{}"#),
        after: r#"{"data":{"users":[{"name":"ann","id":1},{"name":"bob","id":2}],"meta":{"keep":"yes","gone":true},"nums":[3,1,2]}}"#,
    },
    Case {
        name: "request_variables_read",
        subject: Subject::Request,
        script: r#"request.variables"#,
        result: Ok(r#"#{"a": #{"x": 1, "y": 2}, "b": 1}"#),
        after: r#"{"query":"{ me }","variables":{"b":1,"a":{"y":2,"x":1}},"extensions":{"z":true,"c":[1]}}"#,
    },
    Case {
        name: "request_extensions_read",
        subject: Subject::Request,
        script: r#"request.extensions"#,
        result: Ok(r#"#{"c": [1], "z": true}"#),
        after: r#"{"query":"{ me }","variables":{"b":1,"a":{"y":2,"x":1}},"extensions":{"z":true,"c":[1]}}"#,
    },
    Case {
        name: "request_variables_chain_edit",
        subject: Subject::Request,
        script: r#"request.variables.a.x = 10; request.variables.c = "new";"#,
        result: Ok(r#"()"#),
        after: r#"{"query":"{ me }","variables":{"a":{"x":10,"y":2},"b":1,"c":"new"},"extensions":{"z":true,"c":[1]}}"#,
    },
    Case {
        name: "request_extensions_chain_edit",
        subject: Subject::Request,
        script: r#"request.extensions.c.push(2);"#,
        result: Ok(r#"()"#),
        after: r#"{"query":"{ me }","variables":{"b":1,"a":{"y":2,"x":1}},"extensions":{"c":[1,2],"z":true}}"#,
    },
    Case {
        name: "request_variables_replacement",
        subject: Subject::Request,
        script: r#"request.variables = #{ only: [1, 2] };"#,
        result: Ok(r#"()"#),
        after: r#"{"query":"{ me }","variables":{"only":[1,2]},"extensions":{"z":true,"c":[1]}}"#,
    },
    Case {
        name: "request_variables_local_edit_without_write_back",
        subject: Subject::Request,
        script: r#"let variables = request.variables; variables.b = 2; variables.b"#,
        result: Ok(r#"2"#),
        after: r#"{"query":"{ me }","variables":{"b":1,"a":{"y":2,"x":1}},"extensions":{"z":true,"c":[1]}}"#,
    },
    Case {
        name: "request_variables_setter_rejects_array",
        subject: Subject::Request,
        script: r#"request.variables = [1];"#,
        result: Err(
            r#"No writable property 'variables' - a setter is not registered for type 'apollo_router::graphql::request::Request' to handle 'array' (line 1, position 9)"#,
        ),
        after: r#"{"query":"{ me }","variables":{"b":1,"a":{"y":2,"x":1}},"extensions":{"z":true,"c":[1]}}"#,
    },
    Case {
        name: "request_extensions_setter_rejects_string",
        subject: Subject::Request,
        script: r#"request.extensions = "x";"#,
        result: Err(
            r#"No writable property 'extensions' - a setter is not registered for type 'apollo_router::graphql::request::Request' to handle 'string' (line 1, position 9)"#,
        ),
        after: r#"{"query":"{ me }","variables":{"b":1,"a":{"y":2,"x":1}},"extensions":{"z":true,"c":[1]}}"#,
    },
];

/// Rows where the current Router deliberately differs from v2.17.0, with the current result.
///
/// Each entry needs a reason. Only undocumented details such as Rhai's own error wording may
/// change; the released expectation stays in the table above.
const ACCEPTED_CHANGES: &[(&str, Result<&str, &str>)] = &[
    // Rhai 1.25 reworded this error. Router does not document Rhai's error text.
    (
        "array_sort_mixed_types",
        Err(
            "Function not found: elements of different types cannot be sorted (line 2, position 47)",
        ),
    ),
];

#[derive(Debug, PartialEq)]
struct Outcome {
    result: Result<String, String>,
    after: String,
}

impl Outcome {
    fn expected(name: &str, result: Result<&str, &str>, after: &str) -> Self {
        let result = ACCEPTED_CHANGES
            .iter()
            .find(|(changed, _)| *changed == name)
            .map_or(result, |(_, current)| *current);
        Self {
            result: result.map(str::to_string).map_err(str::to_string),
            after: after.to_string(),
        }
    }
}

fn run(subject: Subject, script: &str) -> Outcome {
    let response = |data: &str| {
        graphql::Response::builder()
            .data(serde_json::from_str::<crate::json_ext::Value>(data).unwrap())
            .build()
    };
    let mut scope = Scope::new();
    match subject {
        Subject::Collections => scope.push("response", response(COLLECTIONS)),
        Subject::Scalars => scope.push("response", response(SCALARS)),
        Subject::Request => scope.push(
            "request",
            serde_json::from_str::<graphql::Request>(REQUEST).unwrap(),
        ),
    };
    let result = new_rhai_test_engine()
        .eval_with_scope::<Dynamic>(&mut scope, script)
        .map(|value| format!("{value:?}"))
        .map_err(|error| error.to_string());
    let after = match subject {
        Subject::Request => {
            serde_json::to_string(&scope.get_value::<graphql::Request>("request").unwrap())
        }
        _ => serde_json::to_string(&scope.get_value::<graphql::Response>("response").unwrap()),
    }
    .unwrap();
    Outcome { result, after }
}

fn operation_script(expression: &str) -> String {
    format!("let json = response.data;\nlet result = {expression};\nresponse.data = json;\nresult")
}

/// Run every case, then fail once, listing each mismatch with its actual outcome.
fn check(cases: impl Iterator<Item = (&'static str, Subject, String, Outcome)>) {
    let mismatches: Vec<String> = cases
        .filter_map(|(name, subject, script, expected)| {
            let actual = run(subject, &script);
            (actual != expected)
                .then(|| format!("{name}:\n  expected: {expected:?}\n  actual:   {actual:?}"))
        })
        .collect();
    assert!(
        mismatches.is_empty(),
        "{} Rhai JSON compatibility case(s) differ from Router v2.17.0:\n{}",
        mismatches.len(),
        mismatches.join("\n")
    );
}

#[test]
fn operation_surface() {
    check(OPERATIONS.iter().map(|operation| {
        (
            operation.name,
            Subject::Collections,
            operation_script(operation.expression),
            Outcome::expected(operation.name, operation.result, operation.after),
        )
    }));
}

#[test]
fn value_semantics() {
    check(CASES.iter().map(|case| {
        (
            case.name,
            case.subject,
            case.script.to_string(),
            Outcome::expected(case.name, case.result, case.after),
        )
    }));
}

#[test]
fn accepted_changes_name_existing_cases() {
    let names: Vec<_> = OPERATIONS
        .iter()
        .map(|operation| operation.name)
        .chain(CASES.iter().map(|case| case.name))
        .collect();
    for (name, _) in ACCEPTED_CHANGES {
        assert!(names.contains(name), "no case named {name}");
    }
}
