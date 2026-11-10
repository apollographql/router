//! A request through the whole pipeline at the router's defaults: the HTTP service the server
//! factory builds over the production stage builders, with a probe at each stage's plugin hook
//! recording the tasks the stage beneath it was readied and called on and the copies made of it;
//! and requests a layer of the router stage answers itself, through the copies the ingress pools.

use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::task::Poll;

use http::header::ACCEPT;
use http::header::CONTENT_TYPE;
use parking_lot::Mutex;
use tower::Service;
use tower::ServiceExt;

use crate::services::router;
use crate::test_harness::HttpService;

/// What a probe saw of the service beneath it.
#[derive(Default)]
struct Seen {
    readied_on: Mutex<Vec<Option<tokio::task::Id>>>,
    called_on: Mutex<Vec<Option<tokio::task::Id>>>,
    clones: AtomicUsize,
}

impl Seen {
    /// Forgets what the probe saw so far.
    fn clear(&self) {
        self.readied_on.lock().clear();
        self.called_on.lock().clear();
        self.clones.store(0, Ordering::Relaxed);
    }

    /// Whether every readiness and every call the probe saw ran on `task`, and it saw some.
    fn all_on(&self, task: tokio::task::Id) -> bool {
        let (readied, called) = (self.readied_on.lock(), self.called_on.lock());
        !readied.is_empty()
            && !called.is_empty()
            && readied
                .iter()
                .chain(called.iter())
                .all(|on| *on == Some(task))
    }
}

/// A service over a stage, recording into its [`Seen`].
struct Probe<S> {
    inner: S,
    seen: Arc<Seen>,
}

impl<S: Clone> Clone for Probe<S> {
    fn clone(&self) -> Self {
        self.seen.clones.fetch_add(1, Ordering::Relaxed);
        Self {
            inner: self.inner.clone(),
            seen: self.seen.clone(),
        }
    }
}

impl<S: Service<Request>, Request> Service<Request> for Probe<S> {
    type Response = S::Response;
    type Error = S::Error;
    type Future = S::Future;

    fn poll_ready(&mut self, cx: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.seen.readied_on.lock().push(tokio::task::try_id());
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, request: Request) -> Self::Future {
        self.seen.called_on.lock().push(tokio::task::try_id());
        self.inner.call(request)
    }
}

/// The canned operation over the canned schema, as a client posts it, sent on a task of its own:
/// the service back, the response's status, and the task.
async fn send(mut service: HttpService) -> (HttpService, http::StatusCode, tokio::task::Id) {
    tokio::spawn(async move {
        let request = http::Request::builder()
            .method(http::Method::POST)
            .uri("http://router/")
            .header(CONTENT_TYPE, "application/json")
            .header(ACCEPT, "application/json")
            .body(router::body::from_bytes(
                serde_json::json!({
                    "query": "query TopProducts($first: Int) { topProducts(first: $first) { upc name reviews { id product { name } author { id name } } } }",
                    "variables": {"first": 2}
                })
                .to_string(),
            ))
            .expect("a request");
        let response = service
            .ready()
            .await
            .expect("the router is ready")
            .call(request)
            .await
            .expect("an answer");
        (service, response.status(), tokio::task::id())
    })
    .await
    .expect("the request's task")
}

/// The telemetry plugin at its defaults, which a test adds to the harness: the harness leaves
/// telemetry out of the pipelines it builds, and with it telemetry's router and supergraph layers.
async fn telemetry() -> crate::plugins::telemetry::Telemetry {
    let init = crate::plugin::PluginInit::fake_builder()
        .config(serde_json::json!({}))
        .full_config(serde_json::json!({"telemetry": {}}))
        .build()
        .with_deserialized_config()
        .expect("a telemetry configuration");
    <crate::plugins::telemetry::Telemetry as crate::plugin::PluginPrivate>::new(init)
        .await
        .expect("a telemetry plugin")
}

/// A request through the router at its defaults is readied and called on its own task, from the
/// router stage down to every subgraph exchange: no buffer worker stands where no limit is
/// configured, at the ingress, at traffic shaping's router stage or on a subgraph's stack.
#[tokio::test(flavor = "multi_thread")]
async fn a_request_at_the_defaults_is_readied_and_called_on_its_own_task() {
    let [router, subgraph] = std::array::from_fn::<_, 2, _>(|_| Arc::new(Seen::default()));
    let service = crate::TestHarness::builder()
        .router_hook({
            let seen = router.clone();
            move |service| {
                Probe {
                    inner: service,
                    seen: seen.clone(),
                }
                .boxed_clone()
            }
        })
        .subgraph_hook({
            let seen = subgraph.clone();
            move |_, service| {
                Probe {
                    inner: service,
                    seen: seen.clone(),
                }
                .boxed_clone()
            }
        })
        .build_http_service()
        .await
        .expect("the router");

    // the first request fills the caches the second reads, the parsed document's and the plan's
    let (service, status, _) = send(service).await;
    assert_eq!(status, http::StatusCode::OK);
    for seen in [&router, &subgraph] {
        seen.clear();
    }
    let (_service, status, task) = send(service).await;

    assert_eq!(status, http::StatusCode::OK);
    assert_eq!(
        (router.all_on(task), subgraph.all_on(task)),
        (true, true),
        "whether the router stage and the subgraph stacks were readied and called on the \
         request's own task alone ({:?} and {:?} readied them, the request's task {task:?})",
        router.readied_on.lock(),
        subgraph.readied_on.lock(),
    );
}

/// A request a layer of the router stage answers itself leaves no reservation its stack's
/// readiness took: beside a plugin's concurrency limit of one, shared by every copy of the router
/// stage, each such request answered through one copy and then a GraphQL request through another
/// both answer as they would alone. Where the layer answered with the stack beneath it readied,
/// the first copy went back to the pool holding the limit's permit and the second was shed. The
/// answers: the static page's synchronous checkpoint, the client library check's 400 and the
/// request body limit's 413.
// the telemetry plugin's construction blocks in place, which a multi-threaded runtime allows
#[tokio::test(flavor = "multi_thread")]
async fn a_short_circuit_leaves_no_reservation_in_its_copy() {
    use std::sync::Arc;

    use crate::axum_factory::utils::connection_router_service;
    use crate::pipeline::build_apq_expander;
    use crate::pipeline::build_query_parsing_service;
    use crate::pipeline::build_router_service;
    use crate::pipeline::connect_apq_redis;
    use crate::services::layers::persisted_queries::PersistedQueryExpander;

    let (configuration, schema, plugins, supergraph) = crate::TestHarness::builder()
        .configuration_json(serde_json::json!({
            "limits": {"router": {"http_max_request_bytes": 100_000}}
        }))
        .expect("a configuration")
        .extra_private_plugin(telemetry().await)
        .router_hook(|service| {
            tower::ServiceBuilder::new()
                .concurrency_limit(1)
                .service(service)
                .boxed_clone()
        })
        .build_common()
        .await
        .expect("the pipeline");
    let router = build_router_service(
        supergraph,
        build_apq_expander(
            &configuration,
            connect_apq_redis(&configuration)
                .await
                .expect("no APQ redis"),
        ),
        Arc::new(
            PersistedQueryExpander::new(&configuration)
                .await
                .expect("no manifest"),
        ),
        build_query_parsing_service(schema.clone(), configuration.clone()),
        schema,
        &configuration,
        plugins,
    );
    let operation = || {
        router::Request::fake_builder()
            .method(http::Method::POST)
            .header(CONTENT_TYPE, "application/json")
            .header(ACCEPT, "application/json")
            .body(router::body::from_bytes(
                serde_json::json!({
                    "query": "query TopProducts($first: Int) { topProducts(first: $first) { upc name reviews { id product { name } author { id name } } } }",
                    "variables": {"first": 2}
                })
                .to_string(),
            ))
    };
    let short_circuits = [
        (
            "the static page",
            router::Request::fake_builder()
                .method(http::Method::GET)
                .header(ACCEPT, "text/html")
                .build()
                .expect("a page request"),
            http::StatusCode::OK,
        ),
        (
            "the client library check",
            operation()
                .header("apollographql-library-name", "!")
                .build()
                .expect("a request with an invalid library name"),
            http::StatusCode::BAD_REQUEST,
        ),
        (
            "the request body limit",
            operation()
                .header(http::header::CONTENT_LENGTH, "1000000")
                .build()
                .expect("a request past the body limit"),
            http::StatusCode::PAYLOAD_TOO_LARGE,
        ),
    ];

    for (layer, short_circuit, status) in short_circuits {
        let mut connection = connection_router_service(router.clone());

        // each call takes its own copy of the router stage before either is readied
        let answered = connection.call(short_circuit);
        let called = connection.call(operation().build().expect("an operation"));
        let answered = answered.await.expect("the short circuit's answer");
        let called = called.await.expect("the operation's answer");

        assert_eq!(
            (answered.response.status(), called.response.status()),
            (status, http::StatusCode::OK),
            "the status {layer} answered with, and the status of the operation called through \
             the second copy after it"
        );
    }
}

/// A layer's synchronous work in `call` runs inside the span of the stage it belongs to: a probe
/// at the router stage's plugin hook, beneath the router span, and one at the supergraph stage's,
/// beneath the supergraph span, each called on the request's task, see that span as the current
/// one where they are called, as the checkpoints beneath them, the POST-only mutation check and
/// the operation limits among them, do.
// the telemetry plugin's construction blocks in place, which a multi-threaded runtime allows; the
// request is sent on the test's own thread, where the subscriber is the default
#[tokio::test(flavor = "multi_thread")]
async fn a_layers_call_runs_inside_its_stages_span() {
    #[derive(Clone)]
    struct SpanProbe<S> {
        inner: S,
        seen: Arc<Mutex<Vec<Option<&'static str>>>>,
    }
    impl<S: Service<Request>, Request> Service<Request> for SpanProbe<S> {
        type Response = S::Response;
        type Error = S::Error;
        type Future = S::Future;

        fn poll_ready(&mut self, cx: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
            self.inner.poll_ready(cx)
        }

        fn call(&mut self, request: Request) -> Self::Future {
            self.seen
                .lock()
                .push(tracing::Span::current().metadata().map(|m| m.name()));
            self.inner.call(request)
        }
    }

    let _subscriber = tracing::subscriber::set_default(tracing_subscriber::registry());
    let [router, supergraph] = std::array::from_fn::<_, 2, _>(|_| Arc::new(Mutex::new(Vec::new())));
    let service = crate::TestHarness::builder()
        .extra_private_plugin(telemetry().await)
        .router_hook({
            let seen = router.clone();
            move |service| {
                SpanProbe {
                    inner: service,
                    seen: seen.clone(),
                }
                .boxed_clone()
            }
        })
        .supergraph_hook({
            let seen = supergraph.clone();
            move |service| {
                SpanProbe {
                    inner: service,
                    seen: seen.clone(),
                }
                .boxed_clone()
            }
        })
        .build_http_service()
        .await
        .expect("the router");

    let mut service = service;
    let response = service
        .ready()
        .await
        .expect("the router is ready")
        .call(
            http::Request::builder()
                .method(http::Method::POST)
                .uri("http://router/")
                .header(CONTENT_TYPE, "application/json")
                .header(ACCEPT, "application/json")
                .body(router::body::from_bytes(
                    serde_json::json!({"query": "{ topProducts { upc } }"}).to_string(),
                ))
                .expect("a request"),
        )
        .await
        .expect("an answer");

    assert_eq!(response.status(), http::StatusCode::OK);
    assert_eq!(
        (router.lock().clone(), supergraph.lock().clone()),
        (
            vec![Some(crate::plugins::telemetry::consts::ROUTER_SPAN_NAME)],
            vec![Some(
                crate::plugins::telemetry::consts::SUPERGRAPH_SPAN_NAME
            )]
        ),
        "the current span where the router and supergraph stages' hooks were called"
    );
}
