use crate::plugins::telemetry::config::Trace;
use opentelemetry::sdk::trace::Builder;
use tower::BoxError;

pub mod apollo;
pub mod apollo_telemetry;
pub mod datadog;
pub mod jaeger;
pub mod otlp;
pub mod zipkin;

pub trait TracingConfigurator {
    fn apply(&self, builder: Builder, trace_config: &Trace) -> Result<Builder, BoxError>;
}

#[cfg(test)]
mod test {
    use http::{Method, Request, Uri};
    use opentelemetry::global;
    use opentelemetry_http::HttpClient;
    use tracing::{info_span, Instrument};
    use tracing_opentelemetry::OpenTelemetrySpanExt;
    pub async fn run_query() {
        let span = info_span!("client_request");
        let client = reqwest::Client::new();

        let mut request = Request::builder()
            .method(Method::POST)
            .uri(Uri::from_static("http://localhost:4000"))
            .body(r#"{"query":"query {\n  topProducts {\n    name\n  }\n}","variables":{}}"#.into())
            .unwrap();

        global::get_text_map_propagator(|propagator| {
            propagator.inject_context(
                &span.context(),
                &mut opentelemetry_http::HeaderInjector(request.headers_mut()),
            )
        });

        client.send(request).instrument(span).await.unwrap();
    }
}
