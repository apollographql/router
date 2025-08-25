mod apply_to;
pub(crate) mod helpers;
mod immutable;
mod known_var;
mod lit_expr;
pub(crate) mod location;
mod methods;
mod parser;
mod pretty;
mod selection_set;
mod selection_trie;

pub use apply_to::*;
// Pretty code is currently only used in tests, so this cfg is to suppress the
// unused lint warning. If pretty code is needed in not test code, feel free to
// remove the `#[cfg(test)]`.
pub(crate) use location::Ranged;
pub use parser::*;
#[cfg(test)]
pub(crate) use pretty::*;
pub(crate) use selection_trie::SelectionTrie;
#[cfg(test)]
mod fixtures;

#[cfg(test)]
mod test {
    use rstest::rstest;
    use serde_json_bytes::Value;
    use serde_json_bytes::json;

    use super::*;
    use crate::connectors::ConnectSpec;

    #[rstest]
    #[case::select_field                    ("rootField",                           Some(json!({"rootField": "hello"})),                                          "0.2" )]
    #[case::select_field_value              ("$.rootField",                         Some(json!("hello")),                                                         "0.2" )]
    #[case::basic_subselection              ("user { firstName lastName }",         Some(json!({"user": { "firstName": "Alice", "lastName": "InChains" }})),      "0.2" )]
    #[case::array_subselection              ("results { name }",                    Some(json!({"results": [{ "name": "Alice" }, { "name": "John" }]})),          "0.2" )]
    #[case::array_value_subselection        ("$.results { name }",                  Some(json!([{ "name": "Alice" }, { "name": "John" }])),                       "0.2" )]
    #[case::arrow_method                    ("results->first { name }",             Some(json!({"name": "Alice"})),                                               "0.2" )]
    #[case::arbitrary_spaces                ("results ->  first {    name }",       Some(json!({"name": "Alice"})),                                               "0.2" )]
    #[case::select_field_optional           ("rootField?",                          Some(json!({"rootField": "hello"})),                                          "0.3" )]
    #[case::select_null_optional            ("nullField?",                          Some(json!({})),                                                              "0.3" )]
    #[case::select_missing_optional         ("missingField?",                       Some(json!({})),                                                              "0.3" )]
    #[case::arrow_method_optional           ("results?->first { name }",            Some(json!({"name": "Alice"})),                                               "0.3" )]
    #[case::arrow_method_null_optional("nullField?->first { name }", None, "0.3")]
    #[case::arrow_method_missing_optional("missingField?->first { name }", None, "0.3")]
    #[case::optional_subselection           ("user: user? { firstName lastName }",  Some(json!({"user": { "firstName": "Alice", "lastName": "InChains" }})),      "0.3" )]
    #[case::optional_subselection_short     ("user? { firstName lastName }",        Some(json!({"user": { "firstName": "Alice", "lastName": "InChains" }})),      "0.3" )]
    fn kitchen_sink(
        #[case] selection: &str,
        #[case] expected: Option<Value>,
        #[case] minimum_version: &str,
        #[values(ConnectSpec::V0_2, ConnectSpec::V0_3)] version: ConnectSpec,
    ) {
        // We're effectively skipping the test but it will be reported as passed because Rust has no runtime "mark as skipped" capability
        if version.as_str() < minimum_version {
            return;
        }

        let data = json!({
            "rootField": "hello",
            "nullField": null,
            "user": {
                    "firstName": "Alice",
                    "lastName": "InChains"
            },
            "results": [
                {
                    "name": "Alice",
                },
                {
                    "name": "John",
                },
            ]
        });

        let (result, errors) = JSONSelection::parse_with_spec(selection, version)
            .unwrap()
            .apply_to(&data);
        println!("errors: {errors:?}");
        assert!(errors.is_empty());
        assert_eq!(result, expected);
    }
}
