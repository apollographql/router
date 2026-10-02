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
use crate::pipeline::build_subgraph_services;
use crate::plugin::PluginInit;
use crate::plugin::PluginUnstable;
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

    let http_services = [(
        name.to_string(),
        crate::services::http::test_http_client_service(name),
    )]
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
