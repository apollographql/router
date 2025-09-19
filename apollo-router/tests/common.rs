use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::fs;
use std::net::SocketAddr;
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::OnceLock;
use std::time::Duration;

use buildstructor::buildstructor;
use fred::clients::Client as RedisClient;
use fred::interfaces::ClientLike;
use fred::interfaces::KeysInterface;
use fred::prelude::Config as RedisConfig;
use fred::types::scan::ScanType;
use fred::types::scan::Scanner;
use futures::StreamExt;
use http::header::ACCEPT;
use http::header::CONTENT_TYPE;
use mime::APPLICATION_JSON;
use opentelemetry::Context;
use opentelemetry::KeyValue;
use opentelemetry::global;
use opentelemetry::propagation::TextMapPropagator;
use opentelemetry::trace::SpanContext;
use opentelemetry::trace::TraceContextExt;
use opentelemetry::trace::TraceId;
use opentelemetry::trace::TracerProvider as OtherTracerProvider;
use opentelemetry_otlp::HttpExporterBuilder;
use opentelemetry_otlp::Protocol;
use opentelemetry_otlp::SpanExporterBuilder;
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_proto::tonic::collector::metrics::v1::ExportMetricsServiceRequest;
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::testing::trace::NoopSpanExporter;
use opentelemetry_sdk::trace::BatchConfigBuilder;
use opentelemetry_sdk::trace::BatchSpanProcessor;
use opentelemetry_sdk::trace::Config;
use opentelemetry_sdk::trace::TracerProvider;
use opentelemetry_semantic_conventions::resource::SERVICE_NAME;
use parking_lot::Mutex;
use prost::Message;
use regex::Regex;
use reqwest::Request;
use serde_json::Value;
use serde_json::json;
use tokio::io::AsyncBufReadExt;
use tokio::io::AsyncWriteExt;
use tokio::io::BufReader;
use tokio::process::Child;
use tokio::process::Command;
use tokio::task;
use tokio::time::Instant;
use tracing::info_span;
use tracing_core::Dispatch;
use tracing_core::LevelFilter;
use tracing_futures::Instrument;
use tracing_futures::WithSubscriber;
use tracing_opentelemetry::OpenTelemetrySpanExt;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::Layer;
use tracing_subscriber::Registry;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use uuid::Uuid;
use wiremock::Mock;
use wiremock::Respond;
use wiremock::ResponseTemplate;
use wiremock::http::Method;
use wiremock::matchers::method;
use wiremock::matchers::path;
use wiremock::matchers::path_regex;

/// Global registry to keep track of allocated ports across all tests
/// This helps avoid port conflicts between concurrent tests
static ALLOCATED_PORTS: OnceLock<Arc<Mutex<HashMap<u16, String>>>> = OnceLock::new();

fn get_allocated_ports() -> &'static Arc<Mutex<HashMap<u16, String>>> {
    ALLOCATED_PORTS.get_or_init(|| Arc::new(Mutex::new(HashMap::new())))
}

/// Allocate a port that's currently available
/// The port is not actually bound, just marked as allocated to avoid conflicts
fn allocate_port(name: &str) -> std::io::Result<u16> {
    let ports_registry = get_allocated_ports();

    // Try to find an available port
    for _ in 0..100 {
        // Try up to 100 times to find a port
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let port = listener.local_addr()?.port();
        drop(listener); // Release the port immediately

        let mut ports = ports_registry.lock();
        if let Entry::Vacant(e) = ports.entry(port) {
            e.insert(name.to_string());
            return Ok(port);
        }
    }

    Err(std::io::Error::new(
        std::io::ErrorKind::AddrInUse,
        "Could not find available port after 100 attempts",
    ))
}

pub struct Query {
    traced: bool,
    psr: Option<&'static str>,
    headers: HashMap<String, String>,
    content_type: String,
    body: Value,
}

impl Default for Query {
    fn default() -> Self {
        Query::builder().build()
    }
}

#[buildstructor::buildstructor]
impl Query {
    #[builder]
    pub fn new(
        traced: Option<bool>,
        psr: Option<&'static str>,
        body: Option<Value>,
        content_type: Option<String>,
        headers: HashMap<String, String>,
    ) -> Self {
        Self {
            traced: traced.unwrap_or(true),
            psr,
            body: body.unwrap_or(
                json!({"query":"query ExampleQuery {topProducts{name}}","variables":{}}),
            ),
            content_type: content_type
                .unwrap_or_else(|| APPLICATION_JSON.essence_str().to_string()),
            headers,
        }
    }
}
impl Query {
    #[allow(dead_code)]
    pub fn with_bad_content_type(mut self) -> Self {
        self.content_type = "garbage".to_string();
        self
    }

    #[allow(dead_code)]
    pub fn with_bad_query(mut self) -> Self {
        self.body = json!({"garbage":{}});
        self
    }

    #[allow(dead_code)]
    pub fn with_invalid_query(mut self) -> Self {
        self.body = json!({"query": "query {anInvalidField}", "variables":{}});
        self
    }

    #[allow(dead_code)]
    pub fn with_anonymous(mut self) -> Self {
        self.body = json!({"query":"query {topProducts{name}}","variables":{}});
        self
    }

    #[allow(dead_code)]
    pub fn with_huge_query(mut self) -> Self {
        self.body = json!({"query":"query {topProducts{name, name, name, name, name, name, name, name, name, name, name, name, name, name, name, name, name, name, name, name, name, name, name, name, name, name, name, name, name, name, name}}","variables":{}});
        self
    }

    #[allow(dead_code)]
    pub fn introspection() -> Query {
        Query::builder()
            .body(json!({"query":"{__schema {types {name}}}","variables":{}}))
            .build()
    }
}

pub struct IntegrationTest {
    router: Option<Child>,
    test_config_location: PathBuf,
    test_schema_location: PathBuf,
    router_location: PathBuf,
    stdio_tx: tokio::sync::mpsc::Sender<String>,
    stdio_rx: tokio::sync::mpsc::Receiver<String>,
    apollo_otlp_metrics_rx: tokio::sync::mpsc::Receiver<ExportMetricsServiceRequest>,
    collect_stdio: Option<(tokio::sync::oneshot::Sender<String>, regex::Regex)>,
    _subgraphs: wiremock::MockServer,
    _apollo_otlp_server: wiremock::MockServer,
    telemetry: Telemetry,
    extra_propagator: Telemetry,

    pub _tracer_provider_client: TracerProvider,
    pub _tracer_provider_subgraph: TracerProvider,
    subscriber_client: Dispatch,

    _subgraph_overrides: HashMap<String, String>,
    bind_address: Arc<Mutex<Option<SocketAddr>>>,
    redis_namespace: String,
    log: String,
    subgraph_context: Arc<Mutex<Option<SpanContext>>>,
    logs: Vec<String>,
    port_replacements: HashMap<String, u16>,
}

impl IntegrationTest {
    pub(crate) fn bind_address(&self) -> SocketAddr {
        self.bind_address
            .lock()
            .expect("no bind address set, router must be started first.")
    }

    /// Reserve a port for use in the test and return it
    /// The port placeholder will be immediately replaced in the config file
    /// Panics if the placeholder is not found in the config
    /// This helps avoid port conflicts between concurrent tests
    #[allow(dead_code)]
    pub fn reserve_address(&mut self, placeholder_name: &str) -> u16 {
        let port = allocate_port(placeholder_name).expect("Failed to allocate port");
        self.set_address(placeholder_name, port);
        port
    }

    /// Reserve a specific port for use in the test
    /// The port placeholder will be immediately replaced in the config file
    /// Panics if the placeholder is not found in the config
    #[allow(dead_code)]
    pub fn set_address(&mut self, placeholder_name: &str, port: u16) {
        // Read current config
        let current_config = std::fs::read_to_string(&self.test_config_location)
            .expect("Failed to read config file");

        // Check if placeholder exists in config
        let placeholder_pattern = format!("{{{{{}}}}}", placeholder_name);
        let port_pattern = format!(":{{{{{}}}}}", placeholder_name);
        let addr_pattern = format!("127.0.0.1:{{{{{}}}}}", placeholder_name);

        if !current_config.contains(&placeholder_pattern)
            && !current_config.contains(&port_pattern)
            && !current_config.contains(&addr_pattern)
        {
            panic!(
                "Placeholder '{}' not found in config file. Expected one of: '{}', '{}', or '{}'",
                placeholder_name, placeholder_pattern, port_pattern, addr_pattern
            );
        }

        // Store the replacement
        self.port_replacements
            .insert(placeholder_name.to_string(), port);

        // Apply the replacement immediately
        let updated_config = merge_overrides(
            &current_config,
            &self._subgraph_overrides,
            &self._apollo_otlp_server.uri().to_string(),
            None, // Don't override bind address here
            &self.redis_namespace,
            Some(&self.port_replacements),
        );

        std::fs::write(&self.test_config_location, updated_config)
            .expect("Failed to write updated config");
    }

    /// Set an address placeholder using a URI, extracting the port automatically
    /// This is a convenience method for the common pattern of extracting port from a server URI
    #[allow(dead_code)]
    pub fn set_address_from_uri(&mut self, placeholder_name: &str, uri: &str) {
        let port = uri
            .split(':')
            .next_back()
            .expect("URI should contain a port")
            .parse::<u16>()
            .expect("Port should be a valid u16");
        self.set_address(placeholder_name, port);
    }

    /// Replace a string in the config file (for non-port replacements)
    /// This is useful for dynamic config adjustments beyond port replacements
    #[allow(dead_code)]
    pub fn replace_config_string(&mut self, from: &str, to: &str) {
        let current_config = std::fs::read_to_string(&self.test_config_location)
            .expect("Failed to read config file");

        let updated_config = current_config.replace(from, to);

        std::fs::write(&self.test_config_location, updated_config)
            .expect("Failed to write updated config");
    }

    /// Replace a string in the config file (for non-port replacements)
    /// This is useful for dynamic config adjustments beyond port replacements
    #[allow(dead_code)]
    pub fn replace_schema_string(&mut self, from: &str, to: &str) {
        let current_schema = std::fs::read_to_string(&self.test_schema_location)
            .expect("Failed to read schema file");

        let updated_schema = current_schema.replace(from, to);

        std::fs::write(&self.test_schema_location, updated_schema)
            .expect("Failed to write updated schema");
    }
}

struct TracedResponder {
    response_template: ResponseTemplate,
    telemetry: Telemetry,
    extra_propagator: Telemetry,
    subscriber_subgraph: Dispatch,
    subgraph_callback: Option<Box<dyn Fn() + Send + Sync>>,
    subgraph_context: Arc<Mutex<Option<SpanContext>>>,
}

impl Respond for TracedResponder {
    fn respond(&self, request: &wiremock::Request) -> ResponseTemplate {
        let context = self.telemetry.extract_context(request, &Context::new());
        let context = self.extra_propagator.extract_context(request, &context);

        *self.subgraph_context.lock() = Some(context.span().span_context().clone());
        tracing_core::dispatcher::with_default(&self.subscriber_subgraph, || {
            let _context_guard = context.attach();
            let span = info_span!("subgraph server");
            let _span_guard = span.enter();
            if let Some(callback) = &self.subgraph_callback {
                callback();
            }
            self.response_template.clone()
        })
    }
}

#[derive(Debug, Clone, Default)]
#[allow(dead_code)]
pub enum Telemetry {
    Otlp {
        endpoint: Option<String>,
    },
    Datadog,
    Zipkin,
    #[default]
    None,
}

impl Telemetry {
    fn tracer_provider(&self, service_name: &str) -> TracerProvider {
        let config = Config::default().with_resource(Resource::new(vec![KeyValue::new(
            SERVICE_NAME,
            service_name.to_string(),
        )]));

        match self {
            Telemetry::Otlp {
                endpoint: Some(endpoint),
            } => TracerProvider::builder()
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
                        opentelemetry_sdk::runtime::Tokio,
                    )
                    .with_batch_config(
                        BatchConfigBuilder::default()
                            .with_scheduled_delay(Duration::from_millis(10))
                            .build(),
                    )
                    .build(),
                )
                .build(),
            Telemetry::Datadog => TracerProvider::builder()
                .with_config(config)
                .with_span_processor(
                    BatchSpanProcessor::builder(
                        opentelemetry_datadog::new_pipeline()
                            .with_service_name(service_name)
                            .build_exporter()
                            .expect("datadog pipeline failed"),
                        opentelemetry_sdk::runtime::Tokio,
                    )
                    .with_batch_config(
                        BatchConfigBuilder::default()
                            .with_scheduled_delay(Duration::from_millis(10))
                            .build(),
                    )
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
                        opentelemetry_sdk::runtime::Tokio,
                    )
                    .with_batch_config(
                        BatchConfigBuilder::default()
                            .with_scheduled_delay(Duration::from_millis(10))
                            .build(),
                    )
                    .build(),
                )
                .build(),
            Telemetry::None | Telemetry::Otlp { endpoint: None } => TracerProvider::builder()
                .with_config(config)
                .with_simple_exporter(NoopSpanExporter::default())
                .build(),
        }
    }

    fn inject_context(&self, request: &mut Request) {
        let ctx = tracing::span::Span::current().context();

        match self {
            Telemetry::Datadog => {
                // Get the existing PSR header if it exists. This is because the existing telemetry propagator doesn't support PSR properly yet.
                // In testing we are manually setting the PSR header, and we don't want to override it.
                let psr = request
                    .headers()
                    .get("x-datadog-sampling-priority")
                    .cloned();
                let propagator = opentelemetry_datadog::DatadogPropagator::new();
                propagator.inject_context(
                    &ctx,
                    &mut apollo_router::otel_compat::HeaderInjector(request.headers_mut()),
                );

                if let Some(psr) = psr {
                    request
                        .headers_mut()
                        .insert("x-datadog-sampling-priority", psr);
                }
            }
            Telemetry::Otlp { .. } => {
                let propagator = opentelemetry_sdk::propagation::TraceContextPropagator::default();
                propagator.inject_context(
                    &ctx,
                    &mut apollo_router::otel_compat::HeaderInjector(request.headers_mut()),
                )
            }
            Telemetry::Zipkin => {
                let propagator = opentelemetry_zipkin::Propagator::new();
                propagator.inject_context(
                    &ctx,
                    &mut apollo_router::otel_compat::HeaderInjector(request.headers_mut()),
                )
            }
            _ => {}
        }
    }

    pub(crate) fn extract_context(
        &self,
        request: &wiremock::Request,
        context: &Context,
    ) -> Context {
        let headers: HashMap<String, String> = request
            .headers
            .iter()
            .map(|(name, value)| {
                (
                    name.as_str().to_string(),
                    value
                        .to_str()
                        .expect("non-UTF-8 header value in tests")
                        .to_string(),
                )
            })
            .collect();

        match self {
            Telemetry::Datadog => {
                let span_ref = context.span();
                let original_span_context = span_ref.span_context();
                let propagator = opentelemetry_datadog::DatadogPropagator::new();
                let mut context = propagator.extract_with_context(context, &headers);
                // We're going to override the sampled so that we can test sampling priority
                if let Some(psr) = headers.get("x-datadog-sampling-priority") {
                    let state = context
                        .span()
                        .span_context()
                        .trace_state()
                        .insert("psr", psr.to_string())
                        .expect("psr");
                    let new_trace_id = if original_span_context.is_valid() {
                        original_span_context.trace_id()
                    } else {
                        context.span().span_context().trace_id()
                    };
                    context = context.with_remote_span_context(SpanContext::new(
                        new_trace_id,
                        context.span().span_context().span_id(),
                        context.span().span_context().trace_flags(),
                        true,
                        state,
                    ));
                }

                context
            }
            Telemetry::Otlp { .. } => {
                let propagator = opentelemetry_sdk::propagation::TraceContextPropagator::default();
                propagator.extract_with_context(context, &headers)
            }
            Telemetry::Zipkin => {
                let propagator = opentelemetry_zipkin::Propagator::new();
                propagator.extract_with_context(context, &headers)
            }
            _ => context.clone(),
        }
    }
}

#[buildstructor]
impl IntegrationTest {
    #[builder]
    pub async fn new(
        config: String,
        telemetry: Option<Telemetry>,
        extra_propagator: Option<Telemetry>,
        responder: Option<ResponseTemplate>,
        collect_stdio: Option<tokio::sync::oneshot::Sender<String>>,
        supergraph: Option<PathBuf>,
        mut subgraph_overrides: HashMap<String, String>,
        log: Option<String>,
        subgraph_callback: Option<Box<dyn Fn() + Send + Sync>>,
        http_method: Option<String>,
    ) -> Self {
        let redis_namespace = Uuid::new_v4().to_string();
        let telemetry = telemetry.unwrap_or_default();
        let extra_propagator = extra_propagator.unwrap_or_default();
        let tracer_provider_client = telemetry.tracer_provider("client");
        let subscriber_client = Self::dispatch(&tracer_provider_client);
        let tracer_provider_subgraph = telemetry.tracer_provider("subgraph");

        let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0))).unwrap();
        let address = listener.local_addr().unwrap();
        let url = format!("http://{address}/");

        let apollo_otlp_listener =
            TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0))).unwrap();
        let apollo_otlp_address = apollo_otlp_listener.local_addr().unwrap();
        let apollo_otlp_endpoint = format!("http://{apollo_otlp_address}");

        // Add a default override for products, if not specified
        subgraph_overrides
            .entry("products".into())
            .or_insert(url.clone());

        // Add a default override for jsonPlaceholder (connectors), if not specified
        subgraph_overrides
            .entry("jsonPlaceholder".into())
            .or_insert(url.clone());

        // Insert the overrides into the config
        let config_str = merge_overrides(
            &config,
            &subgraph_overrides,
            &apollo_otlp_endpoint,
            None,
            &redis_namespace,
            None,
        );

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

        // Allow for GET or POST so that connectors works
        let http_method = match http_method.unwrap_or("POST".to_string()).as_str() {
            "GET" => Method::GET,
            "POST" => Method::POST,
            _ => panic!("Unknown http method specified"),
        };
        let subgraph_context = Arc::new(Mutex::new(None));
        Mock::given(method(http_method))
            .and(path_regex(".*")) // Match any path so that connectors functions
            .respond_with(TracedResponder {
                response_template: responder.unwrap_or_else(|| {
                    ResponseTemplate::new(200).set_body_json(json!({
                        "data": {
                            "topProducts": [
                                { "name": "Table" },
                                { "name": "Couch" },
                                { "name": "Chair" },
                            ],
                        },
                    }))
                }),
                telemetry: telemetry.clone(),
                extra_propagator: extra_propagator.clone(),
                subscriber_subgraph: Self::dispatch(&tracer_provider_subgraph),
                subgraph_callback,
                subgraph_context: subgraph_context.clone(),
            })
            .mount(&subgraphs)
            .await;

        let mut test_config_location = std::env::temp_dir();
        let mut test_schema_location = test_config_location.clone();
        let location = format!("apollo-router-test-{}.yaml", Uuid::new_v4());
        test_config_location.push(location);
        test_schema_location.push(format!("apollo-router-test-{}.graphql", Uuid::new_v4()));

        fs::write(&test_config_location, &config_str).expect("could not write config");
        fs::copy(&supergraph, &test_schema_location).expect("could not write schema");

        let (stdio_tx, stdio_rx) = tokio::sync::mpsc::channel(2000);
        let collect_stdio = collect_stdio.map(|sender| {
            let version_line_re = regex::Regex::new("Apollo Router v[^ ]+ ").unwrap();
            (sender, version_line_re)
        });

        let (apollo_otlp_metrics_tx, apollo_otlp_metrics_rx) = tokio::sync::mpsc::channel(100);
        let apollo_otlp_server = wiremock::MockServer::builder()
            .listener(apollo_otlp_listener)
            .start()
            .await;
        Mock::given(method(Method::POST))
            .and(path("/v1/metrics"))
            .and(move |req: &wiremock::Request| {
                // Decode the OTLP request
                if let Ok(msg) = ExportMetricsServiceRequest::decode(req.body.as_ref()) {
                    // We don't care about the result of send here
                    let _ = apollo_otlp_metrics_tx.try_send(msg);
                }
                false
            })
            .respond_with(ResponseTemplate::new(200))
            .mount(&apollo_otlp_server)
            .await;

        Self {
            router: None,
            router_location: Self::router_location(),
            test_config_location,
            test_schema_location,
            stdio_tx,
            stdio_rx,
            apollo_otlp_metrics_rx,
            collect_stdio,
            _subgraphs: subgraphs,
            _subgraph_overrides: subgraph_overrides,
            _apollo_otlp_server: apollo_otlp_server,
            bind_address: Default::default(),
            _tracer_provider_client: tracer_provider_client,
            subscriber_client,
            _tracer_provider_subgraph: tracer_provider_subgraph,
            telemetry,
            extra_propagator,
            redis_namespace,
            log: log.unwrap_or_else(|| "error,apollo_router=info".to_owned()),
            subgraph_context,
            logs: vec![],
            port_replacements: HashMap::new(),
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

    #[allow(dead_code)]
    pub fn subgraph_context(&self) -> SpanContext {
        self.subgraph_context.lock().as_ref().unwrap().clone()
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
            .args(dbg!([
                "--hr",
                "--config",
                &self.test_config_location.to_string_lossy(),
                "--supergraph",
                &self.test_schema_location.to_string_lossy(),
                "--log",
                &self.log,
            ]))
            .stdout(Stdio::piped());

        let mut router = router.spawn().expect("router should start");
        let reader = BufReader::new(router.stdout.take().expect("out"));
        let stdio_tx = self.stdio_tx.clone();
        let collect_stdio = self.collect_stdio.take();
        let bind_address = self.bind_address.clone();
        let bind_address_regex =
            Regex::new(r".*GraphQL endpoint exposed at http://(?<address>[^/]+).*").unwrap();
        // We need to read from stdout otherwise we will hang
        task::spawn(async move {
            let mut collected = Vec::new();
            let mut lines = reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                // Extract the bind address from a log line that looks like this: GraphQL endpoint exposed at http://127.0.0.1:51087/
                if let Some(captures) = bind_address_regex.captures(&line) {
                    let address = captures.name("address").unwrap().as_str();
                    let mut bind_address = bind_address.lock();
                    *bind_address = Some(address.parse().unwrap());
                }

                if let Some((_sender, version_line_re)) = &collect_stdio {
                    #[derive(serde::Deserialize)]
                    struct Log {
                        #[allow(unused)]
                        timestamp: String,
                        level: String,
                        message: String,
                    }
                    let Ok(log) = serde_json::from_str::<Log>(&line) else {
                        panic!(
                            "line: '{line}' isn't JSON, might you have some debug output in the logging?"
                        );
                    };
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
        self.wait_for_log_message("GraphQL endpoint exposed").await;
    }

    #[allow(dead_code)]
    pub async fn assert_not_started(&mut self) {
        self.wait_for_log_message("no valid configuration").await;
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
            &merge_overrides(
                yaml,
                &self._subgraph_overrides,
                &self._apollo_otlp_server.uri().to_string(),
                Some(self.bind_address()),
                &self.redis_namespace,
                Some(&self.port_replacements),
            ),
        )
        .await
        .expect("must be able to write config");
    }

    #[allow(dead_code)]
    pub fn update_subgraph_overrides(&mut self, overrides: HashMap<String, String>) {
        self._subgraph_overrides = overrides;
    }

    #[allow(dead_code)]
    pub async fn update_schema(&self, supergraph_path: &PathBuf) {
        fs::copy(supergraph_path, &self.test_schema_location).expect("could not write schema");
    }

    #[allow(dead_code)]
    pub fn execute_default_query(
        &self,
    ) -> impl std::future::Future<Output = (TraceId, reqwest::Response)> + use<> {
        self.execute_query(Query::builder().build())
    }

    #[allow(dead_code)]
    pub fn execute_query(
        &self,
        query: Query,
    ) -> impl std::future::Future<Output = (TraceId, reqwest::Response)> + use<> {
        assert!(
            self.router.is_some(),
            "router was not started, call `router.start().await; router.assert_started().await`"
        );
        let telemetry = self.telemetry.clone();
        let extra_propagator = self.extra_propagator.clone();

        let url = format!("http://{}", self.bind_address());
        let subgraph_context = self.subgraph_context.clone();
        async move {
            let span = info_span!("client_request");
            let trace_id = span.context().span().span_context().trace_id();
            async move {
                let client = reqwest::Client::new();

                let mut builder = client.post(url).header(CONTENT_TYPE, query.content_type);

                for (name, value) in query.headers {
                    builder = builder.header(name, value);
                }

                if let Some(psr) = query.psr {
                    builder = builder.header("x-datadog-sampling-priority", psr);
                }

                let mut request = builder.json(&query.body).build().unwrap();
                if query.traced {
                    telemetry.inject_context(&mut request);
                    extra_propagator.inject_context(&mut request);
                }

                match client.execute(request).await {
                    Ok(response) => {
                        if query.traced {
                            (trace_id, response)
                        } else {
                            (
                                subgraph_context
                                    .lock()
                                    .as_ref()
                                    .expect("subgraph context")
                                    .trace_id(),
                                response,
                            )
                        }
                    }
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

    /// Make a raw multipart request to the router.
    #[allow(dead_code)]
    pub fn execute_multipart_request(
        &self,
        request: reqwest::multipart::Form,
        transform: Option<fn(reqwest::Request) -> reqwest::Request>,
    ) -> impl std::future::Future<Output = (String, reqwest::Response)> + use<> {
        assert!(
            self.router.is_some(),
            "router was not started, call `router.start().await; router.assert_started().await`"
        );

        let url = format!("http://{}", self.bind_address());
        async move {
            let span = info_span!("client_raw_request");
            let span_id = span.context().span().span_context().trace_id().to_string();

            async move {
                let client = reqwest::Client::new();
                let mut request = client
                    .post(url)
                    .header("apollographql-client-name", "custom_name")
                    .header("apollographql-client-version", "1.0")
                    .header("apollo-require-preflight", "test")
                    .multipart(request)
                    .build()
                    .unwrap();

                // Optionally transform the request if needed
                let transformer = transform.unwrap_or(core::convert::identity);

                global::get_text_map_propagator(|propagator| {
                    propagator.inject_context(
                        &tracing::span::Span::current().context(),
                        &mut apollo_router::otel_compat::HeaderInjector(request.headers_mut()),
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
            .post(format!("http://{}", self.bind_address()))
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
                &mut apollo_router::otel_compat::HeaderInjector(request.headers_mut()),
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
            .get(format!("http://{}/metrics", self.bind_address()))
            .header("apollographql-client-name", "custom_name")
            .header("apollographql-client-version", "1.0")
            .build()
            .unwrap();

        client.execute(request).await
    }

    /// Waits for any metrics to be emitted for the given duration. This will return as soon as the
    /// first batch of metrics is received.
    #[allow(dead_code)]
    pub async fn wait_for_emitted_otel_metrics(
        &mut self,
        duration: Duration,
    ) -> Vec<ExportMetricsServiceRequest> {
        let deadline = Instant::now() + duration;
        let mut metrics = Vec::new();

        while Instant::now() < deadline {
            if let Some(msg) = self.apollo_otlp_metrics_rx.recv().await {
                // Only break once we see a batch with metrics in it
                if msg
                    .resource_metrics
                    .iter()
                    .any(|rm| !rm.scope_metrics.is_empty())
                {
                    metrics.push(msg);
                    break;
                }
            } else {
                // channel closed
                break;
            }
        }

        metrics
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
    pub(crate) fn pid(&self) -> i32 {
        self.router
            .as_ref()
            .expect("router must have been started")
            .id()
            .expect("id expected") as i32
    }

    #[allow(dead_code)]
    pub async fn assert_reloaded(&mut self) {
        self.wait_for_log_message("reload complete").await;
    }

    #[allow(dead_code)]
    pub async fn assert_no_reload_necessary(&mut self) {
        self.wait_for_log_message("no reload necessary").await;
    }

    #[allow(dead_code)]
    pub async fn assert_not_reloaded(&mut self) {
        self.wait_for_log_message("continuing with previous configuration")
            .await;
    }

    #[allow(dead_code)]
    pub async fn wait_for_log_message(&mut self, msg: &str) {
        let now = Instant::now();
        while now.elapsed() < Duration::from_secs(10) {
            if let Ok(line) = self.stdio_rx.try_recv() {
                self.logs.push(line.to_string());
                if line.contains(msg) {
                    return;
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        self.dump_stack_traces();
        panic!(
            "'{msg}' not detected in logs. Log dump below:\n\n{logs}",
            logs = self.logs.join("\n")
        );
    }

    #[allow(dead_code)]
    pub fn print_logs(&self) {
        for line in &self.logs {
            println!("{}", line);
        }
    }

    #[allow(dead_code)]
    pub fn read_logs(&mut self) {
        while let Ok(line) = self.stdio_rx.try_recv() {
            self.logs.push(line);
        }
    }

    #[allow(dead_code)]
    pub fn capture_logs<T>(&mut self, try_match_line: impl Fn(String) -> Option<T>) -> Vec<T> {
        let mut logs = Vec::new();
        while let Ok(line) = self.stdio_rx.try_recv() {
            if let Some(log) = try_match_line(line) {
                logs.push(log);
            }
        }
        logs
    }

    #[allow(dead_code)]
    pub fn assert_log_contained(&self, msg: &str) {
        for line in &self.logs {
            if line.contains(msg) {
                return;
            }
        }

        panic!(
            "'{msg}' not detected in logs. Log dump below:\n\n{logs}",
            logs = self.logs.join("\n")
        );
    }

    #[allow(dead_code)]
    pub async fn assert_log_not_contains(&mut self, msg: &str) {
        let now = Instant::now();
        while now.elapsed() < Duration::from_secs(5) {
            if let Ok(line) = self.stdio_rx.try_recv() {
                if line.contains(msg) {
                    self.dump_stack_traces();
                    panic!(
                        "'{msg}' detected in logs. Log dump below:\n\n{logs}",
                        logs = self.logs.join("\n")
                    );
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    #[allow(dead_code)]
    pub fn assert_log_not_contained(&self, msg: &str) {
        for line in &self.logs {
            if line.contains(msg) {
                panic!(
                    "'{msg}' detected in logs. Log dump below:\n\n{logs}",
                    logs = self.logs.join("\n")
                );
            }
        }
    }

    #[allow(dead_code)]
    pub fn error_logs(&mut self) -> Vec<String> {
        // Read any remaining logs from buffer
        self.read_logs();

        const JSON_ERROR_INDICATORS: [&str; 3] = ["\"level\":\"ERROR\"", "panic", "PANIC"];

        let mut error_logs = Vec::new();
        for line in &self.logs {
            if JSON_ERROR_INDICATORS.iter().any(|err| line.contains(err))
                || (line.contains("ERROR") && !line.contains("level"))
            {
                error_logs.push(line.clone());
            }
        }
        error_logs
    }
    #[allow(dead_code)]
    pub fn assert_no_error_logs(&mut self) {
        let error_logs = self.error_logs();
        if !error_logs.is_empty() {
            panic!(
                "Found {} unexpected error(s) in router logs:\n\n{}\n\nFull log dump:\n\n{}",
                error_logs.len(),
                error_logs.join("\n"),
                self.logs.join("\n")
            );
        }
    }
    #[allow(dead_code)]
    pub fn assert_no_error_logs_with_exceptions(&mut self, exceptions: &[&str]) {
        let mut error_logs = self.error_logs();

        // remove any logs that contain our exceptions
        error_logs.retain(|line| !exceptions.iter().any(|exception| line.contains(exception)));
        if !error_logs.is_empty() {
            panic!(
                "Found {} unexpected error(s) in router logs (excluding {} exceptions):\n\n{}\n\nFull log dump:\n\n{}",
                error_logs.len(),
                exceptions.len(),
                error_logs.join("\n"),
                self.logs.join("\n")
            );
        }
    }

    /// Assert that some metric is non-zero. Useful for those metrics that are non-zero but whose
    /// values might change across integration test runs.  
    ///
    /// example use: `.assert_metric_non_zero("some_metric_name{label="example"}", None)`
    ///
    /// Note: make sure you strip off the value at the end or you'll potentially get false
    /// negatives
    #[allow(dead_code)]
    pub async fn assert_metric_non_zero(&self, text: &str, duration: Option<Duration>) {
        let now = Instant::now();
        let mut last_metrics = String::new();

        let pattern = regex::escape(text);
        let pattern = format!(
            // disjunction between two patterns: the first (before the `|`) says to look for a value
            // starting with a digit between 1-9, matching however many, optionally with a decimal; the
            // second pattern matches values starting with 0 and then a decimal (both required), at least
            // on non-zero digit, and then however many (if any) other digits
            "(?m)^{}\\s+([1-9]\\d*(\\.\\d+)?|0\\.[0-9]*[1-9][0-9]*)",
            pattern
        );
        let re = Regex::new(&format!("(?m)^{}", pattern)).expect("Invalid regex");

        while now.elapsed() < duration.unwrap_or_else(|| Duration::from_secs(15)) {
            if let Ok(metrics) = self
                .get_metrics_response()
                .await
                .expect("failed to fetch metrics")
                .text()
                .await
            {
                if re.is_match(&metrics) {
                    return;
                }
                last_metrics = metrics;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("'{text}' not detected in metrics\n{last_metrics}");
    }
    #[allow(dead_code)]
    /// Checks the metrics contain the supplied string in prometheus format.
    /// To allow checking of metrics where the value is not stable the magic tag `<any>` can be used.
    /// For example:
    /// ```rust,ignore
    /// router.assert_metrics_contains(r#"apollo_router_pipelines{config_hash="<any>",schema_id="<any>",otel_scope_name="apollo/router"} 1"#, None)
    /// ```
    /// Will allow the metric to be checked even if the config hash and schema id are fluid.
    pub async fn assert_metrics_contains(&self, text: &str, duration: Option<Duration>) {
        let now = Instant::now();
        let mut last_metrics = String::new();
        let text = regex::escape(text).replace("<any>", ".+");
        let re = Regex::new(&format!("(?m)^{}", text)).expect("Invalid regex");
        while now.elapsed() < duration.unwrap_or_else(|| Duration::from_secs(15)) {
            if let Ok(metrics) = self
                .get_metrics_response()
                .await
                .expect("failed to fetch metrics")
                .text()
                .await
            {
                if re.is_match(&metrics) {
                    return;
                }
                last_metrics = metrics;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("'{text}' not detected in metrics\n{last_metrics}");
    }

    #[allow(dead_code)]
    pub async fn assert_metrics_contains_multiple(
        &self,
        mut texts: Vec<&str>,
        duration: Option<Duration>,
    ) {
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
                let mut v = vec![];
                for text in &texts {
                    if !metrics.contains(text) {
                        v.push(*text);
                    }
                }
                if v.len() == texts.len() {
                    return;
                } else {
                    texts = v;
                }
                last_metrics = metrics;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("'{texts:?}' not detected in metrics\n{last_metrics}");
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
    pub fn dump_stack_traces(&self) {
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
    pub fn dump_stack_traces(&self) {}

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

    #[allow(dead_code)]
    pub async fn clear_redis_cache(&self) {
        let config = RedisConfig::from_url("redis://127.0.0.1:6379").unwrap();

        let client = RedisClient::new(config, None, None, None);
        let connection_task = client.connect();
        client
            .wait_for_connect()
            .await
            .expect("could not connect to redis");
        let namespace = &self.redis_namespace;
        let mut scan = client.scan(format!("{namespace}:*"), None, Some(ScanType::String));
        while let Some(result) = scan.next().await {
            if let Some(page) = result.expect("could not scan redis").take_results() {
                for key in page {
                    let key = key.as_str().expect("key should be a string");
                    if key.starts_with(&self.redis_namespace) {
                        client
                            .del::<usize, _>(key)
                            .await
                            .expect("could not delete key");
                    }
                }
            }
        }

        client.quit().await.expect("could not quit redis");
        // calling quit ends the connection and event listener tasks
        let _ = connection_task.await;
    }

    #[allow(dead_code)]
    pub async fn assert_redis_cache_contains(&self, key: &str, ignore: Option<&str>) -> String {
        let config = RedisConfig::from_url("redis://127.0.0.1:6379").unwrap();
        let client = RedisClient::new(config, None, None, None);
        let connection_task = client.connect();
        client.wait_for_connect().await.unwrap();
        let redis_namespace = &self.redis_namespace;
        let namespaced_key = format!("{redis_namespace}:{key}");
        let s = match client.get(&namespaced_key).await {
            Ok(s) => s,
            Err(e) => {
                println!("non-ignored keys in the same namespace in Redis:");

                let mut scan = client.scan(
                    format!("{redis_namespace}:*"),
                    Some(u32::MAX),
                    Some(ScanType::String),
                );

                while let Some(result) = scan.next().await {
                    let keys = result.as_ref().unwrap().results().as_ref().unwrap();
                    for key in keys {
                        let key = key.as_str().expect("key should be a string");
                        let unnamespaced_key = key.replace(&format!("{redis_namespace}:"), "");
                        if Some(unnamespaced_key.as_str()) != ignore {
                            println!("\t{unnamespaced_key}");
                        }
                    }
                }
                panic!(
                    "key {key} not found: {e}\n This may be caused by a number of things including federation version changes"
                );
            }
        };

        client.quit().await.unwrap();
        // calling quit ends the connection and event listener tasks
        let _ = connection_task.await;
        s
    }
}

impl Drop for IntegrationTest {
    fn drop(&mut self) {
        if let Some(child) = &mut self.router {
            let _ = child.start_kill();
        }
    }
}

/// Merge in overrides to a yaml config.
///
/// The test harness needs some options to be present for it to work, so this
/// function allows patching any config to include the needed values.
fn merge_overrides(
    yaml: &str,
    subgraph_overrides: &HashMap<String, String>,
    apollo_otlp_endpoint: &str,
    bind_addr: Option<SocketAddr>,
    redis_namespace: &str,
    port_replacements: Option<&HashMap<String, u16>>,
) -> String {
    let bind_addr = bind_addr
        .map(|a| a.to_string())
        .unwrap_or_else(|| "127.0.0.1:0".into());

    // Apply port replacements to the YAML string first
    let mut yaml_with_ports = yaml.to_string();
    if let Some(port_replacements) = port_replacements {
        for (placeholder, port) in port_replacements {
            // Replace placeholder patterns like {{PLACEHOLDER_NAME}} with the actual port
            let placeholder_pattern = format!("{{{{{}}}}}", placeholder);
            yaml_with_ports = yaml_with_ports.replace(&placeholder_pattern, &port.to_string());

            // Also replace patterns like :{{PLACEHOLDER_NAME}} with :port
            let port_pattern = format!(":{{{{{}}}}}", placeholder);
            yaml_with_ports = yaml_with_ports.replace(&port_pattern, &format!(":{}", port));

            // Replace full address patterns like 127.0.0.1:{{PLACEHOLDER_NAME}}
            let addr_pattern = format!("127.0.0.1:{{{{{}}}}}", placeholder);
            yaml_with_ports =
                yaml_with_ports.replace(&addr_pattern, &format!("127.0.0.1:{}", port));
        }
    }

    // Parse the config as yaml
    let mut config: Value = serde_yaml::from_str(&yaml_with_ports).unwrap();

    // Insert subgraph overrides, making sure to keep other overrides if present
    let overrides = subgraph_overrides
        .iter()
        .map(|(name, url)| (name.clone(), serde_json::Value::String(url.clone())));
    let overrides2 = overrides.clone();
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
    if let Some(sources) = config
        .as_object_mut()
        .and_then(|o| o.get_mut("connectors"))
        .and_then(|o| o.as_object_mut())
        .and_then(|o| o.get_mut("sources"))
        .and_then(|o| o.as_object_mut())
    {
        for (name, url) in overrides2 {
            let mut obj = serde_json::Map::new();
            obj.insert("override_url".to_string(), url.clone());
            sources.insert(format!("connectors.{}", name), Value::Object(obj));
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

    // Override the Apollo OTLP metrics listening address
    if let Some(apollo_config) = config
        .as_object_mut()
        .and_then(|o| o.get_mut("telemetry"))
        .and_then(|o| o.as_object_mut())
        .and_then(|o| o.get_mut("apollo"))
        .and_then(|o| o.as_object_mut())
    {
        apollo_config.insert(
            "experimental_otlp_endpoint".to_string(),
            serde_json::Value::String(apollo_otlp_endpoint.to_string()),
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

    // Set query plan redis namespace
    if let Some(query_plan) = config
        .as_object_mut()
        .and_then(|o| o.get_mut("supergraph"))
        .and_then(|o| o.as_object_mut())
        .and_then(|o| o.get_mut("query_planning"))
        .and_then(|o| o.as_object_mut())
        .and_then(|o| o.get_mut("cache"))
        .and_then(|o| o.as_object_mut())
        .and_then(|o| o.get_mut("redis"))
        .and_then(|o| o.as_object_mut())
    {
        query_plan.insert("namespace".to_string(), redis_namespace.into());
    }

    serde_yaml::to_string(&config).unwrap()
}

#[allow(dead_code)]
pub fn graph_os_enabled() -> bool {
    matches!(
        (
            std::env::var("TEST_APOLLO_KEY"),
            std::env::var("TEST_APOLLO_GRAPH_REF"),
        ),
        (Ok(_), Ok(_))
    )
}

/// Automatic tracing initialization using ctor for integration tests
#[ctor::ctor]
fn init_integration_test_tracing() {
    // Initialize tracing for integration tests
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .or_else(|_| tracing_subscriber::EnvFilter::try_new("info,apollo_router=debug"))
        .unwrap();

    let _ = tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::Layer::default()
                .with_target(false)
                .with_thread_ids(false)
                .with_thread_names(false)
                .compact()
                .with_filter(filter),
        )
        .try_init();
}
