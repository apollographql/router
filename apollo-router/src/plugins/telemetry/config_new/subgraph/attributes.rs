use std::fmt::Debug;

use opentelemetry::Key;
use opentelemetry::KeyValue;
use schemars::JsonSchema;
use serde::Deserialize;
use tower::BoxError;

use crate::Context;
use crate::context::OPERATION_KIND;
use crate::plugins::telemetry::config_new::DefaultForLevel;
use crate::plugins::telemetry::config_new::Selectors;
use crate::plugins::telemetry::config_new::attributes::DefaultAttributeRequirementLevel;
use crate::plugins::telemetry::config_new::attributes::HTTP_REQUEST_RESEND_COUNT;
use crate::plugins::telemetry::config_new::attributes::StandardAttribute;
use crate::plugins::telemetry::otlp::TelemetryDataKind;
use crate::services::subgraph;
use crate::services::subgraph::SubgraphRequestId;

pub(crate) const SUBGRAPH_NAME: Key = Key::from_static_str("subgraph.name");
pub(crate) const SUBGRAPH_GRAPHQL_DOCUMENT: Key = Key::from_static_str("subgraph.graphql.document");
pub(crate) const SUBGRAPH_GRAPHQL_OPERATION_NAME: Key =
    Key::from_static_str("subgraph.graphql.operation.name");
pub(crate) const SUBGRAPH_GRAPHQL_OPERATION_TYPE: Key =
    Key::from_static_str("subgraph.graphql.operation.type");

#[derive(Deserialize, JsonSchema, Clone, Default, Debug, buildstructor::Builder)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct SubgraphAttributes {
    /// The name of the subgraph
    /// Examples:
    ///
    /// * products
    ///
    /// Requirement level: Required
    #[serde(rename = "subgraph.name")]
    subgraph_name: Option<StandardAttribute>,

    /// The GraphQL document being executed.
    /// Examples:
    ///
    /// * `query findBookById { bookById(id: ?) { name } }`
    ///
    /// Requirement level: Recommended
    #[serde(rename = "subgraph.graphql.document")]
    graphql_document: Option<StandardAttribute>,

    /// The name of the operation being executed.
    /// Examples:
    ///
    /// * findBookById
    ///
    /// Requirement level: Recommended
    #[serde(rename = "subgraph.graphql.operation.name")]
    graphql_operation_name: Option<StandardAttribute>,

    /// The type of the operation being executed.
    /// Examples:
    ///
    /// * query
    /// * subscription
    /// * mutation
    ///
    /// Requirement level: Recommended
    #[serde(rename = "subgraph.graphql.operation.type")]
    graphql_operation_type: Option<StandardAttribute>,

    /// The number of times the request has been resent
    #[serde(rename = "http.request.resend_count")]
    http_request_resend_count: Option<StandardAttribute>,
}

impl Selectors<subgraph::Request, subgraph::Response, ()> for SubgraphAttributes {
    fn on_request(&self, request: &subgraph::Request) -> Vec<KeyValue> {
        let mut attrs = Vec::new();
        if let Some(key) = self
            .graphql_document
            .as_ref()
            .and_then(|a| a.key(SUBGRAPH_GRAPHQL_DOCUMENT))
            && let Some(query) = &request.subgraph_request.body().query
        {
            attrs.push(KeyValue::new(key, query.clone()));
        }
        if let Some(key) = self
            .graphql_operation_name
            .as_ref()
            .and_then(|a| a.key(SUBGRAPH_GRAPHQL_OPERATION_NAME))
            && let Some(op_name) = &request.subgraph_request.body().operation_name
        {
            attrs.push(KeyValue::new(key, op_name.clone()));
        }
        if let Some(key) = self
            .graphql_operation_type
            .as_ref()
            .and_then(|a| a.key(SUBGRAPH_GRAPHQL_OPERATION_TYPE))
        {
            // Subgraph operation type wil always match the supergraph operation type
            if let Some(operation_type) = &request
                .context
                .get::<_, String>(OPERATION_KIND)
                .unwrap_or_default()
            {
                attrs.push(KeyValue::new(key, operation_type.clone()));
            }
        }
        if let Some(key) = self
            .subgraph_name
            .as_ref()
            .and_then(|a| a.key(SUBGRAPH_NAME))
        {
            attrs.push(KeyValue::new(key, request.subgraph_name.clone()));
        }

        attrs
    }

    fn on_response(&self, response: &subgraph::Response) -> Vec<KeyValue> {
        let mut attrs = Vec::new();
        if let Some(key) = self
            .http_request_resend_count
            .as_ref()
            .and_then(|a| a.key(HTTP_REQUEST_RESEND_COUNT))
            && let Some(resend_count) = response
                .context
                .get::<_, usize>(SubgraphRequestResendCountKey::new(&response.id))
                .ok()
                .flatten()
        {
            attrs.push(KeyValue::new(key, resend_count as i64));
        }

        attrs
    }

    fn on_error(&self, _error: &BoxError, _ctx: &Context) -> Vec<KeyValue> {
        Vec::default()
    }
}

/// Key used in context to save number of retries for a subgraph http request
pub(crate) struct SubgraphRequestResendCountKey<'a> {
    subgraph_req_id: &'a SubgraphRequestId,
}

impl<'a> SubgraphRequestResendCountKey<'a> {
    pub(crate) fn new(subgraph_req_id: &'a SubgraphRequestId) -> Self {
        Self { subgraph_req_id }
    }
}

impl From<SubgraphRequestResendCountKey<'_>> for String {
    fn from(value: SubgraphRequestResendCountKey) -> Self {
        format!(
            "apollo::telemetry::http_request_resend_count_{}",
            value.subgraph_req_id
        )
    }
}

impl DefaultForLevel for SubgraphAttributes {
    fn defaults_for_level(
        &mut self,
        requirement_level: DefaultAttributeRequirementLevel,
        _kind: TelemetryDataKind,
    ) {
        match requirement_level {
            DefaultAttributeRequirementLevel::Required => {
                if self.subgraph_name.is_none() {
                    self.subgraph_name = Some(StandardAttribute::Bool(true));
                }
            }
            DefaultAttributeRequirementLevel::Recommended => {
                if self.subgraph_name.is_none() {
                    self.subgraph_name = Some(StandardAttribute::Bool(true));
                }
                if self.graphql_document.is_none() {
                    self.graphql_document = Some(StandardAttribute::Bool(true));
                }
                if self.graphql_operation_name.is_none() {
                    self.graphql_operation_name = Some(StandardAttribute::Bool(true));
                }
                if self.graphql_operation_type.is_none() {
                    self.graphql_operation_type = Some(StandardAttribute::Bool(true));
                }
                if self.http_request_resend_count.is_none() {
                    self.http_request_resend_count = Some(StandardAttribute::Bool(true));
                }
            }
            DefaultAttributeRequirementLevel::None => {}
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::context::OPERATION_KIND;
    use crate::graphql;
    use crate::plugins::telemetry::config_new::Selectors;
    use crate::services::subgraph;

    #[test]
    fn test_subgraph_graphql_document() {
        let attributes = SubgraphAttributes {
            graphql_document: Some(StandardAttribute::Bool(true)),
            ..Default::default()
        };
        let attributes = attributes.on_request(
            &subgraph::Request::fake_builder()
                .subgraph_request(
                    ::http::Request::builder()
                        .uri("http://localhost/graphql")
                        .body(
                            graphql::Request::fake_builder()
                                .query("query { __typename }")
                                .build(),
                        )
                        .unwrap(),
                )
                .build(),
        );
        assert_eq!(
            attributes
                .iter()
                .find(|key_val| key_val.key == SUBGRAPH_GRAPHQL_DOCUMENT)
                .map(|key_val| &key_val.value),
            Some(&"query { __typename }".into())
        );
    }

    #[test]
    fn test_subgraph_graphql_operation_name() {
        let attributes = SubgraphAttributes {
            graphql_operation_name: Some(StandardAttribute::Bool(true)),
            ..Default::default()
        };

        let attributes = attributes.on_request(
            &subgraph::Request::fake_builder()
                .subgraph_request(
                    ::http::Request::builder()
                        .uri("http://localhost/graphql")
                        .body(
                            graphql::Request::fake_builder()
                                .operation_name("topProducts")
                                .build(),
                        )
                        .unwrap(),
                )
                .build(),
        );
        assert_eq!(
            attributes
                .iter()
                .find(|key_val| key_val.key == SUBGRAPH_GRAPHQL_OPERATION_NAME)
                .map(|key_val| &key_val.value),
            Some(&"topProducts".into())
        );
    }

    #[test]
    fn test_subgraph_graphql_operation_type() {
        let attributes = SubgraphAttributes {
            graphql_operation_type: Some(StandardAttribute::Bool(true)),
            ..Default::default()
        };

        let context = crate::Context::new();
        let _ = context.insert(OPERATION_KIND, "query".to_string());
        let attributes = attributes.on_request(
            &subgraph::Request::fake_builder()
                .context(context)
                .subgraph_request(
                    ::http::Request::builder()
                        .uri("http://localhost/graphql")
                        .body(graphql::Request::fake_builder().build())
                        .unwrap(),
                )
                .build(),
        );
        assert_eq!(
            attributes
                .iter()
                .find(|key_val| key_val.key == SUBGRAPH_GRAPHQL_OPERATION_TYPE)
                .map(|key_val| &key_val.value),
            Some(&"query".into())
        );
    }

    #[test]
    fn test_subgraph_name() {
        let attributes = SubgraphAttributes {
            subgraph_name: Some(StandardAttribute::Bool(true)),
            ..Default::default()
        };

        let attributes = attributes.on_request(
            &subgraph::Request::fake_builder()
                .subgraph_name("products")
                .subgraph_request(
                    ::http::Request::builder()
                        .uri("http://localhost/graphql")
                        .body(graphql::Request::fake_builder().build())
                        .unwrap(),
                )
                .build(),
        );
        assert_eq!(
            attributes
                .iter()
                .find(|key_val| key_val.key == SUBGRAPH_NAME)
                .map(|key_val| &key_val.value),
            Some(&"products".into())
        );
    }
}
