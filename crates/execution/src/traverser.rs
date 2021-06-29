use futures::stream::{empty, iter};
use futures::{Stream, StreamExt};
use serde_json::Value;
use std::pin::Pin;

use crate::PathElement::Flatmap;
use crate::{GraphQLRequest, PathElement};
use std::sync::Arc;

#[allow(dead_code)]
type TraverserStream = Pin<Box<dyn Stream<Item = Traverser> + Send>>;

/// Each traverser contains some json content and a path that defines where the content came from.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Traverser {
    pub(crate) path: Vec<PathElement>,
    pub(crate) content: Option<Value>,
    pub(crate) request: Arc<GraphQLRequest>,
}

impl Traverser {
    pub(crate) fn new(request: Arc<GraphQLRequest>) -> Traverser {
        Traverser {
            path: vec![],
            content: Option::None,
            request,
        }
    }

    /// Create a stream of child traversers.
    /// This expands the path supplied such that any array path elements are exploded into all
    /// combinations possible.
    /// The new path is relative and does not include the parent's original path.
    #[allow(dead_code)]
    pub(crate) fn stream(&self, path: Vec<PathElement>) -> TraverserStream {
        // The root of our stream. We don't need the parent path as everything is relative to content.
        let mut stream = iter(vec![Traverser {
            path: vec![],
            content: self.content.to_owned(),
            request: self.request.to_owned(),
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
                    let new_content = context.content.get_at_path(path_chunk.to_owned()).cloned();

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
                                        request: context.request.to_owned(),
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
}

trait ValueUtils {
    /// Get a reference to the value at a particular path.
    /// Note that a flatmap path element will return an array if that is the value at that path.
    /// It does not actually do any flatmapping, which is instead handled by Traverser::stream.
    fn get_at_path(&self, path: Vec<PathElement>) -> Option<&Value>;
}
impl ValueUtils for Option<Value> {
    fn get_at_path(&self, path: Vec<PathElement>) -> Option<&Value> {
        let mut current = self.as_ref();
        for path_element in path {
            current = match path_element {
                PathElement::Index(index) => current
                    .map(|value| value.as_array())
                    .flatten()
                    .map(|array| array.get(index))
                    .flatten(),
                PathElement::Key(key) => current
                    .map(|value| value.as_object())
                    .flatten()
                    .map(|object| object.get(key.as_str()))
                    .flatten(),
                PathElement::Flatmap => current
                    .map(|value| if value.is_array() { Some(value) } else { None })
                    .flatten(),
            }
        }
        current
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use serde_json::Value;

    use crate::traverser::{Traverser, ValueUtils};
    use crate::PathElement::Flatmap;
    use crate::{GraphQLRequest, PathElement};
    use futures::StreamExt;
    use serde_json::value::Value::Number;
    use std::sync::Arc;
    use PathElement::{Index, Key};
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
        }
    }

    #[test]
    fn test_get_at_path() {
        let json = Some(json!({"obj":{"arr":[{"prop1":1},{"prop1":2}]}}));
        let result = json.get_at_path(vec![
            Key("obj".into()),
            Key("arr".into()),
            Index(1),
            Key("prop1".into()),
        ]);
        assert_eq!(result, Some(&Value::Number(2.into())));
    }

    #[test]
    fn test_get_at_path_flatmap() {
        let json = Some(json!({"obj":{"arr":[{"prop1":1},{"prop1":2}]}}));
        let result = json.get_at_path(vec![Key("obj".into()), Key("arr".into()), Flatmap]);
        assert_eq!(result, Some(&json!([{"prop1":1},{"prop1":2}])));
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
                request: Arc::new(stub_request())
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
                request: Arc::new(stub_request())
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
                    request: Arc::new(stub_request())
                },
                Traverser {
                    path: vec![
                        Key("obj".into()),
                        Key("arr".into()),
                        Index(1),
                        Key("prop1".into())
                    ],
                    content: Some(Number(2.into())),
                    request: Arc::new(stub_request())
                }
            ]
        )
    }
}
