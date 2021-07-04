use std::pin::Pin;
use std::sync::{Arc, Mutex};

use futures::stream::{empty, iter};
use futures::{Stream, StreamExt};
use serde_json::{Map, Value};

use query_planner::model::{Field, InlineFragment, Selection, SelectionSet};

use crate::json_utils::{deep_merge, JsonUtils};
use crate::PathElement::Flatmap;
use crate::{
    FetchError, GraphQLError, GraphQLPrimaryResponse, GraphQLRequest, GraphQLResponse,
    GraphQLResponseStream, Object, PathElement,
};
use derivative::Derivative;
use std::fmt::Formatter;

#[allow(dead_code)]
type TraverserStream = Pin<Box<dyn Stream<Item = Traverser> + Send>>;

/// A traverser is a object that is used to keep track of paths in the traversal and holds references
/// to the output that we want to collect.
/// Traversers may be cloned but will all share the same output via an Arc<Mutex<_>>
/// Traversers may spawn child traversers with different paths via the stream method.
#[derive(Derivative, Clone)]
#[derivative(Debug)]
pub(crate) struct Traverser {
    path: Vec<PathElement>,
    content: Arc<Mutex<Option<Value>>>,
    request: Arc<GraphQLRequest>,

    #[allow(dead_code)]
    #[derivative(Debug(format_with = "Traverser::format_streams"))]
    patches: Arc<Mutex<Vec<GraphQLResponseStream>>>,
    #[allow(dead_code)]
    errors: Arc<Mutex<Vec<GraphQLError>>>,
}

impl Traverser {
    fn format_streams(
        streams: &Arc<Mutex<Vec<GraphQLResponseStream>>>,
        fmt: &mut Formatter,
    ) -> std::fmt::Result {
        let streams = streams.lock().unwrap();
        fmt.write_fmt(format_args!("PatchStream[{}]", streams.len()))
    }

    pub(crate) fn new(request: GraphQLRequest) -> Traverser {
        Traverser {
            path: vec![],
            content: Arc::new(Mutex::new(Option::None)),
            request: Arc::new(request),
            patches: Arc::new(Mutex::new(vec![])),
            errors: Arc::new(Mutex::new(vec![])),
        }
    }

    pub fn descendant(&self, path: &[PathElement]) -> Traverser {
        let mut new_path = self.path.clone();
        new_path.append(&mut path.to_owned());
        Traverser {
            path: new_path,
            ..self.to_owned()
        }
    }

    pub(crate) fn err(self, err: FetchError) -> Traverser {
        self.errors.lock().unwrap().push(GraphQLError {
            message: err.to_string(),
            locations: vec![],
            path: self.path.to_owned(),
            extensions: None,
        });
        self
    }

    pub(crate) fn to_primary(&self) -> GraphQLResponse {
        GraphQLResponse::Primary(GraphQLPrimaryResponse {
            data: match self.content.lock().unwrap().to_owned() {
                Some(Value::Object(obj)) => obj,
                _ => Map::new(),
            },
            has_next: None,
            errors: None,
            extensions: None,
        })
    }

    pub(crate) fn merge(mut self, value: Option<Value>) -> Traverser {
        {
            let mut content = self.content.lock().unwrap();
            match (content.get_at_path_mut(&self.path), value) {
                (Some(a), Some(Value::Object(b)))
                    if { b.contains_key("_entities") && b.len() == 1 } =>
                {
                    if let Some(Value::Array(array)) = b.get("_entities") {
                        for value in array {
                            deep_merge(a, &value);
                        }
                    }
                }
                (Some(a), Some(b)) => {
                    deep_merge(a, &b);
                }
                (None, Some(b)) => *content = Some(b),
                (_, None) => (),
            };
        }
        self
    }

    /// Create a stream of child traversers that match the supplied path in the current content \
    /// relative to the current traverser path.
    pub(crate) fn stream_descendants(&self, path: Vec<PathElement>) -> TraverserStream {
        // The root of our stream. We start at ourself!
        let mut stream = iter(vec![self.to_owned()]).boxed();

        // Split the path on array. We only need to flatmap at arrays.
        let path_split_by_arrays =
            path.split_inclusive(|path_element| path_element == &PathElement::Flatmap);

        for path_chunk in path_split_by_arrays {
            // Materialise the path chunk so it can be moved into closures.
            let path_chunk = path_chunk.to_vec();
            stream = stream
                .flat_map(move |traverser| {
                    // Fetch the child content and convert it to a stream
                    let descendant = traverser.descendant(&path_chunk);
                    let content = &descendant.content.lock().unwrap();
                    let content_at_path = content.get_at_path(&descendant.path);

                    match content_at_path {
                        // This was an array and we wanted a flatmap
                        Some(Value::Array(array)) if Some(&Flatmap) == path_chunk.last() => {
                            let parent = descendant.parent();
                            iter(0..array.len())
                                .map(move |index| {
                                    parent.descendant(&vec![PathElement::Index(index)])
                                })
                                .boxed()
                        }
                        // No flatmap requested, just return the element.
                        Some(_child) if Some(&Flatmap) != path_chunk.last() => {
                            iter(vec![descendant.to_owned()]).boxed()
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
    pub(crate) fn select(&self, selection: &Option<SelectionSet>) -> Option<Value> {
        let content = self.content.lock().unwrap();
        match (content.get_at_path(&self.path), selection) {
            (Some(Value::Object(content)), Some(requires)) => select_object(&content, &requires),
            (_, _) => None,
        }
    }

    fn parent(&self) -> Traverser {
        let mut path = self.path.to_owned();
        path.pop();
        Traverser {
            path,
            ..self.to_owned()
        }
    }
}

fn select_object(content: &Object, selections: &[Selection]) -> Option<Value> {
    let mut output = Object::new();
    for selection in selections {
        match selection {
            Selection::Field(field) => {
                let value = select_field(content, &field)?;
                output.insert(field.name.to_owned(), value);
            }
            Selection::InlineFragment(fragment) => {
                if let Value::Object(value) = select_inline_fragment(content, fragment)? {
                    output.append(&mut value.to_owned())
                }
            }
        };
    }
    Some(Value::Object(output))
}

fn select_field(content: &Object, field: &Field) -> Option<Value> {
    match (&field.selections, content.get(&field.name)) {
        (Some(selections), Some(Value::Object(child))) => select_object(&child, selections),
        (None, Some(value)) => Some(value.to_owned()),
        _ => None,
    }
}

fn select_inline_fragment(content: &Object, fragment: &InlineFragment) -> Option<Value> {
    match (&fragment.type_condition, &content.get("__typename")) {
        (Some(condition), Some(Value::String(typename))) => {
            if condition == typename {
                select_object(content, &fragment.selections)
            } else {
                None
            }
        }
        (None, _) => select_object(content, &fragment.selections),
        (_, _) => None,
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
    use crate::{FetchError, GraphQLError, GraphQLRequest, PathElement};
    use log::LevelFilter;

    impl PartialEq for Traverser {
        fn eq(&self, other: &Self) -> bool {
            self.path.eq(&other.path)
                && self.request.eq(&other.request)
                && self
                    .content
                    .lock()
                    .unwrap()
                    .eq(&other.content.lock().unwrap())
                && self
                    .errors
                    .lock()
                    .unwrap()
                    .eq(&*other.errors.lock().unwrap())
        }
    }

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
            content: Arc::new(Mutex::new(Some(
                json!({"obj":{"arr":[{"prop1":1},{"prop1":2}]}}),
            ))),
            request: Arc::new(stub_request()),
            patches: Arc::new(Mutex::new(vec![])),
            errors: Arc::new(Mutex::new(vec![fetch_error()])),
        }
    }

    fn fetch_error() -> GraphQLError {
        GraphQLError {
            path: vec![],
            extensions: None,
            locations: vec![],
            message: "Nooo".into(),
        }
    }

    #[tokio::test]
    async fn test_stream_no_array() {
        assert_eq!(
            stub_traverser()
                .stream_descendants(vec![Key("obj".into())])
                .collect::<Vec<Traverser>>()
                .await,
            vec![Traverser {
                path: vec![Key("obj".into())],
                content: stub_traverser().content,
                request: Arc::new(stub_request()),
                patches: Arc::new(Mutex::new(vec![])),
                errors: Arc::new(Mutex::new(vec![fetch_error()])),
            }]
        )
    }

    #[tokio::test]
    async fn test_stream_from_obj() {
        let _ = env_logger::builder()
            .filter("execution".into(), LevelFilter::Debug)
            .is_test(true)
            .try_init();
        assert_eq!(
            stub_traverser()
                .stream_descendants(vec![Key("obj".into())])
                .next()
                .await
                .unwrap()
                .stream_descendants(vec![Key("arr".into())])
                .collect::<Vec<Traverser>>()
                .await,
            vec![Traverser {
                path: vec![Key("obj".into()), Key("arr".into())],
                content: stub_traverser().content,
                request: Arc::new(stub_request()),
                patches: Arc::new(Mutex::new(vec![])),
                errors: Arc::new(Mutex::new(vec![fetch_error()])),
            }]
        )
    }

    #[tokio::test]
    async fn test_stream_with_array() {
        assert_eq!(
            stub_traverser()
                .stream_descendants(vec![Key("obj".into()), Key("arr".into())])
                .collect::<Vec<Traverser>>()
                .await,
            vec![Traverser {
                path: vec![Key("obj".into()), Key("arr".into())],
                content: stub_traverser().content,
                request: Arc::new(stub_request()),
                patches: Arc::new(Mutex::new(vec![])),
                errors: Arc::new(Mutex::new(vec![fetch_error()])),
            }]
        )
    }

    #[tokio::test]
    async fn test_stream_flatmap() {
        assert_eq!(
            stub_traverser()
                .stream_descendants(vec![
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
                    content: stub_traverser().content,
                    request: Arc::new(stub_request()),
                    patches: Arc::new(Mutex::new(vec![])),
                    errors: Arc::new(Mutex::new(vec![fetch_error()])),
                },
                Traverser {
                    path: vec![
                        Key("obj".into()),
                        Key("arr".into()),
                        Index(1),
                        Key("prop1".into())
                    ],
                    content: stub_traverser().content,
                    request: Arc::new(stub_request()),
                    patches: Arc::new(Mutex::new(vec![])),
                    errors: Arc::new(Mutex::new(vec![fetch_error()])),
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
            vec![],
        );
        assert_eq!(
            result,
            Some(json!({
                "__typename": "User",
                "id": 2,
                "job": {
                    "name": "astronaut"
                }
            }))
        );
    }

    #[test]
    fn test_selection_missing_field() {
        let result = selection(
            stub_selection(),
            Some(json!({"__typename": "User", "name":"Bob", "job":{"name":"astronaut"}})),
            vec![],
        );
        assert_eq!(result, None);
    }

    #[test]
    fn test_selection_no_content() {
        let result = selection(stub_selection(), None, vec![]);
        assert_eq!(result, None);
    }

    #[test]
    fn test_selection_at_path() {
        let result = selection(
            json!([{
              "kind": "Field",
              "name": "name"
            }]),
            Some(json!({"__typename": "User", "id":2, "name":"Bob", "job":{"name":"astronaut"}})),
            vec![PathElement::Key("job".into())],
        );
        assert_eq!(
            result,
            Some(json!({
                "name": "astronaut"
            }))
        );
    }

    fn selection(
        selection_set: Value,
        content: Option<Value>,
        path: Vec<PathElement>,
    ) -> Option<Value> {
        let selection_set = serde_json::from_value::<SelectionSet>(selection_set).unwrap();

        let traverser = Traverser {
            path,
            content: Arc::new(Mutex::new(content)),
            request: Arc::new(stub_request()),
            patches: Arc::new(Mutex::new(vec![])),
            errors: Arc::new(Mutex::new(vec![])),
        };

        traverser.select(&Some(selection_set))
    }
}
