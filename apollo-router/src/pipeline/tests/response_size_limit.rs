//! Subgraph and connector source response size limits as the pipeline places them.
//!
//! The stacks are built with the stage builders down to each HTTP client, and a
//! [`tower_test::mock`] stands in for each subgraph's or source's connection, so a response body
//! passes through every layer between the connection and the subgraph or connector request
//! service. Every test sends its fetches with one shared [`Context`], as the fetches of one client
//! request are.

use std::collections::HashMap;
use std::sync::Arc;

use http_body_util::BodyExt as _;
use indexmap::IndexMap;
use tower::BoxError;
use tower::ServiceExt as _;
use tower_test::mock::Mock;

use super::stage_stack;
use crate::Configuration;
use crate::Context;
use crate::graphql;
use crate::pipeline::HttpClientTarget;
use crate::pipeline::build_http_client_service;
use crate::pipeline::build_subgraph_services;
use crate::plugin::PluginInit;
use crate::plugin::PluginPrivate;
use crate::services::Plugins;
use crate::services::SubgraphRequest;
use crate::services::SubgraphServices;
use crate::services::http;
use crate::services::http::HttpRequest;
use crate::services::http::HttpResponse;
use crate::services::router;

/// A subgraph with a 10-byte response size limit.
const LIMITED: &str = "products";
/// A subgraph with no response size limit.
const UNLIMITED: &str = "reviews";
/// A subgraph whose connector sources call out over HTTP.
const CONNECTORS: &str = "connectors";
/// A connector source of [`CONNECTORS`].
const UNLIMITED_SOURCE: &str = "connectors.unlimited";

/// Every response body: valid GraphQL, and longer than [`LIMITED`]'s limit.
const BODY: &str = r#"{"data":{"name":"longer than ten bytes"}}"#;

/// Puts a mock in place of each subgraph's HTTP connection.
struct StubConnections {
    mocks: HashMap<String, Mock<HttpRequest, HttpResponse>>,
}

#[async_trait::async_trait]
impl PluginPrivate for StubConnections {
    type Config = ();

    async fn new(_: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        unreachable!("inserted into the plugin registry directly")
    }

    fn http_client_service(
        &self,
        subgraph_name: &str,
        _: http::BoxCloneService,
    ) -> http::BoxCloneService {
        self.mocks[subgraph_name].clone().boxed_clone()
    }
}

struct Subgraphs {
    plugins: Arc<Plugins>,
    services: SubgraphServices,
    connections: HashMap<&'static str, tower_test::mock::Handle<HttpRequest, HttpResponse>>,
}

impl Subgraphs {
    /// The services of [`LIMITED`] and [`UNLIMITED`], with only [`LIMITED`] given a response size
    /// limit.
    async fn new() -> Self {
        let limits = serde_json::json!({ "subgraph": { "subgraphs": {
            LIMITED: { "http_max_response_size": "10B" }
        } } });
        let mut mocks = HashMap::new();
        let mut connections = HashMap::new();
        for name in [LIMITED, UNLIMITED, CONNECTORS] {
            let (mock, handle) = tower_test::mock::pair();
            mocks.insert(name.to_string(), mock);
            connections.insert(name, handle);
        }
        let mut plugins =
            stage_stack::required_stage_plugin_registry(&[("apollo.limits", limits)]).await;
        plugins.insert("stub".to_string(), Box::new(StubConnections { mocks }));
        let plugins = Arc::new(plugins);

        let http_services: IndexMap<_, _> = [LIMITED, UNLIMITED]
            .into_iter()
            .map(|name| {
                let client = http_client(HttpClientTarget::Subgraph(name), name, &plugins);
                (name.to_string(), client)
            })
            .collect();
        let services = build_subgraph_services(http_services, &plugins, &Configuration::default());
        Self {
            plugins,
            services,
            connections,
        }
    }

    async fn fetch(&mut self, name: &str, context: &Context) -> Vec<graphql::Error> {
        let request = SubgraphRequest::fake_builder()
            .subgraph_name(name)
            .context(context.clone())
            .build();
        let service = self.services.get(name).expect("the subgraph has a service");
        let connection = self
            .connections
            .get_mut(name)
            .expect("the subgraph has a connection");
        let (response, ()) = tokio::join!(service.oneshot(request), answer(connection));
        response
            .expect("the fetch is answered")
            .response
            .into_body()
            .errors
    }
}

fn http_client(
    target: HttpClientTarget<'_>,
    name: &str,
    plugins: &Arc<Plugins>,
) -> http::BoxCloneService {
    let inputs = http::service::HttpClientInputs::for_subgraph(
        name,
        &Configuration::default(),
        &rustls::RootCertStore::empty(),
        crate::configuration::shared::Client::default(),
        &mut http::service::DnsResolverCache::default(),
    )
    .unwrap();
    build_http_client_service(target, inputs, plugins.clone())
}

async fn answer(connection: &mut tower_test::mock::Handle<HttpRequest, HttpResponse>) {
    let (_, respond) = connection
        .next_request()
        .await
        .expect("the subgraph receives a request");
    respond.send_response(HttpResponse {
        http_response: ::http::Response::builder()
            .header(::http::header::CONTENT_TYPE, "application/json")
            .body(router::body::from_bytes(BODY))
            .unwrap(),
        context: Context::new(),
    });
}

fn assert_over_limit(errors: &[graphql::Error]) {
    assert!(
        errors
            .iter()
            .any(|error| error.message.contains("exceeded limit of 10 bytes")),
        "expected the response to exceed the limit: {errors:?}"
    );
}

#[tokio::test]
async fn an_unlimited_subgraph_is_not_held_to_a_limit_fetched_before_it() {
    let mut subgraphs = Subgraphs::new().await;
    let context = Context::new();

    assert_over_limit(&subgraphs.fetch(LIMITED, &context).await);
    assert_eq!(subgraphs.fetch(UNLIMITED, &context).await, []);
}

#[tokio::test]
async fn a_limited_subgraph_is_held_to_its_limit_after_an_unlimited_one() {
    let mut subgraphs = Subgraphs::new().await;
    let context = Context::new();

    assert_eq!(subgraphs.fetch(UNLIMITED, &context).await, []);
    assert_over_limit(&subgraphs.fetch(LIMITED, &context).await);
}

#[tokio::test]
async fn parallel_fetches_are_each_held_to_their_own_limit() {
    let mut subgraphs = Subgraphs::new().await;
    let context = Context::new();
    let mut limited_connection = subgraphs.connections.remove(LIMITED).unwrap();
    let mut unlimited_connection = subgraphs.connections.remove(UNLIMITED).unwrap();
    let fetch = |name: &str| {
        let request = SubgraphRequest::fake_builder()
            .subgraph_name(name)
            .context(context.clone())
            .build();
        let service = subgraphs
            .services
            .get(name)
            .expect("the subgraph has a service");
        service.oneshot(request)
    };

    let (limited, unlimited, (), ()) = tokio::join!(
        fetch(LIMITED),
        fetch(UNLIMITED),
        answer(&mut limited_connection),
        answer(&mut unlimited_connection),
    );

    assert_over_limit(&limited.unwrap().response.into_body().errors);
    assert_eq!(unlimited.unwrap().response.into_body().errors, []);
}

#[tokio::test]
async fn a_connector_client_is_not_held_to_a_subgraph_limit() {
    let mut subgraphs = Subgraphs::new().await;
    let context = Context::new();
    assert_over_limit(&subgraphs.fetch(LIMITED, &context).await);

    let client = http_client(
        HttpClientTarget::ConnectorSource(UNLIMITED_SOURCE),
        CONNECTORS,
        &subgraphs.plugins,
    );
    let request = HttpRequest {
        http_request: ::http::Request::new(router::body::empty()),
        context,
    };
    let connection = subgraphs.connections.get_mut(CONNECTORS).unwrap();
    let (response, ()) = tokio::join!(client.oneshot(request), answer(connection));
    let body = response
        .expect("the source answers")
        .http_response
        .into_body()
        .collect()
        .await
        .expect("the whole body is read");
    assert_eq!(body.to_bytes(), BODY);
}
