//! # OpenTelemetry Datadog Exporter
//!
//! An OpenTelemetry datadog exporter implementation
//!
//! See the [Datadog Docs](https://docs.datadoghq.com/agent/) for information on how to run the datadog-agent
//!
//! ## Quirks
//!
//! There are currently some incompatibilities between Datadog and OpenTelemetry, and this manifests
//! as minor quirks to this exporter.
//!
//! Firstly Datadog uses operation_name to describe what OpenTracing would call a component.
//! Or to put it another way, in OpenTracing the operation / span name's are relatively
//! granular and might be used to identify a specific endpoint. In datadog, however, they
//! are less granular - it is expected in Datadog that a service will have single
//! primary span name that is the root of all traces within that service, with an additional piece of
//! metadata called resource_name providing granularity. See [here](https://docs.datadoghq.com/tracing/guide/configuring-primary-operation/)
//!
//! The Datadog Golang API takes the approach of using a `resource.name` OpenTelemetry attribute to set the
//! resource_name. See [here](https://github.com/DataDog/dd-trace-go/blob/ecb0b805ef25b00888a2fb62d465a5aa95e7301e/ddtrace/opentracer/tracer.go#L10)
//!
//! Unfortunately, this breaks compatibility with other OpenTelemetry exporters which expect
//! a more granular operation name - as per the OpenTracing specification.
//!
//! This exporter therefore takes a different approach of naming the span with the name of the
//! tracing provider, and using the span name to set the resource_name. This should in most cases
//! lead to the behaviour that users expect.
//!
//! Datadog additionally has a span_type string that alters the rendering of the spans in the web UI.
//! This can be set as the `span.type` OpenTelemetry span attribute.
//!
//! For standard values see [here](https://github.com/DataDog/dd-trace-go/blob/ecb0b805ef25b00888a2fb62d465a5aa95e7301e/ddtrace/ext/app_types.go#L31).
//!
//! If the default mapping is not fit for your use case, you may change some of them by providing [`FieldMappingFn`]s in pipeline.
//!
//! ## Performance
//!
//! For optimal performance, a batch exporter is recommended as the simple exporter will export
//! each span synchronously on drop. You can enable the [`rt-tokio`], [`rt-tokio-current-thread`]
//! or [`rt-async-std`] features and specify a runtime on the pipeline to have a batch exporter
//! configured for you automatically.
//!
//! ```toml
//! [dependencies]
//! opentelemetry = { version = "*", features = ["rt-tokio"] }
//! opentelemetry-datadog = "*"
//! ```
//!
//! ```no_run
//! # fn main() -> Result<(), opentelemetry::trace::TraceError> {
//! let tracer = opentelemetry_datadog::new_pipeline()
//!     .install_batch(opentelemetry_sdk::runtime::Tokio)?;
//! # Ok(())
//! # }
//! ```
//!
//! [`rt-tokio`]: https://tokio.rs
//! [`rt-tokio-current-thread`]: https://tokio.rs
//! [`rt-async-std`]: https://async.rs
//!
//! ## Bring your own http client
//!
//! Users can choose appropriate http clients to align with their runtime.
//!
//! Based on the feature enabled. The default http client will be different. If user doesn't specific
//! features or enabled `reqwest-blocking-client` feature. The blocking reqwest http client will be used as
//! default client. If `reqwest-client` feature is enabled. The async reqwest http client will be used. If
//! `surf-client` feature is enabled. The surf http client will be used.
//!
//! Note that async http clients may need specific runtime otherwise it will panic. User should make
//! sure the http client is running in appropriate runime.
//!
//! Users can always use their own http clients by implementing `HttpClient` trait.
//!
//! ## Kitchen Sink Full Configuration
//!
//! Example showing how to override all configuration options. See the
//! [`DatadogPipelineBuilder`] docs for details of each option.
//!
//! [`DatadogPipelineBuilder`]: struct.DatadogPipelineBuilder.html
//!
//! ```no_run
//! use opentelemetry::{KeyValue, trace::Tracer};
//! use opentelemetry_sdk::{trace::{self, RandomIdGenerator, Sampler}, Resource};
//! use opentelemetry_sdk::export::trace::ExportResult;
//! use opentelemetry::global::shutdown_tracer_provider;
//! use opentelemetry_datadog::{new_pipeline, ApiVersion, Error};
//! use opentelemetry_http::{HttpClient, HttpError};
//! use async_trait::async_trait;
//! use bytes::Bytes;
//! use futures_util::io::AsyncReadExt as _;
//! use http::{Request, Response};
//! use std::convert::TryInto as _;
//!
//! // `reqwest` and `surf` are supported through features, if you prefer an
//! // alternate http client you can add support by implementing `HttpClient` as
//! // shown here.
//! #[derive(Debug)]
//! struct IsahcClient(isahc::HttpClient);
//!
//! #[async_trait]
//! impl HttpClient for IsahcClient {
//!     async fn send(&self, request: Request<Vec<u8>>) -> Result<Response<Bytes>, HttpError> {
//!         let mut response = self.0.send_async(request).await?;
//!         let status = response.status();
//!         let mut bytes = Vec::with_capacity(response.body().len().unwrap_or(0).try_into()?);
//!         isahc::AsyncReadResponseExt::copy_to(&mut response, &mut bytes).await?;
//!
//!         Ok(Response::builder()
//!             .status(response.status())
//!             .body(bytes.into())?)
//!     }
//! }
//!
//! fn main() -> Result<(), opentelemetry::trace::TraceError> {
//!     let tracer = new_pipeline()
//!         .with_service_name("my_app")
//!         .with_api_version(ApiVersion::Version05)
//!         .with_agent_endpoint("http://localhost:8126")
//!         .with_trace_config(
//!             trace::config()
//!                 .with_sampler(Sampler::AlwaysOn)
//!                 .with_id_generator(RandomIdGenerator::default())
//!         )
//!         .install_batch(opentelemetry_sdk::runtime::Tokio)?;
//!
//!     tracer.in_span("doing_work", |cx| {
//!         // Traced app logic here...
//!     });
//!
//!     shutdown_tracer_provider(); // sending remaining spans before exit
//!
//!     Ok(())
//! }
//! ```

mod exporter;

#[allow(unused_imports)]
pub use exporter::new_pipeline;
#[allow(unused_imports)]
pub use exporter::ApiVersion;
#[allow(unused_imports)]
pub use exporter::DatadogExporter;
#[allow(unused_imports)]
pub use exporter::DatadogPipelineBuilder;
#[allow(unused_imports)]
pub use exporter::Error;
#[allow(unused_imports)]
pub use exporter::FieldMappingFn;
#[allow(unused_imports)]
pub use exporter::ModelConfig;
#[allow(unused_imports)]
pub use propagator::DatadogPropagator;
#[allow(unused_imports)]
pub use propagator::DatadogTraceState;
#[allow(unused_imports)]
pub use propagator::DatadogTraceStateBuilder;

pub(crate) mod propagator {
    use once_cell::sync::Lazy;
    use opentelemetry::propagation::text_map_propagator::FieldIter;
    use opentelemetry::propagation::Extractor;
    use opentelemetry::propagation::Injector;
    use opentelemetry::propagation::TextMapPropagator;
    use opentelemetry::trace::SpanContext;
    use opentelemetry::trace::SpanId;
    use opentelemetry::trace::TraceContextExt;
    use opentelemetry::trace::TraceFlags;
    use opentelemetry::trace::TraceId;
    use opentelemetry::trace::TraceState;
    use opentelemetry::Context;

    const DATADOG_TRACE_ID_HEADER: &str = "x-datadog-trace-id";
    const DATADOG_PARENT_ID_HEADER: &str = "x-datadog-parent-id";
    const DATADOG_SAMPLING_PRIORITY_HEADER: &str = "x-datadog-sampling-priority";

    const TRACE_FLAG_DEFERRED: TraceFlags = TraceFlags::new(0x02);
    const TRACE_STATE_PRIORITY_SAMPLING: &str = "psr";
    pub(crate) const TRACE_STATE_MEASURE: &str = "m";
    pub(crate) const TRACE_STATE_TRUE_VALUE: &str = "1";
    pub(crate) const TRACE_STATE_FALSE_VALUE: &str = "0";

    static DATADOG_HEADER_FIELDS: Lazy<[String; 3]> = Lazy::new(|| {
        [
            DATADOG_TRACE_ID_HEADER.to_string(),
            DATADOG_PARENT_ID_HEADER.to_string(),
            DATADOG_SAMPLING_PRIORITY_HEADER.to_string(),
        ]
    });

    #[derive(Default)]
    pub struct DatadogTraceStateBuilder {
        priority_sampling: bool,
        measuring: bool,
    }

    fn boolean_to_trace_state_flag(value: bool) -> &'static str {
        if value {
            TRACE_STATE_TRUE_VALUE
        } else {
            TRACE_STATE_FALSE_VALUE
        }
    }

    fn trace_flag_to_boolean(value: &str) -> bool {
        value == TRACE_STATE_TRUE_VALUE
    }

    #[allow(clippy::needless_update)]
    impl DatadogTraceStateBuilder {
        pub fn with_priority_sampling(self, enabled: bool) -> Self {
            Self {
                priority_sampling: enabled,
                ..self
            }
        }

        pub fn with_measuring(self, enabled: bool) -> Self {
            Self {
                measuring: enabled,
                ..self
            }
        }

        pub fn build(self) -> TraceState {
            let values = [
                (
                    TRACE_STATE_MEASURE,
                    boolean_to_trace_state_flag(self.measuring),
                ),
                (
                    TRACE_STATE_PRIORITY_SAMPLING,
                    boolean_to_trace_state_flag(self.priority_sampling),
                ),
            ];

            TraceState::from_key_value(values).unwrap_or_default()
        }
    }

    pub trait DatadogTraceState {
        fn with_measuring(&self, enabled: bool) -> TraceState;

        fn measuring_enabled(&self) -> bool;

        fn with_priority_sampling(&self, enabled: bool) -> TraceState;

        fn priority_sampling_enabled(&self) -> bool;
    }

    impl DatadogTraceState for TraceState {
        fn with_measuring(&self, enabled: bool) -> TraceState {
            self.insert(TRACE_STATE_MEASURE, boolean_to_trace_state_flag(enabled))
                .unwrap_or_else(|_err| self.clone())
        }

        fn measuring_enabled(&self) -> bool {
            self.get(TRACE_STATE_MEASURE)
                .map(trace_flag_to_boolean)
                .unwrap_or_default()
        }

        fn with_priority_sampling(&self, enabled: bool) -> TraceState {
            self.insert(
                TRACE_STATE_PRIORITY_SAMPLING,
                boolean_to_trace_state_flag(enabled),
            )
            .unwrap_or_else(|_err| self.clone())
        }

        fn priority_sampling_enabled(&self) -> bool {
            self.get(TRACE_STATE_PRIORITY_SAMPLING)
                .map(trace_flag_to_boolean)
                .unwrap_or_default()
        }
    }

    enum SamplingPriority {
        UserReject = -1,
        AutoReject = 0,
        AutoKeep = 1,
        UserKeep = 2,
    }

    #[derive(Debug)]
    enum ExtractError {
        TraceId,
        SpanId,
        SamplingPriority,
    }

    /// Extracts and injects `SpanContext`s into `Extractor`s or `Injector`s using Datadog's header format.
    ///
    /// The Datadog header format does not have an explicit spec, but can be divined from the client libraries,
    /// such as [dd-trace-go]
    ///
    /// ## Example
    ///
    /// ```
    /// use opentelemetry::global;
    /// use opentelemetry_datadog::DatadogPropagator;
    ///
    /// global::set_text_map_propagator(DatadogPropagator::default());
    /// ```
    ///
    /// [dd-trace-go]: https://github.com/DataDog/dd-trace-go/blob/v1.28.0/ddtrace/tracer/textmap.go#L293
    #[derive(Clone, Debug, Default)]
    pub struct DatadogPropagator {
        _private: (),
    }

    fn create_trace_state_and_flags(trace_flags: TraceFlags) -> (TraceState, TraceFlags) {
        if trace_flags & TRACE_FLAG_DEFERRED == TRACE_FLAG_DEFERRED {
            (TraceState::default(), trace_flags)
        } else {
            (
                DatadogTraceStateBuilder::default()
                    .with_priority_sampling(trace_flags.is_sampled())
                    .build(),
                TraceFlags::SAMPLED,
            )
        }
    }

    impl DatadogPropagator {
        /// Creates a new `DatadogPropagator`.
        pub fn new() -> Self {
            DatadogPropagator::default()
        }

        fn extract_trace_id(&self, trace_id: &str) -> Result<TraceId, ExtractError> {
            trace_id
                .parse::<u64>()
                .map(|id| TraceId::from(id as u128))
                .map_err(|_| ExtractError::TraceId)
        }

        fn extract_span_id(&self, span_id: &str) -> Result<SpanId, ExtractError> {
            span_id
                .parse::<u64>()
                .map(SpanId::from)
                .map_err(|_| ExtractError::SpanId)
        }

        fn extract_sampling_priority(
            &self,
            sampling_priority: &str,
        ) -> Result<SamplingPriority, ExtractError> {
            let i = sampling_priority
                .parse::<i32>()
                .map_err(|_| ExtractError::SamplingPriority)?;

            match i {
                -1 => Ok(SamplingPriority::UserReject),
                0 => Ok(SamplingPriority::AutoReject),
                1 => Ok(SamplingPriority::AutoKeep),
                2 => Ok(SamplingPriority::UserKeep),
                _ => Err(ExtractError::SamplingPriority),
            }
        }

        fn extract_span_context(
            &self,
            extractor: &dyn Extractor,
        ) -> Result<SpanContext, ExtractError> {
            let trace_id =
                self.extract_trace_id(extractor.get(DATADOG_TRACE_ID_HEADER).unwrap_or(""))?;
            // If we have a trace_id but can't get the parent span, we default it to invalid instead of completely erroring
            // out so that the rest of the spans aren't completely lost
            let span_id = self
                .extract_span_id(extractor.get(DATADOG_PARENT_ID_HEADER).unwrap_or(""))
                .unwrap_or(SpanId::INVALID);
            let sampling_priority = self.extract_sampling_priority(
                extractor
                    .get(DATADOG_SAMPLING_PRIORITY_HEADER)
                    .unwrap_or(""),
            );
            let sampled = match sampling_priority {
                Ok(SamplingPriority::UserReject) | Ok(SamplingPriority::AutoReject) => {
                    TraceFlags::default()
                }
                Ok(SamplingPriority::UserKeep) | Ok(SamplingPriority::AutoKeep) => {
                    TraceFlags::SAMPLED
                }
                // Treat the sampling as DEFERRED instead of erroring on extracting the span context
                Err(_) => TRACE_FLAG_DEFERRED,
            };

            let (trace_state, trace_flags) = create_trace_state_and_flags(sampled);

            Ok(SpanContext::new(
                trace_id,
                span_id,
                trace_flags,
                true,
                trace_state,
            ))
        }
    }

    fn get_sampling_priority(span_context: &SpanContext) -> SamplingPriority {
        if span_context.trace_state().priority_sampling_enabled() {
            SamplingPriority::AutoKeep
        } else {
            SamplingPriority::AutoReject
        }
    }

    impl TextMapPropagator for DatadogPropagator {
        fn inject_context(&self, cx: &Context, injector: &mut dyn Injector) {
            let span = cx.span();
            let span_context = span.span_context();
            if span_context.is_valid() {
                injector.set(
                    DATADOG_TRACE_ID_HEADER,
                    (u128::from_be_bytes(span_context.trace_id().to_bytes()) as u64).to_string(),
                );
                injector.set(
                    DATADOG_PARENT_ID_HEADER,
                    u64::from_be_bytes(span_context.span_id().to_bytes()).to_string(),
                );

                if span_context.trace_flags() & TRACE_FLAG_DEFERRED != TRACE_FLAG_DEFERRED {
                    let sampling_priority = get_sampling_priority(span_context);

                    injector.set(
                        DATADOG_SAMPLING_PRIORITY_HEADER,
                        (sampling_priority as i32).to_string(),
                    );
                }
            }
        }

        fn extract_with_context(&self, cx: &Context, extractor: &dyn Extractor) -> Context {
            self.extract_span_context(extractor)
                .map(|sc| cx.with_remote_span_context(sc))
                .unwrap_or_else(|_| cx.clone())
        }

        fn fields(&self) -> FieldIter<'_> {
            FieldIter::new(DATADOG_HEADER_FIELDS.as_ref())
        }
    }

    #[cfg(test)]
    mod tests {
        use std::collections::HashMap;

        use opentelemetry::trace::TraceState;
        use opentelemetry_sdk::testing::trace::TestSpan;

        use super::*;

        #[rustfmt::skip]
        fn extract_test_data() -> Vec<(Vec<(&'static str, &'static str)>, SpanContext)> {
            return vec![
                (vec![], SpanContext::empty_context()),
                (vec![(DATADOG_SAMPLING_PRIORITY_HEADER, "0")], SpanContext::empty_context()),
                (vec![(DATADOG_TRACE_ID_HEADER, "garbage")], SpanContext::empty_context()),
                (vec![(DATADOG_TRACE_ID_HEADER, "1234"), (DATADOG_PARENT_ID_HEADER, "garbage")], SpanContext::new(TraceId::from_u128(1234), SpanId::INVALID, TRACE_FLAG_DEFERRED, true, TraceState::default())),
                (vec![(DATADOG_TRACE_ID_HEADER, "1234"), (DATADOG_PARENT_ID_HEADER, "12")], SpanContext::new(TraceId::from_u128(1234), SpanId::from_u64(12), TRACE_FLAG_DEFERRED, true, TraceState::default())),
                (vec![(DATADOG_TRACE_ID_HEADER, "1234"), (DATADOG_PARENT_ID_HEADER, "12"), (DATADOG_SAMPLING_PRIORITY_HEADER, "0")], SpanContext::new(TraceId::from_u128(1234), SpanId::from_u64(12), TraceFlags::SAMPLED, true, DatadogTraceStateBuilder::default().with_priority_sampling(false).build())),
                (vec![(DATADOG_TRACE_ID_HEADER, "1234"), (DATADOG_PARENT_ID_HEADER, "12"), (DATADOG_SAMPLING_PRIORITY_HEADER, "1")], SpanContext::new(TraceId::from_u128(1234), SpanId::from_u64(12), TraceFlags::SAMPLED, true, DatadogTraceStateBuilder::default().with_priority_sampling(true).build())),
            ];
        }

        #[rustfmt::skip]
        fn inject_test_data() -> Vec<(Vec<(&'static str, &'static str)>, SpanContext)> {
            return vec![
                (vec![], SpanContext::empty_context()),
                (vec![], SpanContext::new(TraceId::INVALID, SpanId::INVALID, TRACE_FLAG_DEFERRED, true, TraceState::default())),
                (vec![], SpanContext::new(TraceId::from_hex("1234").unwrap(), SpanId::INVALID, TRACE_FLAG_DEFERRED, true, TraceState::default())),
                (vec![], SpanContext::new(TraceId::from_hex("1234").unwrap(), SpanId::INVALID, TraceFlags::SAMPLED, true, TraceState::default())),
                (vec![(DATADOG_TRACE_ID_HEADER, "1234"), (DATADOG_PARENT_ID_HEADER, "12")], SpanContext::new(TraceId::from_u128(1234), SpanId::from_u64(12), TRACE_FLAG_DEFERRED, true, TraceState::default())),
                (vec![(DATADOG_TRACE_ID_HEADER, "1234"), (DATADOG_PARENT_ID_HEADER, "12"), (DATADOG_SAMPLING_PRIORITY_HEADER, "0")], SpanContext::new(TraceId::from_u128(1234), SpanId::from_u64(12), TraceFlags::SAMPLED, true, DatadogTraceStateBuilder::default().with_priority_sampling(false).build())),
                (vec![(DATADOG_TRACE_ID_HEADER, "1234"), (DATADOG_PARENT_ID_HEADER, "12"), (DATADOG_SAMPLING_PRIORITY_HEADER, "1")], SpanContext::new(TraceId::from_u128(1234), SpanId::from_u64(12), TraceFlags::SAMPLED, true, DatadogTraceStateBuilder::default().with_priority_sampling(true).build())),
            ];
        }

        #[test]
        fn test_extract() {
            for (header_list, expected) in extract_test_data() {
                let map: HashMap<String, String> = header_list
                    .into_iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect();

                let propagator = DatadogPropagator::default();
                let context = propagator.extract(&map);
                assert_eq!(context.span().span_context(), &expected);
            }
        }

        #[test]
        fn test_extract_empty() {
            let map: HashMap<String, String> = HashMap::new();
            let propagator = DatadogPropagator::default();
            let context = propagator.extract(&map);
            assert_eq!(context.span().span_context(), &SpanContext::empty_context())
        }

        #[test]
        fn test_extract_with_empty_remote_context() {
            let map: HashMap<String, String> = HashMap::new();
            let propagator = DatadogPropagator::default();
            let context = propagator.extract_with_context(&Context::new(), &map);
            assert!(!context.has_active_span())
        }

        #[test]
        fn test_inject() {
            let propagator = DatadogPropagator::default();
            for (header_values, span_context) in inject_test_data() {
                let mut injector: HashMap<String, String> = HashMap::new();
                propagator.inject_context(
                    &Context::current_with_span(TestSpan(span_context)),
                    &mut injector,
                );

                if !header_values.is_empty() {
                    for (k, v) in header_values.into_iter() {
                        let injected_value: Option<&String> = injector.get(k);
                        assert_eq!(injected_value, Some(&v.to_string()));
                        injector.remove(k);
                    }
                }
                assert!(injector.is_empty());
            }
        }
    }
}
