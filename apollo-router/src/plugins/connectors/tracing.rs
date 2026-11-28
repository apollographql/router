use std::collections::HashMap;

use apollo_federation::connectors::expand::Connectors;
use opentelemetry::KeyValue;
use opentelemetry::metrics::MeterProvider as _;
use opentelemetry::metrics::ObservableGauge;

use crate::metrics::meter_provider;
use crate::spec::Schema;

pub(crate) const CONNECTOR_TYPE_HTTP: &str = "http";

/// Create a gauge instrument for the number of connectors and their spec versions
pub(crate) fn connect_spec_version_instrument(
    connectors: Option<&Connectors>,
) -> Option<ObservableGauge<u64>> {
    connectors.map(|connectors| {
        let spec_counts = connect_spec_counts(connectors);
        meter_provider()
            .meter("apollo/router")
            .u64_observable_gauge("apollo.router.schema.connectors")
            .with_description("Number connect directives in the supergraph")
            .with_callback(move |observer| {
                spec_counts.iter().for_each(|(spec, &count)| {
                    observer.observe(
                        count,
                        &[KeyValue::new("connect.spec.version", spec.clone())],
                    )
                })
            })
            .build()
    })
}

/// Create a gauge instrument for the number of connectors with each `http.stream` mode, or `None`
/// when no connector has one
///
/// Streaming is switched on by the schema, not the router config, so this follows the
/// `apollo.router.schema.connectors` gauge rather than `populate_config_instruments`.
pub(crate) fn connect_stream_mode_instrument(
    connectors: Option<&Connectors>,
) -> Option<ObservableGauge<u64>> {
    let mode_counts = connect_stream_mode_counts(connectors?);
    if mode_counts.is_empty() {
        return None;
    }
    Some(
        meter_provider()
            .meter("apollo/router")
            .u64_observable_gauge("apollo.router.schema.connectors.stream")
            .with_description(
                "Number of connect directives with an http.stream mode in the supergraph",
            )
            .with_callback(move |observer| {
                mode_counts.iter().for_each(|(&mode, &count)| {
                    observer.observe(count, &[KeyValue::new("connect.stream.mode", mode)])
                })
            })
            .build(),
    )
}

/// Map from `http.stream` mode to the number of connectors with that mode
fn connect_stream_mode_counts(connectors: &Connectors) -> HashMap<&'static str, u64> {
    connectors
        .by_service_name
        .values()
        .filter_map(|connector| connector.transport.as_ref()?.stream)
        .fold(HashMap::new(), |mut acc, mode| {
            *acc.entry(mode.as_str()).or_insert(0u64) += 1u64;
            acc
        })
}

/// Create a gauge instrument for the number of connectors on the subscription root type, by
/// `http.stream` mode (`none` for one event per upstream event), or `None` when there are none
pub(crate) fn connector_subscriptions_instrument(schema: &Schema) -> Option<ObservableGauge<u64>> {
    let mode_counts = connector_subscription_counts(schema)?;
    Some(
        meter_provider()
            .meter("apollo/router")
            .u64_observable_gauge("apollo.router.schema.connectors.subscriptions")
            .with_description(
                "Number of connect directives on subscription fields in the supergraph",
            )
            .with_callback(move |observer| {
                mode_counts.iter().for_each(|(&mode, &count)| {
                    observer.observe(count, &[KeyValue::new("connect.stream.mode", mode)])
                })
            })
            .build(),
    )
}

/// Map from `http.stream` mode (`none` when absent) to the number of connectors on the
/// subscription root type with that mode, or `None` if there are none
fn connector_subscription_counts(schema: &Schema) -> Option<HashMap<&'static str, u64>> {
    let subscription = schema
        .supergraph_schema()
        .schema_definition
        .subscription
        .as_ref()?;
    let mode_counts = schema
        .connectors
        .as_ref()?
        .by_service_name
        .values()
        .filter(|connector| connector.id.parent_type_name().as_ref() == Some(&**subscription))
        .map(|connector| {
            connector
                .transport
                .as_ref()
                .and_then(|transport| transport.stream)
                .map_or("none", |mode| mode.as_str())
        })
        .fold(HashMap::new(), |mut acc, mode| {
            *acc.entry(mode).or_insert(0u64) += 1u64;
            acc
        });
    (!mode_counts.is_empty()).then_some(mode_counts)
}

/// Map from connect spec version to the number of connectors with that version
fn connect_spec_counts(connectors: &Connectors) -> HashMap<String, u64> {
    connectors
        .by_service_name
        .values()
        .map(|connector| connector.spec.as_str().to_string())
        .fold(HashMap::new(), |mut acc, spec| {
            *acc.entry(spec).or_insert(0u64) += 1u64;
            acc
        })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use apollo_compiler::name;
    use apollo_federation::connectors::ConnectId;
    use apollo_federation::connectors::ConnectSpec;
    use apollo_federation::connectors::Connector;
    use apollo_federation::connectors::HttpJsonTransport;
    use apollo_federation::connectors::JSONSelection;
    use apollo_federation::connectors::StreamMode;
    use apollo_federation::connectors::expand::Connectors;

    use crate::metrics::FutureMetricsExt as _;
    use crate::plugins::connectors::tracing::connect_spec_counts;
    use crate::plugins::connectors::tracing::connect_stream_mode_counts;
    use crate::services::connector_service::ConnectorServices;
    use crate::spec::Schema;

    #[test]
    fn test_connect_spec_counts() {
        let connector = Connector {
            spec: ConnectSpec::V0_1,
            schema_subtypes_map: Default::default(),
            id: ConnectId::new(
                "subgraph_name".into(),
                None,
                name!(Query),
                name!(users),
                None,
                0,
            ),
            transport: Some(HttpJsonTransport {
                source_template: "http://localhost/".parse().ok(),
                connect_template: "/path".parse().unwrap(),
                ..Default::default()
            }),
            selection: JSONSelection::parse("$.data").unwrap(),
            entity_resolver: None,
            config: Default::default(),
            max_requests: None,
            batch_settings: None,
            request_headers: Default::default(),
            response_headers: Default::default(),
            request_variable_keys: Default::default(),
            response_variable_keys: Default::default(),
            error_settings: Default::default(),
            output_type: None,
            label: "label".into(),
        };

        let connectors = Connectors {
            by_service_name: Arc::new(
                [
                    ("service_name_1".into(), connector.clone()),
                    ("service_name_2".into(), connector.clone()),
                    ("service_name_3".into(), connector),
                ]
                .into(),
            ),
            labels_by_service_name: Default::default(),
            source_config_keys: Default::default(),
        };

        assert_eq!(
            connect_spec_counts(&connectors),
            [(ConnectSpec::V0_1.to_string(), 3u64)].into()
        );
    }

    #[test]
    fn test_connect_stream_mode_counts() {
        let connector = Connector {
            spec: ConnectSpec::V0_5,
            schema_subtypes_map: Default::default(),
            id: ConnectId::new(
                "subgraph_name".into(),
                None,
                name!(Query),
                name!(users),
                None,
                0,
            ),
            transport: Some(HttpJsonTransport {
                source_template: "http://localhost/".parse().ok(),
                connect_template: "/path".parse().unwrap(),
                ..Default::default()
            }),
            selection: JSONSelection::parse("$.data").unwrap(),
            entity_resolver: None,
            config: Default::default(),
            max_requests: None,
            batch_settings: None,
            request_headers: Default::default(),
            response_headers: Default::default(),
            request_variable_keys: Default::default(),
            response_variable_keys: Default::default(),
            error_settings: Default::default(),
            output_type: None,
            label: "label".into(),
        };
        let mut streaming = connector.clone();
        if let Some(transport) = streaming.transport.as_mut() {
            transport.stream = Some(StreamMode::Array);
        }

        let connectors = Connectors {
            by_service_name: Arc::new(
                [
                    ("service_name_1".into(), connector.clone()),
                    ("service_name_2".into(), streaming),
                    ("service_name_3".into(), connector),
                ]
                .into(),
            ),
            labels_by_service_name: Default::default(),
            source_config_keys: Default::default(),
        };

        assert_eq!(
            connect_stream_mode_counts(&connectors),
            [("array", 1u64)].into()
        );
    }

    const STEEL_THREAD_SCHEMA: &str = include_str!("./testdata/steelthread.graphql");

    #[tokio::test]
    async fn test_connect_spec_version_instrument() {
        async {
            let config = Arc::default();
            let schema = Schema::parse(STEEL_THREAD_SCHEMA, &config).unwrap();
            let _services = ConnectorServices::empty(Arc::from(schema));

            assert_gauge!(
                "apollo.router.schema.connectors",
                6,
                connect.spec.version = "0.1"
            );
            // No connector in this schema has a stream mode.
            assert!(!crate::metrics::collect_metrics().metric_exists(
                "apollo.router.schema.connectors.stream",
                crate::metrics::test_utils::MetricType::Gauge,
                &[opentelemetry::KeyValue::new("connect.stream.mode", "array")],
            ));
        }
        .with_metrics()
        .await;
    }

    #[tokio::test]
    async fn test_connect_stream_mode_instrument() {
        async {
            let config = Arc::default();
            // Six connectors with `stream: "array"`, five on fields and one on a type
            let schema = Schema::parse(include_str!("./testdata/stream.graphql"), &config).unwrap();
            let _services = ConnectorServices::empty(Arc::from(schema));

            assert_gauge!(
                "apollo.router.schema.connectors.stream",
                6,
                connect.stream.mode = "array"
            );
        }
        .with_metrics()
        .await;
    }

    #[tokio::test]
    async fn connector_subscriptions_instrument_counts_subscription_connectors() {
        async {
            let config = Arc::default();
            // One connector on `Query`, and four on `Subscription`, one with `stream: "array"`
            let schema =
                Schema::parse(include_str!("./testdata/subscription.graphql"), &config).unwrap();
            let _services = ConnectorServices::empty(Arc::from(schema));

            assert_gauge!(
                "apollo.router.schema.connectors.subscriptions",
                3,
                connect.stream.mode = "none"
            );
            assert_gauge!(
                "apollo.router.schema.connectors.subscriptions",
                1,
                connect.stream.mode = "array"
            );
        }
        .with_metrics()
        .await;
    }

    #[tokio::test]
    async fn connector_subscriptions_instrument_is_absent_without_subscription_connectors() {
        async {
            let config = Arc::default();
            let schema = Schema::parse(STEEL_THREAD_SCHEMA, &config).unwrap();
            let _services = ConnectorServices::empty(Arc::from(schema));

            assert_gauge!(
                "apollo.router.schema.connectors",
                6,
                connect.spec.version = "0.1"
            );
            assert!(!crate::metrics::collect_metrics().metric_exists(
                "apollo.router.schema.connectors.subscriptions",
                crate::metrics::test_utils::MetricType::Gauge,
                &[],
            ));
        }
        .with_metrics()
        .await;
    }
}
