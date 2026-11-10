//! A subgraph's service stack as the subgraph stage builds it: the tasks and copies a subgraph
//! request meets, and, over a real HTTP client and a local server, what each subgraph request reads
//! of its own subgraph's configuration.

use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::task::Poll;

use http::header::CONTENT_TYPE;
use opentelemetry::Value;
use serde_json::json;
use tower::BoxError;
use tower::Service;
use tower::ServiceExt;

use crate::Configuration;
use crate::Context;
use crate::graphql;
use crate::pipeline::build_http_client_service;
use crate::pipeline::stages::build_subgraph_service;
use crate::pipeline::tests::stage_stack::required_stage_plugins;
use crate::pipeline::tests::stage_stack::stage_plugins;
use crate::plugins::telemetry::config_new::Selector as _;
use crate::plugins::telemetry::config_new::subgraph::selectors::SubgraphSelector;
use crate::plugins::traffic_shaping::APOLLO_TRAFFIC_SHAPING;
use crate::services::Plugins;
use crate::services::SubgraphRequest;
use crate::services::SubgraphResponse;
use crate::services::http::HttpRequest;
use crate::services::http::HttpResponse;
use crate::services::http::service::DnsResolverCache;
use crate::services::http::service::HttpClientInputs;
use crate::services::subgraph::service::PooledSubgraphService;

/// An HTTP client standing in for a subgraph: it answers every request with an empty GraphQL
/// response, and counts the clones made of it.
#[derive(Default)]
struct CountedClient {
    clones: Arc<AtomicUsize>,
}

impl Clone for CountedClient {
    fn clone(&self) -> Self {
        self.clones.fetch_add(1, Ordering::Relaxed);
        Self {
            clones: self.clones.clone(),
        }
    }
}

impl Service<HttpRequest> for CountedClient {
    type Response = HttpResponse;
    type Error = BoxError;
    type Future = std::future::Ready<Result<HttpResponse, BoxError>>;

    fn poll_ready(&mut self, _: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: HttpRequest) -> Self::Future {
        std::future::ready(Ok(HttpResponse {
            http_response: http::Response::builder()
                .header(CONTENT_TYPE, "application/json")
                .body(crate::services::router::body::from_bytes(r#"{"data":{}}"#))
                .expect("a response"),
            context: request.context,
        }))
    }
}

/// The worker tasks building subgraph `name`'s stack over `client` spawns.
fn workers_spawned_building(name: &str, client: CountedClient, plugins: &Arc<Plugins>) -> usize {
    let metrics = tokio::runtime::Handle::current().metrics();
    let tasks = metrics.num_alive_tasks();
    let _stack = build_subgraph_service(
        name,
        client.boxed_clone(),
        plugins,
        &Configuration::default(),
    );
    metrics.num_alive_tasks() - tasks
}

/// A subgraph's stack runs no worker task: every subgraph request readies and calls it on the
/// request's own task, traffic shaping configured or not. With shaping configured, its rate limit
/// is charged where a request is called, where its admission and backpressure buffers each ran a
/// worker.
#[tokio::test]
async fn a_subgraph_stack_runs_no_worker_task_of_its_own() {
    let plugins = Arc::new(
        stage_plugins(&[(
            APOLLO_TRAFFIC_SHAPING,
            json!({ "all": {"global_rate_limit": {"capacity": 10, "interval": "1s"}} }),
        )])
        .await,
    );

    assert_eq!(
        workers_spawned_building("s", CountedClient::default(), &plugins),
        0,
        "the worker tasks of a shaped subgraph's stack"
    );
}

/// A subgraph request with nothing configured for its subgraph meets only the layers that serve it:
/// no worker task, where traffic shaping's admission buffer ran one for a rate limit nobody
/// configured, and one copy of the HTTP client's stack, the subgraph service's, where the
/// subscription and APQ layers each copied the stack beneath them too.
#[tokio::test]
async fn an_unconfigured_subgraph_request_meets_only_the_layers_that_serve_it() {
    let plugins = required_stage_plugins().await;
    let client = CountedClient::default();
    let clones = client.clones.clone();
    let metrics = tokio::runtime::Handle::current().metrics();
    let tasks = metrics.num_alive_tasks();
    let stack = build_subgraph_service(
        "s",
        client.boxed_clone(),
        &plugins,
        &Configuration::default(),
    );
    let workers = metrics.num_alive_tasks() - tasks;
    let request = || {
        SubgraphRequest::fake_builder()
            .subgraph_request(
                http::Request::builder()
                    .method(http::Method::POST)
                    .uri("http://s/")
                    .body(graphql::Request::fake_builder().query("{ a }").build())
                    .expect("a request"),
            )
            .build()
    };

    // the first request makes the copy of the stack the second one reuses
    stack.clone().oneshot(request()).await.expect("an answer");
    let clones_before = clones.load(Ordering::Relaxed);
    let response = stack.clone().oneshot(request()).await.expect("an answer");

    assert_eq!(response.response.status(), http::StatusCode::OK);
    assert_eq!(
        (workers, clones.load(Ordering::Relaxed) - clones_before),
        (0, 1),
        "the stack's worker tasks, and the copies of its HTTP client's stack one request made"
    );
}

/// Serves `routes`, each a path answered with a fixed `application/json` body, on a local port, and
/// returns the server's address.
async fn serve(routes: &[(&str, String)]) -> std::net::SocketAddr {
    let mut app = axum::Router::new();
    for (path, body) in routes {
        let body = body.clone();
        app = app.route(
            path,
            axum::routing::post(
                move || async move { ([(CONTENT_TYPE, "application/json")], body) },
            ),
        );
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a listener");
    let address = listener.local_addr().expect("its address");
    tokio::spawn(async move { axum::serve(listener, app).await });
    address
}

/// The stack the subgraph stage builds for subgraph `name` over a real HTTP client.
fn subgraph_stack(name: &str, plugins: &Arc<Plugins>) -> PooledSubgraphService {
    let configuration = Configuration::default();
    let inputs = HttpClientInputs::for_subgraph(
        name,
        &configuration,
        &rustls::RootCertStore::empty(),
        Default::default(),
        &mut DnsResolverCache::default(),
    )
    .expect("the client's inputs");
    build_subgraph_service(
        name,
        build_http_client_service(name, inputs, plugins.clone(), &configuration),
        plugins,
        &configuration,
    )
}

/// A request for `query` to subgraph `name`, served at `/<name>` of `address`, in `context`.
fn subgraph_request(
    address: std::net::SocketAddr,
    name: &str,
    query: &str,
    context: &Context,
) -> SubgraphRequest {
    SubgraphRequest::fake_builder()
        .subgraph_request(
            http::Request::builder()
                .method(http::Method::POST)
                .uri(format!("http://{address}/{name}"))
                .body(graphql::Request::fake_builder().query(query).build())
                .expect("a request"),
        )
        .context(context.clone())
        .subgraph_name(name.to_string())
        .build()
}

/// Two subgraph requests of one client request, in flight together, each read their own subgraph's
/// response size limit and their own request and response body sizes, which the subgraph stage
/// kept in the client request's shared context, where one request's values overwrote the other's.
#[tokio::test]
async fn two_subgraph_requests_of_one_request_read_their_own_values() {
    const A_RESPONSE: &str = r#"{"data":{"a":"0123456789abcdefghij"}}"#;
    const B_RESPONSE: &str = r#"{"data":{"b":"x"}}"#;
    let address = serve(&[
        ("/a", A_RESPONSE.to_string()),
        ("/b", B_RESPONSE.to_string()),
    ])
    .await;

    // a's limit is under its response, b's well over both responses
    let plugins = Arc::new(
        stage_plugins(&[
            (APOLLO_TRAFFIC_SHAPING, json!({})),
            (
                "apollo.limits",
                json!({ "subgraph": {
                    "all": { "http_max_response_size": "1048576b" },
                    "subgraphs": { "a": { "http_max_response_size": "16b" } }
                } }),
            ),
        ])
        .await,
    );
    let context = Context::new();
    let a = subgraph_request(address, "a", "{ a }", &context);
    let b = subgraph_request(address, "b", "{ bbbbbbbbbbbb }", &context);
    let sent = |request: &SubgraphRequest| {
        let body = serde_json::to_string(request.subgraph_request.body()).expect("a body");
        Some(Value::I64(body.len() as i64))
    };
    let (a_sent, b_sent) = (sent(&a), sent(&b));

    let (a, b) = futures::join!(
        subgraph_stack("a", &plugins).oneshot(a),
        subgraph_stack("b", &plugins).oneshot(b)
    );
    let (a, b) = (a.expect("a's answer"), b.expect("b's answer"));

    let request_size = SubgraphSelector::SubgraphRequestBodySize {
        subgraph_request_body_size: true,
    };
    let response_size = SubgraphSelector::SubgraphResponseBodySize {
        subgraph_response_body_size: true,
    };
    let cut = |response: &SubgraphResponse| {
        response
            .response
            .body()
            .errors
            .iter()
            .any(|error| error.message.contains("exceeded limit of 16 bytes"))
    };
    assert_eq!(
        (
            cut(&a),
            cut(&b),
            request_size.on_response(&a),
            request_size.on_response(&b),
            response_size.on_response(&b),
        ),
        (
            true,
            false,
            a_sent,
            b_sent,
            Some(Value::I64(B_RESPONSE.len() as i64)),
        ),
        "whether each response was cut at its own subgraph's limit, each request's body size and \
         b's response body size, as each subgraph request read them"
    );
}

/// A subgraph with no response size limit configured, beside one limited to 16 bytes, reads a
/// response of a mebibyte and more whole.
#[tokio::test]
async fn a_subgraph_with_no_limit_configured_reads_its_whole_response() {
    let response = format!(r#"{{"data":{{"a":"{}"}}}}"#, "x".repeat(1 << 20));
    let address = serve(&[("/a", response)]).await;
    let plugins = Arc::new(
        stage_plugins(&[
            (APOLLO_TRAFFIC_SHAPING, json!({})),
            (
                "apollo.limits",
                json!({ "subgraph": {
                    "subgraphs": { "b": { "http_max_response_size": "16b" } }
                } }),
            ),
        ])
        .await,
    );

    let response = subgraph_stack("a", &plugins)
        .oneshot(subgraph_request(address, "a", "{ a }", &Context::new()))
        .await
        .expect("a's answer");

    assert!(response.response.body().errors.is_empty());
    assert_eq!(
        response
            .response
            .body()
            .data
            .as_ref()
            .and_then(|data| data.as_object())
            .and_then(|data| data.get("a"))
            .and_then(|a| a.as_str())
            .map(str::len),
        Some(1 << 20)
    );
}
