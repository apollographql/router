use std::fmt::Display;
use std::task::{Context, Poll};

use crate::layer::ConfigurableLayer;
use crate::{register_layer, SubgraphRequest};
use http::HeaderValue;
use opentelemetry::trace::TraceContextExt;
use schemars::JsonSchema;
use serde::Deserialize;
use tower::{BoxError, Layer, Service};
use tracing::Span;
use tracing_opentelemetry::OpenTelemetrySpanExt;

register_layer!("opentracing", OpenTracingLayer);

#[derive(Clone, JsonSchema, Deserialize, Debug)]
#[serde(rename_all = "snake_case")]
enum PropagationFormat {
    Jaeger,
    ZipkinB3,
}

impl Display for PropagationFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PropagationFormat::Jaeger => write!(f, "jaeger"),
            PropagationFormat::ZipkinB3 => write!(f, "zipkin_b3"),
        }
    }
}

#[derive(Clone, JsonSchema, Deserialize)]
struct OpenTracingConfig {
    format: PropagationFormat,
}

struct OpenTracingLayer {
    format: PropagationFormat,
}

impl ConfigurableLayer for OpenTracingLayer {
    type Config = OpenTracingConfig;
    fn new(configuration: Self::Config) -> Result<Self, BoxError> {
        Ok(Self {
            format: configuration.format,
        })
    }
}

impl<S> Layer<S> for OpenTracingLayer {
    type Service = OpenTracingService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        OpenTracingService {
            inner,
            format: self.format.clone(),
        }
    }
}

struct OpenTracingService<S> {
    inner: S,
    format: PropagationFormat,
}

impl<S> Service<SubgraphRequest> for OpenTracingService<S>
where
    S: Service<SubgraphRequest>,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = S::Future;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, mut req: SubgraphRequest) -> Self::Future {
        let span_context = Span::current().context();
        let span_ref = span_context.span();
        let current_span_ctx = span_ref.span_context();
        let (trace_id, span_id, trace_flags) = (
            current_span_ctx.trace_id(),
            current_span_ctx.span_id(),
            current_span_ctx.trace_flags(),
        );

        match self.format {
            PropagationFormat::Jaeger => {
                req.http_request.headers_mut().insert(
                    "uber-trace-id",
                    HeaderValue::from_str(&format!(
                        "{}:{}:{}:{}",
                        trace_id,
                        span_id,
                        span_id,
                        trace_flags.to_u8()
                    ))
                    .unwrap(),
                );
            }
            PropagationFormat::ZipkinB3 => {
                req.http_request.headers_mut().insert(
                    "X-B3-TraceId",
                    HeaderValue::from_str(&trace_id.to_string()).unwrap(),
                );
                req.http_request.headers_mut().insert(
                    "X-B3-SpanId",
                    HeaderValue::from_str(&span_id.to_string()).unwrap(),
                );
                req.http_request.headers_mut().insert(
                    "X-B3-ParentSpanId",
                    HeaderValue::from_str(&span_id.to_string()).unwrap(),
                );
                req.http_request.headers_mut().insert(
                    "X-B3-Sampled",
                    HeaderValue::from_static(
                        current_span_ctx.is_sampled().then(|| "1").unwrap_or("0"),
                    ),
                );
            }
        }

        self.inner.call(req)
    }
}
