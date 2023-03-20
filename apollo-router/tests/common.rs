use std::collections::HashMap;
use std::fs;
use std::net::SocketAddr;
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use std::time::SystemTime;

use buildstructor::buildstructor;
use http::header::ACCEPT;
use http::header::CONTENT_TYPE;
use jsonpath_lib::Selector;
use mime::APPLICATION_JSON;
use once_cell::sync::OnceCell;
use opentelemetry::global;
use opentelemetry::propagation::TextMapPropagator;
use opentelemetry::trace::Span;
use opentelemetry::trace::Tracer;
use opentelemetry::trace::TracerProvider;
use serde_json::json;
use serde_json::Value;
use tokio::io::AsyncBufReadExt;
use tokio::io::AsyncWriteExt;
use tokio::io::BufReader;
use tokio::process::Child;
use tokio::process::Command;
use tokio::sync::Mutex;
use tokio::task;
use tokio::time::Instant;
use tower::BoxError;
use tracing::info_span;
use tracing_core::LevelFilter;
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

static LOCK: OnceCell<Arc<Mutex<bool>>> = OnceCell::new();

pub struct IntegrationTest {
    router: Option<Child>,
    test_config_location: PathBuf,
    router_location: PathBuf,
    _lock: tokio::sync::OwnedMutexGuard<bool>,
    stdio_tx: tokio::sync::mpsc::Sender<String>,
    stdio_rx: tokio::sync::mpsc::Receiver<String>,
    _subgraphs: wiremock::MockServer,
}

struct TracedResponder(pub(crate) ResponseTemplate);

impl Respond for TracedResponder {
    fn respond(&self, request: &wiremock::Request) -> ResponseTemplate {
        let tracer_provider = opentelemetry_jaeger::new_agent_pipeline()
            .with_service_name("products")
            .build_simple()
            .unwrap();
        let tracer = tracer_provider.tracer("products");
        let headers: HashMap<String, String> = request
            .headers
            .iter()
            .map(|(name, value)| (name.as_str().to_string(), value.as_str().to_string()))
            .collect();
        let context = opentelemetry_jaeger::Propagator::new().extract(&headers);
        let mut span = tracer.start_with_context("HTTP POST", &context);
        span.end_with_timestamp(SystemTime::now());
        tracer_provider.force_flush();
        self.0.clone()
    }
}

#[allow(dead_code)]
pub enum Telemetry {
    Jaeger,
    Otlp,
    Datadog,
    Zipkin,
}

#[buildstructor]
impl IntegrationTest {
    #[builder]
    pub async fn new(
        config: &'static str,
        telemetry: Option<Telemetry>,
        responder: Option<ResponseTemplate>,
    ) -> Self {
        Self::init_telemetry(telemetry);

        // Prevent multiple integration tests from running at the same time
        let lock = LOCK
            .get_or_init(Default::default)
            .clone()
            .lock_owned()
            .await;

        let mut listener = None;
        for _ in 0..100 {
            if let Ok(new_listener) = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 4005))) {
                listener = Some(new_listener);
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        if listener.is_none() {
            panic!("could not listen")
        }

        let subgraphs = wiremock::MockServer::builder()
            .listener(listener.expect("just checked; qed"))
            .start()
            .await;

        Mock::given(method("POST"))
            .respond_with(TracedResponder(responder.unwrap_or_else(||
                ResponseTemplate::new(200).set_body_json(json!({"data":{"topProducts":[{"name":"Table"},{"name":"Couch"},{"name":"Chair"}]}})))))
            .mount(&subgraphs)
            .await;

        let mut test_config_location = std::env::temp_dir();
        test_config_location.push("test_config.yaml");

        fs::write(&test_config_location, config).expect("could not write config");

        let router_location = PathBuf::from(env!("CARGO_BIN_EXE_router"));
        let (stdio_tx, stdio_rx) = tokio::sync::mpsc::channel(2000);
        Self {
            router: None,
            router_location,
            test_config_location,
            _lock: lock,
            stdio_tx,
            stdio_rx,
            _subgraphs: subgraphs,
        }
    }

    #[allow(dead_code)]
    pub async fn start(&mut self) {
        let mut router = Command::new(&self.router_location)
            .args([
                "--hr",
                "--config",
                &self.test_config_location.to_string_lossy(),
                "--supergraph",
                &PathBuf::from_iter(["..", "examples", "graphql", "local.graphql"])
                    .to_string_lossy(),
                "--log",
                "error,apollo_router=info",
            ])
            .stdout(Stdio::piped())
            .spawn()
            .expect("router should start");
        let reader = BufReader::new(router.stdout.take().expect("out"));
        let stdio_tx = self.stdio_tx.clone();
        // We need to read from stdout otherwise we will hang
        task::spawn(async move {
            let mut lines = reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                println!("{line}");
                let _ = stdio_tx.send(line).await;
            }
        });

        self.router = Some(router);
    }

    fn init_telemetry(telemetry: Option<Telemetry>) {
        match telemetry {
            Some(Telemetry::Jaeger) => {
                let tracer = opentelemetry_jaeger::new_agent_pipeline()
                    .with_service_name("my_app")
                    .install_simple()
                    .expect("jaeger pipeline failed");
                let telemetry = tracing_opentelemetry::layer()
                    .with_tracer(tracer)
                    .with_filter(LevelFilter::INFO);
                let subscriber = Registry::default().with(telemetry).with(
                    tracing_subscriber::fmt::Layer::default()
                        .compact()
                        .with_filter(EnvFilter::from_default_env()),
                );

                let _ = tracing::subscriber::set_global_default(subscriber);
                global::set_text_map_propagator(opentelemetry_jaeger::Propagator::new());
            }
            Some(Telemetry::Datadog) => {
                let tracer = opentelemetry_datadog::new_pipeline()
                    .with_service_name("my_app")
                    .install_simple()
                    .expect("datadog pipeline failed");
                let telemetry = tracing_opentelemetry::layer()
                    .with_tracer(tracer)
                    .with_filter(LevelFilter::INFO);
                let subscriber = Registry::default().with(telemetry).with(
                    tracing_subscriber::fmt::Layer::default()
                        .compact()
                        .with_filter(EnvFilter::from_default_env()),
                );

                let _ = tracing::subscriber::set_global_default(subscriber);
                global::set_text_map_propagator(opentelemetry_datadog::DatadogPropagator::new());
            }
            Some(Telemetry::Otlp) => {
                let tracer = opentelemetry_otlp::new_pipeline()
                    .tracing()
                    .install_simple()
                    .expect("otlp pipeline failed");
                let telemetry = tracing_opentelemetry::layer()
                    .with_tracer(tracer)
                    .with_filter(LevelFilter::INFO);
                let subscriber = Registry::default().with(telemetry).with(
                    tracing_subscriber::fmt::Layer::default()
                        .compact()
                        .with_filter(EnvFilter::from_default_env()),
                );

                let _ = tracing::subscriber::set_global_default(subscriber);
                global::set_text_map_propagator(
                    opentelemetry::sdk::propagation::TraceContextPropagator::new(),
                );
            }
            Some(Telemetry::Zipkin) => {
                let tracer = opentelemetry_zipkin::new_pipeline()
                    .with_service_name("my_app")
                    .install_simple()
                    .expect("zipkin pipeline failed");
                let telemetry = tracing_opentelemetry::layer()
                    .with_tracer(tracer)
                    .with_filter(LevelFilter::INFO);
                let subscriber = Registry::default().with(telemetry).with(
                    tracing_subscriber::fmt::Layer::default()
                        .compact()
                        .with_filter(EnvFilter::from_default_env()),
                );

                let _ = tracing::subscriber::set_global_default(subscriber);
                global::set_text_map_propagator(opentelemetry_zipkin::Propagator::new());
            }
            _ => {}
        }
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
        tokio::fs::write(&self.test_config_location, yaml)
            .await
            .expect("must be able to write config");
    }

    pub fn run_query(&self) -> impl std::future::Future<Output = (String, reqwest::Response)> {
        assert!(
            self.router.is_some(),
            "router was not started, call `router.start().await; router.assert_started().await`"
        );
        async {
            let client = reqwest::Client::new();
            let id = Uuid::new_v4().to_string();
            let span = info_span!("client_request", unit_test = id.as_str());
            let _span_guard = span.enter();

            let mut request = client
                .post("http://localhost:4000")
                .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                .header("apollographql-client-name", "custom_name")
                .header("apollographql-client-version", "1.0")
                .json(&json!({"query":"{topProducts{name}}","variables":{}}))
                .build()
                .unwrap();

            global::get_text_map_propagator(|propagator| {
                propagator.inject_context(
                    &span.context(),
                    &mut opentelemetry_http::HeaderInjector(request.headers_mut()),
                );
            });
            request.headers_mut().remove(ACCEPT);
            match client.execute(request).await {
                Ok(response) => (id, response),
                Err(err) => {
                    panic!("unable to send successful request to router, {err}")
                }
            }
        }
    }

    #[allow(dead_code)]
    pub async fn get_metrics_response(&self) -> reqwest::Result<reqwest::Response> {
        let client = reqwest::Client::new();

        let request = client
            .get("http://localhost:4000/metrics")
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
        // On windows we have to do complicated things to gracefully shutdown. One day we may get around to this, but it's not a priority.
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
    pub async fn assert_not_reloaded(&mut self) {
        self.assert_log_contains("continuing with previous configuration")
            .await;
    }

    #[allow(dead_code)]
    async fn assert_log_contains(&mut self, msg: &str) {
        let now = Instant::now();
        while now.elapsed() < Duration::from_secs(5) {
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
}

impl Drop for IntegrationTest {
    fn drop(&mut self) {
        if let Some(child) = &mut self.router {
            let _ = child.start_kill();
        }
        global::shutdown_tracer_provider();
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
