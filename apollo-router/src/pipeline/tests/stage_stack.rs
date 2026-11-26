//! Subgraph and connector source stacks built with the pipeline's own stage builders, for tests of
//! the layers those builders place.
//!
//! A [`tower_test::mock`] sits beneath every placed layer, so its handle sees what the target
//! actually receives. For a subgraph, a stub plugin returns the mock from its hook. For a connector
//! source, the mock is the source's HTTP client.

use std::sync::Arc;

use tower::BoxError;
use tower::ServiceExt;
use tower_test::mock::Mock;

use crate::Configuration;
use crate::pipeline::build_connector_request_services;
use crate::pipeline::build_http_client_service;
use crate::pipeline::build_subgraph_services;
use crate::plugin::DynPlugin;
use crate::plugin::PluginInit;
use crate::plugin::PluginPrivate;
use crate::plugin::PluginUnstable;
use crate::plugins::authorization::AuthorizationPlugin;
use crate::plugins::limits::LimitsPlugin;
use crate::plugins::traffic_shaping::APOLLO_TRAFFIC_SHAPING;
use crate::services::Plugins;
use crate::services::SubgraphRequest;
use crate::services::SubgraphResponse;
use crate::services::SubgraphServices;
use crate::services::connector::request_service;
use crate::services::http::HttpRequest;
use crate::services::http::HttpResponse;
use crate::services::subgraph;

/// The plugins both stages require, in the order the router registers them, then each of
/// `plugins`, built from its config and keyed by full name such as `apollo.traffic_shaping`.
async fn stage_plugins(plugins: &[(&str, serde_json::Value)]) -> Plugins {
    let required = [
        ("apollo.include_subgraph_errors", serde_json::json!({})),
        ("apollo.headers", serde_json::json!({})),
    ];
    let mut registry = Plugins::default();
    for (name, config) in required.iter().chain(plugins) {
        let plugin = crate::plugin::plugins()
            .find(|factory| factory.name == *name)
            .expect("plugin is registered")
            .create_instance_without_schema(config)
            .await
            .expect("plugin builds");
        registry.insert(name.to_string(), plugin);
    }
    registry
}

/// Every mandatory plugin the stack builders apply, with traffic shaping left at its defaults.
pub(crate) async fn required_stage_plugins() -> Arc<Plugins> {
    required_stage_plugins_with_limits(serde_json::json!({})).await
}

/// As [`required_stage_plugins`], with limits built from `limits`.
async fn required_stage_plugins_with_limits(limits: serde_json::Value) -> Arc<Plugins> {
    Arc::new(
        stage_plugins(&[
            (APOLLO_TRAFFIC_SHAPING, serde_json::json!({})),
            ("apollo.limits", limits),
            ("apollo.batching", serde_json::json!({})),
        ])
        .await,
    )
}

/// The mandatory plugins the query parsing service applies, at their defaults and without a
/// schema.
pub(crate) async fn query_parsing_plugins() -> Plugins {
    query_parsing_plugins_for("", &Configuration::default()).await
}

/// The mandatory plugins the query parsing service applies, built for the supergraph `sdl` from
/// `configuration`. An empty `sdl` builds them without a schema.
pub(crate) async fn query_parsing_plugins_for(sdl: &str, configuration: &Configuration) -> Plugins {
    let authorization = <AuthorizationPlugin as PluginPrivate>::new(PluginInit::fake_new(
        AuthorizationPlugin::configuration(configuration),
        Arc::new(sdl.to_string()),
    ))
    .await
    .expect("plugin builds");
    let limits = <LimitsPlugin as PluginPrivate>::new(PluginInit::fake_new(
        configuration.limits.clone(),
        Arc::new(sdl.to_string()),
    ))
    .await
    .expect("plugin builds");
    Plugins::from([
        (
            "apollo.limits".to_string(),
            Box::new(limits) as Box<dyn DynPlugin>,
        ),
        (
            "apollo.authorization".to_string(),
            Box::new(authorization) as Box<dyn DynPlugin>,
        ),
    ])
}

pub(crate) type Handle = tower_test::mock::Handle<SubgraphRequest, SubgraphResponse>;

/// Replaces the subgraph service with `mock`.
struct StubSubgraph {
    mock: Mock<SubgraphRequest, SubgraphResponse>,
}

#[async_trait::async_trait]
impl PluginUnstable for StubSubgraph {
    type Config = ();

    async fn new(_: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        unreachable!("inserted into the plugin registry directly")
    }

    fn subgraph_service(
        &self,
        _subgraph_name: &str,
        _service: subgraph::BoxCloneService,
    ) -> subgraph::BoxCloneService {
        self.mock.clone().boxed_clone()
    }

    fn unstable_method(&self) {}
}

/// The services for subgraph `name`, built with `plugins` configured, and the handle of the mock
/// that stands in for the subgraph.
pub(crate) async fn subgraph_services(
    name: &str,
    plugins: &[(&str, serde_json::Value)],
) -> (SubgraphServices, Handle) {
    let (mock, handle) = tower_test::mock::pair();
    let mut plugins = stage_plugins(plugins).await;
    plugins.insert("stub".to_string(), Box::new(StubSubgraph { mock }));

    let http_services = [(name.to_string(), test_http_client_service(name).await)]
        .into_iter()
        .collect();
    let services =
        build_subgraph_services(http_services, &Arc::new(plugins), &Configuration::default());
    (services, handle)
}

pub(crate) type SourceHandle = tower_test::mock::Handle<HttpRequest, HttpResponse>;

/// A function that hands out clones of the request service for connector source `source`, keyed
/// `<subgraph name>.<source name>` and built with `plugins` configured, and the handle of the mock
/// that stands in for the source.
pub(crate) async fn source_services(
    source: &str,
    plugins: &[(&str, serde_json::Value)],
) -> (
    impl Fn() -> request_service::BoxCloneService + use<>,
    SourceHandle,
) {
    let (mock, handle) = tower_test::mock::pair();
    let plugins = stage_plugins(plugins).await;

    let http_services = [(source.to_string(), mock.boxed_clone())]
        .into_iter()
        .collect();
    let services = build_connector_request_services(http_services, &Arc::new(plugins));
    let source = source.to_string();
    (move || services.get(source.clone()), handle)
}

/// A subgraph HTTP client for `name`, built from default configuration by the pipeline's own
/// [`build_http_client_service`] with every mandatory plugin in the registry.
pub(crate) async fn test_http_client_service(name: &str) -> crate::services::http::BoxCloneService {
    test_http_client_service_with_limits(name, serde_json::json!({})).await
}

/// A subgraph HTTP client for `name`, as [`test_http_client_service`] builds it, with the limits
/// plugin built from `limits`.
pub(crate) async fn test_http_client_service_with_limits(
    name: &str,
    limits: serde_json::Value,
) -> crate::services::http::BoxCloneService {
    let inputs = crate::services::http::service::HttpClientInputs::for_subgraph(
        name,
        &Configuration::default(),
        &rustls::RootCertStore::empty(),
        crate::configuration::shared::Client::default(),
        &mut crate::services::http::service::DnsResolverCache::default(),
    )
    .unwrap();
    build_http_client_service(
        name,
        inputs,
        required_stage_plugins_with_limits(limits).await,
    )
}
