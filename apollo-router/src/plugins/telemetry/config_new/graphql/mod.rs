use apollo_compiler::ast::NamedType;
use apollo_compiler::executable::Field;
use apollo_compiler::ExecutableDocument;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json_bytes::Value;
use tower::BoxError;

use super::instruments::CustomCounter;
use super::instruments::CustomInstruments;
use crate::graphql::ResponseVisitor;
use crate::json_ext::Object;
use crate::plugins::telemetry::config_new::attributes::DefaultAttributeRequirementLevel;
use crate::plugins::telemetry::config_new::extendable::Extendable;
use crate::plugins::telemetry::config_new::graphql::attributes::GraphQLAttributes;
use crate::plugins::telemetry::config_new::graphql::selectors::GraphQLSelector;
use crate::plugins::telemetry::config_new::graphql::selectors::GraphQLValue;
use crate::plugins::telemetry::config_new::instruments::CustomHistogram;
use crate::plugins::telemetry::config_new::instruments::DefaultedStandardInstrument;
use crate::plugins::telemetry::config_new::instruments::Instrumented;
use crate::plugins::telemetry::config_new::DefaultForLevel;
use crate::plugins::telemetry::otlp::TelemetryDataKind;
use crate::services::supergraph;
use crate::Context;

pub(crate) mod attributes;
pub(crate) mod selectors;

pub(crate) const FIELD_LENGTH: &str = "graphql.field.list.length";
pub(crate) const FIELD_EXECUTION: &str = "graphql.field.execution";

#[derive(Deserialize, JsonSchema, Clone, Default, Debug)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct GraphQLInstrumentsConfig {
    /// A histogram of the length of a selected field in the GraphQL response
    #[serde(rename = "list.length")]
    pub(crate) list_length:
        DefaultedStandardInstrument<Extendable<GraphQLAttributes, GraphQLSelector>>,

    /// A counter of the number of times a field is used.
    #[serde(rename = "field.execution")]
    pub(crate) field_execution:
        DefaultedStandardInstrument<Extendable<GraphQLAttributes, GraphQLSelector>>,
}

impl DefaultForLevel for GraphQLInstrumentsConfig {
    fn defaults_for_level(
        &mut self,
        requirement_level: DefaultAttributeRequirementLevel,
        kind: TelemetryDataKind,
    ) {
        if self.list_length.is_enabled() {
            self.list_length.defaults_for_level(requirement_level, kind);
        }
        if self.field_execution.is_enabled() {
            self.field_execution
                .defaults_for_level(requirement_level, kind);
        }
    }
}

pub(crate) type GraphQLCustomInstruments = CustomInstruments<
    supergraph::Request,
    supergraph::Response,
    GraphQLAttributes,
    GraphQLSelector,
    GraphQLValue,
>;

pub(crate) struct GraphQLInstruments {
    pub(crate) list_length: Option<
        CustomHistogram<
            supergraph::Request,
            supergraph::Response,
            GraphQLAttributes,
            GraphQLSelector,
        >,
    >,
    pub(crate) field_execution: Option<
        CustomCounter<
            supergraph::Request,
            supergraph::Response,
            GraphQLAttributes,
            GraphQLSelector,
        >,
    >,
    pub(crate) custom: GraphQLCustomInstruments,
}

impl Instrumented for GraphQLInstruments {
    type Request = supergraph::Request;
    type Response = supergraph::Response;
    type EventResponse = crate::graphql::Response;

    fn on_request(&self, request: &Self::Request) {
        if let Some(field_length) = &self.list_length {
            field_length.on_request(request);
        }
        if let Some(field_execution) = &self.field_execution {
            field_execution.on_request(request);
        }
        self.custom.on_request(request);
    }

    fn on_response(&self, response: &Self::Response) {
        if let Some(field_length) = &self.list_length {
            field_length.on_response(response);
        }
        if let Some(field_execution) = &self.field_execution {
            field_execution.on_response(response);
        }
        self.custom.on_response(response);
    }

    fn on_error(&self, error: &BoxError, ctx: &crate::Context) {
        if let Some(field_length) = &self.list_length {
            field_length.on_error(error, ctx);
        }
        if let Some(field_execution) = &self.field_execution {
            field_execution.on_error(error, ctx);
        }
        self.custom.on_error(error, ctx);
    }

    fn on_response_event(&self, response: &Self::EventResponse, ctx: &Context) {
        if let Some(field_length) = &self.list_length {
            field_length.on_response_event(response, ctx);
        }
        if let Some(field_execution) = &self.field_execution {
            field_execution.on_response_event(response, ctx);
        }
        self.custom.on_response_event(response, ctx);

        if !self.custom.is_empty() || self.list_length.is_some() || self.field_execution.is_some() {
            if let Some(executable_document) = ctx.unsupported_executable_document() {
                GraphQLInstrumentsVisitor {
                    ctx,
                    instruments: self,
                }
                .visit(
                    &executable_document,
                    response,
                    &ctx.extensions()
                        .with_lock(|lock| lock.get().cloned())
                        .unwrap_or_default(),
                );
            }
        }
    }

    fn on_response_field(&self, ty: &NamedType, field: &Field, value: &Value, ctx: &Context) {
        if let Some(field_length) = &self.list_length {
            field_length.on_response_field(ty, field, value, ctx);
        }
        if let Some(field_execution) = &self.field_execution {
            field_execution.on_response_field(ty, field, value, ctx);
        }
        self.custom.on_response_field(ty, field, value, ctx);
    }
}

struct GraphQLInstrumentsVisitor<'a> {
    ctx: &'a Context,
    instruments: &'a GraphQLInstruments,
}

impl<'a> ResponseVisitor for GraphQLInstrumentsVisitor<'a> {
    fn visit_field(
        &mut self,
        request: &ExecutableDocument,
        variables: &Object,
        ty: &NamedType,
        field: &Field,
        value: &Value,
    ) {
        self.instruments
            .on_response_field(ty, field, value, self.ctx);

        match value {
            Value::Array(items) => {
                for item in items {
                    self.visit_list_item(
                        request,
                        variables,
                        field.ty().inner_named_type(),
                        field,
                        item,
                    );
                }
            }
            Value::Object(children) => {
                self.visit_selections(request, variables, &field.selection_set, children);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
pub(crate) mod test {

    use super::*;
    use crate::metrics::FutureMetricsExt;
    use crate::plugins::telemetry::Telemetry;
    use crate::plugins::test::PluginTestHarness;
    use crate::Configuration;

    #[test_log::test(tokio::test)]
    async fn basic_metric_publishing() {
        async {
            let schema_str = include_str!(
                "../../../demand_control/cost_calculator/fixtures/federated_ships_schema.graphql"
            );
            let query_str = include_str!("../../../demand_control/cost_calculator/fixtures/federated_ships_named_query.graphql");


            let request = supergraph::Request::fake_builder()
                .query(query_str)
                .context(context(schema_str, query_str))
                .build()
                .unwrap();

            let harness = PluginTestHarness::<Telemetry>::builder()
                .config(include_str!("fixtures/field_length_enabled.router.yaml"))
                .schema(schema_str)
                .build()
                .await;

            harness
                .call_supergraph(request, |req| {
                    let response: serde_json::Value = serde_json::from_str(include_str!(
                        "../../../demand_control/cost_calculator/fixtures/federated_ships_named_response.json"
                    ))
                    .unwrap();
                    supergraph::Response::builder()
                        .data(response["data"].clone())
                        .context(req.context)
                        .build()
                        .unwrap()
                })
                .await
                .unwrap();

            assert_histogram_sum!(
                "graphql.field.list.length",
                2.0,
                "graphql.field.name" = "users",
                "graphql.field.type" = "User",
                "graphql.type.name" = "Query"
            );
        }
        .with_metrics()
        .await;
    }

    #[test_log::test(tokio::test)]
    async fn multiple_fields_metric_publishing() {
        async {
            let schema_str = include_str!(
                "../../../demand_control/cost_calculator/fixtures/federated_ships_schema.graphql"
            );
            let query_str = include_str!("../../../demand_control/cost_calculator/fixtures/federated_ships_fragment_query.graphql");


            let request = supergraph::Request::fake_builder()
                .query(query_str)
                .context(context(schema_str, query_str))
                .build()
                .unwrap();

            let harness: PluginTestHarness<Telemetry> = PluginTestHarness::<Telemetry>::builder()
                .config(include_str!("fixtures/field_length_enabled.router.yaml"))
                .schema(schema_str)
                .build()
                .await;
            harness
                .call_supergraph(request, |req| {
                    let response: serde_json::Value = serde_json::from_str(include_str!(
                        "../../../demand_control/cost_calculator/fixtures/federated_ships_fragment_response.json"
                    ))
                    .unwrap();
                    supergraph::Response::builder()
                        .data(response["data"].clone())
                        .context(req.context)
                        .build()
                        .unwrap()
                })
                .await
                .unwrap();

            assert_histogram_sum!(
                "graphql.field.list.length",
                2.0,
                "graphql.field.name" = "ships",
                "graphql.field.type" = "Ship",
                "graphql.type.name" = "Query"
            );
            assert_histogram_sum!(
                "graphql.field.list.length",
                2.0,
                "graphql.field.name" = "users",
                "graphql.field.type" = "User",
                "graphql.type.name" = "Query"
            );
        }
        .with_metrics()
        .await;
    }

    #[test_log::test(tokio::test)]
    async fn disabled_metric_publishing() {
        async {
            let schema_str = include_str!(
                "../../../demand_control/cost_calculator/fixtures/federated_ships_schema.graphql"
            );
            let query_str = include_str!("../../../demand_control/cost_calculator/fixtures/federated_ships_named_query.graphql");


            let request = supergraph::Request::fake_builder()
                .query(query_str)
                .context(context(schema_str, query_str))
                .build()
                .unwrap();

            let harness = PluginTestHarness::<Telemetry>::builder()
                .config(include_str!("fixtures/field_length_disabled.router.yaml"))
                .schema(schema_str)
                .build()
                .await;

            harness
                .call_supergraph(request, |req| {
                    let response: serde_json::Value = serde_json::from_str(include_str!(
                        "../../../demand_control/cost_calculator/fixtures/federated_ships_named_response.json"
                    ))
                    .unwrap();
                    supergraph::Response::builder()
                        .data(response["data"].clone())
                        .context(req.context)
                        .build()
                        .unwrap()
                })
                .await
                .unwrap();

            assert_histogram_not_exists!("graphql.field.list.length", f64);
        }
        .with_metrics()
        .await;
    }

    #[test_log::test(tokio::test)]
    async fn filtered_metric_publishing() {
        async {
            let schema_str = include_str!(
                "../../../demand_control/cost_calculator/fixtures/federated_ships_schema.graphql"
            );
            let query_str = include_str!("../../../demand_control/cost_calculator/fixtures/federated_ships_fragment_query.graphql");


            let request = supergraph::Request::fake_builder()
                .query(query_str)
                .context(context(schema_str, query_str))
                .build()
                .unwrap();

            let harness = PluginTestHarness::<Telemetry>::builder()
                .config(include_str!("fixtures/filtered_field_length.router.yaml"))
                .schema(schema_str)
                .build()
                .await;

            harness
                .call_supergraph(request, |req| {
                    let response: serde_json::Value = serde_json::from_str(include_str!(
                        "../../../demand_control/cost_calculator/fixtures/federated_ships_fragment_response.json"
                    ))
                    .unwrap();
                    supergraph::Response::builder()
                        .data(response["data"].clone())
                        .context(req.context)
                        .build()
                        .unwrap()
                })
                .await
                .unwrap();

            assert_histogram_sum!("ships.list.length", 2.0);
        }
        .with_metrics()
        .await;
    }

    fn context(schema_str: &str, query_str: &str) -> Context {
        let schema = crate::spec::Schema::parse(schema_str, &Default::default()).unwrap();
        let query =
            crate::spec::Query::parse_document(query_str, None, &schema, &Configuration::default())
                .unwrap();
        let context = Context::new();
        context
            .extensions()
            .with_lock(|mut lock| lock.insert(query));

        context
    }
}
