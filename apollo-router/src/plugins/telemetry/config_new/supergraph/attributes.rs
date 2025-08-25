use std::fmt::Debug;

use opentelemetry::KeyValue;
use opentelemetry_semantic_conventions::trace::GRAPHQL_DOCUMENT;
use opentelemetry_semantic_conventions::trace::GRAPHQL_OPERATION_NAME;
use opentelemetry_semantic_conventions::trace::GRAPHQL_OPERATION_TYPE;
use schemars::JsonSchema;
use serde::Deserialize;
use tower::BoxError;

use crate::Context;
use crate::context::OPERATION_KIND;
use crate::context::OPERATION_NAME;
use crate::plugins::telemetry::config_new::DefaultForLevel;
use crate::plugins::telemetry::config_new::Selectors;
use crate::plugins::telemetry::config_new::attributes::DefaultAttributeRequirementLevel;
use crate::plugins::telemetry::config_new::attributes::StandardAttribute;
use crate::plugins::telemetry::config_new::cost::SupergraphCostAttributes;
use crate::plugins::telemetry::otlp::TelemetryDataKind;
use crate::services::supergraph;

#[derive(Deserialize, JsonSchema, Clone, Default, Debug)]
#[cfg_attr(test, derive(PartialEq))]
#[serde(deny_unknown_fields, default)]
pub(crate) struct SupergraphAttributes {
    /// The GraphQL document being executed.
    /// Examples:
    ///
    /// * `query findBookById { bookById(id: ?) { name } }`
    ///
    /// Requirement level: Recommended
    #[serde(rename = "graphql.document")]
    pub(crate) graphql_document: Option<StandardAttribute>,

    /// The name of the operation being executed.
    /// Examples:
    ///
    /// * findBookById
    ///
    /// Requirement level: Recommended
    #[serde(rename = "graphql.operation.name")]
    pub(crate) graphql_operation_name: Option<StandardAttribute>,

    /// The type of the operation being executed.
    /// Examples:
    ///
    /// * query
    /// * subscription
    /// * mutation
    ///
    /// Requirement level: Recommended
    #[serde(rename = "graphql.operation.type")]
    pub(crate) graphql_operation_type: Option<StandardAttribute>,

    /// Cost attributes for the operation being executed
    #[serde(flatten)]
    pub(crate) cost: SupergraphCostAttributes,
}

impl DefaultForLevel for SupergraphAttributes {
    fn defaults_for_level(
        &mut self,
        requirement_level: DefaultAttributeRequirementLevel,
        _kind: TelemetryDataKind,
    ) {
        match requirement_level {
            DefaultAttributeRequirementLevel::Required => {}
            DefaultAttributeRequirementLevel::Recommended => {
                if self.graphql_document.is_none() {
                    self.graphql_document = Some(StandardAttribute::Bool(true));
                }
                if self.graphql_operation_name.is_none() {
                    self.graphql_operation_name = Some(StandardAttribute::Bool(true));
                }
                if self.graphql_operation_type.is_none() {
                    self.graphql_operation_type = Some(StandardAttribute::Bool(true));
                }
            }
            DefaultAttributeRequirementLevel::None => {}
        }
    }
}

impl Selectors<supergraph::Request, supergraph::Response, crate::graphql::Response>
    for SupergraphAttributes
{
    fn on_request(&self, request: &supergraph::Request) -> Vec<KeyValue> {
        let mut attrs = Vec::new();
        if let Some(key) = self
            .graphql_document
            .as_ref()
            .and_then(|a| a.key(GRAPHQL_DOCUMENT.into()))
            && let Some(query) = &request.supergraph_request.body().query
        {
            attrs.push(KeyValue::new(key, query.clone()));
        }
        if let Some(key) = self
            .graphql_operation_name
            .as_ref()
            .and_then(|a| a.key(GRAPHQL_OPERATION_NAME.into()))
            && let Some(operation_name) = &request
                .context
                .get::<_, String>(OPERATION_NAME)
                .unwrap_or_default()
        {
            attrs.push(KeyValue::new(key, operation_name.clone()));
        }
        if let Some(key) = self
            .graphql_operation_type
            .as_ref()
            .and_then(|a| a.key(GRAPHQL_OPERATION_TYPE.into()))
            && let Some(operation_type) = &request
                .context
                .get::<_, String>(OPERATION_KIND)
                .unwrap_or_default()
        {
            attrs.push(KeyValue::new(key, operation_type.clone()));
        }

        attrs
    }

    fn on_response(&self, response: &supergraph::Response) -> Vec<KeyValue> {
        let mut attrs = Vec::new();
        attrs.append(&mut self.cost.on_response(response));
        attrs
    }

    fn on_response_event(
        &self,
        response: &crate::graphql::Response,
        context: &Context,
    ) -> Vec<KeyValue> {
        let mut attrs = Vec::new();
        attrs.append(&mut self.cost.on_response_event(response, context));
        attrs
    }

    fn on_error(&self, _error: &BoxError, _ctx: &Context) -> Vec<KeyValue> {
        Vec::default()
    }
}

#[cfg(test)]
mod test {
    use opentelemetry_semantic_conventions::trace::GRAPHQL_DOCUMENT;
    use opentelemetry_semantic_conventions::trace::GRAPHQL_OPERATION_NAME;
    use opentelemetry_semantic_conventions::trace::GRAPHQL_OPERATION_TYPE;

    use super::*;
    use crate::context::OPERATION_KIND;
    use crate::context::OPERATION_NAME;
    use crate::plugins::telemetry::config_new::Selectors;
    use crate::services::supergraph;

    #[test]
    fn test_supergraph_graphql_document() {
        let attributes = SupergraphAttributes {
            graphql_document: Some(StandardAttribute::Bool(true)),
            ..Default::default()
        };
        let attributes = attributes.on_request(
            &supergraph::Request::fake_builder()
                .query("query { __typename }")
                .build()
                .unwrap(),
        );
        assert_eq!(
            attributes
                .iter()
                .find(|key_val| key_val.key.as_str() == GRAPHQL_DOCUMENT)
                .map(|key_val| &key_val.value),
            Some(&"query { __typename }".into())
        );
    }

    #[test]
    fn test_supergraph_graphql_operation_name() {
        let attributes = SupergraphAttributes {
            graphql_operation_name: Some(StandardAttribute::Bool(true)),
            ..Default::default()
        };
        let context = crate::Context::new();
        let _ = context.insert(OPERATION_NAME, "topProducts".to_string());
        let attributes = attributes.on_request(
            &supergraph::Request::fake_builder()
                .context(context)
                .build()
                .unwrap(),
        );
        assert_eq!(
            attributes
                .iter()
                .find(|key_val| key_val.key.as_str() == GRAPHQL_OPERATION_NAME)
                .map(|key_val| &key_val.value),
            Some(&"topProducts".into())
        );
        let attributes = SupergraphAttributes {
            graphql_operation_name: Some(StandardAttribute::Aliased {
                alias: String::from("graphql_query"),
            }),
            ..Default::default()
        };
        let context = crate::Context::new();
        let _ = context.insert(OPERATION_NAME, "topProducts".to_string());
        let attributes = attributes.on_request(
            &supergraph::Request::fake_builder()
                .context(context)
                .build()
                .unwrap(),
        );
        assert_eq!(
            attributes
                .iter()
                .find(|key_val| key_val.key.as_str() == "graphql_query")
                .map(|key_val| &key_val.value),
            Some(&"topProducts".into())
        );
    }

    #[test]
    fn test_supergraph_graphql_operation_type() {
        let attributes = SupergraphAttributes {
            graphql_operation_type: Some(StandardAttribute::Bool(true)),
            ..Default::default()
        };
        let context = crate::Context::new();
        let _ = context.insert(OPERATION_KIND, "query".to_string());
        let attributes = attributes.on_request(
            &supergraph::Request::fake_builder()
                .context(context)
                .build()
                .unwrap(),
        );
        assert_eq!(
            attributes
                .iter()
                .find(|key_val| key_val.key.as_str() == GRAPHQL_OPERATION_TYPE)
                .map(|key_val| &key_val.value),
            Some(&"query".into())
        );
    }
}
