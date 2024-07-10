use apollo_compiler::executable::Field;
use apollo_compiler::executable::NamedType;
use opentelemetry_api::KeyValue;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json_bytes::Value;
use tower::BoxError;

use crate::plugins::cache::entity::CacheSubgraph;
use crate::plugins::telemetry::config_new::graphql::selectors::FieldName;
use crate::plugins::telemetry::config_new::graphql::selectors::FieldType;
use crate::plugins::telemetry::config_new::graphql::selectors::GraphQLSelector;
use crate::plugins::telemetry::config_new::graphql::selectors::ListLength;
use crate::plugins::telemetry::config_new::graphql::selectors::TypeName;
use crate::plugins::telemetry::config_new::selectors::OperationName;
use crate::plugins::telemetry::config_new::DefaultAttributeRequirementLevel;
use crate::plugins::telemetry::config_new::DefaultForLevel;
use crate::plugins::telemetry::config_new::Selector;
use crate::plugins::telemetry::config_new::Selectors;
use crate::plugins::telemetry::otlp::TelemetryDataKind;
use crate::services::subgraph;
use crate::services::supergraph;
use crate::Context;

#[derive(Deserialize, JsonSchema, Clone, Default, Debug, PartialEq)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct CacheAttributes {
    /// Boolean if we have a cache hit or not
    #[serde(rename = "cache.hit")]
    pub(crate) hit: Option<bool>,
    /// Entity type
    #[serde(rename = "entity.type")]
    pub(crate) entity_type: Option<bool>,
}

impl DefaultForLevel for CacheAttributes {
    fn defaults_for_level(
        &mut self,
        requirement_level: DefaultAttributeRequirementLevel,
        kind: TelemetryDataKind,
    ) {
        if let TelemetryDataKind::Metrics = kind {
            if let DefaultAttributeRequirementLevel::Required = requirement_level {
                self.hit.get_or_insert(true);
                self.entity_type.get_or_insert(true);
            }
        }
    }
}

impl Selectors for CacheAttributes {
    type Request = subgraph::Request;
    type Response = subgraph::Response;
    type EventResponse = ();

    fn on_request(&self, _request: &Self::Request) -> Vec<KeyValue> {
        Vec::default()
    }

    fn on_response(&self, response: &Self::Response) -> Vec<KeyValue> {
        // let mut attrs = Vec::new();
        // let is_enabled = self.entity_type == Some(true) || self.hit == Some(true);
        // if !is_enabled {
        //     return attrs;
        // }
        // let subgraph_name = match &response.subgraph_name {
        //     Some(subgraph_name) => subgraph_name,
        //     None => {
        //         return attrs;
        //     }
        // };
        // let cache_info: CacheSubgraph = match response.context.get(subgraph_name).ok().flatten() {
        //     Some(cache_info) => cache_info,
        //     None => {
        //         return attrs;
        //     }
        // };

        // if let Some(true) = self.hit {
        //     if let Some(name) = (GraphQLSelector::FieldName {
        //         field_name: FieldName::String,
        //     })
        //     .on_response_field(ty, field, value, ctx)
        //     {
        //         attrs.push(KeyValue::new("graphql.field.name", name));
        //     }
        // }
        // if let Some(true) = self.entity_type {
        //     if let Some(ty) = (GraphQLSelector::FieldType {
        //         field_type: FieldType::Name,
        //     })
        //     .on_response_field(ty, field, value, ctx)
        //     {
        //         attrs.push(KeyValue::new("graphql.field.type", ty));
        //     }
        // }

        Vec::default()
    }

    fn on_error(&self, _error: &BoxError, _ctx: &Context) -> Vec<KeyValue> {
        Vec::default()
    }
}

// #[cfg(test)]
// mod test {
//     use serde_json_bytes::json;

//     use crate::context::OPERATION_NAME;
//     use crate::plugins::telemetry::config_new::test::field;
//     use crate::plugins::telemetry::config_new::test::ty;
//     use crate::plugins::telemetry::config_new::DefaultForLevel;
//     use crate::plugins::telemetry::config_new::Selectors;
//     use crate::Context;

//     #[test]
//     fn test_default_for_level() {
//         let mut attributes = super::CacheAttributes::default();
//         attributes.defaults_for_level(
//             super::DefaultAttributeRequirementLevel::Required,
//             super::TelemetryDataKind::Metrics,
//         );
//         assert_eq!(attributes.field_name, Some(true));
//         assert_eq!(attributes.field_type, Some(true));
//         assert_eq!(attributes.type_name, Some(true));
//         assert_eq!(attributes.list_length, None);
//         assert_eq!(attributes.operation_name, None);
//     }

//     #[test]
//     fn test_on_response_field_non_list() {
//         let attributes = super::CacheAttributes {
//             field_name: Some(true),
//             field_type: Some(true),
//             list_length: Some(true),
//             operation_name: Some(true),
//             type_name: Some(true),
//         };
//         let ctx = Context::default();
//         let _ = ctx.insert(OPERATION_NAME, "operation_name".to_string());
//         let mut result = Default::default();
//         attributes.on_response_field(&mut result, &ty(), field(), &json!(true), &ctx);
//         assert_eq!(result.len(), 4);
//         assert_eq!(result[0].key.as_str(), "graphql.field.name");
//         assert_eq!(result[0].value.as_str(), "field_name");
//         assert_eq!(result[1].key.as_str(), "graphql.field.type");
//         assert_eq!(result[1].value.as_str(), "field_type");
//         assert_eq!(result[2].key.as_str(), "graphql.type.name");
//         assert_eq!(result[2].value.as_str(), "type_name");
//         assert_eq!(result[3].key.as_str(), "graphql.operation.name");
//         assert_eq!(result[3].value.as_str(), "operation_name");
//     }

//     #[test]
//     fn test_on_response_field_list() {
//         let attributes = super::CacheAttributes {
//             field_name: Some(true),
//             field_type: Some(true),
//             list_length: Some(true),
//             operation_name: Some(true),
//             type_name: Some(true),
//         };
//         let ctx = Context::default();
//         let _ = ctx.insert(OPERATION_NAME, "operation_name".to_string());
//         let mut result = Default::default();
//         attributes.on_response_field(
//             &mut result,
//             &ty(),
//             field(),
//             &json!(vec![true, true, true]),
//             &ctx,
//         );
//         assert_eq!(result.len(), 5);
//         assert_eq!(result[0].key.as_str(), "graphql.field.name");
//         assert_eq!(result[0].value.as_str(), "field_name");
//         assert_eq!(result[1].key.as_str(), "graphql.field.type");
//         assert_eq!(result[1].value.as_str(), "field_type");
//         assert_eq!(result[2].key.as_str(), "graphql.type.name");
//         assert_eq!(result[2].value.as_str(), "type_name");
//         assert_eq!(result[3].key.as_str(), "graphql.list.length");
//         assert_eq!(result[3].value.as_str(), "3");
//         assert_eq!(result[4].key.as_str(), "graphql.operation.name");
//         assert_eq!(result[4].value.as_str(), "operation_name");
//     }
// }
