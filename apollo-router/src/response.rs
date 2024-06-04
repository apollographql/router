#![allow(missing_docs)] // FIXME
use std::time::Instant;

use bytes::Bytes;
use serde::Deserialize;
use serde::Serialize;
use serde_json_bytes::ByteString;
use serde_json_bytes::Map;

use crate::error::Error;
use crate::error::FetchError;
use crate::json_ext::Object;
use crate::json_ext::Path;
use crate::json_ext::Value;

/// A graphql primary response.
/// Used for federated and subgraph queries.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct Response {
    /// The label that was passed to the defer or stream directive for this patch.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub label: Option<String>,

    /// The response data.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub data: Option<Value>,

    /// The path that the data should be merged at.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub path: Option<Path>,

    /// The optional graphql errors encountered.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub errors: Vec<Error>,

    /// The optional graphql extensions.
    #[serde(skip_serializing_if = "Object::is_empty", default)]
    pub extensions: Object,

    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub has_next: Option<bool>,

    #[serde(skip, default)]
    pub subscribed: Option<bool>,

    /// Used for subscription event to compute the duration of a subscription event
    #[serde(skip, default)]
    pub created_at: Option<Instant>,

    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub incremental: Vec<IncrementalResponse>,
}

#[buildstructor::buildstructor]
impl Response {
    /// Constructor
    #[builder(visibility = "pub")]
    #[allow(clippy::too_many_arguments)]
    fn new(
        label: Option<String>,
        data: Option<Value>,
        path: Option<Path>,
        errors: Vec<Error>,
        extensions: Map<ByteString, Value>,
        _subselection: Option<String>,
        has_next: Option<bool>,
        subscribed: Option<bool>,
        incremental: Vec<IncrementalResponse>,
        created_at: Option<Instant>,
    ) -> Self {
        Self {
            label,
            data,
            path,
            errors,
            extensions,
            has_next,
            subscribed,
            incremental,
            created_at,
        }
    }

    /// If path is None, this is a primary response.
    pub fn is_primary(&self) -> bool {
        self.path.is_none()
    }

    /// append_errors default the errors `path` with the one provided.
    pub fn append_errors(&mut self, errors: &mut Vec<Error>) {
        self.errors.append(errors)
    }

    /// Create a [`Response`] from the supplied [`Bytes`].
    ///
    /// This will return an error (identifying the faulty service) if the input is invalid.
    pub(crate) fn from_bytes(service_name: &str, b: Bytes) -> Result<Response, FetchError> {
        let value =
            Value::from_bytes(b).map_err(|error| FetchError::SubrequestMalformedResponse {
                service: service_name.to_string(),
                reason: error.to_string(),
            })?;
        let object =
            ensure_object!(value).map_err(|error| FetchError::SubrequestMalformedResponse {
                service: service_name.to_string(),
                reason: error.to_string(),
            })?;
        Response::from_object(service_name, object)
    }

    pub(crate) fn from_object(
        service_name: &str,
        mut object: Object,
    ) -> Result<Response, FetchError> {
        let data = object.remove("data");
        let errors = extract_key_value_from_object!(object, "errors", Value::Array(v) => v)
            .map_err(|err| FetchError::SubrequestMalformedResponse {
                service: service_name.to_string(),
                reason: err.to_string(),
            })?
            .into_iter()
            .flatten()
            .map(|v| Error::from_value(service_name, v))
            .collect::<Result<Vec<Error>, FetchError>>()?;
        let extensions =
            extract_key_value_from_object!(object, "extensions", Value::Object(o) => o)
                .map_err(|err| FetchError::SubrequestMalformedResponse {
                    service: service_name.to_string(),
                    reason: err.to_string(),
                })?
                .unwrap_or_default();
        let label = extract_key_value_from_object!(object, "label", Value::String(s) => s)
            .map_err(|err| FetchError::SubrequestMalformedResponse {
                service: service_name.to_string(),
                reason: err.to_string(),
            })?
            .map(|s| s.as_str().to_string());
        let path = extract_key_value_from_object!(object, "path")
            .map(serde_json_bytes::from_value)
            .transpose()
            .map_err(|err| FetchError::SubrequestMalformedResponse {
                service: service_name.to_string(),
                reason: err.to_string(),
            })?;
        let has_next = extract_key_value_from_object!(object, "hasNext", Value::Bool(b) => b)
            .map_err(|err| FetchError::SubrequestMalformedResponse {
                service: service_name.to_string(),
                reason: err.to_string(),
            })?;
        let incremental =
            extract_key_value_from_object!(object, "incremental", Value::Array(a) => a).map_err(
                |err| FetchError::SubrequestMalformedResponse {
                    service: service_name.to_string(),
                    reason: err.to_string(),
                },
            )?;
        let incremental: Vec<IncrementalResponse> = match incremental {
            Some(v) => v
                .into_iter()
                .map(serde_json_bytes::from_value)
                .collect::<Result<Vec<IncrementalResponse>, _>>()
                .map_err(|err| FetchError::SubrequestMalformedResponse {
                    service: service_name.to_string(),
                    reason: err.to_string(),
                })?,
            None => vec![],
        };
        // Graphql spec says:
        // If the data entry in the response is not present, the errors entry in the response must not be empty.
        // It must contain at least one error. The errors it contains should indicate why no data was able to be returned.
        if data.is_none() && errors.is_empty() {
            return Err(FetchError::SubrequestMalformedResponse {
                service: service_name.to_string(),
                reason: "graphql response without data must contain at least one error".to_string(),
            });
        }

        Ok(Response {
            label,
            data,
            path,
            errors,
            extensions,
            has_next,
            subscribed: None,
            incremental,
            created_at: None,
        })
    }
}

/// A graphql incremental response.
/// Used with `@defer`
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct IncrementalResponse {
    /// The label that was passed to the defer or stream directive for this patch.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub label: Option<String>,

    /// The response data.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub data: Option<Value>,

    /// The path that the data should be merged at.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub path: Option<Path>,

    /// The optional graphql errors encountered.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub errors: Vec<Error>,

    /// The optional graphql extensions.
    #[serde(skip_serializing_if = "Object::is_empty", default)]
    pub extensions: Object,
}

#[buildstructor::buildstructor]
impl IncrementalResponse {
    /// Constructor
    #[builder(visibility = "pub")]
    fn new(
        label: Option<String>,
        data: Option<Value>,
        path: Option<Path>,
        errors: Vec<Error>,
        extensions: Map<ByteString, Value>,
    ) -> Self {
        Self {
            label,
            data,
            path,
            errors,
            extensions,
        }
    }

    /// append_errors default the errors `path` with the one provided.
    pub fn append_errors(&mut self, errors: &mut Vec<Error>) {
        self.errors.append(errors)
    }
}

pub(crate) trait ResponseVisitor {
    fn visit_field(
        &mut self,
        request: &apollo_compiler::ExecutableDocument,
        _ty: &apollo_compiler::executable::NamedType,
        field: &apollo_compiler::executable::Field,
        value: &Value,
    ) {
        match value {
            Value::Array(items) => {
                for item in items {
                    self.visit_list_item(request, field.ty().inner_named_type(), field, item);
                }
            }
            Value::Object(children) => {
                self.visit_selections(request, &field.selection_set, children);
            }
            _ => {}
        }
    }

    fn visit_list_item(
        &mut self,
        request: &apollo_compiler::ExecutableDocument,
        _ty: &apollo_compiler::executable::NamedType,
        field: &apollo_compiler::executable::Field,
        value: &Value,
    ) {
        match value {
            Value::Array(items) => {
                for item in items {
                    self.visit_list_item(request, _ty, field, item);
                }
            }
            Value::Object(children) => {
                self.visit_selections(request, &field.selection_set, children);
            }
            _ => {}
        }
    }

    fn visit(&mut self, request: &apollo_compiler::ExecutableDocument, response: &Response) {
        if response.path.is_some() {
            // TODO: In this case, we need to find the selection inside `request` corresponding to the path so we can start zipping.
            // Exiting here means any implementing visitor will not operate on deffered responses.
            return;
        }

        if let Some(Value::Object(children)) = &response.data {
            if let Some(operation) = &request.anonymous_operation {
                self.visit_selections(request, &operation.selection_set, children);
            }
            for operation in request.named_operations.values() {
                self.visit_selections(request, &operation.selection_set, children);
            }
        }
    }

    fn visit_selections(
        &mut self,
        request: &apollo_compiler::ExecutableDocument,
        selection_set: &apollo_compiler::executable::SelectionSet,
        fields: &Map<ByteString, Value>,
    ) {
        for selection in &selection_set.selections {
            match selection {
                apollo_compiler::executable::Selection::Field(inner_field) => {
                    if let Some(value) = fields.get(inner_field.name.as_str()) {
                        self.visit_field(request, &selection_set.ty, inner_field.as_ref(), value);
                    } else {
                        tracing::warn!("The response did not include a field corresponding to query field {:?}", inner_field);
                    }
                }
                apollo_compiler::executable::Selection::FragmentSpread(fragment_spread) => {
                    if let Some(fragment) = fragment_spread.fragment_def(request) {
                        self.visit_selections(request, &fragment.selection_set, fields);
                    } else {
                        tracing::warn!(
                            "The fragment {} was not found in the query document.",
                            fragment_spread.fragment_name
                        );
                    }
                }
                apollo_compiler::executable::Selection::InlineFragment(inline_fragment) => {
                    self.visit_selections(request, &inline_fragment.selection_set, fields);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use apollo_compiler::ExecutableDocument;
    use apollo_compiler::Schema;
    use insta::assert_yaml_snapshot;
    use router_bridge::planner::Location;
    use serde::ser::SerializeMap;
    use serde::Serializer;
    use serde_json::json;
    use serde_json_bytes::json as bjson;

    use super::*;

    #[test]
    fn test_append_errors_path_fallback_and_override() {
        let expected_errors = vec![
            Error {
                message: "Something terrible happened!".to_string(),
                path: Some(Path::from("here")),
                ..Default::default()
            },
            Error {
                message: "I mean for real".to_string(),
                ..Default::default()
            },
        ];

        let mut errors_to_append = vec![
            Error {
                message: "Something terrible happened!".to_string(),
                path: Some(Path::from("here")),
                ..Default::default()
            },
            Error {
                message: "I mean for real".to_string(),
                ..Default::default()
            },
        ];

        let mut response = Response::builder().build();
        response.append_errors(&mut errors_to_append);
        assert_eq!(response.errors, expected_errors);
    }

    #[test]
    fn test_response() {
        let result = serde_json::from_str::<Response>(
            json!(
            {
              "errors": [
                {
                  "message": "Name for character with ID 1002 could not be fetched.",
                  "locations": [{ "line": 6, "column": 7 }],
                  "path": ["hero", "heroFriends", 1, "name"],
                  "extensions": {
                    "error-extension": 5,
                  }
                }
              ],
              "data": {
                "hero": {
                  "name": "R2-D2",
                  "heroFriends": [
                    {
                      "id": "1000",
                      "name": "Luke Skywalker"
                    },
                    {
                      "id": "1002",
                      "name": null
                    },
                    {
                      "id": "1003",
                      "name": "Leia Organa"
                    }
                  ]
                }
              },
              "extensions": {
                "response-extension": 3,
              }
            })
            .to_string()
            .as_str(),
        );
        assert_eq!(
            result.unwrap(),
            Response::builder()
                .data(json!({
                  "hero": {
                    "name": "R2-D2",
                    "heroFriends": [
                      {
                        "id": "1000",
                        "name": "Luke Skywalker"
                      },
                      {
                        "id": "1002",
                        "name": null
                      },
                      {
                        "id": "1003",
                        "name": "Leia Organa"
                      }
                    ]
                  }
                }))
                .errors(vec![Error {
                    message: "Name for character with ID 1002 could not be fetched.".into(),
                    locations: vec!(Location { line: 6, column: 7 }),
                    path: Some(Path::from("hero/heroFriends/1/name")),
                    extensions: bjson!({
                        "error-extension": 5,
                    })
                    .as_object()
                    .cloned()
                    .unwrap()
                }])
                .extensions(
                    bjson!({
                        "response-extension": 3,
                    })
                    .as_object()
                    .cloned()
                    .unwrap()
                )
                .build()
        );
    }

    #[test]
    fn test_patch_response() {
        let result = serde_json::from_str::<Response>(
            json!(
            {
              "label": "part",
              "hasNext": true,
              "path": ["hero", "heroFriends", 1, "name"],
              "errors": [
                {
                  "message": "Name for character with ID 1002 could not be fetched.",
                  "locations": [{ "line": 6, "column": 7 }],
                  "path": ["hero", "heroFriends", 1, "name"],
                  "extensions": {
                    "error-extension": 5,
                  }
                }
              ],
              "data": {
                "hero": {
                  "name": "R2-D2",
                  "heroFriends": [
                    {
                      "id": "1000",
                      "name": "Luke Skywalker"
                    },
                    {
                      "id": "1002",
                      "name": null
                    },
                    {
                      "id": "1003",
                      "name": "Leia Organa"
                    }
                  ]
                }
              },
              "extensions": {
                "response-extension": 3,
              }
            })
            .to_string()
            .as_str(),
        );
        assert_eq!(
            result.unwrap(),
            Response::builder()
                .label("part".to_owned())
                .data(json!({
                  "hero": {
                    "name": "R2-D2",
                    "heroFriends": [
                      {
                        "id": "1000",
                        "name": "Luke Skywalker"
                      },
                      {
                        "id": "1002",
                        "name": null
                      },
                      {
                        "id": "1003",
                        "name": "Leia Organa"
                      }
                    ]
                  }
                }))
                .path(Path::from("hero/heroFriends/1/name"))
                .errors(vec![Error {
                    message: "Name for character with ID 1002 could not be fetched.".into(),
                    locations: vec!(Location { line: 6, column: 7 }),
                    path: Some(Path::from("hero/heroFriends/1/name")),
                    extensions: bjson!({
                        "error-extension": 5,
                    })
                    .as_object()
                    .cloned()
                    .unwrap()
                }])
                .extensions(
                    bjson!({
                        "response-extension": 3,
                    })
                    .as_object()
                    .cloned()
                    .unwrap()
                )
                .has_next(true)
                .build()
        );
    }

    #[test]
    fn test_no_data_and_no_errors() {
        let response = Response::from_bytes("test", "{\"errors\":null}".into());
        assert_eq!(
            response.expect_err("no data and no errors"),
            FetchError::SubrequestMalformedResponse {
                service: "test".to_string(),
                reason: "graphql response without data must contain at least one error".to_string(),
            }
        );
    }

    #[test]
    fn test_visit_response() {
        let schema_str = include_str!(
            "plugins/demand_control/cost_calculator/fixtures/federated_ships_schema.graphql"
        );
        let query_str = include_str!("plugins/demand_control/cost_calculator/fixtures/federated_ships_required_query.graphql");
        let response_bytes = include_bytes!("plugins/demand_control/cost_calculator/fixtures/federated_ships_required_response.json");

        let schema = Schema::parse_and_validate(schema_str, "").unwrap();
        let request = ExecutableDocument::parse(&schema, query_str, "").unwrap();
        let response = Response::from_bytes("test", Bytes::from_static(response_bytes)).unwrap();

        let mut visitor = FieldCounter::new();
        visitor.visit(&request, &response);
        insta::with_settings!({sort_maps=>true}, { assert_yaml_snapshot!(visitor) })
    }

    #[test]
    fn test_visit_response_with_fragments() {
        let schema_str = include_str!(
            "plugins/demand_control/cost_calculator/fixtures/federated_ships_schema.graphql"
        );
        let query_str = include_str!("plugins/demand_control/cost_calculator/fixtures/federated_ships_fragment_query.graphql");
        let response_bytes = include_bytes!("plugins/demand_control/cost_calculator/fixtures/federated_ships_fragment_response.json");

        let schema = Schema::parse_and_validate(schema_str, "").unwrap();
        let request = ExecutableDocument::parse(&schema, query_str, "").unwrap();
        let response = Response::from_bytes("test", Bytes::from_static(response_bytes)).unwrap();

        let mut visitor = FieldCounter::new();
        visitor.visit(&request, &response);
        insta::with_settings!({sort_maps=>true}, { assert_yaml_snapshot!(visitor) })
    }

    #[test]
    fn test_visit_response_with_inline_fragments() {
        let schema_str = include_str!(
            "plugins/demand_control/cost_calculator/fixtures/federated_ships_schema.graphql"
        );
        let query_str = include_str!("plugins/demand_control/cost_calculator/fixtures/federated_ships_inline_fragment_query.graphql");
        let response_bytes = include_bytes!("plugins/demand_control/cost_calculator/fixtures/federated_ships_fragment_response.json");

        let schema = Schema::parse_and_validate(schema_str, "").unwrap();
        let request = ExecutableDocument::parse(&schema, query_str, "").unwrap();
        let response = Response::from_bytes("test", Bytes::from_static(response_bytes)).unwrap();

        let mut visitor = FieldCounter::new();
        visitor.visit(&request, &response);
        insta::with_settings!({sort_maps=>true}, { assert_yaml_snapshot!(visitor) })
    }

    #[test]
    fn test_visit_response_with_named_operation() {
        let schema_str = include_str!(
            "plugins/demand_control/cost_calculator/fixtures/federated_ships_schema.graphql"
        );
        let query_str = include_str!(
            "plugins/demand_control/cost_calculator/fixtures/federated_ships_named_query.graphql"
        );
        let response_bytes = include_bytes!(
            "plugins/demand_control/cost_calculator/fixtures/federated_ships_named_response.json"
        );

        let schema = Schema::parse_and_validate(schema_str, "").unwrap();
        let request = ExecutableDocument::parse(&schema, query_str, "").unwrap();
        let response = Response::from_bytes("test", Bytes::from_static(response_bytes)).unwrap();

        let mut visitor = FieldCounter::new();
        visitor.visit(&request, &response);
        insta::with_settings!({sort_maps=>true}, { assert_yaml_snapshot!(visitor) })
    }

    struct FieldCounter {
        counts: HashMap<String, usize>,
    }

    impl FieldCounter {
        fn new() -> Self {
            Self {
                counts: HashMap::new(),
            }
        }
    }

    impl ResponseVisitor for FieldCounter {
        fn visit_field(
            &mut self,
            request: &ExecutableDocument,
            _ty: &apollo_compiler::executable::NamedType,
            field: &apollo_compiler::executable::Field,
            value: &Value,
        ) {
            let count = self.counts.entry(field.name.to_string()).or_insert(0);
            *count += 1;
            match value {
                Value::Array(items) => {
                    for item in items {
                        self.visit_list_item(request, field.ty().inner_named_type(), field, item);
                    }
                }
                Value::Object(children) => {
                    self.visit_selections(request, &field.selection_set, children);
                }
                _ => {}
            }
        }
    }

    impl Serialize for FieldCounter {
        fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
        where
            S: Serializer,
        {
            let mut map = serializer.serialize_map(Some(self.counts.len()))?;
            for (key, value) in &self.counts {
                map.serialize_entry(key, value)?;
            }
            map.end()
        }
    }
}
