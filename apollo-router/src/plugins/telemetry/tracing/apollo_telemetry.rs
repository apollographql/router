use std::collections::HashMap;
use std::io::Cursor;
use std::str::FromStr;
use std::time::SystemTimeError;

use apollo_spaceport::trace::http::Values;
use apollo_spaceport::trace::query_plan_node::FetchNode;
use apollo_spaceport::trace::query_plan_node::FlattenNode;
use apollo_spaceport::trace::query_plan_node::ParallelNode;
use apollo_spaceport::trace::query_plan_node::SequenceNode;
use apollo_spaceport::trace::Details;
use apollo_spaceport::trace::Http;
use apollo_spaceport::trace::QueryPlanNode;
use apollo_spaceport::Message;
use async_trait::async_trait;
use derivative::Derivative;
use http::header::HeaderName;
use http::Uri;
use lru::LruCache;
use opentelemetry::sdk::export::trace::ExportResult;
use opentelemetry::sdk::export::trace::SpanData;
use opentelemetry::sdk::export::trace::SpanExporter;
use opentelemetry::trace::SpanId;
use opentelemetry::trace::TraceId;
use opentelemetry::Key;
use opentelemetry::Value;
use thiserror::Error;
use url::Url;

use super::apollo::TracesReport;
use crate::plugins::telemetry::apollo::ForwardValues;
use crate::plugins::telemetry::apollo::Sender;
use crate::plugins::telemetry::apollo::SingleReport;
use crate::plugins::telemetry::config;

#[derive(Error, Debug)]
pub(crate) enum Error {
    #[error("subgraph protobuf decode error")]
    ProtobufDecode(#[from] apollo_spaceport::DecodeError),

    #[error("subgraph trace payload was not base64")]
    Base64Decode(#[from] base64::DecodeError),

    #[error("ftv1 span attribute should have been a string")]
    Ftv1SpanAttribute,

    #[error("there were multiple tracing errors")]
    MultipleErrors(Vec<Error>),

    #[error("duration could not be calculated")]
    SystemTime(#[from] SystemTimeError),
}

/// A [`SpanExporter`] that writes to [`Reporter`].
///
/// [`SpanExporter`]: super::SpanExporter
/// [`Reporter`]: apollo_spaceport::Reporter
#[derive(Derivative)]
#[derivative(Debug)]
#[allow(dead_code)]
pub(crate) struct Exporter {
    trace_config: config::Trace,
    spans_by_parent_id: LruCache<SpanId, Vec<SpanData>>,
    endpoint: Url,
    apollo_key: String,
    apollo_graph_ref: String,
    client_name_header: HeaderName,
    client_version_header: HeaderName,
    schema_id: String,
    field_level_instrumentation: bool,
    #[derivative(Debug = "ignore")]
    apollo_sender: Sender,
    send_headers: ForwardValues,
    send_variable_values: ForwardValues,
}

enum TreeData {
    QueryPlanNode(QueryPlanNode),
    Trace(Result<Option<Box<apollo_spaceport::Trace>>, Error>),
}

#[buildstructor::buildstructor]
impl Exporter {
    #[allow(clippy::too_many_arguments)]
    #[builder]
    pub(crate) fn new(
        trace_config: config::Trace,
        endpoint: Url,
        apollo_key: String,
        apollo_graph_ref: String,
        client_name_header: HeaderName,
        client_version_header: HeaderName,
        schema_id: String,
        apollo_sender: Sender,
        buffer_size: usize,
        field_level_instrumentation: bool,
        send_headers: Option<ForwardValues>,
        send_variable_values: Option<ForwardValues>,
    ) -> Self {
        Self {
            spans_by_parent_id: LruCache::new(buffer_size),
            trace_config,
            endpoint,
            apollo_key,
            apollo_graph_ref,
            client_name_header,
            client_version_header,
            schema_id,
            field_level_instrumentation,
            apollo_sender,
            send_headers: send_headers.unwrap_or_default(),
            send_variable_values: send_variable_values.unwrap_or_default(),
        }
    }

    pub(crate) fn extract_query_plan_trace(
        &mut self,
        span: SpanData,
    ) -> Result<apollo_spaceport::Trace, Error> {
        let query_plan = self
            .extract_query_plan_node(&span, &span)?
            .pop()
            .and_then(|node| {
                if let TreeData::QueryPlanNode(node) = node {
                    Some(Box::new(node))
                } else {
                    None
                }
            });
        let variables = span
            .attributes
            .get(&Key::new("graphql.variables"))
            .map(|data| data.as_str())
            .unwrap_or_default();
        let variables_json = if variables != "{}" {
            serde_json::from_str(&variables).unwrap_or_default()
        } else {
            HashMap::new()
        };

        let details = Details {
            variables_json,
            operation_name: span
                .attributes
                .get(&Key::new("graphql.operation.name"))
                .map(|data| data.as_str())
                .unwrap_or_default()
                .to_string(),
        };
        Ok(apollo_spaceport::Trace {
            start_time: Some(span.start_time.into()),
            end_time: Some(span.end_time.into()),
            duration_ns: span.end_time.duration_since(span.start_time)?.as_nanos() as u64,
            root: None,
            signature: "".to_string(),
            unexecuted_operation_body: "".to_string(),
            unexecuted_operation_name: "".to_string(),
            details: Some(details),
            client_name: "".to_string(),
            client_version: "".to_string(),
            http: None,
            cache_policy: None,
            query_plan,
            full_query_cache_hit: false,
            persisted_query_hit: false,
            persisted_query_register: false,
            registered_operation: false,
            forbidden_operation: false,
            field_execution_weight: 0.0,
        })
    }

    fn extract_query_plan_node(
        &mut self,
        root_span: &SpanData,
        span: &SpanData,
    ) -> Result<Vec<TreeData>, Error> {
        let (mut child_nodes, errors) = self
            .spans_by_parent_id
            .pop_entry(&span.span_context.span_id())
            .map(|(_, spans)| spans)
            .unwrap_or_default()
            .into_iter()
            .map(|span| self.extract_query_plan_node(root_span, &span))
            .fold((Vec::new(), Vec::new()), |(mut oks, mut errors), next| {
                match next {
                    Ok(mut children) => oks.append(&mut children),
                    Err(err) => errors.push(err),
                }
                (oks, errors)
            });
        if !errors.is_empty() {
            return Err(Error::MultipleErrors(errors));
        }

        Ok(match span.name.as_ref() {
            "parallel" => vec![TreeData::QueryPlanNode(QueryPlanNode {
                node: Some(apollo_spaceport::trace::query_plan_node::Node::Parallel(
                    ParallelNode {
                        nodes: child_nodes
                            .into_iter()
                            .filter_map(|child| match child {
                                TreeData::QueryPlanNode(node) => Some(node),
                                _ => None,
                            })
                            .collect(),
                    },
                )),
            })],
            "sequence" => vec![TreeData::QueryPlanNode(QueryPlanNode {
                node: Some(apollo_spaceport::trace::query_plan_node::Node::Sequence(
                    SequenceNode {
                        nodes: child_nodes
                            .into_iter()
                            .filter_map(|child| match child {
                                TreeData::QueryPlanNode(node) => Some(node),
                                _ => None,
                            })
                            .collect(),
                    },
                )),
            })],
            "fetch" => {
                let (trace_parsing_failed, trace) = match child_nodes.pop() {
                    Some(TreeData::Trace(Ok(trace))) => (false, trace),
                    Some(TreeData::Trace(Err(_err))) => (true, None),
                    _ => (false, None),
                };
                let service_name = (span
                    .attributes
                    .get(&Key::new("service.name"))
                    .cloned()
                    .unwrap_or_else(|| Value::String("unknown service".into()))
                    .as_str())
                .to_string();

                vec![TreeData::QueryPlanNode(QueryPlanNode {
                    node: Some(apollo_spaceport::trace::query_plan_node::Node::Fetch(
                        Box::new(FetchNode {
                            service_name,
                            trace_parsing_failed,
                            trace,
                            sent_time_offset: span
                                .start_time
                                .duration_since(root_span.start_time)?
                                .as_nanos() as u64,
                            sent_time: Some(span.start_time.into()),
                            received_time: Some(span.end_time.into()),
                        }),
                    )),
                })]
            }
            "flatten" => vec![TreeData::QueryPlanNode(QueryPlanNode {
                node: Some(apollo_spaceport::trace::query_plan_node::Node::Flatten(
                    Box::new(FlattenNode {
                        response_path: vec![],
                        node: None,
                    }),
                )),
            })],
            "subgraph" => {
                vec![TreeData::Trace(self.find_ftv1_trace(span))]
            }
            _ => child_nodes,
        })
    }

    fn find_ftv1_trace(
        &mut self,
        span: &SpanData,
    ) -> Result<Option<Box<apollo_spaceport::Trace>>, Error> {
        if !self.field_level_instrumentation {
            Ok(None)
        } else {
            Ok(span
                .attributes
                .get(&Key::new("apollo_private_ftv1"))
                .map(|data| {
                    if let Value::String(data) = data {
                        Ok(Box::new(apollo_spaceport::Trace::decode(Cursor::new(
                            base64::decode(data.to_string())?,
                        ))?))
                    } else {
                        Err(Error::Ftv1SpanAttribute)
                    }
                })
                .transpose()?)
        }
    }

    fn extract_http_data(&self, span: &SpanData) -> Http {
        let method = match span
            .attributes
            .get(&Key::new("method"))
            .map(|data| data.as_str())
            .unwrap_or_default()
            .as_ref()
        {
            "OPTIONS" => 1,
            "GET" => 2,
            "HEAD" => 3,
            "POST" => 4,
            "PUT" => 5,
            "DELETE" => 6,
            "TRACE" => 7,
            "CONNECT" => 8,
            "PATCH" => 9,
            _ => 0,
        };
        let version = span
            .attributes
            .get(&Key::new("version"))
            .map(|data| data.as_str())
            .unwrap_or_default();
        let headers = span
            .attributes
            .get(&Key::new("headers"))
            .map(|data| data.as_str())
            .unwrap_or_default();
        let uri = Uri::from_str(
            &span
                .attributes
                .get(&Key::new("uri"))
                .map(|data| data.as_str())
                .unwrap_or_default(),
        )
        .unwrap_or_default();
        let mut request_headers: HashMap<String, Values> =
            if let ForwardValues::None = &self.send_headers {
                HashMap::new()
            } else {
                serde_json::from_str::<HashMap<String, Vec<String>>>(&headers)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|(header_name, value)| (header_name.to_lowercase(), Values { value }))
                    .collect()
            };

        match &self.send_headers {
            ForwardValues::Only(headers_to_keep) => {
                request_headers.retain(|header_name, _v| headers_to_keep.contains(header_name));
            }
            ForwardValues::Except(skip_headers) => {
                request_headers.retain(|header_name, _v| !skip_headers.contains(header_name));
            }
            ForwardValues::None | ForwardValues::All => {}
        }

        Http {
            method,
            host: uri.host().map(|h| h.to_string()).unwrap_or_default(),
            path: uri.path().to_owned(),
            request_headers,
            response_headers: HashMap::new(),
            status_code: 0,
            secure: false,
            protocol: version.to_string(),
        }
    }
}

// Wrapper to add otel trace id on an apollo_spaceport::Trace
#[derive(Debug)]
struct TraceWithId {
    trace_id: TraceId,
    signature: String,
    trace: apollo_spaceport::Trace,
}

impl TraceWithId {
    fn new(trace_id: TraceId, trace: apollo_spaceport::Trace, signature: String) -> Self {
        Self {
            trace_id,
            trace,
            signature,
        }
    }
}

impl From<TraceWithId> for apollo_spaceport::Trace {
    fn from(trace_with_id: TraceWithId) -> Self {
        trace_with_id.trace
    }
}

#[async_trait]
impl SpanExporter for Exporter {
    /// Export spans to apollo telemetry
    async fn export(&mut self, batch: Vec<SpanData>) -> ExportResult {
        // Exporting to apollo means that we must have complete trace as the entire trace must be built.
        // We do what we can, and if there are any traces that are not complete then we keep them for the next export event.
        // We may get spams that simply don't complete. These need to be cleaned up after a period. It's the price of using ftv1.

        // Note that apollo-tracing won't really work with defer/stream/live queries. In this situation it's difficult to know when a request has actually finished.
        let mut traces_per_query: HashMap<String, TraceWithId> = HashMap::new();
        for span in batch {
            if span.name == "router" {
                let operation_signature_attr = span
                    .attributes
                    .get(&Key::new("operation.signature"))
                    .map(Value::to_string);
                let request_id_attr = span
                    .attributes
                    .get(&Key::new("request.id"))
                    .map(Value::to_string);
                if let (Some(operation_signature), Some(request_id)) =
                    (operation_signature_attr, request_id_attr)
                {
                    let trace_id = span.span_context.trace_id();
                    match self.extract_query_plan_trace(span) {
                        Ok(trace) => {
                            traces_per_query.insert(
                                request_id,
                                TraceWithId::new(trace_id, trace, operation_signature),
                            );
                        }
                        Err(error) => {
                            tracing::error!("failed to construct trace: {}, skipping", error);
                        }
                    }
                }
            } else {
                if span.name == "request" {
                    if let Some(trace_found) = traces_per_query.values_mut().find(|trace_with_id| {
                        trace_with_id.trace_id == span.span_context.trace_id()
                    }) {
                        trace_found.trace.http = self.extract_http_data(&span).into();
                    }
                }
                // Not a root span, we may need it later so stash it.

                // This is sad, but with LRU there is no `get_insert_mut` so a double lookup is required
                // It is safe to expect the entry to exist as we just inserted it, however capacity of the LRU must not be 0.
                self.spans_by_parent_id
                    .get_or_insert(span.parent_span_id, Vec::new);
                self.spans_by_parent_id
                    .get_mut(&span.parent_span_id)
                    .expect("capacity of cache was zero")
                    .push(span);
            }
        }

        self.apollo_sender.send(SingleReport::Traces(TracesReport {
            traces: traces_per_query
                .into_iter()
                .map(|(k, v)| (k, (v.signature, v.trace)))
                .collect(),
        }));

        return ExportResult::Ok(());
    }
}
