use serde_json_bytes::json;

use super::*;
use crate::selection;

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
                "message": "Method ->bogus not found (no built-in or `methods:` method with that name)",
                "path": ["nested", "path", "->bogus"],
                "range": [13, 18],
            }))],
        ),
    );
}
