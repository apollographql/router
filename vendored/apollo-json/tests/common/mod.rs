//! Shared helpers for the integration test suites.
// Not every helper is used in every test binary.
#![allow(dead_code)]

use apollo_json::Value;
use proptest::prelude::*;

/// Parses a test fixture, panicking on failure.
pub fn parse(json: impl AsRef<[u8]>) -> Value {
    Value::parse(json.as_ref().to_vec()).expect("test document parses")
}

/// Short strings over the full char space: escapes, control characters, and
/// multi-byte UTF-8 all occur.
pub fn arb_text() -> impl Strategy<Value = String> {
    prop::collection::vec(any::<char>(), 0..8).prop_map(String::from_iter)
}

/// Arbitrary JSON values over `scalar`: small arrays and objects nested a
/// few levels deep, the shape space the property suites share.
pub fn arb_json_with(
    scalar: impl Strategy<Value = serde_json::Value> + 'static,
) -> impl Strategy<Value = serde_json::Value> {
    scalar.prop_recursive(4, 48, 5, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..5).prop_map(serde_json::Value::Array),
            prop::collection::vec((arb_text(), inner), 0..5)
                .prop_map(|kvs| serde_json::Value::Object(kvs.into_iter().collect())),
        ]
    })
}
