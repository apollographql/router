use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use ::tracing::Span;
use futures::StreamExt;
use futures::future::ready;
use futures::stream::once;
use http::HeaderValue;
use opentelemetry::KeyValue;
use opentelemetry::trace::TraceId;
use rand::RngExt as _;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;
use uuid::Uuid;

use crate::Context;
use crate::apollo_studio_interop::UsageReporting;
use crate::context::OPERATION_KIND;
use crate::context::OPERATION_NAME;
use crate::graphql::ResponseVisitor;
use crate::layers::ServiceBuilderExt;
use crate::plugins::limits::operation_limits::OperationLimits;
use crate::plugins::telemetry::APOLLO_PRIVATE_QUERY_ALIASES;
use crate::plugins::telemetry::APOLLO_PRIVATE_QUERY_DEPTH;
use crate::plugins::telemetry::APOLLO_PRIVATE_QUERY_HEIGHT;
use crate::plugins::telemetry::APOLLO_PRIVATE_QUERY_ROOT_FIELDS;
use crate::plugins::telemetry::DEFAULT_EXPOSE_TRACE_ID_HEADER_NAME;
use crate::plugins::telemetry::EnableSubgraphFtv1;
use crate::plugins::telemetry::EnabledFeatures;
use crate::plugins::telemetry::Telemetry;
use crate::plugins::telemetry::apollo::OperationSubType;
use crate::plugins::telemetry::apollo_exporter;
use crate::plugins::telemetry::apollo_exporter::Sender;
use crate::plugins::telemetry::config;
use crate::plugins::telemetry::config::AttributeValue;
use crate::plugins::telemetry::config::Conf;
use crate::plugins::telemetry::config::TraceIdFormat;
use crate::plugins::telemetry::config_new::DatadogId;
use crate::plugins::telemetry::config_new::Selectors;
use crate::plugins::telemetry::config_new::cost::add_cost_attributes;
use crate::plugins::telemetry::config_new::graphql::GraphQLInstruments;
use crate::plugins::telemetry::config_new::instruments::Instrumented;
use crate::plugins::telemetry::config_new::instruments::StaticInstrument;
use crate::plugins::telemetry::config_new::instruments::SupergraphInstruments;
use crate::plugins::telemetry::config_new::supergraph::events::SupergraphEvents;
use crate::plugins::telemetry::config_new::trace_id;
use crate::plugins::telemetry::dynamic_attribute::SpanDynAttribute;
use crate::plugins::telemetry::error_counter::count_supergraph_errors;
use crate::plugins::telemetry::metrics::local_type_stats::LocalTypeStatRecorder;
use crate::plugins::telemetry::span_factory;
use crate::plugins::telemetry::tracing::apollo_telemetry::APOLLO_PRIVATE_OPERATION_SIGNATURE;
use crate::query_planner::OperationKind;
use crate::services::SupergraphRequest;
use crate::services::SupergraphResponse;
use crate::services::supergraph;

fn populate_context(field_level_instrumentation_ratio: f64, req: &SupergraphRequest) {
    let context = &req.context;

    // List of custom attributes for metrics
    let mut attributes: HashMap<String, AttributeValue> = HashMap::new();
    if let Some(operation_name) = &req.supergraph_request.body().operation_name {
        attributes.insert(
            OPERATION_NAME.to_string(),
            AttributeValue::String(operation_name.clone()),
        );
    }

    if rand::rng().random_bool(field_level_instrumentation_ratio) {
        context
            .extensions()
            .with_lock(|lock| lock.insert(EnableSubgraphFtv1));
    }
}

async fn update_otel_metrics(
    config: Arc<Conf>,
    context: Context,
    result: Result<SupergraphResponse, BoxError>,
    custom_instruments: SupergraphInstruments,
    custom_events: SupergraphEvents,
    custom_graphql_instruments: GraphQLInstruments,
) -> Result<SupergraphResponse, BoxError> {
    let response = result?;
    let ctx = context.clone();
    // Wait for the first response of the stream
    let (parts, stream) = response.response.into_parts();
    let config_cloned = config.clone();
    let stream = stream.inspect(move |resp| {
        let span = Span::current();
        span.set_span_dyn_attributes(
            config_cloned
                .instrumentation
                .spans
                .supergraph
                .attributes
                .on_response_event(resp, &ctx),
        );
        custom_instruments.on_response_event(resp, &ctx);
        custom_events.on_response_event(resp, &ctx);
        custom_graphql_instruments.on_response_event(resp, &ctx);
    });
    let (first_response, rest) = StreamExt::into_future(stream).await;

    let response = http::Response::from_parts(
        parts,
        once(ready(first_response.unwrap_or_default()))
            .chain(rest)
            .boxed(),
    );

    Ok(SupergraphResponse { context, response })
}

#[allow(clippy::too_many_arguments)]
fn update_metrics_on_response_events(
    ctx: &Context,
    config: Arc<Conf>,
    field_level_instrumentation_ratio: f64,
    sender: Sender,
    start: Instant,
    result: Result<supergraph::Response, BoxError>,
    enabled_features: EnabledFeatures,
) -> Result<supergraph::Response, BoxError> {
    let operation_kind: OperationKind = ctx.get(OPERATION_KIND).ok().flatten().unwrap_or_default();

    match result {
        Err(e) => {
            if !matches!(sender, Sender::Noop) {
                let operation_subtype = (operation_kind == OperationKind::Subscription)
                    .then_some(OperationSubType::SubscriptionRequest);
                Telemetry::update_apollo_metrics(
                    ctx,
                    field_level_instrumentation_ratio,
                    sender,
                    true,
                    start.elapsed(),
                    operation_kind,
                    operation_subtype,
                    Default::default(),
                    enabled_features.clone(),
                );
            }

            Err(e)
        }
        Ok(router_response) => {
            let http_status_is_success = router_response.response.status().is_success();

            // Only send the subscription-request metric if it's an http status in error because we won't always enter the stream after.
            if operation_kind == OperationKind::Subscription && !http_status_is_success {
                Telemetry::update_apollo_metrics(
                    ctx,
                    field_level_instrumentation_ratio,
                    sender.clone(),
                    true,
                    start.elapsed(),
                    operation_kind,
                    Some(OperationSubType::SubscriptionRequest),
                    Default::default(),
                    enabled_features.clone(),
                );
            }
            Ok(router_response.map(move |response_stream| {
                let sender = sender.clone();
                let ctx = ctx.clone();

                // Local field stats are recorded when enabled by the experimental configuration flag.
                // For subscriptions, metrics are sent to Studio after each event, otherwise the metrics
                // are sent to Studio after the last response. In the case of deferred responses, the last
                // response needs to submit an aggregation of metrics across the primary and incremental
                // responses. To avoid submitting duplicates, the recorder's contents are drained each time
                // metrics are submitted.
                let mut local_stat_recorder = LocalTypeStatRecorder::new();

                response_stream
                    .enumerate()
                    .map(move |(idx, response)| {
                        let has_errors = !response.errors.is_empty();
                        if !matches!(sender, Sender::Noop) {
                            if let (true, Some(query)) = (
                                config.apollo.experimental_local_field_metrics,
                                ctx.executable_document(),
                            ) {
                                local_stat_recorder.visit(
                                    &query,
                                    &response,
                                    &ctx.get_demand_control_context()
                                        .map(|c| c.variables)
                                        .unwrap_or_default(),
                                );
                            }

                            if operation_kind == OperationKind::Subscription {
                                // The first empty response is always a heartbeat except if it's an error
                                if idx == 0 {
                                    // Don't count for subscription-request if http status was in error because it has been counted before
                                    if http_status_is_success {
                                        Telemetry::update_apollo_metrics(
                                            &ctx,
                                            field_level_instrumentation_ratio,
                                            sender.clone(),
                                            has_errors,
                                            start.elapsed(),
                                            operation_kind,
                                            Some(OperationSubType::SubscriptionRequest),
                                            std::mem::take(
                                                &mut local_stat_recorder.local_type_stats,
                                            ),
                                            enabled_features.clone(),
                                        );
                                    }
                                } else {
                                    // Only for subscription events
                                    Telemetry::update_apollo_metrics(
                                        &ctx,
                                        field_level_instrumentation_ratio,
                                        sender.clone(),
                                        has_errors,
                                        response
                                            .created_at
                                            .map(|c| c.elapsed())
                                            .unwrap_or_else(|| start.elapsed()),
                                        operation_kind,
                                        Some(OperationSubType::SubscriptionEvent),
                                        std::mem::take(&mut local_stat_recorder.local_type_stats),
                                        enabled_features.clone(),
                                    );
                                }
                            } else {
                                // If it's the last response
                                if !response.has_next.unwrap_or(false) {
                                    Telemetry::update_apollo_metrics(
                                        &ctx,
                                        field_level_instrumentation_ratio,
                                        sender.clone(),
                                        has_errors,
                                        start.elapsed(),
                                        operation_kind,
                                        None,
                                        std::mem::take(&mut local_stat_recorder.local_type_stats),
                                        enabled_features.clone(),
                                    );
                                }
                            }
                        }

                        response
                    })
                    .boxed()
            }))
        }
    }
}

fn add_query_attributes(context: &Context, custom_attributes: &mut Vec<KeyValue>) {
    context.extensions().with_lock(|c| {
        if let Some(limits) = c.get::<OperationLimits<u32>>() {
            custom_attributes.push(KeyValue::new(
                APOLLO_PRIVATE_QUERY_ALIASES.clone(),
                AttributeValue::I64(limits.aliases.into()),
            ));
            custom_attributes.push(KeyValue::new(
                APOLLO_PRIVATE_QUERY_DEPTH.clone(),
                AttributeValue::I64(limits.depth.into()),
            ));
            custom_attributes.push(KeyValue::new(
                APOLLO_PRIVATE_QUERY_HEIGHT.clone(),
                AttributeValue::I64(limits.height.into()),
            ));
            custom_attributes.push(KeyValue::new(
                APOLLO_PRIVATE_QUERY_ROOT_FIELDS.clone(),
                AttributeValue::I64(limits.root_fields.into()),
            ));
        }
    });
}

/// Layer type for [Telemetry::instrument_supergraph_layer].
#[derive(Clone)]
pub(crate) struct InstrumentSupergraphLayer {
    config: Arc<config::Conf>,
    metrics_sender: apollo_exporter::Sender,
    enabled_features: EnabledFeatures,
    field_level_instrumentation_ratio: f64,
    static_supergraph_instruments: Arc<HashMap<String, StaticInstrument>>,
    static_graphql_instruments: Arc<HashMap<String, StaticInstrument>>,
}

impl InstrumentSupergraphLayer {
    fn new(
        config: Arc<config::Conf>,
        metrics_sender: apollo_exporter::Sender,
        enabled_features: EnabledFeatures,
        field_level_instrumentation_ratio: f64,
        static_supergraph_instruments: Arc<HashMap<String, StaticInstrument>>,
        static_graphql_instruments: Arc<HashMap<String, StaticInstrument>>,
    ) -> Self {
        Self {
            config,
            metrics_sender,
            enabled_features,
            field_level_instrumentation_ratio,
            static_supergraph_instruments,
            static_graphql_instruments,
        }
    }
}

impl<S> tower::Layer<S> for InstrumentSupergraphLayer
where
    S: tower::Service<SupergraphRequest, Response = SupergraphResponse, Error = BoxError>
        + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
{
    type Service = supergraph::BoxCloneService;

    fn layer(&self, service: S) -> Self::Service {
        let metrics_sender = self.metrics_sender.clone();
        let config = self.config.clone();
        let config_instrument = self.config.clone();
        let config_map_res_first = config.clone();
        let config_map_res = config.clone();
        let enabled_features = self.enabled_features.clone();
        let field_level_instrumentation_ratio = self.field_level_instrumentation_ratio;
        let static_supergraph_instruments = self.static_supergraph_instruments.clone();
        let static_graphql_instruments = self.static_graphql_instruments.clone();
        ServiceBuilder::new()
            .instrument(move |supergraph_req: &SupergraphRequest| {
                span_factory::create_supergraph(
                    &config_instrument.apollo,
                    supergraph_req,
                    field_level_instrumentation_ratio,
                )
            })
            .map_response(move |mut resp: SupergraphResponse| {
                let config = config_map_res_first.clone();
                if let Some(usage_reporting) = resp
                    .context
                    .extensions()
                    .with_lock(|lock| lock.get::<Arc<UsageReporting>>().cloned())
                {
                    // Record the operation signature on the router span
                    Span::current().record(
                        APOLLO_PRIVATE_OPERATION_SIGNATURE.as_str(),
                        usage_reporting.get_stats_report_key().as_str(),
                    );
                }
                // To expose trace_id or not
                let expose_trace_id_header =
                    config.exporters.tracing.response_trace_id.enabled.then(|| {
                        config
                            .exporters
                            .tracing
                            .response_trace_id
                            .header_name
                            .clone()
                            .unwrap_or_else(|| DEFAULT_EXPOSE_TRACE_ID_HEADER_NAME.clone())
                    });

                // Append the trace ID with the right format, based on the config
                let format_id = |trace_id: TraceId| {
                    let id = match config.exporters.tracing.response_trace_id.format {
                        TraceIdFormat::Hexadecimal | TraceIdFormat::OpenTelemetry => {
                            format!("{trace_id:032x}")
                        }
                        TraceIdFormat::Decimal => {
                            format!("{}", u128::from_be_bytes(trace_id.to_bytes()))
                        }
                        TraceIdFormat::Datadog => trace_id.to_datadog(),
                        TraceIdFormat::Uuid => Uuid::from_bytes(trace_id.to_bytes()).to_string(),
                    };

                    HeaderValue::from_str(&id).ok()
                };
                if let (Some(header_name), Some(trace_id)) =
                    (expose_trace_id_header, trace_id().and_then(format_id))
                {
                    resp.response.headers_mut().append(header_name, trace_id);
                }

                resp
            })
            .map_future_with_request_data(
                move |req: &SupergraphRequest| {
                    let custom_attributes = config
                        .instrumentation
                        .spans
                        .supergraph
                        .attributes
                        .on_request(req);
                    populate_context(field_level_instrumentation_ratio, req);
                    let custom_instruments = config
                        .instrumentation
                        .instruments
                        .new_supergraph_instruments(static_supergraph_instruments.clone());
                    custom_instruments.on_request(req);
                    let custom_graphql_instruments: GraphQLInstruments = config
                        .instrumentation
                        .instruments
                        .new_graphql_instruments(static_graphql_instruments.clone());
                    custom_graphql_instruments.on_request(req);

                    let mut supergraph_events =
                        config.instrumentation.events.new_supergraph_events();
                    supergraph_events.on_request(req);

                    (
                        req.context.clone(),
                        custom_instruments,
                        custom_attributes,
                        supergraph_events,
                        custom_graphql_instruments,
                    )
                },
                move |(
                    ctx,
                    custom_instruments,
                    mut custom_attributes,
                    mut supergraph_events,
                    custom_graphql_instruments,
                ): (
                    Context,
                    SupergraphInstruments,
                    Vec<KeyValue>,
                    SupergraphEvents,
                    GraphQLInstruments,
                ),
                      fut| {
                    let config = config_map_res.clone();
                    let sender = metrics_sender.clone();
                    let enabled_features = enabled_features.clone();
                    let start = Instant::now();

                    async move {
                        let span = Span::current();
                        let mut result: Result<SupergraphResponse, BoxError> = fut.await;

                        add_query_attributes(&ctx, &mut custom_attributes);
                        add_cost_attributes(&ctx, &mut custom_attributes);
                        span.set_span_dyn_attributes(custom_attributes);
                        match &result {
                            Ok(resp) => {
                                span.set_span_dyn_attributes(
                                    config
                                        .instrumentation
                                        .spans
                                        .supergraph
                                        .attributes
                                        .on_response(resp),
                                );
                                custom_instruments.on_response(resp);
                                supergraph_events.on_response(resp);
                                custom_graphql_instruments.on_response(resp);
                            }
                            Err(err) => {
                                span.set_span_dyn_attributes(
                                    config
                                        .instrumentation
                                        .spans
                                        .supergraph
                                        .attributes
                                        .on_error(err, &ctx),
                                );
                                custom_instruments.on_error(err, &ctx);
                                supergraph_events.on_error(err, &ctx);
                                custom_graphql_instruments.on_error(err, &ctx);
                            }
                        }

                        if let Ok(resp) = result {
                            result = Ok(count_supergraph_errors(resp, &config.apollo.errors).await);
                        }

                        result = update_otel_metrics(
                            config.clone(),
                            ctx.clone(),
                            result,
                            custom_instruments,
                            supergraph_events,
                            custom_graphql_instruments,
                        )
                        .await;
                        update_metrics_on_response_events(
                            &ctx,
                            config,
                            field_level_instrumentation_ratio,
                            sender,
                            start,
                            result,
                            enabled_features,
                        )
                    }
                },
            )
            .service(service)
            .boxed_clone()
    }
}

impl Telemetry {
    /// Returns a layer that instruments the supergraph service with Apollo and custom
    /// instrumentation.
    pub(crate) fn instrument_supergraph_layer(&self) -> InstrumentSupergraphLayer {
        let static_supergraph_instruments = self
            .builtin_instruments
            .read()
            .supergraph_custom_instruments
            .clone();
        let static_graphql_instruments = self
            .builtin_instruments
            .read()
            .graphql_custom_instruments
            .clone();
        InstrumentSupergraphLayer::new(
            self.config.clone(),
            self.apollo_metrics_sender.clone(),
            self.enabled_features.clone(),
            self.field_level_instrumentation_ratio,
            static_supergraph_instruments,
            static_graphql_instruments,
        )
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::Ordering;

    use http::StatusCode;
    use serde_json_bytes::json;
    use tower::Service as _;

    use super::*;
    use crate::metrics::FutureMetricsExt;
    use crate::plugins::test::PluginTestHarness;

    #[tokio::test(flavor = "multi_thread")]
    async fn test_custom_supergraph_instruments() {
        async {
            let plugin = PluginTestHarness::<Telemetry>::builder()
                .config(include_str!("../testdata/custom_instruments.router.yaml"))
                .build()
                .await
                .expect("test harness");

            let (mock_bad_request_service, mut handle) =
                tower_test::mock::pair::<SupergraphRequest, SupergraphResponse>();
            let driver = tokio::spawn(async move {
                for _ in 0..3 {
                    let (req, responder) = handle.next_request().await.unwrap();
                    responder.send_response(
                        SupergraphResponse::fake_builder()
                            .context(req.context)
                            .status_code(StatusCode::BAD_REQUEST)
                            .header("content-type", "application/json")
                            .data(json!({"errors": [{"message": "nope"}]}))
                            .build()
                            .unwrap(),
                    );
                }
            });
            let mut bad_request_supergraph_service = ServiceBuilder::new()
                .layer(plugin.instrument_supergraph_layer())
                .service(mock_bad_request_service);
            let supergraph_req = SupergraphRequest::fake_builder()
                .header("x-custom", "TEST")
                .header("conditional-custom", "X")
                .header("custom-length", "55")
                .header("content-length", "55")
                .header("content-type", "application/graphql")
                .query("Query test { me {name} }")
                .operation_name("test".to_string());
            let _router_response = bad_request_supergraph_service
                .ready()
                .await
                .unwrap()
                .call(supergraph_req.build().unwrap())
                .await
                .unwrap()
                .next_response()
                .await
                .unwrap();

            assert_counter!(
                "acme.graphql.requests",
                1.0,
                "acme.my_attribute" = "application/json",
                "graphql_query" = "Query test { me {name} }",
                "graphql.document" = "Query test { me {name} }"
            );

            let supergraph_req = SupergraphRequest::fake_builder()
                .header("x-custom", "TEST")
                .header("custom-length", "5")
                .header("content-length", "5")
                .header("content-type", "application/graphql")
                .query("Query test { me {name} }")
                .operation_name("test".to_string());

            let _router_response = bad_request_supergraph_service
                .ready()
                .await
                .unwrap()
                .call(supergraph_req.build().unwrap())
                .await
                .unwrap()
                .next_response()
                .await
                .unwrap();
            assert_counter!(
                "acme.graphql.requests",
                2.0,
                "acme.my_attribute" = "application/json",
                "graphql_query" = "Query test { me {name} }",
                "graphql.document" = "Query test { me {name} }"
            );

            let supergraph_req = SupergraphRequest::fake_builder()
                .header("custom-length", "5")
                .header("content-length", "5")
                .header("content-type", "application/graphql")
                .query("Query test { me {name} }")
                .operation_name("test".to_string());

            let _router_response = bad_request_supergraph_service
                .ready()
                .await
                .unwrap()
                .call(supergraph_req.build().unwrap())
                .await
                .unwrap()
                .next_response()
                .await
                .unwrap();
            assert_counter!(
                "acme.graphql.requests",
                2.0,
                "acme.my_attribute" = "application/json",
                "graphql_query" = "Query test { me {name} }",
                "graphql.document" = "Query test { me {name} }"
            );
            drop(bad_request_supergraph_service);
            crate::plugin::test::await_mock_driver(driver).await;
        }
        .with_metrics()
        .await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_field_instrumentation_sampler_with_preview_datadog_agent_sampling() {
        let plugin = PluginTestHarness::<Telemetry>::builder()
            .config(include_str!(
                "../testdata/config.field_instrumentation_sampler.router.yaml"
            ))
            .build()
            .with_metrics()
            .await
            .expect("test harness");

        let ftv1_counter = Arc::new(AtomicUsize::new(0));
        let ftv1_counter_cloned = ftv1_counter.clone();

        let (mock_request_service, mut handle) =
            tower_test::mock::pair::<SupergraphRequest, SupergraphResponse>();
        let driver = tokio::spawn(async move {
            for _ in 0..10 {
                let (req, responder) = handle.next_request().await.unwrap();
                if req
                    .context
                    .extensions()
                    .with_lock(|lock| lock.contains_key::<EnableSubgraphFtv1>())
                {
                    ftv1_counter_cloned.fetch_add(1, Ordering::Relaxed);
                }
                responder.send_response(
                    SupergraphResponse::fake_builder()
                        .context(req.context)
                        .status_code(StatusCode::OK)
                        .header("content-type", "application/json")
                        .data(json!({"errors": [{"message": "nope"}]}))
                        .build()
                        .unwrap(),
                );
            }
        });
        let mut request_supergraph_service = ServiceBuilder::new()
            .layer(plugin.instrument_supergraph_layer())
            .service(mock_request_service);

        for _ in 0..10 {
            let supergraph_req = SupergraphRequest::fake_builder()
                .header("x-custom", "TEST")
                .header("conditional-custom", "X")
                .header("custom-length", "55")
                .header("content-length", "55")
                .header("content-type", "application/graphql")
                .query("Query test { me {name} }")
                .operation_name("test".to_string());
            let _router_response = request_supergraph_service
                .ready()
                .await
                .unwrap()
                .call(supergraph_req.build().unwrap())
                .await
                .unwrap()
                .next_response()
                .await
                .unwrap();
        }
        // It should be 100% because when we set preview_datadog_agent_sampling, we only take the value of field_level_instrumentation_sampler
        drop(request_supergraph_service);
        crate::plugin::test::await_mock_driver(driver).await;
        assert_eq!(ftv1_counter.load(Ordering::Relaxed), 10);
    }
}
