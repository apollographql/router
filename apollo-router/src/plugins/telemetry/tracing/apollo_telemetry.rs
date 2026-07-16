use std::borrow::Cow;
use std::collections::HashMap;
use std::collections::HashSet;
use std::io::Cursor;
use std::num::NonZeroUsize;
use std::time::SystemTime;

use base64::Engine as _;
use base64::prelude::BASE64_STANDARD;
use derivative::Derivative;
use lru::LruCache;
use opentelemetry::Key;
use opentelemetry::Value;
use opentelemetry::trace::SpanId;
use opentelemetry::trace::SpanKind;
use opentelemetry::trace::Status;
use opentelemetry::trace::TraceId;
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::error::OTelSdkResult;
use opentelemetry_sdk::trace::SpanData;
use opentelemetry_sdk::trace::SpanExporter;
use parking_lot::Mutex;
use prost::Message;
use serde_json::Value as JSONValue;
use thiserror::Error;
use tracing::Level;
use url::Url;

use crate::json_ext::Path;
use crate::plugins::telemetry::APOLLO_PRIVATE_QUERY_ALIASES;
use crate::plugins::telemetry::APOLLO_PRIVATE_QUERY_DEPTH;
use crate::plugins::telemetry::APOLLO_PRIVATE_QUERY_HEIGHT;
use crate::plugins::telemetry::APOLLO_PRIVATE_QUERY_ROOT_FIELDS;
use crate::plugins::telemetry::BoxError;
use crate::plugins::telemetry::LruSizeInstrument;
use crate::plugins::telemetry::apollo::ErrorConfiguration;
use crate::plugins::telemetry::apollo::ErrorRedactionPolicy;
use crate::plugins::telemetry::apollo::ErrorsConfiguration;
use crate::plugins::telemetry::apollo_exporter::proto;
use crate::plugins::telemetry::apollo_otlp_exporter::ApolloOtlpExporter;
use crate::plugins::telemetry::config_new::cost::APOLLO_PRIVATE_COST_ACTUAL;
use crate::plugins::telemetry::config_new::cost::APOLLO_PRIVATE_COST_ESTIMATED;
use crate::plugins::telemetry::config_new::cost::APOLLO_PRIVATE_COST_RESULT;
use crate::plugins::telemetry::config_new::cost::APOLLO_PRIVATE_COST_STRATEGY;
use crate::plugins::telemetry::consts::EVENT_ATTRIBUTE_OMIT_LOG;
use crate::plugins::telemetry::consts::FIELD_EXCEPTION_MESSAGE;
use crate::plugins::telemetry::otlp::Protocol;
use crate::plugins::telemetry::tracing::BatchProcessorConfig;
use crate::query_planner::subscription::SUBSCRIPTION_EVENT_SPAN_NAME;
use crate::services::connector_service::APOLLO_CONNECTOR_DETAIL;
use crate::services::connector_service::APOLLO_CONNECTOR_FIELD_ALIAS;
use crate::services::connector_service::APOLLO_CONNECTOR_FIELD_NAME;
use crate::services::connector_service::APOLLO_CONNECTOR_FIELD_RETURN_TYPE;
use crate::services::connector_service::APOLLO_CONNECTOR_SELECTION;
use crate::services::connector_service::APOLLO_CONNECTOR_SOURCE_DETAIL;
use crate::services::connector_service::APOLLO_CONNECTOR_SOURCE_NAME;
use crate::services::connector_service::APOLLO_CONNECTOR_TYPE;

pub(crate) const APOLLO_PRIVATE_REQUEST: Key = Key::from_static_str("apollo_private.request");
pub(crate) const APOLLO_PRIVATE_DURATION_NS: &str = "apollo_private.duration_ns";
pub(crate) const APOLLO_PRIVATE_DURATION_NS_KEY: Key =
    Key::from_static_str(APOLLO_PRIVATE_DURATION_NS);
const APOLLO_PRIVATE_SENT_TIME_OFFSET: Key =
    Key::from_static_str("apollo_private.sent_time_offset");
const APOLLO_PRIVATE_GRAPHQL_VARIABLES: Key =
    Key::from_static_str("apollo_private.graphql.variables");
const APOLLO_PRIVATE_HTTP_REQUEST_HEADERS: Key =
    Key::from_static_str("apollo_private.http.request_headers");
const APOLLO_PRIVATE_HTTP_RESPONSE_HEADERS: Key =
    Key::from_static_str("apollo_private.http.response_headers");
pub(crate) const APOLLO_PRIVATE_OPERATION_SIGNATURE: Key =
    Key::from_static_str("apollo_private.operation_signature");
pub(crate) const APOLLO_PRIVATE_FTV1: Key = Key::from_static_str("apollo_private.ftv1");
const PATH: Key = Key::from_static_str("graphql.path");
const SUBGRAPH_NAME: Key = Key::from_static_str("apollo.subgraph.name");
pub(crate) const CLIENT_NAME_KEY: Key = Key::from_static_str("client.name");
pub(crate) const CLIENT_VERSION_KEY: Key = Key::from_static_str("client.version");
const DEPENDS: Key = Key::from_static_str("graphql.depends");
const LABEL: Key = Key::from_static_str("graphql.label");
const CONDITION: Key = Key::from_static_str("graphql.condition");
const OPERATION_NAME: Key = Key::from_static_str("graphql.operation.name");
const OPERATION_TYPE: Key = Key::from_static_str("graphql.operation.type");
pub(crate) const OPERATION_SUBTYPE: Key = Key::from_static_str("apollo_private.operation.subtype");
const EXT_TRACE_ID: Key = Key::from_static_str("trace_id");
pub(crate) const GRAPHQL_ERROR_EXT_CODE: &str = "graphql.error.extensions.code";
pub(crate) const GRAPHQL_ERROR_PATH: &str = "graphql.error.path";

/// The set of attributes to include when sending to the Apollo Reports protocol.
const REPORTS_INCLUDE_ATTRS: [Key; 26] = [
    APOLLO_PRIVATE_REQUEST,
    APOLLO_PRIVATE_DURATION_NS_KEY,
    APOLLO_PRIVATE_SENT_TIME_OFFSET,
    APOLLO_PRIVATE_GRAPHQL_VARIABLES,
    APOLLO_PRIVATE_HTTP_REQUEST_HEADERS,
    APOLLO_PRIVATE_HTTP_RESPONSE_HEADERS,
    APOLLO_PRIVATE_OPERATION_SIGNATURE,
    APOLLO_PRIVATE_FTV1,
    APOLLO_PRIVATE_COST_STRATEGY,
    APOLLO_PRIVATE_COST_RESULT,
    APOLLO_PRIVATE_COST_ESTIMATED,
    APOLLO_PRIVATE_COST_ACTUAL,
    APOLLO_PRIVATE_QUERY_ALIASES,
    APOLLO_PRIVATE_QUERY_DEPTH,
    APOLLO_PRIVATE_QUERY_HEIGHT,
    APOLLO_PRIVATE_QUERY_ROOT_FIELDS,
    PATH,
    SUBGRAPH_NAME,
    CLIENT_NAME_KEY,
    CLIENT_VERSION_KEY,
    DEPENDS,
    LABEL,
    CONDITION,
    OPERATION_NAME,
    OPERATION_TYPE,
    Key::from_static_str(opentelemetry_semantic_conventions::trace::HTTP_REQUEST_METHOD),
];

/// Additional attributes to include when sending to the OTLP protocol.
const OTLP_EXT_INCLUDE_ATTRS: [Key; 13] = [
    OPERATION_SUBTYPE,
    EXT_TRACE_ID,
    Key::from_static_str(opentelemetry_semantic_conventions::attribute::HTTP_REQUEST_BODY_SIZE),
    Key::from_static_str(opentelemetry_semantic_conventions::attribute::HTTP_RESPONSE_BODY_SIZE),
    Key::from_static_str(opentelemetry_semantic_conventions::trace::HTTP_RESPONSE_STATUS_CODE),
    APOLLO_CONNECTOR_TYPE,
    APOLLO_CONNECTOR_DETAIL,
    APOLLO_CONNECTOR_SELECTION,
    APOLLO_CONNECTOR_FIELD_NAME,
    APOLLO_CONNECTOR_FIELD_ALIAS,
    APOLLO_CONNECTOR_FIELD_RETURN_TYPE,
    APOLLO_CONNECTOR_SOURCE_NAME,
    APOLLO_CONNECTOR_SOURCE_DETAIL,
];

/// Attributes on events to include when sending to the OTLP protocol.
const OTLP_EXT_INCLUDE_EVENT_ATTRS: [Key; 3] = [
    Key::from_static_str(GRAPHQL_ERROR_EXT_CODE),
    Key::from_static_str(FIELD_EXCEPTION_MESSAGE),
    Key::from_static_str(GRAPHQL_ERROR_PATH),
];

pub(crate) fn emit_error_event(error_code: &str, error_message: &str, error_path: Option<Path>) {
    if let Some(path) = error_path {
        tracing::event!(
            Level::ERROR,
            { GRAPHQL_ERROR_EXT_CODE } = error_code,
            { FIELD_EXCEPTION_MESSAGE } = error_message,
            { GRAPHQL_ERROR_PATH } = path.to_string().as_str(),
            { EVENT_ATTRIBUTE_OMIT_LOG } = true,
            error_message
        );
    } else {
        tracing::event!(
            Level::ERROR,
            { GRAPHQL_ERROR_EXT_CODE } = error_code,
            { FIELD_EXCEPTION_MESSAGE } = error_message,
            { EVENT_ATTRIBUTE_OMIT_LOG } = true,
            error_message
        );
    }
}

#[derive(Error, Debug)]
pub(crate) enum Error {
    #[error("trace parsing failed")]
    TraceParsingFailed,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct LightSpanEventData {
    pub(crate) timestamp: SystemTime,
    pub(crate) name: Cow<'static, str>,
    pub(crate) attributes: HashMap<Key, Value>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct LightSpanData {
    pub(crate) trace_id: TraceId,
    pub(crate) span_id: SpanId,
    pub(crate) parent_span_id: SpanId,
    pub(crate) span_kind: SpanKind,
    pub(crate) name: Cow<'static, str>,
    pub(crate) start_time: SystemTime,
    pub(crate) end_time: SystemTime,
    pub(crate) attributes: HashMap<Key, Value>,
    pub(crate) status: Status,
    pub(crate) droppped_attribute_count: u32,
    pub(crate) events: Vec<LightSpanEventData>,
}

impl LightSpanData {
    /// Convert from a full Span into a lighter more memory-efficient span for caching purposes.
    /// Only attributes/events whose keys are in the allowlists are retained; everything else is
    /// dropped so we never forward un-vetted attributes to Apollo.
    fn from_span_data(
        value: SpanData,
        include_attr_names: &HashSet<Key>,
        include_attr_event_names: &HashSet<Key>,
    ) -> Self {
        let filtered_attributes = value
            .attributes
            .into_iter()
            .filter_map(|kv| {
                if include_attr_names.contains(&kv.key) {
                    Some((kv.key, kv.value))
                } else {
                    None
                }
            })
            .collect();

        let filtered_events = value
            .events
            .into_iter()
            .map(|event| LightSpanEventData {
                timestamp: event.timestamp,
                name: event.name,
                attributes: event
                    .attributes
                    .into_iter()
                    .filter_map(|kv| {
                        if include_attr_event_names.contains(&kv.key) {
                            Some((kv.key, kv.value))
                        } else {
                            None
                        }
                    })
                    .collect(),
            })
            .filter(|event| !event.attributes.is_empty())
            .collect();

        Self {
            trace_id: value.span_context.trace_id(),
            span_id: value.span_context.span_id(),
            parent_span_id: value.parent_span_id,
            span_kind: value.span_kind,
            name: value.name,
            start_time: value.start_time,
            end_time: value.end_time,
            attributes: filtered_attributes,
            status: value.status,
            droppped_attribute_count: value.dropped_attributes_count,
            events: filtered_events,
        }
    }
}

/// A [`SpanExporter`] that writes to [`Reporter`].
///
/// [`SpanExporter`]: super::SpanExporter
/// [`Reporter`]: crate::plugins::telemetry::Reporter
#[derive(Derivative)]
#[derivative(Debug)]
pub(crate) struct Exporter {
    span_cache: Mutex<SpanCache>,
    /// An externally updateable gauge for "apollo.router.exporter.span.lru.size".
    span_lru_size_instrument: LruSizeInstrument,
    #[derivative(Debug = "ignore")]
    otlp_exporter: ApolloOtlpExporter,
    include_attr_names: HashSet<Key>,
    include_attr_event_names: HashSet<Key>,
}

#[buildstructor::buildstructor]
impl Exporter {
    #[builder]
    pub(crate) fn new<'a>(
        endpoint: &'a Url,
        tracing_protocol: &'a Protocol,
        apollo_key: &'a str,
        apollo_graph_ref: &'a str,
        schema_id: &'a str,
        buffer_size: NonZeroUsize,
        errors_configuration: &'a ErrorsConfiguration,
        batch_processor_config: &'a BatchProcessorConfig,
    ) -> Result<Self, BoxError> {
        tracing::info!("configuring Apollo tracing: {}", batch_processor_config);

        let span_lru_size_instrument =
            LruSizeInstrument::new("apollo.router.exporter.span.lru.size");

        let span_cache = SpanCache {
            spans_by_parent_id: LruCache::new(buffer_size),
        };

        Ok(Self {
            span_cache: Mutex::new(span_cache),
            span_lru_size_instrument,
            otlp_exporter: ApolloOtlpExporter::new(
                endpoint,
                tracing_protocol,
                batch_processor_config,
                apollo_key,
                apollo_graph_ref,
                schema_id,
                errors_configuration,
            )?,
            include_attr_names: HashSet::from_iter(
                [&REPORTS_INCLUDE_ATTRS[..], &OTLP_EXT_INCLUDE_ATTRS[..]].concat(),
            ),
            include_attr_event_names: HashSet::from(OTLP_EXT_INCLUDE_EVENT_ATTRS),
        })
    }
}

impl SpanExporter for Exporter {
    /// Export spans to apollo telemetry
    async fn export(&self, batch: Vec<SpanData>) -> OTelSdkResult {
        // Exporting to apollo means that we must have complete trace as the entire trace must be built.
        // We do what we can, and if there are any traces that are not complete then we keep them for the next export event.
        // We may get spans that simply don't complete. These need to be cleaned up after a period. It's the price of using ftv1.
        let mut grouped_traces: Vec<Vec<LightSpanData>> = Vec::new();

        {
            let mut span_cache = self.span_cache.lock();
            for span in batch {
                if span.name == SUBSCRIPTION_EVENT_SPAN_NAME
                    || span
                        .attributes
                        .iter()
                        .any(|kv| kv.key == APOLLO_PRIVATE_REQUEST)
                {
                    let root_span: LightSpanData = LightSpanData::from_span_data(
                        span,
                        &self.include_attr_names,
                        &self.include_attr_event_names,
                    );
                    grouped_traces.push(span_cache.pop_spans_for_tree(root_span));
                } else if span.parent_span_id != SpanId::INVALID {
                    // Not a root span, we may need it later so stash it.
                    span_cache.insert(LightSpanData::from_span_data(
                        span,
                        &self.include_attr_names,
                        &self.include_attr_event_names,
                    ));
                }
            }

            // Note this won't be correct anymore if there is any way outside of `.export()`
            // to affect the size of the cache.
            self.span_lru_size_instrument
                .update(span_cache.len() as u64);
        }

        // ftv1 decode/re-encode is CPU-heavy, so run it after releasing the span_cache lock.
        let otlp_trace_spans: Vec<Vec<SpanData>> = grouped_traces
            .into_iter()
            .filter_map(|grouped| self.otlp_exporter.prepare_for_export(grouped))
            .collect();

        if !otlp_trace_spans.is_empty() {
            self.otlp_exporter
                .export(otlp_trace_spans.into_iter().flatten().collect())
                .await
        } else {
            Ok(())
        }
    }

    fn shutdown_with_timeout(&self, timeout: std::time::Duration) -> OTelSdkResult {
        self.otlp_exporter.shutdown_with_timeout(timeout)
    }

    fn force_flush(&self) -> OTelSdkResult {
        Ok(())
    }

    fn set_resource(&mut self, _resource: &Resource) {
        // This is intentionally a NOOP. The reason for this is that we do not allow users to set the resource attributes
        // for telemetry that is sent to Apollo. To do so would expose potential private information that the user did not intend for us.
    }
}

/// Accumulate span data so we can build full trace reports for Apollo Studio telemetry once a
/// trace is complete.
///
/// Normally we'd send spans off to APMs whenever we can, and the APM builds the full trace
/// progressively, but the Apollo backend doesn't do this.
#[derive(Debug)]
struct SpanCache {
    spans_by_parent_id: LruCache<SpanId, LruCache<usize, LightSpanData>>,
}

impl SpanCache {
    fn insert(&mut self, span: LightSpanData) {
        // This is sad, but with LRU there is no `get_insert_mut` so a double lookup is required
        // It is safe to expect the entry to exist as we just inserted it, however capacity of the LRU must not be 0.
        let len = self
            .spans_by_parent_id
            .get_or_insert(span.parent_span_id, || {
                LruCache::new(NonZeroUsize::new(50).unwrap())
            })
            .len();
        self.spans_by_parent_id
            .get_mut(&span.parent_span_id)
            .expect("capacity of cache was zero")
            .push(len, span);
    }

    /// Collects the subtree for a trace by calling pop() on the LRU cache for
    /// all spans in the tree, given an initial "root span". Used by the OTLP exporter
    /// to build up a complete trace.
    /// For a future iteration, consider using the same algorithm in the `groupbytrace`
    /// processor, which groups based on trace ID instead of connecting recursively by parent ID.
    fn pop_spans_for_tree(&mut self, root_span: LightSpanData) -> Vec<LightSpanData> {
        let root_span_id = root_span.span_id;
        let mut child_spans = match self.spans_by_parent_id.pop(&root_span_id) {
            Some(spans) => spans
                .into_iter()
                .flat_map(|(_, span)| self.pop_spans_for_tree(span))
                .collect(),
            None => Vec::new(),
        };
        let mut spans_for_tree = vec![root_span];
        spans_for_tree.append(&mut child_spans);
        spans_for_tree
    }

    /// Returns the size of the span LRU cache.
    fn len(&self) -> usize {
        self.spans_by_parent_id.len()
    }
}

pub(crate) fn extract_string(v: &Value) -> Option<String> {
    if let Value::String(v) = v {
        Some(v.to_string())
    } else {
        None
    }
}

pub(crate) fn extract_ftv1_trace_with_error_count(
    v: &Value,
    error_config: &ErrorConfiguration,
) -> Option<Result<(Box<proto::reports::Trace>, u64), Error>> {
    let mut error_count: u64 = 0;
    if let Value::String(s) = v {
        if let Some(mut t) = decode_ftv1_trace(s.as_str()) {
            if let Some(root) = &mut t.root {
                error_count += preprocess_errors(root, error_config);
            }
            return Some(Ok((Box::new(t), error_count)));
        }
        return Some(Err(Error::TraceParsingFailed));
    }
    None
}

fn perform_extended_redaction(error_json: &str) -> String {
    serde_json::from_str::<JSONValue>(error_json)
        .ok()
        .and_then(|error_json_value| {
            let error_code = &error_json_value["extensions"]["code"];
            if !error_code.is_null() {
                Some(
                    serde_json_bytes::json!({
                        "extensions": {
                            "code": error_code,
                        }
                    })
                    .to_string(),
                )
            } else {
                None
            }
        })
        .unwrap_or_default()
}

fn preprocess_errors(
    t: &mut proto::reports::trace::Node,
    error_config: &ErrorConfiguration,
) -> u64 {
    let mut error_count: u64 = 0;
    if error_config.send {
        if error_config.redact {
            t.error.iter_mut().for_each(|err| {
                err.message = String::from("<redacted>");
                err.location = Vec::new();
                err.json = if matches!(
                    error_config.redaction_policy,
                    ErrorRedactionPolicy::Extended
                ) {
                    perform_extended_redaction(&err.json)
                } else {
                    String::new()
                }
            });
        }
        error_count += u64::try_from(t.error.len()).expect("expected u64");
    } else {
        t.error = Vec::new();
    }
    t.child
        .iter_mut()
        .for_each(|n| error_count += preprocess_errors(n, error_config));
    error_count
}

pub(crate) fn decode_ftv1_trace(string: &str) -> Option<proto::reports::Trace> {
    let bytes = BASE64_STANDARD.decode(string).ok()?;
    proto::reports::Trace::decode(Cursor::new(bytes)).ok()
}

pub(crate) fn encode_ftv1_trace(trace: &proto::reports::Trace) -> String {
    BASE64_STANDARD.encode(trace.encode_to_vec())
}

#[cfg(test)]
mod test {
    use opentelemetry::Value;

    use crate::plugins::telemetry::apollo::ErrorConfiguration;
    use crate::plugins::telemetry::apollo::ErrorRedactionPolicy;
    use crate::plugins::telemetry::apollo_exporter::proto::reports::Trace;
    use crate::plugins::telemetry::apollo_exporter::proto::reports::trace::Error;
    use crate::plugins::telemetry::apollo_exporter::proto::reports::trace::Node;
    use crate::plugins::telemetry::tracing::apollo_telemetry::encode_ftv1_trace;
    use crate::plugins::telemetry::tracing::apollo_telemetry::extract_ftv1_trace_with_error_count;
    use crate::plugins::telemetry::tracing::apollo_telemetry::extract_string;
    use crate::plugins::telemetry::tracing::apollo_telemetry::preprocess_errors;

    #[test]
    fn test_extract_string() {
        assert_eq!(
            extract_string(&Value::String("hi".into())),
            Some("hi".to_string())
        );
    }

    #[test]
    fn test_extract_ftv1_trace_with_error_count() {
        let mut trace = Trace::default();
        let sub_node = Node {
            error: vec![Error {
                message: "this is my error".to_string(),
                location: Vec::new(),
                time_ns: 5,
                json: String::from(r#"{"foo": "bar"}"#),
            }],
            ..Default::default()
        };
        let mut node = Node {
            error: vec![
                Error {
                    message: "this is my error".to_string(),
                    location: Vec::new(),
                    time_ns: 5,
                    json: String::from(r#"{"foo": "bar"}"#),
                },
                Error {
                    message: "this is my other error".to_string(),
                    location: Vec::new(),
                    time_ns: 5,
                    json: String::from(r#"{"foo": "bar"}"#),
                },
            ],
            ..Default::default()
        };
        node.child.push(sub_node);
        trace.root = Some(node);
        let encoded = encode_ftv1_trace(&trace);
        let extracted = extract_ftv1_trace_with_error_count(
            &Value::String(encoded.into()),
            &ErrorConfiguration {
                send: true,
                redact: false,
                redaction_policy: ErrorRedactionPolicy::Strict,
            },
        )
        .expect("there was a trace here")
        .expect("the trace must be decoded");
        assert_eq!(*extracted.0, trace);
        assert_eq!(extracted.1, 3);
    }

    #[test]
    fn test_preprocess_errors_with_strict_redaction() {
        let sub_node = Node {
            error: vec![Error {
                message: "this is my error".to_string(),
                location: Vec::new(),
                time_ns: 5,
                json: String::from(
                    r#"{"extensions":{"code":"AN_ERROR_CODE","ignored":"other stuff"}}"#,
                ),
            }],
            ..Default::default()
        };
        let mut node = Node {
            error: vec![
                Error {
                    message: "this is my error".to_string(),
                    location: Vec::new(),
                    time_ns: 5,
                    json: String::from(r#"{"foo": "bar"}"#),
                },
                Error {
                    message: "this is my other error".to_string(),
                    location: Vec::new(),
                    time_ns: 5,
                    json: String::from(r#"{"foo": "bar"}"#),
                },
            ],
            ..Default::default()
        };
        node.child.push(sub_node);
        let error_config = ErrorConfiguration {
            send: true,
            redact: true,
            redaction_policy: ErrorRedactionPolicy::Strict,
        };
        let error_count = preprocess_errors(&mut node, &error_config);
        assert_eq!(error_count, 3);
        assert!(node.error[0].json.is_empty());
        assert!(node.error[0].location.is_empty());
        assert_eq!(node.error[0].message.as_str(), "<redacted>");
        assert_eq!(node.error[0].time_ns, 5u64);
        assert!(node.error[1].json.is_empty());
        assert!(node.error[1].location.is_empty());
        assert_eq!(node.error[1].message.as_str(), "<redacted>");
        assert_eq!(node.error[1].time_ns, 5u64);

        assert!(node.child[0].error[0].json.is_empty());
        assert!(node.child[0].error[0].location.is_empty());
        assert_eq!(node.child[0].error[0].message.as_str(), "<redacted>");
        assert_eq!(node.child[0].error[0].time_ns, 5u64);
    }

    #[test]
    fn test_preprocess_errors_with_redaction_disabled() {
        let sub_node = Node {
            error: vec![Error {
                message: "this is my error".to_string(),
                location: Vec::new(),
                time_ns: 5,
                json: String::from(
                    r#"{"extensions":{"code":"AN_ERROR_CODE","ignored":"other stuff"}}"#,
                ),
            }],
            ..Default::default()
        };
        let mut node = Node {
            error: vec![
                Error {
                    message: "this is my error".to_string(),
                    location: Vec::new(),
                    time_ns: 5,
                    json: String::from(r#"{"foo": "bar"}"#),
                },
                Error {
                    message: "this is my other error".to_string(),
                    location: Vec::new(),
                    time_ns: 5,
                    json: String::from(r#"{"foo": "bar"}"#),
                },
            ],
            ..Default::default()
        };
        node.child.push(sub_node);
        let error_config = ErrorConfiguration {
            send: true,
            redact: false,
            redaction_policy: ErrorRedactionPolicy::Strict,
        };
        let error_count = preprocess_errors(&mut node, &error_config);
        assert_eq!(error_count, 3);
        assert_eq!(node.error[0].message.as_str(), "this is my error");
        assert_eq!(node.error[0].time_ns, 5u64);
        assert!(!node.error[1].json.is_empty());
        assert_eq!(node.error[1].message.as_str(), "this is my other error");
        assert_eq!(node.error[1].time_ns, 5u64);

        assert_eq!(
            node.child[0].error[0].json,
            String::from(r#"{"extensions":{"code":"AN_ERROR_CODE","ignored":"other stuff"}}"#,)
        );
        assert_eq!(node.child[0].error[0].message.as_str(), "this is my error");
        assert_eq!(node.child[0].error[0].time_ns, 5u64);
    }

    #[test]
    fn test_preprocess_errors_with_extended_redaction_enabled() {
        let sub_node = Node {
            error: vec![Error {
                message: "this is my error".to_string(),
                location: Vec::new(),
                time_ns: 5,
                json: String::from(
                    r#"{"extensions":{"code":"AN_ERROR_CODE","ignored":"other stuff"}}"#,
                ),
            }],
            ..Default::default()
        };
        let mut node = Node {
            error: vec![
                Error {
                    message: "this is my error".to_string(),
                    location: Vec::new(),
                    time_ns: 5,
                    json: String::from(r#"{"foo": "bar"}"#),
                },
                Error {
                    message: "this is my other error".to_string(),
                    location: Vec::new(),
                    time_ns: 5,
                    json: String::from(r#"{"foo": "bar"}"#),
                },
            ],
            ..Default::default()
        };
        node.child.push(sub_node);
        let error_config = ErrorConfiguration {
            send: true,
            redact: true,
            redaction_policy: ErrorRedactionPolicy::Extended,
        };
        let error_count = preprocess_errors(&mut node, &error_config);
        assert_eq!(error_count, 3);
        assert!(node.error[0].location.is_empty());
        assert_eq!(node.error[0].message.as_str(), "<redacted>");
        assert_eq!(node.error[0].time_ns, 5u64);
        assert!(node.error[1].json.is_empty());
        assert!(node.error[1].location.is_empty());
        assert_eq!(node.error[1].message.as_str(), "<redacted>");
        assert_eq!(node.error[1].time_ns, 5u64);

        // the "ignored" field should be filtered out in this scenario, but the
        // code left alone.
        assert_eq!(
            node.child[0].error[0].json,
            String::from(r#"{"extensions":{"code":"AN_ERROR_CODE"}}"#,)
        );
        assert!(node.child[0].error[0].location.is_empty());
        assert_eq!(node.child[0].error[0].message.as_str(), "<redacted>");
        assert_eq!(node.child[0].error[0].time_ns, 5u64);
    }

    #[test]
    fn test_delete_node_errors() {
        let sub_node = Node {
            error: vec![Error {
                message: "this is my error".to_string(),
                location: Vec::new(),
                time_ns: 5,
                json: String::from(r#"{"foo": "bar"}"#),
            }],
            ..Default::default()
        };
        let mut node = Node {
            error: vec![
                Error {
                    message: "this is my error".to_string(),
                    location: Vec::new(),
                    time_ns: 5,
                    json: String::from(r#"{"foo": "bar"}"#),
                },
                Error {
                    message: "this is my other error".to_string(),
                    location: Vec::new(),
                    time_ns: 5,
                    json: String::from(r#"{"foo": "bar"}"#),
                },
            ],
            ..Default::default()
        };
        node.child.push(sub_node);
        let error_config = ErrorConfiguration {
            send: false,
            redact: true,
            redaction_policy: ErrorRedactionPolicy::Strict,
        };
        let error_count = preprocess_errors(&mut node, &error_config);
        assert_eq!(error_count, 0);
        assert!(node.error.is_empty());
        assert!(node.child[0].error.is_empty());
    }
}
