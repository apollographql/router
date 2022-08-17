use std::collections::HashMap;
use std::io::Cursor;
use std::str::FromStr;
use std::time::SystemTimeError;

use apollo_spaceport::trace::http::Values;
use apollo_spaceport::trace::query_plan_node::FetchNode;
use apollo_spaceport::trace::query_plan_node::FlattenNode;
use apollo_spaceport::trace::query_plan_node::ParallelNode;
use apollo_spaceport::trace::query_plan_node::SequenceNode;
use apollo_spaceport::trace::Http;
use apollo_spaceport::trace::QueryPlanNode;
use apollo_spaceport::Message;
use apollo_spaceport::Report;
use apollo_spaceport::TracesAndStats;
use async_trait::async_trait;
use derivative::Derivative;
use http::header::HeaderName;
use http::Uri;
use lru::LruCache;
use opentelemetry::sdk::export::trace::ExportResult;
use opentelemetry::sdk::export::trace::SpanData;
use opentelemetry::sdk::export::trace::SpanExporter;
use opentelemetry::trace::SpanId;
use opentelemetry::Key;
use opentelemetry::Value;
use thiserror::Error;
use url::Url;

use super::apollo::SingleTraces;
use super::apollo::SingleTracesReport;
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
        Ok(apollo_spaceport::Trace {
            start_time: Some(span.start_time.into()),
            end_time: Some(span.end_time.into()),
            duration_ns: span.end_time.duration_since(span.start_time)?.as_nanos() as u64,
            root: None,
            signature: "".to_string(),
            unexecuted_operation_body: "".to_string(),
            unexecuted_operation_name: "".to_string(),
            details: None,
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
                .get(&Key::new("ftv1"))
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
        let request_headers: HashMap<String, Values> =
            serde_json::from_str::<HashMap<String, Vec<String>>>(&headers)
                .unwrap_or_default()
                .into_iter()
                .map(|(header_name, value)| (header_name, Values { value }))
                .collect();

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

#[async_trait]
impl SpanExporter for Exporter {
    /// Export spans to apollo telemetry
    async fn export(&mut self, batch: Vec<SpanData>) -> ExportResult {
        // Exporting to apollo means that we must have complete trace as the entire trace must be built.
        // We do what we can, and if there are any traces that are not complete then we keep them for the next export event.
        // We may get spams that simply don't complete. These need to be cleaned up after a period. It's the price of using ftv1.

        // Note that apollo-tracing won't really work with defer/stream/live queries. In this situation it's difficult to know when a request has actually finished.
        let mut report = Report {
            header: None,
            traces_per_query: Default::default(),
            end_time: None,
            operation_count: 0,
        };
        for span in batch {
            if span.name == "router" {
                if let Some(operation_signature) = span
                    .attributes
                    .get(&Key::new("operation.signature"))
                    .map(Value::to_string)
                {
                    let traces_and_stats = report
                        .traces_per_query
                        .entry(operation_signature)
                        .or_insert_with(|| TracesAndStats {
                            trace: vec![],
                            stats_with_context: vec![],
                            referenced_fields_by_type: Default::default(),
                            internal_traces_contributing_to_stats: vec![],
                        });
                    match self.extract_query_plan_trace(span) {
                        Ok(trace) => {
                            traces_and_stats.trace.push(trace);
                        }
                        Err(error) => {
                            tracing::error!("failed to construct trace: {}, skipping", error);
                        }
                    }
                }
            } else {
                if span.name == "request" {
                    // TODO use it to fill http field in trace
                    let http = self.extract_http_data(&span);
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

        self.apollo_sender
            .send(SingleReport::Traces(SingleTracesReport {
                traces: report
                    .traces_per_query
                    .into_iter()
                    .map(|(k, v)| (k, SingleTraces::from(v.trace)))
                    .collect(),
            }));

        return ExportResult::Ok(());
    }
}
