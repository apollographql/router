use http::header::CONTENT_TYPE;
use http::{HeaderValue, Method, Request, Uri};
use jsonpath_lib::Selector;
use opentelemetry::global;
use opentelemetry::propagation::TextMapPropagator;
use opentelemetry::sdk::trace::Tracer;
use opentelemetry_http::HttpClient;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;
use tower::BoxError;
use tracing::info_span;
use tracing_core::LevelFilter;
use tracing_opentelemetry::OpenTelemetrySpanExt;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::{EnvFilter, Layer, Registry};
use uuid::Uuid;

pub struct TracingTest {
    router: Child,
}

impl TracingTest {
    pub fn new<P: TextMapPropagator + Send + Sync + 'static>(
        tracer: Tracer,
        propagator: P,
        config_location: &Path,
    ) -> Self {
        let telemetry = tracing_opentelemetry::layer()
            .with_tracer(tracer)
            .with_filter(LevelFilter::INFO);
        let subscriber = Registry::default().with(telemetry).with(
            tracing_subscriber::fmt::Layer::default()
                .compact()
                .with_filter(EnvFilter::from_default_env()),
        );

        tracing::subscriber::set_global_default(subscriber).unwrap();
        global::set_text_map_propagator(propagator);

        let router_location = if cfg!(windows) {
            PathBuf::from_iter(["..", "target", "debug", "router.exe"])
        } else {
            PathBuf::from_iter(["..", "target", "debug", "router"])
        };

        Self {
            router: Command::new(router_location)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .args([
                    "--hr",
                    "--config",
                    &PathBuf::from_iter(["..", "apollo-router", "src", "testdata"])
                        .join(config_location)
                        .to_string_lossy()
                        .to_string(),
                    "--supergraph",
                    &PathBuf::from_iter(["..", "examples", "graphql", "local.graphql"])
                        .to_string_lossy()
                        .to_string(),
                ])
                .spawn()
                .expect("Router should start"),
        }
    }

    pub async fn run_query(&self) -> String {
        let client = reqwest::Client::new();
        let id = Uuid::new_v4().to_string();
        let span = info_span!("client_request", unit_test = id.as_str());
        let _span_guard = span.enter();

        for _i in 0..100 {
            let mut request = Request::builder()
                .method(Method::POST)
                .header(CONTENT_TYPE, HeaderValue::from_static("application/json"))
                .header("apollographql-client-name", "custom_name")
                .header("apollographql-client-version", "1.0")
                .uri(Uri::from_static("http://localhost:4000"))
                .body(r#"{"query":"{topProducts{name}}","variables":{}}"#.into())
                .unwrap();

            global::get_text_map_propagator(|propagator| {
                propagator.inject_context(
                    &span.context(),
                    &mut opentelemetry_http::HeaderInjector(request.headers_mut()),
                )
            });
            match client.send(request).await {
                Ok(result) => {
                    tracing::debug!(
                        "got {}",
                        String::from_utf8(result.body().to_vec()).unwrap_or_default()
                    );
                    return id;
                }
                Err(e) => {
                    tracing::debug!("query failed: {}", e);
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("unable to send successful request to router")
    }
}

impl Drop for TracingTest {
    fn drop(&mut self) {
        self.router.kill().expect("router could not be halted");
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
