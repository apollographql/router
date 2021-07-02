use std::pin::Pin;
use std::sync::{Arc, Mutex};

use futures::stream::{empty, iter};
use futures::{Stream, StreamExt};
use serde_json::Value;

use query_planner::model::{Field, InlineFragment, Selection, SelectionSet};

use crate::json_utils::JsonUtils;
use crate::PathElement::Flatmap;
use crate::{FetchError, GraphQLError, GraphQLRequest, GraphQLResponseStream, Object, PathElement};
use derivative::Derivative;
use std::fmt::Formatter;

#[allow(dead_code)]
type TraverserStream = Pin<Box<dyn Stream<Item = Traverser> + Send>>;

/// Each traverser contains some json content and a path that defines where the content came from.
#[derive(Derivative, Clone)]
#[derivative(Debug, PartialEq)]
pub(crate) struct Traverser {
    pub(crate) path: Vec<PathElement>,
    pub(crate) content: Option<Value>,
    pub(crate) request: Arc<GraphQLRequest>,

    #[allow(dead_code)]
    #[derivative(Debug(format_with = "Traverser::format_streams"))]
    #[derivative(PartialEq = "ignore")]
    pub(crate) patches: Arc<Mutex<Vec<GraphQLResponseStream>>>,
    #[allow(dead_code)]
    #[derivative(PartialEq = "ignore")]
    pub(crate) errors: Arc<Mutex<Vec<GraphQLError>>>,
}

impl Traverser {
    fn format_streams(
        streams: &Arc<Mutex<Vec<GraphQLResponseStream>>>,
        fmt: &mut Formatter,
    ) -> std::fmt::Result {
        let streams = streams.lock().unwrap();
        fmt.write_fmt(format_args!("PatchStream[{}]", streams.len()))
    }

    pub(crate) fn new(request: Arc<GraphQLRequest>) -> Traverser {
        Traverser {
            path: vec![],
            content: Option::None,
            request,
            patches: Arc::new(Mutex::new(vec![])),
            errors: Arc::new(Mutex::new(vec![])),
        }
    }

    pub(crate) fn with_content(&self, content: Option<Value>) -> Traverser {
        Traverser {
            path: self.path.to_owned(),
            content,
            request: self.request.to_owned(),
            patches: self.patches.to_owned(),
            errors: self.errors.to_owned(),
        }
    }

    /// Create a stream of child traversers.
    /// This expands the path supplied such that any flatmap path elements are exploded into all
    /// combinations possible.
    /// The new path is relative and does not include the parent's original path.
    pub(crate) fn stream(&self, path: Vec<PathElement>) -> TraverserStream {
        // The root of our stream. We don't need the parent path as everything is relative to content.
        let mut stream = iter(vec![Traverser {
            path: vec![],
            ..self.to_owned()
        }])
        .boxed();

        // Split the path on array. We only need to flatmap at arrays.
        let path_split_by_arrays =
            path.split_inclusive(|path_element| path_element == &PathElement::Flatmap);

        for path_chunk in path_split_by_arrays {
            // Materialise the path chunk so it can be moved into closures.
            let path_chunk = path_chunk.to_vec();
            stream = stream
                .flat_map(move |context| {
                    // Fetch the child content and convert it to a stream
                    let new_content = context.content.get_at_path(&path_chunk).cloned();

                    // Build up the path
                    let mut new_path = context.path.to_owned();
                    new_path.append(&mut path_chunk.to_owned());

                    match new_content {
                        // This was an array and we wanted a flatmap
                        Some(Value::Array(array)) if Some(&Flatmap) == path_chunk.last() => {
                            new_path.pop();
                            iter(array)
                                .enumerate()
                                .map(move |(index, item)| {
                                    let mut array_path = new_path.to_owned();
                                    array_path.push(PathElement::Index(index));
                                    Traverser {
                                        path: array_path,
                                        content: Some(item),
                                        ..context.to_owned()
                                    }
                                })
                                .boxed()
                        }
                        // No flatmap requested, just return the element.
                        Some(child) if Some(&Flatmap) != path_chunk.last() => {
                            iter(vec![Traverser {
                                path: new_path,
                                content: Some(child),
                                request: context.request,
                                ..context
                            }])
                            .boxed()
                        }
                        // Either there was nothing or there was a flatmap requested on a non array.
                        None | Some(_) => empty().boxed(),
                    }
                })
                .boxed();
        }
        stream
    }

    /// Takes a selection set and extracts a json value from the current content for sending to downstream requests.
    pub(crate) fn select(
        &self,
        selection: Option<SelectionSet>,
    ) -> Result<Option<Value>, FetchError> {
        match (self.content.to_owned(), selection) {
            (_, None) => Ok(None),
            (Some(Value::Object(content)), Some(requires)) => select_object(&content, &requires),
            (_, _) => Err(FetchError::RequestError {
                reason: "Selection on empty content".to_string(),
            }),
        }
    }
}

fn select_object(content: &Object, selections: &[Selection]) -> Result<Option<Value>, FetchError> {
    let mut output = Object::new();
    for selection in selections {
        match selection {
            Selection::Field(field) => {
                if let Some(value) = select_field(content, &field)? {
                    output.insert(field.name.to_owned(), value);
                }
            }
            Selection::InlineFragment(fragment) => {
                if let Some(Value::Object(value)) = select_inline_fragment(content, fragment)? {
                    output.append(&mut value.to_owned())
                }
            }
        };
    }
    Ok(Some(Value::Object(output)))
}

fn select_field(content: &Object, field: &Field) -> Result<Option<Value>, FetchError> {
    match (&field.selections, content.get(&field.name)) {
        (Some(selections), Some(Value::Object(child))) => select_object(&child, selections),
        (None, Some(value)) => Ok(Some(value.to_owned())),
        _ => Err(FetchError::RequestError {
            reason: format!("Missing field '{}'", field.name),
        }),
    }
}

fn select_inline_fragment(
    content: &Object,
    fragment: &InlineFragment,
) -> Result<Option<Value>, FetchError> {
    match (&fragment.type_condition, &content.get("__typename")) {
        (Some(condition), Some(Value::String(typename))) => {
            if condition == typename {
                select_object(content, &fragment.selections)
            } else {
                Ok(None)
            }
        }
        (None, _) => select_object(content, &fragment.selections),
        (_, _) => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use futures::StreamExt;
    use serde_json::json;
    use serde_json::value::Value::Number;
    use serde_json::Value;

    use query_planner::model::SelectionSet;
    use PathElement::{Index, Key};

    use crate::traverser::Traverser;
    use crate::PathElement::Flatmap;
    use crate::{FetchError, GraphQLRequest, PathElement};

    fn stub_request() -> GraphQLRequest {
        GraphQLRequest {
            query: "".to_string(),
            operation_name: None,
            variables: None,
            extensions: None,
        }
    }

    fn stub_traverser() -> Traverser {
        Traverser {
            path: vec![],
            content: Some(json!({"obj":{"arr":[{"prop1":1},{"prop1":2}]}})),
            request: Arc::new(stub_request()),
            patches: Arc::new(Mutex::new(vec![])),
            errors: Arc::new(Mutex::new(vec![])),
        }
    }

    #[tokio::test]
    async fn test_stream_no_array() {
        assert_eq!(
            stub_traverser()
                .stream(vec![Key("obj".into())])
                .collect::<Vec<Traverser>>()
                .await,
            vec![Traverser {
                path: vec![Key("obj".into())],
                content: Some(json!({"arr":[{"prop1":1},{"prop1":2}]})),
                request: Arc::new(stub_request()),
                patches: Arc::new(Mutex::new(vec![])),
                errors: Arc::new(Mutex::new(vec![])),
            }]
        )
    }

    #[tokio::test]
    async fn test_stream_with_array() {
        assert_eq!(
            stub_traverser()
                .stream(vec![Key("obj".into()), Key("arr".into())])
                .collect::<Vec<Traverser>>()
                .await,
            vec![Traverser {
                path: vec![Key("obj".into()), Key("arr".into())],
                content: Some(json!([{"prop1":1},{"prop1":2}])),
                request: Arc::new(stub_request()),
                patches: Arc::new(Mutex::new(vec![])),
                errors: Arc::new(Mutex::new(vec![])),
            }]
        )
    }

    #[tokio::test]
    async fn test_stream_flatmap() {
        assert_eq!(
            stub_traverser()
                .stream(vec![
                    Key("obj".into()),
                    Key("arr".into()),
                    Flatmap,
                    Key("prop1".into())
                ])
                .collect::<Vec<Traverser>>()
                .await,
            vec![
                Traverser {
                    path: vec![
                        Key("obj".into()),
                        Key("arr".into()),
                        Index(0),
                        Key("prop1".into())
                    ],
                    content: Some(Number(1.into())),
                    request: Arc::new(stub_request()),
                    patches: Arc::new(Mutex::new(vec![])),
                    errors: Arc::new(Mutex::new(vec![])),
                },
                Traverser {
                    path: vec![
                        Key("obj".into()),
                        Key("arr".into()),
                        Index(1),
                        Key("prop1".into())
                    ],
                    content: Some(Number(2.into())),
                    request: Arc::new(stub_request()),
                    patches: Arc::new(Mutex::new(vec![])),
                    errors: Arc::new(Mutex::new(vec![])),
                }
            ]
        )
    }

    fn stub_selection() -> Value {
        json!([
          {
            "kind": "InlineFragment",
            "typeCondition": "User",
            "selections": [
              {
                "kind": "Field",
                "name": "__typename"
              },
              {
                "kind": "Field",
                "name": "id"
              },
              {
                "kind": "Field",
                "name": "job",
                "selections": [
                  {
                    "kind": "Field",
                    "name": "name"
                  }]
              }
            ]
          }
        ])
    }

    #[test]
    fn test_selection() {
        let result = selection(
            stub_selection(),
            Some(json!({"__typename": "User", "id":2, "name":"Bob", "job":{"name":"astronaut"}})),
        );
        assert_eq!(
            result,
            Ok(Some(json!({
                "__typename": "User",
                "id": 2,
                "job": {
                    "name": "astronaut"
                }
            })))
        );
    }

    #[test]
    fn test_selection_missing_field() {
        let result = selection(
            stub_selection(),
            Some(json!({"__typename": "User", "name":"Bob", "job":{"name":"astronaut"}})),
        );
        assert_eq!(
            result,
            Err(FetchError::RequestError {
                reason: "Missing field 'id'".into()
            })
        );
    }

    #[test]
    fn test_selection_no_content() {
        let result = selection(stub_selection(), None);
        assert_eq!(
            result,
            Err(FetchError::RequestError {
                reason: "Selection on empty content".into()
            })
        );
    }

    fn selection(
        selection_set: Value,
        content: Option<Value>,
    ) -> Result<Option<Value>, FetchError> {
        let selection_set = serde_json::from_value::<SelectionSet>(selection_set).unwrap();

        let traverser = Traverser {
            path: vec![],
            content,
            request: Arc::new(stub_request()),
            patches: Arc::new(Mutex::new(vec![])),
            errors: Arc::new(Mutex::new(vec![])),
        };

        traverser.select(Some(selection_set))
    }
}
