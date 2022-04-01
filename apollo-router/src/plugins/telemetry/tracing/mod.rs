pub mod apollo;
pub mod apollo_telemetry;
pub mod datadog;
pub mod jaeger;
#[cfg(any(feature = "otlp-grpc", feature = "otlp-http"))]
pub mod otlp;
pub mod zipkin;

#[cfg(test)]
mod test {
    use tracing::{info_span, Instrument};

    pub async fn run_query() {
        let span = info_span!("client_request");
        let client = reqwest_middleware::ClientBuilder::new(reqwest::Client::new())
            .with(reqwest_tracing::TracingMiddleware)
            .build();

        client
            .post("http://localhost:4000")
            .header("test", "Boo")
            .body(r#"{"query":"query {\n  topProducts {\n    name\n  }\n}","variables":{}}"#)
            .send()
            .instrument(span)
            .await
            .unwrap();
    }
}
