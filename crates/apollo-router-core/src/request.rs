use crate::prelude::graphql::*;
use derivative::Derivative;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use typed_builder::TypedBuilder;

/// A graphql request.
/// Used for federated and subgraph queries.
#[derive(Clone, Derivative, Serialize, Deserialize, TypedBuilder)]
#[serde(rename_all = "camelCase")]
#[builder(field_defaults(setter(into)))]
#[derivative(Debug, PartialEq)]
pub struct Request {
    /// The graphql query.
    pub query: Query,

    /// The optional graphql operation.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    #[builder(default)]
    pub operation_name: Option<String>,

    /// The optional variables in the form of a json object.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    #[builder(default)]
    pub variables: Option<Arc<Object>>,

    ///  extensions.
    #[serde(skip_serializing_if = "Object::is_empty", default)]
    #[builder(default)]
    pub extensions: Object,
}

#[derive(Debug, Clone, Hash, PartialEq, Eq, Deserialize, Serialize)]
#[serde(transparent)]
pub struct Query {
    string: String,
}

impl Query {
    /// Returns a reference to the underlying query string.
    pub fn as_str(&self) -> &str {
        self.string.as_str()
    }

    /// Re-format the response value to match this query.
    ///
    /// This will discard unrequested fields and re-order the output to match the order of the
    /// query.
    pub fn format_response(
        &self,
        response: &mut Response,
        operations: &[Operation],
        fragments: &HashMap<String, Fragment>,
    ) {
        fn apply_selection_set(
            selection_set: &SelectionSet,
            input: &mut Object,
            output: &mut Object,
            fragments: &HashMap<String, SelectionSet>,
        ) {
            for selection in selection_set.selections.clone() {
                match selection {
                    // Spec: https://spec.graphql.org/draft/#Field
                    Selection::Field(field) => {
                        let name = field.alias.unwrap_or(field.name);

                        if let Some(input_value) = input.remove(&name) {
                            if let Some(selections) = field.selections {
                                let selection_set = SelectionSet { selections };
                                match input_value {
                                    Value::Object(mut input_object) => {
                                        let mut output_object = Object::default();
                                        apply_selection_set(
                                            &selection_set,
                                            &mut input_object,
                                            &mut output_object,
                                            fragments,
                                        );
                                        output.insert(name, output_object.into());
                                    }
                                    Value::Array(input_array) => {
                                        let output_array = input_array
                                            .into_iter()
                                            .enumerate()
                                            .map(|(i, mut element)| {
                                                if let Some(input_object) = element.as_object_mut()
                                                {
                                                    let mut output_object = Object::default();
                                                    apply_selection_set(
                                                        &selection_set,
                                                        input_object,
                                                        &mut output_object,
                                                        fragments,
                                                    );
                                                    output_object.into()
                                                } else {
                                                    failfast_debug!(
                                                        "Array element is not an object: {}[{}]",
                                                        name,
                                                        i,
                                                    );
                                                    element
                                                }
                                            })
                                            .collect::<Value>();
                                        output.insert(name, output_array);
                                    }
                                    _ => {
                                        output.insert(name.clone(), input_value);
                                        failfast_debug!(
                                            "Field is not an object nor an array of object: {}",
                                            name,
                                        );
                                    }
                                }
                            } else {
                                output.insert(name, input_value);
                            }
                        } else {
                            failfast_debug!("Missing field: {}", name);
                        }
                    }
                    // Spec: https://spec.graphql.org/draft/#InlineFragment
                    Selection::InlineFragment(inline_fragment) => {
                        let selection_set = SelectionSet {
                            selections: inline_fragment.selections,
                        };
                        apply_selection_set(&selection_set, input, output, fragments);
                    }
                    // Spec: https://spec.graphql.org/draft/#FragmentSpread
                    Selection::FragmentSpread(fragment_spread) => {
                        let name = fragment_spread.fragment_name;

                        if let Some(selection_set) = fragments.get(&name) {
                            apply_selection_set(selection_set, input, output, fragments);
                        } else {
                            failfast_debug!("Missing fragment named: {}", name);
                        }
                    }
                }
            }
        }

        let fragments: HashMap<String, SelectionSet> = fragments
            .iter()
            .map(|(name, fragment)| {
                (
                    name.to_string(),
                    SelectionSet {
                        selections: fragment.selections.clone(),
                    },
                )
            })
            .collect();
        // FIXME there can be multiple operations in a query
        // we must check the operation parameter to know which one applies
        for operation in operations {
            if let Some(data) = response.data.as_object_mut() {
                let mut output = Object::default();
                apply_selection_set(&operation.selection_set, data, &mut output, &fragments);
                response.data = output.into();
                return;
            } else {
                failfast_debug!("Invalid type for data in response.");
            }
        }

        failfast_debug!("No suitable definition found. This is a bug.");
    }
}

impl<T: Into<String>> From<T> for Query {
    fn from(string: T) -> Self {
        Query {
            string: string.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use apollo_parser::ast;
    use serde_json::json;
    use test_env_log::test;

    fn fragments_and_operations(query: &str) -> (Vec<Operation>, HashMap<String, Fragment>) {
        let parser = apollo_parser::Parser::new(&query);
        let tree = parser.parse();

        if !tree.errors().is_empty() {
            let errors = tree
                .errors()
                .iter()
                .map(|err| format!("{:?}", err))
                .collect::<Vec<_>>();
            panic!("Parsing error(s): {}", errors.join(", "));
        }

        let mut operations = Vec::new();
        let mut fragments = HashMap::new();

        let document = tree.document();

        for definition in document.definitions() {
            match definition {
                // Spec: https://spec.graphql.org/draft/#sec-Language.Operations
                ast::Definition::OperationDefinition(operation) => {
                    operations.push(operation.into());
                }
                ast::Definition::FragmentDefinition(fragment_definition) => {
                    let fragment: Fragment = fragment_definition.into();
                    fragments.insert(fragment.fragment_name.clone(), fragment);
                }
                _ => {}
            }
        }

        (operations, fragments)
    }

    #[test]
    fn test_request() {
        let result = serde_json::from_str::<Request>(
            json!(
            {
              "query": "query aTest($arg1: String!) { test(who: $arg1) }",
              "operationName": "aTest",
              "variables": { "arg1": "me" },
              "extensions": {"extension": 1}
            })
            .to_string()
            .as_str(),
        );
        assert_eq!(
            result.unwrap(),
            Request::builder()
                .query("query aTest($arg1: String!) { test(who: $arg1) }".to_owned())
                .operation_name(Some("aTest".to_owned()))
                .variables(Arc::new(
                    json!({ "arg1": "me" }).as_object().unwrap().clone()
                ))
                .extensions(json!({"extension": 1}).as_object().cloned().unwrap())
                .build()
        );
    }

    #[test]
    fn test_no_variables() {
        let result = serde_json::from_str::<Request>(
            json!(
            {
              "query": "query aTest($arg1: String!) { test(who: $arg1) }",
              "operationName": "aTest",
              "extensions": {"extension": 1}
            })
            .to_string()
            .as_str(),
        );
        assert_eq!(
            result.unwrap(),
            Request::builder()
                .query("query aTest($arg1: String!) { test(who: $arg1) }".to_owned())
                .operation_name(Some("aTest".to_owned()))
                .extensions(json!({"extension": 1}).as_object().cloned().unwrap())
                .build()
        );
    }

    #[test]
    // rover sends { "variables": null } when running the introspection query,
    // and possibly running other queries as well.
    fn test_variables_is_null() {
        let result = serde_json::from_str::<Request>(
            json!(
            {
              "query": "query aTest($arg1: String!) { test(who: $arg1) }",
              "operationName": "aTest",
              "variables": null,
              "extensions": {"extension": 1}
            })
            .to_string()
            .as_str(),
        );
        assert_eq!(
            result.unwrap(),
            Request::builder()
                .query("query aTest($arg1: String!) { test(who: $arg1) }".to_owned())
                .operation_name(Some("aTest".to_owned()))
                .extensions(json!({"extension": 1}).as_object().cloned().unwrap())
                .build()
        );
    }

    macro_rules! assert_eq_and_ordered {
        ($a:expr, $b:expr $(,)?) => {
            assert_eq!($a, $b,);
            assert!(
                $a.eq_and_ordered(&$b),
                "assertion failed: objects are not ordered the same:\
                \n  left: `{:?}`\n right: `{:?}`",
                $a,
                $b,
            );
        };
    }

    #[test]
    fn reformat_response_data_field() {
        let query = Query::from(
            r#"{
                foo
                stuff{bar}
                array{bar}
                baz
                alias:baz
                alias_obj:baz_obj{bar}
                alias_array:baz_array{bar}
            }"#,
        );
        let mut response = Response::builder()
            .data(json! {{
                "foo": "1",
                "stuff": {"bar": "2"},
                "array": [{"bar": "3", "baz": "4"}, {"bar": "5", "baz": "6"}],
                "baz": "7",
                "alias": "7",
                "alias_obj": {"bar": "8"},
                "alias_array": [{"bar": "9", "baz": "10"}, {"bar": "11", "baz": "12"}],
                "other": "13",
            }})
            .build();
        let (operations, fragments) = fragments_and_operations(&query.string);
        query.format_response(&mut response, &operations, &fragments);
        assert_eq_and_ordered!(
            response.data,
            json! {{
                "foo": "1",
                "stuff": {
                    "bar": "2",
                },
                "array": [
                    {"bar": "3"},
                    {"bar": "5"},
                ],
                "baz": "7",
                "alias": "7",
                "alias_obj": {
                    "bar": "8",
                },
                "alias_array": [
                    {"bar": "9"},
                    {"bar": "11"},
                ],
            }},
        );
    }

    #[test]
    fn reformat_response_data_inline_fragment() {
        let query = Query::from(r#"{... on Stuff { stuff{bar}}}"#);
        let mut response = Response::builder()
            .data(json! {{"stuff": {"bar": "2"}}})
            .build();
        let (operations, fragments) = fragments_and_operations(&query.string);

        query.format_response(&mut response, &operations, &fragments);
        assert_eq_and_ordered!(
            response.data,
            json! {{
                "stuff": {
                    "bar": "2",
                },
            }},
        );
    }

    #[test]
    fn reformat_response_data_fragment_spread() {
        let query =
            Query::from(r#"{...foo ...bar} fragment foo on Foo {foo} fragment bar on Bar {bar}"#);
        let mut response = Response::builder()
            .data(json! {{"foo": "1", "bar": "2"}})
            .build();
        let (operations, fragments) = fragments_and_operations(&query.string);

        query.format_response(&mut response, &operations, &fragments);
        assert_eq_and_ordered!(
            response.data,
            json! {{
                "foo": "1",
                "bar": "2",
            }},
        );
    }

    #[test]
    fn reformat_response_data_best_effort() {
        let query = Query::from(r#"{foo stuff{bar baz} ...fragment array{bar baz} other{bar}}"#);
        let mut response = Response::builder()
            .data(json! {{
                "foo": "1",
                "stuff": {"baz": "2"},
                "array": [
                    {"baz": "3"},
                    "4",
                    {"bar": "5"},
                ],
                "other": "6",
            }})
            .build();
        let (operations, fragments) = fragments_and_operations(&query.string);

        query.format_response(&mut response, &operations, &fragments);
        assert_eq_and_ordered!(
            response.data,
            json! {{
                "foo": "1",
                "stuff": {
                    "baz": "2",
                },
                "array": [
                    {"baz": "3"},
                    "4",
                    {"bar": "5"},
                ],
                "other": "6",
            }},
        );
    }
}
