use std::cell::RefCell;
use std::collections::HashMap;
use std::collections::HashSet;
use std::io::IsTerminal;
use std::marker::PhantomData;

use opentelemetry::Key;
use opentelemetry::KeyValue;
use tracing::field;
use tracing_core::Event;
use tracing_core::Field;
use tracing_core::span::Id;
use tracing_core::span::Record;
use tracing_subscriber::Layer;
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::Context;

use super::config_new::ToOtelValue;
use super::dynamic_attribute::LogAttributes;
use super::formatters::EXCLUDED_ATTRIBUTES;
use super::formatters::EventFormatter;
use super::reload::IsSampled;
use crate::plugins::telemetry::config;
use crate::plugins::telemetry::config_new::logging::Format;
use crate::plugins::telemetry::config_new::logging::StdOut;
use crate::plugins::telemetry::consts::EVENT_ATTRIBUTE_OMIT_LOG;
use crate::plugins::telemetry::formatters::RateLimitFormatter;
use crate::plugins::telemetry::formatters::json::Json;
use crate::plugins::telemetry::formatters::text::Text;
use crate::plugins::telemetry::reload::LayeredTracer;
use crate::plugins::telemetry::resource::ConfigResource;

pub(crate) fn create_fmt_layer(
    config: &config::Conf,
) -> Box<dyn Layer<LayeredTracer> + Send + Sync> {
    match &config.exporters.logging.stdout {
        StdOut {
            enabled,
            format,
            tty_format,
            rate_limit,
        } if *enabled => {
            let format = match tty_format {
                Some(tty) if std::io::stdout().is_terminal() => tty,
                _ => format,
            };
            match format {
                Format::Json(format_config) => {
                    let format = Json::new(
                        config.exporters.logging.common.to_resource(),
                        format_config.clone(),
                    );
                    FmtLayer::new(RateLimitFormatter::new(format, rate_limit), std::io::stdout)
                        .boxed()
                }

                Format::Text(format_config) => {
                    let format = Text::new(
                        config.exporters.logging.common.to_resource(),
                        format_config.clone(),
                    );
                    FmtLayer::new(RateLimitFormatter::new(format, rate_limit), std::io::stdout)
                        .boxed()
                }
            }
        }
        _ => NoOpLayer.boxed(),
    }
}

struct NoOpLayer;

impl Layer<LayeredTracer> for NoOpLayer {}

pub(crate) struct FmtLayer<T, S, W> {
    fmt_event: T,
    excluded_attributes: HashSet<&'static str>,
    make_writer: W,
    _inner: PhantomData<S>,
}

impl<T, S, W> FmtLayer<T, S, W>
where
    S: tracing_core::Subscriber + for<'lookup> tracing_subscriber::registry::LookupSpan<'lookup>,
    T: EventFormatter<S>,
    W: for<'writer> MakeWriter<'writer> + 'static,
{
    pub(crate) fn new(fmt_event: T, make_writer: W) -> Self {
        Self {
            fmt_event,
            excluded_attributes: EXCLUDED_ATTRIBUTES.into(),
            make_writer,
            _inner: PhantomData,
        }
    }
}

impl<S, T, W> Layer<S> for FmtLayer<T, S, W>
where
    S: tracing_core::Subscriber + for<'lookup> tracing_subscriber::registry::LookupSpan<'lookup>,
    T: EventFormatter<S> + 'static,
    W: for<'writer> MakeWriter<'writer> + 'static,
{
    fn on_new_span(
        &self,
        attrs: &tracing_core::span::Attributes<'_>,
        id: &tracing_core::span::Id,
        ctx: Context<'_, S>,
    ) {
        if let Some(span) = ctx.span(id) {
            let mut visitor = FieldsVisitor::new(&self.excluded_attributes);
            // We're checking if it's sampled to not add both attributes in OtelData and our LogAttributes
            if !span.is_sampled() {
                attrs.record(&mut visitor);
            }
            let mut extensions = span.extensions_mut();
            if let Some(log_attrs) = extensions.get_mut::<LogAttributes>() {
                log_attrs.extend(visitor.values.into_iter().filter_map(|(k, v)| {
                    Some(KeyValue::new(Key::new(k), v.maybe_to_otel_value()?))
                }));
            } else {
                let mut fields = LogAttributes::default();
                fields.extend(visitor.values.into_iter().filter_map(|(k, v)| {
                    Some(KeyValue::new(Key::new(k), v.maybe_to_otel_value()?))
                }));
                extensions.insert(fields);
            }
        } else {
            eprintln!("FmtLayer::on_new_span: Span not found, this is a bug");
        }
    }

    fn on_record(&self, id: &Id, values: &Record<'_>, ctx: Context<'_, S>) {
        if let Some(span) = ctx.span(id) {
            let mut extensions = span.extensions_mut();
            if let Some(fields) = extensions.get_mut::<LogAttributes>() {
                let mut visitor = FieldsVisitor::new(&self.excluded_attributes);
                values.record(&mut visitor);
                fields.extend(visitor.values.into_iter().filter_map(|(k, v)| {
                    Some(KeyValue::new(Key::new(k), v.maybe_to_otel_value()?))
                }));
            } else {
                eprintln!("FmtLayer::on_record: cannot access to LogAttributes, this is a bug");
            }
        } else {
            eprintln!("FmtLayer::on_record: Span not found, this is a bug");
        }
    }

    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        let mut visitor = FieldsVisitor::new(&self.excluded_attributes);
        event.record(&mut visitor);
        if visitor.omit_from_logs {
            return;
        }

        thread_local! {
            static BUF: RefCell<String> = const { RefCell::new(String::new()) };
        }

        BUF.with(|buf| {
            let borrow = buf.try_borrow_mut();
            let mut a;
            let mut b;
            let mut buf = match borrow {
                Ok(buf) => {
                    a = buf;
                    &mut *a
                }
                _ => {
                    b = String::new();
                    &mut b
                }
            };
            if self.fmt_event.format_event(&ctx, &mut buf, event).is_ok() {
                let mut writer = self.make_writer.make_writer();
                if let Err(err) = std::io::Write::write_all(&mut writer, buf.as_bytes()) {
                    eprintln!("FmtLayer::on_event: cannot flush the logging buffer, this is a bug: {err:?}");
                }
            }
            buf.clear();
        });
    }
}

#[derive(Debug)]
pub(crate) struct FieldsVisitor<'a, 'b> {
    pub(crate) values: HashMap<&'a str, serde_json::Value>,
    excluded_attributes: &'b HashSet<&'static str>,
    omit_from_logs: bool,
}

impl<'b> FieldsVisitor<'_, 'b> {
    fn new(excluded_attributes: &'b HashSet<&'static str>) -> Self {
        Self {
            values: HashMap::with_capacity(0),
            excluded_attributes,
            omit_from_logs: false,
        }
    }
}

impl field::Visit for FieldsVisitor<'_, '_> {
    /// Visit a double precision floating point value.
    fn record_f64(&mut self, field: &Field, value: f64) {
        self.values
            .insert(field.name(), serde_json::Value::from(value));
    }

    /// Visit a signed 64-bit integer value.
    fn record_i64(&mut self, field: &Field, value: i64) {
        self.values
            .insert(field.name(), serde_json::Value::from(value));
    }

    /// Visit an unsigned 64-bit integer value.
    fn record_u64(&mut self, field: &Field, value: u64) {
        self.values
            .insert(field.name(), serde_json::Value::from(value));
    }

    /// Visit a boolean value.
    fn record_bool(&mut self, field: &Field, value: bool) {
        self.values
            .insert(field.name(), serde_json::Value::from(value));

        if field.name() == EVENT_ATTRIBUTE_OMIT_LOG && value {
            self.omit_from_logs = true;
        }
    }

    /// Visit a string value.
    fn record_str(&mut self, field: &Field, value: &str) {
        let field_name = field.name();
        if self.excluded_attributes.contains(field_name) {
            return;
        }
        self.values
            .insert(field_name, serde_json::Value::from(value));
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        let field_name = field.name();
        if self.excluded_attributes.contains(field_name) {
            return;
        }
        match field_name {
            name if name.starts_with("r#") => {
                self.values
                    .insert(&name[2..], serde_json::Value::from(format!("{value:?}")));
            }
            name => {
                self.values
                    .insert(name, serde_json::Value::from(format!("{value:?}")));
            }
        };
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;
    use std::sync::Arc;

    use apollo_compiler::ast::OperationType;
    use apollo_compiler::name;
    use apollo_federation::connectors::ConnectId;
    use apollo_federation::connectors::ConnectSpec;
    use apollo_federation::connectors::Connector;
    use apollo_federation::connectors::HttpJsonTransport;
    use apollo_federation::connectors::JSONSelection;
    use apollo_federation::connectors::ProblemLocation;
    use apollo_federation::connectors::SourceName;
    use apollo_federation::connectors::StringTemplate;
    use apollo_federation::connectors::runtime::http_json_transport::HttpRequest;
    use apollo_federation::connectors::runtime::http_json_transport::HttpResponse;
    use apollo_federation::connectors::runtime::http_json_transport::TransportRequest;
    use apollo_federation::connectors::runtime::http_json_transport::TransportResponse;
    use apollo_federation::connectors::runtime::key::ResponseKey;
    use apollo_federation::connectors::runtime::mapping::Problem;
    use apollo_federation::connectors::runtime::responses::MappedResponse;
    use http::HeaderValue;
    use http::header::CONTENT_LENGTH;
    use parking_lot::Mutex;
    use parking_lot::MutexGuard;
    use tests::events::EventLevel;
    use tracing::error;
    use tracing::info;
    use tracing::info_span;
    use tracing_subscriber::fmt;
    use tracing_subscriber::layer::SubscriberExt;

    use super::*;
    use crate::graphql;
    use crate::plugins::telemetry::config_new::events;
    use crate::plugins::telemetry::config_new::events::log_event;
    use crate::plugins::telemetry::config_new::logging::JsonFormat;
    use crate::plugins::telemetry::config_new::logging::RateLimit;
    use crate::plugins::telemetry::config_new::logging::TextFormat;
    use crate::plugins::telemetry::config_new::router::events::RouterResponseBodyExtensionType;
    use crate::plugins::telemetry::dynamic_attribute::SpanDynAttribute;
    use crate::plugins::telemetry::otel;
    use crate::services::connector::request_service::Request;
    use crate::services::connector::request_service::Response;
    use crate::services::router;
    use crate::services::router::body;
    use crate::services::subgraph;
    use crate::services::supergraph;

    const EVENT_CONFIGURATION: &str = r#"
router:
  # Standard events
  request: info
  response: info
  error: info

  # Custom events
  my.request_event:
    message: "my event message"
    level: info
    on: request
    attributes:
      http.request.body.size: true
    # Only log when the x-log-request header is `log`
    condition:
      eq:
        - "log"
        - request_header: "x-log-request"
  my.response_event:
    message: "my response event message"
    level: info
    on: response
    attributes:
      http.response.body.size: true
    # Only log when the x-log-request header is `log`
    condition:
      eq:
        - "log"
        - response_header: "x-log-request"
supergraph:
  # Standard events
  request: info
  response: warn
  error: info

  # Custom events
  my.request.event:
    message: "my event message"
    level: info
    on: request
    # Only log when the x-log-request header is `log`
    condition:
      eq:
        - "log"
        - request_header: "x-log-request"
  my.response_event:
    message: "my response event message"
    level: warn
    on: response
    condition:
      eq:
        - "log"
        - response_header: "x-log-request"
subgraph:
  # Standard events
  request: info
  response: warn
  error: error

  # Custom events
  my.subgraph.request.event:
    message: "my event message"
    level: info
    on: request
  my.subgraph.response.event:
    message: "my response event message"
    level: error
    on: response
    attributes:
      subgraph.name: true
      response_status:
        subgraph_response_status: code
      "my.custom.attribute":
        subgraph_response_data: "$.*"
        default: "missing"

connector:
  # Standard events cannot be tested, because the test does not call the service that emits them

  # Custom events
  my.connector.request.event:
    message: "my request event message"
    level: info
    on: request
    attributes:
      subgraph.name: true
      connector_source:
        connector_source: name
      http_method:
        connector_http_method: true
      url_template:
        connector_url_template: true
      mapping_problems:
        connector_request_mapping_problems: problems
      mapping_problems_count:
        connector_request_mapping_problems: count
  my.connector.response.event:
    message: "my response event message"
    level: error
    on: response
    attributes:
      subgraph.name: true
      connector_source:
        connector_source: name
      http_method:
        connector_http_method: true
      url_template:
        connector_url_template: true
      response_status:
        connector_http_response_status: code
      mapping_problems:
        connector_response_mapping_problems: problems
      mapping_problems_count:
        connector_response_mapping_problems: count"#;

    #[derive(Default, Clone)]
    struct LogBuffer(Arc<Mutex<Vec<u8>>>);
    impl<'a> MakeWriter<'a> for LogBuffer {
        type Writer = Guard<'a>;

        fn make_writer(&'a self) -> Self::Writer {
            Guard(self.0.lock())
        }
    }

    struct Guard<'a>(MutexGuard<'a, Vec<u8>>);
    impl std::io::Write for Guard<'_> {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.write(buf)
        }

        fn flush(&mut self) -> std::io::Result<()> {
            self.0.flush()
        }
    }

    impl std::fmt::Display for LogBuffer {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            let content = String::from_utf8(self.0.lock().clone()).map_err(|_e| std::fmt::Error)?;

            write!(f, "{content}")
        }
    }

    fn generate_simple_span() {
        let test_span = info_span!(
            "test",
            first = "one",
            apollo_private.should_not_display = "this should be skipped"
        );
        test_span.set_span_dyn_attribute("another".into(), 2.into());
        test_span.set_span_dyn_attribute("custom_dyn".into(), "test".into());
        let _enter = test_span.enter();
        info!(event_attr = "foo", "Hello from test");
    }

    fn generate_nested_spans() {
        let test_span = info_span!(
            "test",
            first = "one",
            apollo_private.should_not_display = "this should be skipped"
        );
        test_span.set_span_dyn_attribute("another".into(), 2.into());
        test_span.set_span_dyn_attribute("custom_dyn".into(), "test".into());
        let _enter = test_span.enter();
        {
            let nested_test_span = info_span!(
                "nested_test",
                two = "two",
                apollo_private.is_private = "this should be skipped"
            );
            let _enter = nested_test_span.enter();

            nested_test_span.set_span_dyn_attributes([
                KeyValue::new("inner", -42_i64),
                KeyValue::new("graphql.operation.kind", "Subscription"),
            ]);

            error!(http.method = "GET", "Hello from nested test");
        }
        info!(event_attr = "foo", "Hello from test");
    }

    #[tokio::test]
    async fn test_text_logging_attributes() {
        let buff = LogBuffer::default();
        let format = Text::default();
        let fmt_layer = FmtLayer::new(format, buff.clone()).boxed();

        ::tracing::subscriber::with_default(
            fmt::Subscriber::new().with(fmt_layer),
            generate_simple_span,
        );
        insta::assert_snapshot!(buff);
    }

    #[tokio::test]
    async fn test_text_logging_attributes_nested_spans() {
        let buff = LogBuffer::default();
        let format = Text::default();
        let fmt_layer = FmtLayer::new(format, buff.clone()).boxed();

        ::tracing::subscriber::with_default(
            fmt::Subscriber::new().with(fmt_layer),
            generate_nested_spans,
        );

        insta::assert_snapshot!(buff.to_string());
    }

    #[tokio::test]
    async fn test_json_logging_attributes() {
        let buff = LogBuffer::default();
        let format = Json::default();
        let fmt_layer = FmtLayer::new(format, buff.clone()).boxed();

        ::tracing::subscriber::with_default(
            fmt::Subscriber::new().with(fmt_layer),
            generate_simple_span,
        );
        insta::assert_snapshot!(buff);
    }

    #[tokio::test]
    async fn test_json_logging_attributes_nested_spans() {
        let buff = LogBuffer::default();
        let format = Json::default();
        let fmt_layer = FmtLayer::new(format, buff.clone()).boxed();

        ::tracing::subscriber::with_default(
            fmt::Subscriber::new().with(fmt_layer),
            generate_nested_spans,
        );

        insta::assert_snapshot!(buff.to_string());
    }

    #[tokio::test]
    async fn test_json_logging_without_span_list() {
        let buff = LogBuffer::default();
        let json_format = JsonFormat {
            display_span_list: false,
            display_current_span: false,
            display_resource: false,
            ..Default::default()
        };
        let format = Json::new(Default::default(), json_format);
        let fmt_layer = FmtLayer::new(format, buff.clone()).boxed();

        ::tracing::subscriber::with_default(
            fmt::Subscriber::new().with(fmt_layer),
            generate_nested_spans,
        );

        insta::assert_snapshot!(buff.to_string());
    }

    #[tokio::test]
    async fn test_text_logging_without_span_list() {
        let buff = LogBuffer::default();
        let text_format = TextFormat {
            display_span_list: false,
            display_current_span: false,
            display_resource: false,
            ansi_escape_codes: false,
            ..Default::default()
        };
        let format = Text::new(Default::default(), text_format);
        let fmt_layer = FmtLayer::new(format, buff.clone()).boxed();

        ::tracing::subscriber::with_default(
            fmt::Subscriber::new().with(fmt_layer),
            generate_nested_spans,
        );

        insta::assert_snapshot!(buff.to_string());
    }

    #[tokio::test]
    async fn test_text_logging_with_custom_events() {
        let buff = LogBuffer::default();
        let text_format = TextFormat {
            ansi_escape_codes: false,
            ..Default::default()
        };
        let format = Text::new(Default::default(), text_format);
        let fmt_layer = FmtLayer::new(format, buff.clone()).boxed();

        ::tracing::subscriber::with_default(
            fmt::Subscriber::new()
                .with(otel::layer().force_sampling())
                .with(fmt_layer),
            || {
                let test_span = info_span!(
                    "test",
                    first = "one",
                    apollo_private.should_not_display = "this should be skipped"
                );
                test_span.set_span_dyn_attribute("another".into(), 2.into());
                test_span.set_span_dyn_attribute("custom_dyn".into(), "test".into());
                let _enter = test_span.enter();
                let attributes = vec![
                    KeyValue::new(
                        Key::from_static_str("http.response.body.size"),
                        opentelemetry::Value::String("125".to_string().into()),
                    ),
                    KeyValue::new(
                        Key::from_static_str("http.response.body"),
                        opentelemetry::Value::String(r#"{"foo": "bar"}"#.to_string().into()),
                    ),
                ];
                log_event(
                    EventLevel::Info,
                    "my_custom_event",
                    attributes,
                    "my message",
                );

                error!(http.method = "GET", "Hello from test");
            },
        );

        insta::assert_snapshot!(buff.to_string());
    }

    #[tokio::test]
    async fn test_json_logging_with_custom_events() {
        let buff = LogBuffer::default();
        let text_format = JsonFormat {
            display_span_list: false,
            display_current_span: false,
            display_resource: false,
            ..Default::default()
        };
        let format = Json::new(Default::default(), text_format);
        let fmt_layer = FmtLayer::new(format, buff.clone()).boxed();

        ::tracing::subscriber::with_default(
            fmt::Subscriber::new()
                .with(otel::layer().force_sampling())
                .with(fmt_layer),
            || {
                let test_span = info_span!(
                    "test",
                    first = "one",
                    apollo_private.should_not_display = "this should be skipped"
                );
                test_span.set_span_dyn_attribute("another".into(), 2.into());
                test_span.set_span_dyn_attribute("custom_dyn".into(), "test".into());
                let _enter = test_span.enter();
                let attributes = vec![
                    KeyValue::new(
                        Key::from_static_str("http.response.body.size"),
                        opentelemetry::Value::String("125".to_string().into()),
                    ),
                    KeyValue::new(
                        Key::from_static_str("http.response.body"),
                        opentelemetry::Value::String(r#"{"foo": "bar"}"#.to_string().into()),
                    ),
                ];
                log_event(
                    EventLevel::Info,
                    "my_custom_event",
                    attributes,
                    "my message",
                );

                error!(http.method = "GET", "Hello from test");
            },
        );

        insta::assert_snapshot!(buff.to_string());
    }

    #[tokio::test]
    async fn test_json_logging_with_custom_events_with_instrumented() {
        let buff = LogBuffer::default();
        let text_format = JsonFormat {
            display_span_list: false,
            display_current_span: false,
            display_resource: false,
            ..Default::default()
        };
        let format = Json::new(Default::default(), text_format);
        let fmt_layer = FmtLayer::new(format, buff.clone()).boxed();

        let event_config: events::Events = serde_yaml::from_str(EVENT_CONFIGURATION).unwrap();

        ::tracing::subscriber::with_default(
            fmt::Subscriber::new()
                .with(otel::layer().force_sampling())
                .with(fmt_layer),
            move || {
                let test_span = info_span!(
                    "test",
                    first = "one",
                    apollo_private.should_not_display = "this should be skipped"
                );
                test_span.set_span_dyn_attribute("another".into(), 2.into());
                test_span.set_span_dyn_attribute("custom_dyn".into(), "test".into());
                let _enter = test_span.enter();

                let attributes = vec![
                    KeyValue::new(
                        Key::from_static_str("http.response.body.size"),
                        opentelemetry::Value::I64(125),
                    ),
                    KeyValue::new(
                        Key::from_static_str("http.response.body"),
                        opentelemetry::Value::String(r#"{"foo": "bar"}"#.to_string().into()),
                    ),
                ];
                log_event(
                    EventLevel::Info,
                    "my_custom_event",
                    attributes,
                    "my message",
                );

                error!(http.method = "GET", "Hello from test");

                let mut router_events = event_config.new_router_events();
                let router_req = router::Request::fake_builder()
                    .header(CONTENT_LENGTH, "0")
                    .header("custom-header", "val1")
                    .header("x-log-request", HeaderValue::from_static("log"))
                    .build()
                    .unwrap();
                router_events.on_request(&router_req);

                let router_resp = router::Response::fake_builder()
                    .header("custom-header", "val1")
                    .header(CONTENT_LENGTH, "25")
                    .header("x-log-request", HeaderValue::from_static("log"))
                    .data(serde_json_bytes::json!({"data": "res"}))
                    .build()
                    .expect("expecting valid response");
                router_events.on_response(&router_resp);

                let mut supergraph_events = event_config.new_supergraph_events();
                let supergraph_req = supergraph::Request::fake_builder()
                    .query("query { foo }")
                    .header("x-log-request", HeaderValue::from_static("log"))
                    .build()
                    .unwrap();
                supergraph_events.on_request(&supergraph_req);

                let supergraph_resp = supergraph::Response::fake_builder()
                    .header("custom-header", "val1")
                    .header("x-log-request", HeaderValue::from_static("log"))
                    .data(serde_json::json!({"data": "res"}).to_string())
                    .build()
                    .expect("expecting valid response");
                supergraph_events.on_response(&supergraph_resp);

                let mut subgraph_events = event_config.new_subgraph_events();
                let mut subgraph_req = http::Request::new(
                    graphql::Request::fake_builder()
                        .query("query { foo }")
                        .build(),
                );
                subgraph_req
                    .headers_mut()
                    .insert("x-log-request", HeaderValue::from_static("log"));

                let subgraph_req = subgraph::Request::fake_builder()
                    .subgraph_name("subgraph")
                    .subgraph_request(subgraph_req)
                    .build();
                subgraph_events.on_request(&subgraph_req);

                let subgraph_resp = subgraph::Response::fake2_builder()
                    .header("custom-header", "val1")
                    .header("x-log-request", HeaderValue::from_static("log"))
                    .data(serde_json::json!({"products": [{"id": 1234, "name": "first_name"}, {"id": 567, "name": "second_name"}]}))
                    .subgraph_name("subgraph")
                    .build()
                    .expect("expecting valid response");
                subgraph_events.on_response(&subgraph_resp);

                let mut subgraph_events = event_config.new_subgraph_events();
                let mut subgraph_req = http::Request::new(
                    graphql::Request::fake_builder()
                        .query("query { foo }")
                        .build(),
                );
                subgraph_req
                    .headers_mut()
                    .insert("x-log-request", HeaderValue::from_static("log"));

                let subgraph_req = subgraph::Request::fake_builder()
                    .subgraph_name("subgraph_bis")
                    .subgraph_request(subgraph_req)
                    .build();
                subgraph_events.on_request(&subgraph_req);

                let subgraph_resp = subgraph::Response::fake2_builder()
                    .header("custom-header", "val1")
                    .header("x-log-request", HeaderValue::from_static("log"))
                    .data(serde_json::json!({"products": [{"id": 1234, "name": "first_name"}, {"id": 567, "name": "second_name"}], "other": {"foo": "bar"}}))
                    .subgraph_name("subgraph_bis")
                    .build()
                    .expect("expecting valid response");
                subgraph_events.on_response(&subgraph_resp);

                let context = crate::Context::default();
                let mut http_request = http::Request::builder().body("".into()).unwrap();
                http_request
                    .headers_mut()
                    .insert("x-log-request", HeaderValue::from_static("log"));
                let transport_request = TransportRequest::Http(HttpRequest {
                    inner: http_request,
                    debug: Default::default(),
                });
                let connector = Arc::new(Connector {
                    id: ConnectId::new(
                        "connector_subgraph".into(),
                        Some(SourceName::cast("source")),
                        name!(Query),
                        name!(users),
                        None,
                        0,
                        name!(BaseType),
                    ),
                    transport: HttpJsonTransport {
                        connect_template: StringTemplate::from_str("/test").unwrap(),
                        ..Default::default()
                    },
                    selection: JSONSelection::empty(),
                    config: None,
                    max_requests: None,
                    entity_resolver: None,
                    spec: ConnectSpec::V0_1,
                    batch_settings: None,
                    request_headers: Default::default(),
                    response_headers: Default::default(),
                    request_variable_keys: Default::default(),
                    response_variable_keys: Default::default(),
                    error_settings: Default::default(),
                    label: "label".into(),
                });
                let response_key = ResponseKey::RootField {
                    name: "hello".to_string(),
                    operation_type: OperationType::Query,
                    output_type: name!("BaseType"),
                    inputs: Default::default(),
                    selection: Arc::new(JSONSelection::parse("$.data").unwrap()),
                };
                let connector_request = Request {
                    context: context.clone(),
                    connector: connector.clone(),
                    transport_request,
                    key: response_key.clone(),
                    mapping_problems: vec![
                        Problem {
                            count: 1,
                            message: "error message".to_string(),
                            path: "@.id".to_string(),
                            location: ProblemLocation::Selection,
                        },
                        Problem {
                            count: 2,
                            message: "warn message".to_string(),
                            path: "@.id".to_string(),
                            location: ProblemLocation::Selection,
                        },
                        Problem {
                            count: 3,
                            message: "info message".to_string(),
                            path: "@.id".to_string(),
                            location: ProblemLocation::Selection,
                        },
                    ],
                    supergraph_request: Default::default(),
                };
                let mut connector_events = event_config.new_connector_events();
                connector_events.on_request(&connector_request);

                let connector_response = Response {
                    transport_result: Ok(TransportResponse::Http(HttpResponse {
                        inner: http::Response::builder()
                            .status(200)
                            .header("x-log-response", HeaderValue::from_static("log"))
                            .body(body::empty())
                            .expect("expecting valid response")
                            .into_parts()
                            .0,
                    })),
                    mapped_response: MappedResponse::Data {
                        data: serde_json::json!({})
                            .try_into()
                            .expect("expecting valid JSON"),
                        key: response_key,
                        problems: vec![
                            Problem {
                                count: 1,
                                message: "error message".to_string(),
                                path: "@.id".to_string(),
                                location: ProblemLocation::Selection,
                            },
                            Problem {
                                count: 2,
                                message: "warn message".to_string(),
                                path: "@.id".to_string(),
                                location: ProblemLocation::Selection,
                            },
                            Problem {
                                count: 3,
                                message: "info message".to_string(),
                                path: "@.id".to_string(),
                                location: ProblemLocation::Selection,
                            },
                        ],
                    },
                };
                connector_events.on_response(&connector_response);
            },
        );

        insta::assert_snapshot!(buff.to_string());
    }

    #[tokio::test]
    async fn test_json_logging_deduplicates_attributes() {
        let buff = LogBuffer::default();
        let text_format = JsonFormat {
            display_span_list: false,
            display_current_span: false,
            display_resource: false,
            ..Default::default()
        };
        let format = Json::new(Default::default(), text_format);
        let fmt_layer = FmtLayer::new(
            RateLimitFormatter::new(format, &RateLimit::default()),
            buff.clone(),
        )
        .boxed();

        let event_config: events::Events = serde_yaml::from_str(
            r#"
subgraph:
  request: info
  response: warn
  error: error
  event.with.duplicate.attribute:
    message: "this event has a duplicate attribute"
    level: error
    on: response
    attributes:
      subgraph.name: true
      static: foo # This shows up twice without attribute deduplication
        "#,
        )
        .unwrap();

        ::tracing::subscriber::with_default(
            fmt::Subscriber::new()
                .with(otel::layer().force_sampling())
                .with(fmt_layer),
            move || {
                let test_span = info_span!("test");
                let _enter = test_span.enter();

                let mut router_events = event_config.new_router_events();
                let mut supergraph_events = event_config.new_supergraph_events();
                let mut subgraph_events = event_config.new_subgraph_events();

                // In: Router -> Supergraph -> Subgraphs
                let router_req = router::Request::fake_builder().build().unwrap();
                router_events.on_request(&router_req);

                let supergraph_req = supergraph::Request::fake_builder()
                    .query("query { foo }")
                    .build()
                    .unwrap();
                supergraph_events.on_request(&supergraph_req);

                let subgraph_req_1 = subgraph::Request::fake_builder()
                    .subgraph_name("subgraph")
                    .subgraph_request(http::Request::new(
                        graphql::Request::fake_builder()
                            .query("query { foo }")
                            .build(),
                    ))
                    .build();
                subgraph_events.on_request(&subgraph_req_1);

                let subgraph_req_2 = subgraph::Request::fake_builder()
                    .subgraph_name("subgraph_bis")
                    .subgraph_request(http::Request::new(
                        graphql::Request::fake_builder()
                            .query("query { foo }")
                            .build(),
                    ))
                    .build();
                subgraph_events.on_request(&subgraph_req_2);

                // Out: Subgraphs -> Supergraph -> Router
                let subgraph_resp_1 = subgraph::Response::fake2_builder()
                    .data(serde_json::json!({"products": [{"id": 1234, "name": "first_name"}, {"id": 567, "name": "second_name"}]}))
                    .build()
                    .expect("expecting valid response");
                subgraph_events.on_response(&subgraph_resp_1);

                let subgraph_resp_2 = subgraph::Response::fake2_builder()
                    .data(serde_json::json!({"products": [{"id": 1234, "name": "first_name"}, {"id": 567, "name": "second_name"}], "other": {"foo": "bar"}}))
                    .build()
                    .expect("expecting valid response");
                subgraph_events.on_response(&subgraph_resp_2);

                let supergraph_resp = supergraph::Response::fake_builder()
                    .data(serde_json::json!({"data": "res"}).to_string())
                    .build()
                    .expect("expecting valid response");
                supergraph_events.on_response(&supergraph_resp);

                let router_resp = router::Response::fake_builder()
                    .data(serde_json_bytes::json!({"data": "res"}))
                    .build()
                    .expect("expecting valid response");
                router_events.on_response(&router_resp);
            },
        );

        insta::assert_snapshot!(buff.to_string());
    }

    #[tokio::test]
    async fn test_text_logging_with_custom_events_with_instrumented() {
        let buff = LogBuffer::default();
        let text_format = TextFormat {
            display_span_list: true,
            display_current_span: false,
            display_resource: false,
            ansi_escape_codes: false,
            ..Default::default()
        };
        let format = Text::new(Default::default(), text_format);
        let fmt_layer = FmtLayer::new(format, buff.clone()).boxed();

        let event_config: events::Events = serde_yaml::from_str(EVENT_CONFIGURATION).unwrap();

        ::tracing::subscriber::with_default(
            fmt::Subscriber::new()
                .with(otel::layer().force_sampling())
                .with(fmt_layer),
            move || {
                let test_span = info_span!(
                    "test",
                    first = "one",
                    apollo_private.should_not_display = "this should be skipped"
                );
                test_span.set_span_dyn_attribute("another".into(), 2.into());
                test_span.set_span_dyn_attribute("custom_dyn".into(), "test".into());
                let _enter = test_span.enter();

                let attributes = vec![
                    KeyValue::new(
                        Key::from_static_str("http.response.body.size"),
                        opentelemetry::Value::String("125".to_string().into()),
                    ),
                    KeyValue::new(
                        Key::from_static_str("http.response.body"),
                        opentelemetry::Value::String(r#"{"foo": "bar"}"#.to_string().into()),
                    ),
                ];
                log_event(
                    EventLevel::Info,
                    "my_custom_event",
                    attributes,
                    "my message",
                );

                error!(http.method = "GET", "Hello from test");

                let mut router_events = event_config.new_router_events();
                let router_req = router::Request::fake_builder()
                    .header(CONTENT_LENGTH, "0")
                    .header("custom-header", "val1")
                    .header("x-log-request", HeaderValue::from_static("log"))
                    .build()
                    .unwrap();
                router_events.on_request(&router_req);
                let ctx = crate::Context::new();
                ctx.extensions().with_lock(|ext| {
                    ext.insert(RouterResponseBodyExtensionType(
                        r#"{"data": {"data": "res"}}"#.to_string(),
                    ));
                });
                let router_resp = router::Response::fake_builder()
                    .header("custom-header", "val1")
                    .header(CONTENT_LENGTH, "25")
                    .header("x-log-request", HeaderValue::from_static("log"))
                    .data(serde_json_bytes::json!({"data": "res"}))
                    .context(ctx)
                    .build()
                    .expect("expecting valid response");
                router_events.on_response(&router_resp);

                let mut supergraph_events = event_config.new_supergraph_events();
                let supergraph_req = supergraph::Request::fake_builder()
                    .query("query { foo }")
                    .header("x-log-request", HeaderValue::from_static("log"))
                    .build()
                    .unwrap();
                supergraph_events.on_request(&supergraph_req);

                let supergraph_resp = supergraph::Response::fake_builder()
                    .header("custom-header", "val1")
                    .header("x-log-request", HeaderValue::from_static("log"))
                    .data(serde_json::json!({"data": "res"}).to_string())
                    .build()
                    .expect("expecting valid response");
                supergraph_events.on_response(&supergraph_resp);

                let mut subgraph_events = event_config.new_subgraph_events();
                let mut subgraph_req = http::Request::new(
                    graphql::Request::fake_builder()
                        .query("query { foo }")
                        .build(),
                );
                subgraph_req
                    .headers_mut()
                    .insert("x-log-request", HeaderValue::from_static("log"));

                let subgraph_req = subgraph::Request::fake_builder()
                    .subgraph_name("subgraph")
                    .subgraph_request(subgraph_req)
                    .build();
                subgraph_events.on_request(&subgraph_req);

                let subgraph_resp = subgraph::Response::fake2_builder()
                    .header("custom-header", "val1")
                    .header("x-log-request", HeaderValue::from_static("log"))
                    .data(serde_json::json!({"products": [{"id": 1234, "name": "first_name"}, {"id": 567, "name": "second_name"}]}))
                    .subgraph_name("subgraph")
                    .build()
                    .expect("expecting valid response");
                subgraph_events.on_response(&subgraph_resp);

                let mut subgraph_events = event_config.new_subgraph_events();
                let mut subgraph_req = http::Request::new(
                    graphql::Request::fake_builder()
                        .query("query { foo }")
                        .build(),
                );
                subgraph_req
                    .headers_mut()
                    .insert("x-log-request", HeaderValue::from_static("log"));

                let subgraph_req = subgraph::Request::fake_builder()
                    .subgraph_name("subgraph_bis")
                    .subgraph_request(subgraph_req)
                    .build();
                subgraph_events.on_request(&subgraph_req);

                let subgraph_resp = subgraph::Response::fake2_builder()
                    .header("custom-header", "val1")
                    .header("x-log-request", HeaderValue::from_static("log"))
                    .data(serde_json::json!({"products": [{"id": 1234, "name": "first_name"}, {"id": 567, "name": "second_name"}], "other": {"foo": "bar"}}))
                    .subgraph_name("subgraph_bis")
                    .build()
                    .expect("expecting valid response");
                subgraph_events.on_response(&subgraph_resp);

                let context = crate::Context::default();
                let mut http_request = http::Request::builder().body("".into()).unwrap();
                http_request
                    .headers_mut()
                    .insert("x-log-request", HeaderValue::from_static("log"));
                let transport_request = TransportRequest::Http(HttpRequest {
                    inner: http_request,
                    debug: Default::default(),
                });
                let connector = Arc::new(Connector {
                    id: ConnectId::new(
                        "connector_subgraph".into(),
                        Some(SourceName::cast("source")),
                        name!(Query),
                        name!(users),
                        None,
                        0,
                        name!(BaseType),
                    ),
                    transport: HttpJsonTransport {
                        connect_template: StringTemplate::from_str("/test").unwrap(),
                        ..Default::default()
                    },
                    selection: JSONSelection::empty(),
                    config: None,
                    max_requests: None,
                    entity_resolver: None,
                    spec: ConnectSpec::V0_1,
                    batch_settings: None,
                    request_headers: Default::default(),
                    response_headers: Default::default(),
                    request_variable_keys: Default::default(),
                    response_variable_keys: Default::default(),
                    error_settings: Default::default(),
                    label: "label".into(),
                });
                let response_key = ResponseKey::RootField {
                    name: "hello".to_string(),
                    operation_type: apollo_compiler::ast::OperationType::Query,
                    output_type: apollo_compiler::name!("BaseType"),
                    inputs: Default::default(),
                    selection: Arc::new(JSONSelection::parse("$.data").unwrap()),
                };
                let connector_request = Request {
                    context: context.clone(),
                    connector: connector.clone(),
                    transport_request,
                    key: response_key.clone(),
                    mapping_problems: vec![
                        Problem {
                            count: 1,
                            message: "error message".to_string(),
                            path: "@.id".to_string(),
                            location: ProblemLocation::Selection,
                        },
                        Problem {
                            count: 2,
                            message: "warn message".to_string(),
                            path: "@.id".to_string(),
                            location: ProblemLocation::Selection,
                        },
                        Problem {
                            count: 3,
                            message: "info message".to_string(),
                            path: "@.id".to_string(),
                            location: ProblemLocation::Selection,
                        },
                    ],
                    supergraph_request: Default::default(),
                };
                let mut connector_events = event_config.new_connector_events();
                connector_events.on_request(&connector_request);

                let connector_response = Response {
                    transport_result: Ok(TransportResponse::Http(HttpResponse {
                        inner: http::Response::builder()
                            .status(200)
                            .header("x-log-response", HeaderValue::from_static("log"))
                            .body(body::empty())
                            .expect("expecting valid response")
                            .into_parts()
                            .0,
                    })),
                    mapped_response: MappedResponse::Data {
                        data: serde_json::json!({})
                            .try_into()
                            .expect("expecting valid JSON"),
                        key: response_key,
                        problems: vec![
                            Problem {
                                count: 1,
                                message: "error message".to_string(),
                                path: "@.id".to_string(),
                                location: ProblemLocation::Selection,
                            },
                            Problem {
                                count: 2,
                                message: "warn message".to_string(),
                                path: "@.id".to_string(),
                                location: ProblemLocation::Selection,
                            },
                            Problem {
                                count: 3,
                                message: "info message".to_string(),
                                path: "@.id".to_string(),
                                location: ProblemLocation::Selection,
                            },
                        ],
                    },
                };
                connector_events.on_response(&connector_response);
            },
        );

        insta::assert_snapshot!(buff.to_string());
    }

    // TODO add test using on_request/on_reponse/on_error
}
