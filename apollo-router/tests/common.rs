use std::collections::HashMap;
use std::fs;
use std::net::SocketAddr;
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use buildstructor::buildstructor;
use http::header::ACCEPT;
use http::header::CONTENT_TYPE;
use http::HeaderValue;
use jsonpath_lib::Selector;
use mediatype::names::BOUNDARY;
use mediatype::names::FORM_DATA;
use mediatype::names::MULTIPART;
use mediatype::MediaType;
use mediatype::WriteParams;
use mime::APPLICATION_JSON;
use opentelemetry::global;
use opentelemetry::propagation::TextMapPropagator;
use opentelemetry::sdk::trace::config;
use opentelemetry::sdk::trace::BatchSpanProcessor;
use opentelemetry::sdk::trace::TracerProvider;
use opentelemetry::sdk::Resource;
use opentelemetry::testing::trace::NoopSpanExporter;
use opentelemetry::trace::TraceContextExt;
use opentelemetry_api::trace::TracerProvider as OtherTracerProvider;
use opentelemetry_api::Context;
use opentelemetry_api::KeyValue;
use opentelemetry_otlp::HttpExporterBuilder;
use opentelemetry_otlp::Protocol;
use opentelemetry_otlp::SpanExporterBuilder;
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_semantic_conventions::resource::SERVICE_NAME;
use reqwest::Request;
use serde_json::json;
use serde_json::Value;
use tokio::io::AsyncBufReadExt;
use tokio::io::AsyncWriteExt;
use tokio::io::BufReader;
use tokio::process::Child;
use tokio::process::Command;
use tokio::task;
use tokio::time::Instant;
use tower::BoxError;
use tracing::info_span;
use tracing_core::Dispatch;
use tracing_core::LevelFilter;
use tracing_futures::Instrument;
use tracing_futures::WithSubscriber;
use tracing_opentelemetry::OpenTelemetrySpanExt;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::Layer;
use tracing_subscriber::Registry;
use uuid::Uuid;
use wiremock::matchers::method;
use wiremock::Mock;
use wiremock::Respond;
use wiremock::ResponseTemplate;

pub struct IntegrationTest {
    router: Option<Child>,
    test_config_location: PathBuf,
    router_location: PathBuf,
    stdio_tx: tokio::sync::mpsc::Sender<String>,
    stdio_rx: tokio::sync::mpsc::Receiver<String>,
    collect_stdio: Option<(tokio::sync::oneshot::Sender<String>, regex::Regex)>,
    supergraph: PathBuf,
    _subgraphs: wiremock::MockServer,
    telemetry: Telemetry,

    // Don't remove these, there is a weak reference to the tracer provider from a tracer and if the provider is dropped then no export will happen.
    pub _tracer_provider_client: TracerProvider,
    pub _tracer_provider_subgraph: TracerProvider,
    subscriber_client: Dispatch,

    _subgraph_overrides: HashMap<String, String>,
    pub bind_address: SocketAddr,
}

struct TracedResponder {
    response_template: ResponseTemplate,
    telemetry: Telemetry,
    subscriber_subgraph: Dispatch,
}

impl Respond for TracedResponder {
    fn respond(&self, request: &wiremock::Request) -> ResponseTemplate {
        let context = self.telemetry.extract_context(request);
        tracing_core::dispatcher::with_default(&self.subscriber_subgraph, || {
            let _context_guard = context.attach();
            let span = info_span!("subgraph server");
            let _span_guard = span.enter();
            self.response_template.clone()
        })
    }
}

#[derive(Debug, Clone, Default)]
#[allow(dead_code)]
pub enum Telemetry {
    Jaeger,
    Otlp {
        endpoint: String,
    },
    Datadog,
    Zipkin,
    #[default]
    None,
}

impl Telemetry {
    fn tracer_provider(&self, service_name: &str) -> TracerProvider {
        let config = config().with_resource(Resource::new(vec![KeyValue::new(
            SERVICE_NAME,
            service_name.to_string(),
        )]));

        match self {
            Telemetry::Jaeger => TracerProvider::builder()
                .with_config(config)
                .with_span_processor(
                    BatchSpanProcessor::builder(
                        opentelemetry_jaeger::new_agent_pipeline()
                            .with_service_name(service_name)
                            .build_sync_agent_exporter()
                            .expect("jaeger pipeline failed"),
                        opentelemetry::runtime::Tokio,
                    )
                    .with_scheduled_delay(Duration::from_millis(10))
                    .build(),
                )
                .build(),
            Telemetry::Otlp { endpoint } => TracerProvider::builder()
                .with_config(config)
                .with_span_processor(
                    BatchSpanProcessor::builder(
                        SpanExporterBuilder::Http(
                            HttpExporterBuilder::default()
                                .with_endpoint(endpoint)
                                .with_protocol(Protocol::HttpBinary),
                        )
                        .build_span_exporter()
                        .expect("otlp pipeline failed"),
                        opentelemetry::runtime::Tokio,
                    )
                    .with_scheduled_delay(Duration::from_millis(10))
                    .build(),
                )
                .build(),
            Telemetry::Datadog => TracerProvider::builder()
                .with_config(config)
                .with_span_processor(
                    BatchSpanProcessor::builder(
                        opentelemetry_datadog::new_pipeline()
                            .build_exporter()
                            .expect("datadog pipeline failed"),
                        opentelemetry::runtime::Tokio,
                    )
                    .with_scheduled_delay(Duration::from_millis(10))
                    .build(),
                )
                .build(),
            Telemetry::Zipkin => TracerProvider::builder()
                .with_config(config)
                .with_span_processor(
                    BatchSpanProcessor::builder(
                        opentelemetry_zipkin::new_pipeline()
                            .with_service_name(service_name)
                            .init_exporter()
                            .expect("zipkin pipeline failed"),
                        opentelemetry::runtime::Tokio,
                    )
                    .with_scheduled_delay(Duration::from_millis(10))
                    .build(),
                )
                .build(),
            Telemetry::None => TracerProvider::builder()
                .with_config(config)
                .with_simple_exporter(NoopSpanExporter::default())
                .build(),
        }
    }

    fn inject_context(&self, request: &mut Request) {
        let ctx = tracing::span::Span::current().context();

        match self {
            Telemetry::Jaeger => {
                let propagator = opentelemetry_jaeger::Propagator::new();
                propagator.inject_context(
                    &ctx,
                    &mut opentelemetry_http::HeaderInjector(request.headers_mut()),
                )
            }
            Telemetry::Datadog => {
                let propagator = opentelemetry_datadog::DatadogPropagator::new();
                propagator.inject_context(
                    &ctx,
                    &mut opentelemetry_http::HeaderInjector(request.headers_mut()),
                )
            }
            Telemetry::Otlp { .. } => {
                let propagator = opentelemetry::sdk::propagation::TraceContextPropagator::default();
                propagator.inject_context(
                    &ctx,
                    &mut opentelemetry_http::HeaderInjector(request.headers_mut()),
                )
            }
            Telemetry::Zipkin => {
                let propagator = opentelemetry_zipkin::Propagator::new();
                propagator.inject_context(
                    &ctx,
                    &mut opentelemetry_http::HeaderInjector(request.headers_mut()),
                )
            }
            _ => {}
        }
    }

    pub(crate) fn extract_context(&self, request: &wiremock::Request) -> Context {
        let headers: HashMap<String, String> = request
            .headers
            .iter()
            .map(|(name, value)| (name.as_str().to_string(), value.as_str().to_string()))
            .collect();

        match self {
            Telemetry::Jaeger => {
                let propagator = opentelemetry_jaeger::Propagator::new();
                propagator.extract(&headers)
            }
            Telemetry::Datadog => {
                let propagator = opentelemetry_datadog::DatadogPropagator::new();
                propagator.extract(&headers)
            }
            Telemetry::Otlp { .. } => {
                let propagator = opentelemetry::sdk::propagation::TraceContextPropagator::default();
                propagator.extract(&headers)
            }
            Telemetry::Zipkin => {
                let propagator = opentelemetry_zipkin::Propagator::new();
                propagator.extract(&headers)
            }
            _ => Context::current(),
        }
    }
}

#[buildstructor]
impl IntegrationTest {
    #[builder]
    pub async fn new(
        config: String,
        telemetry: Option<Telemetry>,
        responder: Option<ResponseTemplate>,
        collect_stdio: Option<tokio::sync::oneshot::Sender<String>>,
        supergraph: Option<PathBuf>,
        mut subgraph_overrides: HashMap<String, String>,
    ) -> Self {
        let telemetry = telemetry.unwrap_or_default();
        let tracer_provider_client = telemetry.tracer_provider("client");
        let subscriber_client = Self::dispatch(&tracer_provider_client);
        let tracer_provider_subgraph = telemetry.tracer_provider("subgraph");

        let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0))).unwrap();
        let address = listener.local_addr().unwrap();
        let url = format!("http://{address}/");

        // Add a default override for products, if not specified
        subgraph_overrides.entry("products".into()).or_insert(url);

        // Bind to a random port
        // Note: This might still fail if a different process binds to the port found here
        // before the router is started.
        // Note: We need the nested scope so that the listener gets dropped once its address
        // is resolved.
        let bind_address = {
            let bound = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0))).unwrap();
            bound.local_addr().unwrap()
        };

        // Insert the overrides into the config
        let config_str = merge_overrides(&config, &subgraph_overrides, &bind_address);

        let supergraph = supergraph.unwrap_or(PathBuf::from_iter([
            "..",
            "examples",
            "graphql",
            "local.graphql",
        ]));
        let subgraphs = wiremock::MockServer::builder()
            .listener(listener)
            .start()
            .await;

        Mock::given(method("POST"))
            .respond_with(TracedResponder{response_template:responder.unwrap_or_else(||
                ResponseTemplate::new(200).set_body_json(json!({"data":{"topProducts":[{"name":"Table"},{"name":"Couch"},{"name":"Chair"}]}}))),
                telemetry: telemetry.clone(),
                subscriber_subgraph: Self::dispatch(&tracer_provider_subgraph),
            })
            .mount(&subgraphs)
            .await;

        let mut test_config_location = std::env::temp_dir();
        let location = format!("apollo-router-test-{}.yaml", Uuid::new_v4());
        test_config_location.push(location);

        fs::write(&test_config_location, &config_str).expect("could not write config");

        let (stdio_tx, stdio_rx) = tokio::sync::mpsc::channel(2000);
        let collect_stdio = collect_stdio.map(|sender| {
            let version_line_re = regex::Regex::new("Apollo Router v[^ ]+ ").unwrap();
            (sender, version_line_re)
        });

        Self {
            router: None,
            router_location: Self::router_location(),
            test_config_location,
            stdio_tx,
            stdio_rx,
            collect_stdio,
            supergraph,
            _subgraphs: subgraphs,
            _subgraph_overrides: subgraph_overrides,
            bind_address,
            _tracer_provider_client: tracer_provider_client,
            subscriber_client,
            _tracer_provider_subgraph: tracer_provider_subgraph,
            telemetry,
        }
    }

    fn dispatch(tracer_provider: &TracerProvider) -> Dispatch {
        let tracer = tracer_provider.tracer("tracer");
        let tracing_layer = tracing_opentelemetry::layer()
            .with_tracer(tracer)
            .with_filter(LevelFilter::INFO);

        let subscriber = Registry::default().with(tracing_layer).with(
            tracing_subscriber::fmt::Layer::default()
                .compact()
                .with_filter(EnvFilter::from_default_env()),
        );
        Dispatch::new(subscriber)
    }

    pub fn router_location() -> PathBuf {
        PathBuf::from(env!("CARGO_BIN_EXE_router"))
    }

    #[allow(dead_code)]
    pub async fn start(&mut self) {
        let mut router = Command::new(&self.router_location);
        if let (Ok(apollo_key), Ok(apollo_graph_ref)) = (
            std::env::var("TEST_APOLLO_KEY"),
            std::env::var("TEST_APOLLO_GRAPH_REF"),
        ) {
            router
                .env("APOLLO_KEY", apollo_key)
                .env("APOLLO_GRAPH_REF", apollo_graph_ref);
        }

        router
            .args([
                "--hr",
                "--config",
                &self.test_config_location.to_string_lossy(),
                "--supergraph",
                &self.supergraph.to_string_lossy(),
                "--log",
                "error,apollo_router=info",
            ])
            .stdout(Stdio::piped());

        let mut router = router.spawn().expect("router should start");
        let reader = BufReader::new(router.stdout.take().expect("out"));
        let stdio_tx = self.stdio_tx.clone();
        let collect_stdio = self.collect_stdio.take();
        // We need to read from stdout otherwise we will hang
        task::spawn(async move {
            let mut collected = Vec::new();
            let mut lines = reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                println!("{line}");
                if let Some((_sender, version_line_re)) = &collect_stdio {
                    #[derive(serde::Deserialize)]
                    struct Log {
                        #[allow(unused)]
                        timestamp: String,
                        level: String,
                        message: String,
                    }
                    let log = serde_json::from_str::<Log>(&line).unwrap();
                    // Omit this message from snapshots since it depends on external environment
                    if !log.message.starts_with("RUST_BACKTRACE=full detected") {
                        collected.push(format!(
                            "{}: {}",
                            log.level,
                            // Redacted so we don't need to update snapshots every release
                            version_line_re
                                .replace(&log.message, "Apollo Router [version number] ")
                        ))
                    }
                }
                let _ = stdio_tx.send(line).await;
            }
            if let Some((sender, _version_line_re)) = collect_stdio {
                let _ = sender.send(collected.join("\n"));
            }
        });

        self.router = Some(router);
    }

    #[allow(dead_code)]
    pub async fn assert_started(&mut self) {
        self.assert_log_contains("GraphQL endpoint exposed").await;
    }

    #[allow(dead_code)]
    pub async fn assert_not_started(&mut self) {
        self.assert_log_contains("no valid configuration").await;
    }

    #[allow(dead_code)]
    pub async fn touch_config(&self) {
        let mut f = tokio::fs::OpenOptions::new()
            .append(true)
            .open(&self.test_config_location)
            .await
            .expect("must have been able to open config file");
        f.write_all("\n#touched\n".as_bytes())
            .await
            .expect("must be able to write config file");
    }

    #[allow(dead_code)]
    pub async fn update_config(&self, yaml: &str) {
        tokio::fs::write(
            &self.test_config_location,
            &merge_overrides(yaml, &self._subgraph_overrides, &self.bind_address),
        )
        .await
        .expect("must be able to write config");
    }

    #[allow(dead_code)]
    pub fn execute_default_query(
        &self,
    ) -> impl std::future::Future<Output = (String, reqwest::Response)> {
        self.execute_query_internal(
            &json!({"query":"query {topProducts{name}}","variables":{}}),
            None,
        )
    }

    #[allow(dead_code)]
    pub fn execute_query(
        &self,
        query: &Value,
    ) -> impl std::future::Future<Output = (String, reqwest::Response)> {
        self.execute_query_internal(query, None)
    }

    #[allow(dead_code)]
    pub fn execute_bad_query(
        &self,
    ) -> impl std::future::Future<Output = (String, reqwest::Response)> {
        self.execute_query_internal(&json!({"garbage":{}}), None)
    }

    #[allow(dead_code)]
    pub fn execute_huge_query(
        &self,
    ) -> impl std::future::Future<Output = (String, reqwest::Response)> {
        self.execute_query_internal(&json!({"query":"query {topProducts{name, name, name, name, name, name, name, name, name, name}}","variables":{}}), None)
    }

    #[allow(dead_code)]
    pub fn execute_bad_content_type(
        &self,
    ) -> impl std::future::Future<Output = (String, reqwest::Response)> {
        self.execute_query_internal(&json!({"garbage":{}}), Some("garbage"))
    }

    fn execute_query_internal(
        &self,
        query: &Value,
        content_type: Option<&'static str>,
    ) -> impl std::future::Future<Output = (String, reqwest::Response)> {
        assert!(
            self.router.is_some(),
            "router was not started, call `router.start().await; router.assert_started().await`"
        );
        let telemetry = self.telemetry.clone();

        let query = query.clone();
        let url = format!("http://{}", self.bind_address);

        async move {
            let span = info_span!("client_request");
            let span_id = span.context().span().span_context().trace_id().to_string();

            async move {
                let client = reqwest::Client::new();

                let mut request = client
                    .post(url)
                    .header(
                        CONTENT_TYPE,
                        content_type.unwrap_or(APPLICATION_JSON.essence_str()),
                    )
                    .header("apollographql-client-name", "custom_name")
                    .header("apollographql-client-version", "1.0")
                    .header("x-my-header", "test")
                    .header("head", "test")
                    .json(&query)
                    .build()
                    .unwrap();
                telemetry.inject_context(&mut request);
                request.headers_mut().remove(ACCEPT);
                match client.execute(request).await {
                    Ok(response) => (span_id, response),
                    Err(err) => {
                        panic!("unable to send successful request to router, {err}")
                    }
                }
            }
            .instrument(span)
            .await
        }
        .with_subscriber(self.subscriber_client.clone())
    }

    #[allow(dead_code)]
    pub fn execute_untraced_query(
        &self,
        query: &Value,
    ) -> impl std::future::Future<Output = (String, reqwest::Response)> {
        assert!(
            self.router.is_some(),
            "router was not started, call `router.start().await; router.assert_started().await`"
        );
        let query = query.clone();
        let url = format!("http://{}", self.bind_address);

        async move {
            let client = reqwest::Client::new();

            let mut request = client
                .post(url)
                .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                .header("apollographql-client-name", "custom_name")
                .header("apollographql-client-version", "1.0")
                .json(&query)
                .build()
                .unwrap();

            request.headers_mut().remove(ACCEPT);
            match client.execute(request).await {
                Ok(response) => (
                    response
                        .headers()
                        .get("apollo-custom-trace-id")
                        .cloned()
                        .unwrap_or(HeaderValue::from_static("no-trace-id"))
                        .to_str()
                        .unwrap_or_default()
                        .to_string(),
                    response,
                ),
                Err(err) => {
                    panic!("unable to send successful request to router, {err}")
                }
            }
        }
        .with_subscriber(self.subscriber_client.clone())
    }

    /// Make a raw multipart request to the router.
    #[allow(dead_code)]
    pub fn execute_multipart_request(
        &self,
        request: reqwest::multipart::Form,
        transform: Option<fn(reqwest::Request) -> reqwest::Request>,
    ) -> impl std::future::Future<Output = (String, reqwest::Response)> {
        assert!(
            self.router.is_some(),
            "router was not started, call `router.start().await; router.assert_started().await`"
        );

        let url = format!("http://{}", self.bind_address);
        async move {
            let span = info_span!("client_raw_request");
            let span_id = span.context().span().span_context().trace_id().to_string();

            async move {
                let client = reqwest::Client::new();
                let mime = {
                    let mut m = MediaType::new(MULTIPART, FORM_DATA);
                    m.set_param(BOUNDARY, mediatype::Value::new(request.boundary()).unwrap());

                    m
                };

                let mut request = client
                    .post(url)
                    .header(CONTENT_TYPE, mime.to_string())
                    .header("apollographql-client-name", "custom_name")
                    .header("apollographql-client-version", "1.0")
                    .header("x-my-header", "test")
                    .multipart(request)
                    .build()
                    .unwrap();

                // Optionally transform the request if needed
                let transformer = transform.unwrap_or(core::convert::identity);

                global::get_text_map_propagator(|propagator| {
                    propagator.inject_context(
                        &tracing::span::Span::current().context(),
                        &mut opentelemetry_http::HeaderInjector(request.headers_mut()),
                    );
                });
                request.headers_mut().remove(ACCEPT);
                match client.execute(transformer(request)).await {
                    Ok(response) => (span_id, response),
                    Err(err) => {
                        panic!("unable to send successful request to router, {err}")
                    }
                }
            }
            .instrument(span)
            .await
        }
        .with_subscriber(self.subscriber_client.clone())
    }

    #[allow(dead_code)]
    pub async fn run_subscription(&self, subscription: &str) -> (String, reqwest::Response) {
        assert!(
            self.router.is_some(),
            "router was not started, call `router.start().await; router.assert_started().await`"
        );
        let client = reqwest::Client::new();
        let id = Uuid::new_v4().to_string();
        let span = info_span!("client_request", unit_test = id.as_str());
        let _span_guard = span.enter();

        let mut request = client
            .post(format!("http://{}", self.bind_address))
            .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
            .header(ACCEPT, "multipart/mixed;subscriptionSpec=1.0")
            .header("apollographql-client-name", "custom_name")
            .header("apollographql-client-version", "1.0")
            .json(&json!({"query":subscription,"variables":{}}))
            .build()
            .unwrap();

        global::get_text_map_propagator(|propagator| {
            propagator.inject_context(
                &span.context(),
                &mut opentelemetry_http::HeaderInjector(request.headers_mut()),
            );
        });

        match client.execute(request).await {
            Ok(response) => (id, response),
            Err(err) => {
                panic!("unable to send successful request to router, {err}")
            }
        }
    }

    #[allow(dead_code)]
    pub async fn get_metrics_response(&self) -> reqwest::Result<reqwest::Response> {
        let client = reqwest::Client::new();

        let request = client
            .get(format!("http://{}/metrics", self.bind_address))
            .header("apollographql-client-name", "custom_name")
            .header("apollographql-client-version", "1.0")
            .build()
            .unwrap();

        client.execute(request).await
    }

    #[allow(dead_code)]
    #[cfg(target_family = "unix")]
    pub async fn graceful_shutdown(&mut self) {
        // Send a sig term and then wait for the process to finish.
        unsafe {
            libc::kill(self.pid(), libc::SIGTERM);
        }
        self.assert_shutdown().await;
    }

    #[cfg(target_os = "windows")]
    pub async fn graceful_shutdown(&mut self) {
        // We don’t have SIGTERM on Windows, so do a non-graceful kill instead
        self.kill().await
    }

    #[allow(dead_code)]
    pub async fn kill(&mut self) {
        let _ = self
            .router
            .as_mut()
            .expect("router not started")
            .kill()
            .await;
        self.assert_shutdown().await;
    }

    #[allow(dead_code)]
    pub(crate) fn pid(&mut self) -> i32 {
        self.router
            .as_ref()
            .expect("router must have been started")
            .id()
            .expect("id expected") as i32
    }

    #[allow(dead_code)]
    pub async fn assert_reloaded(&mut self) {
        self.assert_log_contains("reload complete").await;
    }

    #[allow(dead_code)]
    pub async fn assert_no_reload_necessary(&mut self) {
        self.assert_log_contains("no reload necessary").await;
    }

    #[allow(dead_code)]
    pub async fn assert_not_reloaded(&mut self) {
        self.assert_log_contains("continuing with previous configuration")
            .await;
    }

    #[allow(dead_code)]
    pub async fn assert_log_contains(&mut self, msg: &str) {
        let now = Instant::now();
        while now.elapsed() < Duration::from_secs(10) {
            if let Ok(line) = self.stdio_rx.try_recv() {
                if line.contains(msg) {
                    return;
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        self.dump_stack_traces();
        panic!("'{msg}' not detected in logs");
    }

    #[allow(dead_code)]
    pub async fn assert_log_not_contains(&mut self, msg: &str) {
        let now = Instant::now();
        while now.elapsed() < Duration::from_secs(5) {
            if let Ok(line) = self.stdio_rx.try_recv() {
                if line.contains(msg) {
                    self.dump_stack_traces();
                    panic!("'{msg}' detected in logs");
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    #[allow(dead_code)]
    pub async fn assert_metrics_contains(&self, text: &str, duration: Option<Duration>) {
        let now = Instant::now();
        let mut last_metrics = String::new();
        while now.elapsed() < duration.unwrap_or_else(|| Duration::from_secs(15)) {
            if let Ok(metrics) = self
                .get_metrics_response()
                .await
                .expect("failed to fetch metrics")
                .text()
                .await
            {
                if metrics.contains(text) {
                    return;
                }
                last_metrics = metrics;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("'{text}' not detected in metrics\n{last_metrics}");
    }

    #[allow(dead_code)]
    pub async fn assert_metrics_does_not_contain(&self, text: &str) {
        if let Ok(metrics) = self
            .get_metrics_response()
            .await
            .expect("failed to fetch metrics")
            .text()
            .await
        {
            if metrics.contains(text) {
                panic!("'{text}' detected in metrics\n{metrics}");
            }
        }
    }

    #[allow(dead_code)]
    pub async fn assert_shutdown(&mut self) {
        let router = self.router.as_mut().expect("router must have been started");
        let now = Instant::now();
        while now.elapsed() < Duration::from_secs(3) {
            match router.try_wait() {
                Ok(Some(_)) => {
                    self.router = None;
                    return;
                }
                Ok(None) => tokio::time::sleep(Duration::from_millis(10)).await,
                _ => {}
            }
        }

        self.dump_stack_traces();
        panic!("unable to shutdown router, this probably means a hang and should be investigated");
    }

    #[allow(dead_code)]
    #[cfg(target_family = "unix")]
    pub async fn send_sighup(&mut self) {
        unsafe {
            libc::kill(self.pid(), libc::SIGHUP);
        }
    }

    #[cfg(target_os = "linux")]
    pub fn dump_stack_traces(&mut self) {
        if let Ok(trace) = rstack::TraceOptions::new()
            .symbols(true)
            .thread_names(true)
            .trace(self.pid() as u32)
        {
            println!("dumped stack traces");
            for thread in trace.threads() {
                println!(
                    "thread id: {}, name: {}",
                    thread.id(),
                    thread.name().unwrap_or("<unknown>")
                );

                for frame in thread.frames() {
                    println!(
                        "  {}",
                        frame.symbol().map(|s| s.name()).unwrap_or("<unknown>")
                    );
                }
            }
        } else {
            println!("failed to dump stack trace");
        }
    }
    #[cfg(not(target_os = "linux"))]
    pub fn dump_stack_traces(&mut self) {}

    #[allow(dead_code)]
    pub(crate) fn force_flush(&self) {
        let tracer_provider_client = self._tracer_provider_client.clone();
        let tracer_provider_subgraph = self._tracer_provider_subgraph.clone();
        for r in tracer_provider_subgraph.force_flush() {
            if let Err(e) = r {
                eprintln!("failed to flush subgraph tracer: {e}");
            }
        }

        for r in tracer_provider_client.force_flush() {
            if let Err(e) = r {
                eprintln!("failed to flush client tracer: {e}");
            }
        }
    }
}

impl Drop for IntegrationTest {
    fn drop(&mut self) {
        if let Some(child) = &mut self.router {
            let _ = child.start_kill();
        }
    }
}

pub trait ValueExt {
    fn select_path<'a>(&'a self, path: &str) -> Result<Vec<&'a Value>, BoxError>;
    fn as_string(&self) -> Option<String>;
}

impl ValueExt for Value {
    fn select_path<'a>(&'a self, path: &str) -> Result<Vec<&'a Value>, BoxError> {
        Ok(Selector::new().str_path(path)?.value(self).select()?)
    }
    fn as_string(&self) -> Option<String> {
        self.as_str().map(|s| s.to_string())
    }
}

/// Merge in overrides to a yaml config.
///
/// The test harness needs some options to be present for it to work, so this
/// function allows patching any config to include the needed values.
fn merge_overrides(
    yaml: &str,
    subgraph_overrides: &HashMap<String, String>,
    bind_addr: &SocketAddr,
) -> String {
    // Parse the config as yaml
    let mut config: Value = serde_yaml::from_str(yaml).unwrap();

    // Insert subgraph overrides, making sure to keep other overrides if present
    let overrides = subgraph_overrides
        .iter()
        .map(|(name, url)| (name.clone(), serde_json::Value::String(url.clone())));
    match config
        .as_object_mut()
        .and_then(|o| o.get_mut("override_subgraph_url"))
        .and_then(|o| o.as_object_mut())
    {
        None => {
            if let Some(o) = config.as_object_mut() {
                o.insert("override_subgraph_url".to_string(), overrides.collect());
            }
        }
        Some(override_url) => {
            override_url.extend(overrides);
        }
    }

    // Override the listening address always since we spawn the router on a
    // random port.
    match config
        .as_object_mut()
        .and_then(|o| o.get_mut("supergraph"))
        .and_then(|o| o.as_object_mut())
    {
        None => {
            if let Some(o) = config.as_object_mut() {
                o.insert(
                    "supergraph".to_string(),
                    serde_json::json!({
                        "listen": bind_addr.to_string(),
                    }),
                );
            }
        }
        Some(supergraph_conf) => {
            supergraph_conf.insert(
                "listen".to_string(),
                serde_json::Value::String(bind_addr.to_string()),
            );
        }
    }

    // Override the metrics listening address always since we spawn the router on a
    // random port.
    if let Some(prom_config) = config
        .as_object_mut()
        .and_then(|o| o.get_mut("telemetry"))
        .and_then(|o| o.as_object_mut())
        .and_then(|o| o.get_mut("exporters"))
        .and_then(|o| o.as_object_mut())
        .and_then(|o| o.get_mut("metrics"))
        .and_then(|o| o.as_object_mut())
        .and_then(|o| o.get_mut("prometheus"))
        .and_then(|o| o.as_object_mut())
    {
        prom_config.insert(
            "listen".to_string(),
            serde_json::Value::String(bind_addr.to_string()),
        );
    }

    // Set health check listen address to avoid port conflicts
    config
        .as_object_mut()
        .expect("config should be an object")
        .insert(
            "health_check".to_string(),
            json!({"listen": bind_addr.to_string()}),
        );

    serde_yaml::to_string(&config).unwrap()
}
