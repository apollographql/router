use crate::prelude::graphql::*;
use apollo_parser::ast;
use derivative::Derivative;
use std::collections::HashMap;

#[derive(Debug, Derivative)]
#[derivative(PartialEq, Hash, Eq)]
pub struct Query {
    string: String,
    #[derivative(PartialEq = "ignore", Hash = "ignore")]
    fragments: HashMap<String, Vec<Selection>>,
    #[derivative(PartialEq = "ignore", Hash = "ignore")]
    operations: Vec<Operation>,
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
    #[tracing::instrument]
    pub fn format_response(&self, response: &mut Response) {
        for operation in self.operations.iter() {
            if let Some(data) = response.data.as_object_mut() {
                let mut output = Object::default();
                self.apply_selection_set(&operation.selection_set, data, &mut output);
                response.data = output.into();
                return;
            } else {
                failfast_debug!("Invalid type for data in response.");
            }
        }

        failfast_debug!("No suitable definition found. This is a bug.");
    }

    pub fn parse(query: impl Into<String>) -> tokio::task::JoinHandle<Option<Self>> {
        let string = query.into();
        tokio::task::spawn_blocking(move || {
            let parser = apollo_parser::Parser::new(string.as_str());
            let tree = parser.parse();

            if !tree.errors().is_empty() {
                failfast_debug!(
                    "Parsing error(s): {}",
                    tree.errors()
                        .iter()
                        .map(|err| format!("{:?}", err))
                        .collect::<Vec<_>>()
                        .join(", "),
                );
                return None;
            }

            let document = tree.document();
            let fragments = Self::fragments(&document);

            let operations = document
                .definitions()
                .filter_map(|definition| {
                    if let ast::Definition::OperationDefinition(operation) = definition {
                        Some(operation.into())
                    } else {
                        None
                    }
                })
                .collect();

            Some(Query {
                string,
                fragments,
                operations,
            })
        })
    }

    fn fragments(document: &ast::Document) -> HashMap<String, Vec<Selection>> {
        document
            .definitions()
            .filter_map(|definition| match definition {
                // Spec: https://spec.graphql.org/draft/#FragmentDefinition
                ast::Definition::FragmentDefinition(fragment_definition) => {
                    let name = fragment_definition
                        .fragment_name()
                        .expect("the node FragmentName is not optional in the spec; qed")
                        .name()
                        .unwrap()
                        .text()
                        .to_string();
                    let selection_set = fragment_definition
                        .selection_set()
                        .expect("the node SelectionSet is not optional in the spec; qed");

                    Some((name, selection_set.selections().map(Into::into).collect()))
                }
                _ => None,
            })
            .collect()
    }

    fn apply_selection_set(
        &self,
        selection_set: &[Selection],
        input: &mut Object,
        output: &mut Object,
    ) {
        for selection in selection_set {
            match selection {
                Selection::Field {
                    name,
                    selection_set,
                } => {
                    if let Some(input_value) = input.remove(name) {
                        if let Some(selection_set) = selection_set {
                            match input_value {
                                Value::Object(mut input_object) => {
                                    let mut output_object = Object::default();
                                    self.apply_selection_set(
                                        &selection_set,
                                        &mut input_object,
                                        &mut output_object,
                                    );
                                    output.insert(name.to_string(), output_object.into());
                                }
                                Value::Array(input_array) => {
                                    let output_array = input_array
                                        .into_iter()
                                        .enumerate()
                                        .map(|(i, mut element)| {
                                            if let Some(input_object) = element.as_object_mut() {
                                                let mut output_object = Object::default();
                                                self.apply_selection_set(
                                                    &selection_set,
                                                    input_object,
                                                    &mut output_object,
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
                                    output.insert(name.to_string(), output_array);
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
                            output.insert(name.to_string(), input_value);
                        }
                    } else {
                        failfast_debug!("Missing field: {}", name);
                    }
                }
                Selection::InlineFragment { selection_set } => {
                    self.apply_selection_set(&selection_set, input, output);
                }
                Selection::FragmentSpread { name } => {
                    if let Some(selection_set) = self.fragments.get(name) {
                        self.apply_selection_set(selection_set, input, output);
                    } else {
                        failfast_debug!("Missing fragment named: {}", name);
                    }
                }
            }
        }
    }
}

#[derive(Debug)]
enum Selection {
    Field {
        name: String,
        selection_set: Option<Vec<Selection>>,
    },
    InlineFragment {
        selection_set: Vec<Selection>,
    },
    FragmentSpread {
        name: String,
    },
}

impl From<ast::Selection> for Selection {
    fn from(selection: ast::Selection) -> Self {
        match selection {
            // Spec: https://spec.graphql.org/draft/#Field
            ast::Selection::Field(field) => {
                let name = field
                    .name()
                    .expect("the node Name is not optional in the spec; qed")
                    .text()
                    .to_string();
                let alias = field.alias().map(|x| x.name().unwrap().text().to_string());
                let name = alias.unwrap_or(name);
                let selection_set = field
                    .selection_set()
                    .map(|x| x.selections().into_iter().map(Into::into).collect());

                Self::Field {
                    name,
                    selection_set,
                }
            }
            // Spec: https://spec.graphql.org/draft/#InlineFragment
            ast::Selection::InlineFragment(inline_fragment) => {
                let selection_set = inline_fragment
                    .selection_set()
                    .expect("the node SelectionSet is not optional in the spec; qed")
                    .selections()
                    .into_iter()
                    .map(Into::into)
                    .collect();

                Self::InlineFragment { selection_set }
            }
            // Spec: https://spec.graphql.org/draft/#FragmentSpread
            ast::Selection::FragmentSpread(fragment_spread) => {
                let name = fragment_spread
                    .fragment_name()
                    .expect("the node FragmentName is not optional in the spec; qed")
                    .name()
                    .unwrap()
                    .text()
                    .to_string();

                Self::FragmentSpread { name }
            }
        }
    }
}

#[derive(Debug)]
struct Operation {
    selection_set: Vec<Selection>,
}

impl From<ast::OperationDefinition> for Operation {
    fn from(operation: ast::OperationDefinition) -> Self {
        // Spec: https://spec.graphql.org/draft/#sec-Language.Operations
        let selection_set = operation
            .selection_set()
            .expect("the node SelectionSet is not optional in the spec; qed")
            .selections()
            .map(Into::into)
            .collect();

        Operation { selection_set }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use test_log::test;

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
        query.format_response(&mut response);
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
        query.format_response(&mut response);
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
        query.format_response(&mut response);
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
        query.format_response(&mut response);
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
