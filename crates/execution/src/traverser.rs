use std::fmt::Formatter;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use derivative::Derivative;
use futures::stream::{empty, iter};
use futures::{Stream, StreamExt};
use serde_json::{Map, Value};

use query_planner::model::{Field, InlineFragment, Selection, SelectionSet};

use crate::json_utils::{deep_merge, JsonUtils};
use crate::PathElement::Flatmap;
use crate::{
    FetchError, GraphQLError, GraphQLPrimaryResponse, GraphQLRequest, GraphQLResponse,
    GraphQLResponseStream, Object, Path, PathElement,
};

#[allow(dead_code)]
type TraverserStream = Pin<Box<dyn Stream<Item = Traverser> + Send>>;

/// A traverser is a object that is used to keep track of paths in the traversal and holds references
/// to the output that we want to collect.
/// Traversers may be cloned but will all share the same output via an Arc<Mutex<_>>
/// Traversers may spawn child traversers with different paths via the stream method.
#[derive(Derivative, Clone)]
#[derivative(Debug)]
pub(crate) struct Traverser {
    path: Path,
    content: Arc<Mutex<Option<Value>>>,
    request: Arc<GraphQLRequest>,

    #[allow(dead_code)]
    #[derivative(Debug(format_with = "Traverser::format_streams"))]
    patches: Arc<Mutex<Vec<GraphQLResponseStream>>>,
    #[allow(dead_code)]
    errors: Arc<Mutex<Vec<GraphQLError>>>,
}

impl Traverser {
    #[allow(dead_code)]
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    #[allow(dead_code)]
    pub(crate) fn content(&self) -> Option<Value> {
        self.content.lock().unwrap().to_owned()
    }

    fn format_streams(
        streams: &Arc<Mutex<Vec<GraphQLResponseStream>>>,
        fmt: &mut Formatter<'_>,
    ) -> std::fmt::Result {
        let streams = streams.lock().unwrap();
        fmt.write_fmt(format_args!("PatchStream[{}]", streams.len()))
    }

    pub(crate) fn new(request: GraphQLRequest) -> Self {
        Self {
            path: Path::empty(),
            content: Arc::new(Mutex::new(Option::None)),
            request: Arc::new(request),
            patches: Arc::new(Mutex::new(vec![])),
            errors: Arc::new(Mutex::new(vec![])),
        }
    }

    pub(crate) fn descendant(&self, path: &Path) -> Traverser {
        let mut new_path = self.path.clone();
        new_path.append(&path);
        Traverser {
            path: new_path,
            ..self.to_owned()
        }
    }

    pub(crate) fn add_err(&self, err: &FetchError) {
        self.errors.lock().unwrap().push(GraphQLError {
            message: err.to_string(),
            locations: vec![],
            path: self.path.to_owned(),
            extensions: None,
        });
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

    pub(crate) fn merge(self, value: Option<&Value>) -> Traverser {
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
                (None, Some(b)) => *content = Some(b.to_owned()),
                (_, None) => (),
            };
        }
        self
    }

    /// Create a stream of child traversers that match the supplied path in the current content \
    /// relative to the current traverser path.
    pub(crate) fn stream_descendants(&self, path: &Path) -> TraverserStream {
        // The root of our stream. We start at ourself!
        let mut stream = iter(vec![self.to_owned()]).boxed();

        // Split the path on array. We only need to flatmap at arrays.
        let path_split_by_arrays = path
            .to_vec()
            .split_inclusive(|path_element| path_element == &PathElement::Flatmap);

        for path_chunk in path_split_by_arrays {
            // Materialise the path chunk so it can be moved into closures.
            let path_chunk = path_chunk.to_vec();
            stream = stream
                .flat_map(move |traverser| {
                    // Fetch the child content and convert it to a stream
                    let descendant = traverser.descendant(&Path::new(&path_chunk));
                    let content = &descendant.content.lock().unwrap();
                    let content_at_path = content.get_at_path(&descendant.path);

                    match content_at_path {
                        // This was an array and we wanted a flatmap
                        Some(Value::Array(array)) if Some(&Flatmap) == path_chunk.last() => {
                            let parent = descendant.parent();
                            iter(0..array.len())
                                .map(move |index| {
                                    parent.descendant(&Path::new(&[PathElement::Index(index)]))
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

    pub(crate) fn parent(&self) -> Traverser {
        Traverser {
            path: self.path.parent(),
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
                output
                    .entry(field.name.to_owned())
                    .and_modify(|existing| deep_merge(existing, &value))
                    .or_insert(value);
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
    use serde_json::Value;

    use query_planner::model::SelectionSet;

    use crate::traverser::Traverser;
    use crate::{GraphQLError, GraphQLRequest, Path};

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
            path: Path::empty(),
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
            path: Path::empty(),
            extensions: None,
            locations: vec![],
            message: "Nooo".into(),
        }
    }

    #[tokio::test]
    async fn test_stream_no_array() {
        assert_eq!(
            stub_traverser()
                .stream_descendants(&Path::parse("obj".into()))
                .collect::<Vec<Traverser>>()
                .await,
            vec![Traverser {
                path: Path::parse("obj".into()),
                content: stub_traverser().content,
                request: Arc::new(stub_request()),
                patches: Arc::new(Mutex::new(vec![])),
                errors: Arc::new(Mutex::new(vec![fetch_error()])),
            }]
        )
    }

    #[tokio::test]
    async fn test_stream_from_obj() {
        assert_eq!(
            stub_traverser()
                .stream_descendants(&Path::parse("obj".into()))
                .next()
                .await
                .unwrap()
                .stream_descendants(&Path::parse("arr".into()))
                .collect::<Vec<Traverser>>()
                .await,
            vec![Traverser {
                path: Path::parse("obj/arr".into()),
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
                .stream_descendants(&Path::parse("obj/arr".into()))
                .collect::<Vec<Traverser>>()
                .await,
            vec![Traverser {
                path: Path::parse("obj/arr".into()),
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
                .stream_descendants(&Path::parse("obj/arr/@/prop1".into()))
                .collect::<Vec<Traverser>>()
                .await,
            vec![
                Traverser {
                    path: Path::parse("obj/arr/0/prop1".into()),
                    content: stub_traverser().content,
                    request: Arc::new(stub_request()),
                    patches: Arc::new(Mutex::new(vec![])),
                    errors: Arc::new(Mutex::new(vec![fetch_error()])),
                },
                Traverser {
                    path: Path::parse("obj/arr/1/prop1".into()),
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
            Path::empty(),
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
            Path::empty(),
        );
        assert_eq!(result, None);
    }

    #[test]
    fn test_selection_no_content() {
        let result = selection(stub_selection(), None, Path::empty());
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
            Path::parse("job".into()),
        );
        assert_eq!(
            result,
            Some(json!({
                "name": "astronaut"
            }))
        );
    }

    fn selection(selection_set: Value, content: Option<Value>, path: Path) -> Option<Value> {
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
