use std::fs;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::process::Child;
use std::process::Command;
use std::process::Stdio;
use std::time::Duration;

use http::header::ACCEPT;
use http::header::CONTENT_TYPE;
use jsonpath_lib::Selector;
use mime::APPLICATION_JSON;
use opentelemetry::global;
use opentelemetry::propagation::TextMapPropagator;
use opentelemetry::sdk::trace::Tracer;
use serde_json::json;
use serde_json::Value;
use tower::BoxError;
use tracing::info_span;
use tracing_core::LevelFilter;
use tracing_opentelemetry::OpenTelemetrySpanExt;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::Layer;
use tracing_subscriber::Registry;
use uuid::Uuid;

pub struct TracingTest {
    router: Child,
    test_config_location: PathBuf,
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

        let config_location =
            PathBuf::from_iter(["..", "apollo-router", "src", "testdata"]).join(config_location);
        let test_config_location = PathBuf::from_iter(["..", "target", "test_config.yaml"]);
        fs::copy(config_location, &test_config_location).expect("could not copy config");

        tracing::subscriber::set_global_default(subscriber).unwrap();
        global::set_text_map_propagator(propagator);

        let router_location = if cfg!(windows) {
            PathBuf::from_iter(["..", "target", "debug", "router.exe"])
        } else {
            PathBuf::from_iter(["..", "target", "debug", "router"])
        };

        Self {
            test_config_location: test_config_location.clone(),
            router: Command::new(router_location)
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .args([
                    "--hr",
                    "--config",
                    &test_config_location.to_string_lossy(),
                    "--supergraph",
                    &PathBuf::from_iter(["..", "examples", "graphql", "local.graphql"])
                        .to_string_lossy(),
                ])
                .spawn()
                .expect("Router should start"),
        }
    }

    #[allow(dead_code)]
    pub fn touch_config(&self) -> Result<(), BoxError> {
        let mut f = fs::OpenOptions::new()
            .append(true)
            .open(&self.test_config_location)?;
        f.write_all("#touched\n".as_bytes())?;
        Ok(())
    }

    pub async fn run_query(&self) -> (String, reqwest::Response) {
        let client = reqwest::Client::new();
        let id = Uuid::new_v4().to_string();
        let span = info_span!("client_request", unit_test = id.as_str());
        let _span_guard = span.enter();

        for _i in 0..100 {
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
                Ok(result) => {
                    tracing::debug!("got {result:?}");
                    return (id, result);
                }
                Err(e) => {
                    eprintln!("query failed: {e}");
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("unable to send successful request to router")
    }

    #[allow(dead_code)]
    pub async fn get_metrics(&self) -> reqwest::Result<String> {
        let client = reqwest::Client::new();

        let request = client
            .get("http://localhost:4000/metrics")
            .header("apollographql-client-name", "custom_name")
            .header("apollographql-client-version", "1.0")
            .build()
            .unwrap();

        let res = client.execute(request).await?;

        res.text().await
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
