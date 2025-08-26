use std::collections::HashMap;

use apollo_federation::connectors::expand::Connectors;
use opentelemetry::KeyValue;
use opentelemetry::metrics::MeterProvider as _;
use opentelemetry::metrics::ObservableGauge;

use crate::metrics::meter_provider;

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
            .init()
    })
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
    use apollo_federation::connectors::expand::Connectors;

    use crate::metrics::FutureMetricsExt as _;
    use crate::plugins::connectors::tracing::connect_spec_counts;
    use crate::services::connector_service::ConnectorServiceFactory;
    use crate::spec::Schema;

    #[test]
    fn test_connect_spec_counts() {
        let connector = Connector {
            spec: ConnectSpec::V0_1,
            id: ConnectId::new(
                "subgraph_name".into(),
                None,
                name!(Query),
                name!(users),
                None,
                0,
                name!("BaseType"),
            ),
            transport: HttpJsonTransport {
                source_template: "http://localhost/".parse().ok(),
                connect_template: "/path".parse().unwrap(),
                ..Default::default()
            },
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

    const STEEL_THREAD_SCHEMA: &str = include_str!("./testdata/steelthread.graphql");

    #[tokio::test]
    async fn test_connect_spec_version_instrument() {
        async {
            let config = Arc::default();
            let schema = Schema::parse(STEEL_THREAD_SCHEMA, &config).unwrap();
            let _factory = ConnectorServiceFactory::empty(&schema);

            assert_gauge!(
                "apollo.router.schema.connectors",
                6,
                connect.spec.version = "0.1"
            );
        }
        .with_metrics()
        .await;
    }
}
