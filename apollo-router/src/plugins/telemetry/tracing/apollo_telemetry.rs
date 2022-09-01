use std::borrow::Cow;
use std::collections::HashMap;
use std::io::Cursor;
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
use lru::LruCache;
use opentelemetry::sdk::export::trace::ExportResult;
use opentelemetry::sdk::export::trace::SpanData;
use opentelemetry::sdk::export::trace::SpanExporter;
use opentelemetry::trace::SpanId;
use opentelemetry::Key;
use opentelemetry::Value;
use thiserror::Error;
use url::Url;

use super::apollo::TracesReport;
use crate::plugins::telemetry::apollo::SingleReport;
use crate::plugins::telemetry::apollo_exporter::Sender;
use crate::plugins::telemetry::config;
use crate::plugins::telemetry::REQUEST_SPAN_NAME;

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

    #[error("this trace should not be sampled")]
    DoNotSample(Cow<'static, str>),
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
    #[derivative(Debug = "ignore")]
    apollo_sender: Sender,
}

enum TreeData {
    RootTrace(Result<Box<apollo_spaceport::Trace>, Error>),
    Http(Http),
    QueryPlan(QueryPlanNode),
    Trace(Result<Option<Box<apollo_spaceport::Trace>>, Error>),
}

#[buildstructor::buildstructor]
impl Exporter {
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
            apollo_sender,
        }
    }

    fn extract_root_trace(
        &mut self,
        span: &SpanData,
        child_nodes: Vec<TreeData>,
    ) -> Result<Box<apollo_spaceport::Trace>, Error> {
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
            operation_name: "".to_string(), // Deprecated do not set
        };

        let http = self.extract_http_data(span);

        let mut root_trace = apollo_spaceport::Trace {
            start_time: Some(span.start_time.into()),
            end_time: Some(span.end_time.into()),
            duration_ns: span.end_time.duration_since(span.start_time)?.as_nanos() as u64,
            root: None,
            signature: Default::default(), // This is legacy and should never be set.
            unexecuted_operation_body: "".to_string(),
            unexecuted_operation_name: "".to_string(),
            details: Some(details),
            client_name: "".to_string(),
            client_version: "".to_string(),
            http: Some(http),
            cache_policy: None,
            query_plan: None,
            full_query_cache_hit: false,
            persisted_query_hit: false,
            persisted_query_register: false,
            registered_operation: false,
            forbidden_operation: false,
            field_execution_weight: 0.0,
        };

        for node in child_nodes {
            match node {
                TreeData::QueryPlan(query_plan) => {
                    root_trace.query_plan = Some(Box::new(query_plan))
                }
                TreeData::Http(http) => {
                    root_trace
                        .http
                        .as_mut()
                        .expect("http was extracted earlier, qed")
                        .request_headers = http.request_headers
                }
                _ => panic!("should never have had other node types"),
            }
        }

        Ok(Box::new(root_trace))
    }

    fn extract_trace(&mut self, span: SpanData) -> Result<Box<apollo_spaceport::Trace>, Error> {
        self.extract_data_from_spans(&span, &span)?
            .pop()
            .and_then(|node| {
                if let TreeData::RootTrace(trace) = node {
                    Some(trace)
                } else {
                    None
                }
            })
            .expect("root trace must exist because it is constructed on the request span, qed")
    }

    fn extract_data_from_spans(
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
            .map(|span| self.extract_data_from_spans(root_span, &span))
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
        if let Some(Value::String(reason)) =
            span.attributes.get(&Key::new("ftv1_do_not_sample_reason"))
        {
            if !reason.is_empty() {
                return Err(Error::DoNotSample(reason.clone()));
            }
        }

        Ok(match span.name.as_ref() {
            "parallel" => vec![TreeData::QueryPlan(QueryPlanNode {
                node: Some(apollo_spaceport::trace::query_plan_node::Node::Parallel(
                    ParallelNode {
                        nodes: child_nodes
                            .into_iter()
                            .filter_map(|child| match child {
                                TreeData::QueryPlan(node) => Some(node),
                                _ => None,
                            })
                            .collect(),
                    },
                )),
            })],
            "sequence" => vec![TreeData::QueryPlan(QueryPlanNode {
                node: Some(apollo_spaceport::trace::query_plan_node::Node::Sequence(
                    SequenceNode {
                        nodes: child_nodes
                            .into_iter()
                            .filter_map(|child| match child {
                                TreeData::QueryPlan(node) => Some(node),
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

                vec![TreeData::QueryPlan(QueryPlanNode {
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
            "flatten" => vec![TreeData::QueryPlan(QueryPlanNode {
                node: Some(apollo_spaceport::trace::query_plan_node::Node::Flatten(
                    Box::new(FlattenNode {
                        response_path: vec![],
                        node: child_nodes
                            .into_iter()
                            .filter_map(|child| match child {
                                TreeData::QueryPlan(node) => Some(Box::new(node)),
                                _ => None,
                            })
                            .next(),
                    }),
                )),
            })],
            "subgraph" => {
                vec![TreeData::Trace(self.find_ftv1_trace(span))]
            }
            "supergraph" => {
                //Currently some data is in the supergraph span as we don't have the a request hook in plugin.
                child_nodes.push(TreeData::Http(self.extract_http_data(span)));
                child_nodes
            }
            "request" => {
                vec![TreeData::RootTrace(
                    self.extract_root_trace(span, child_nodes),
                )]
            }
            _ => child_nodes,
        })
    }

    fn find_ftv1_trace(
        &mut self,
        span: &SpanData,
    ) -> Result<Option<Box<apollo_spaceport::Trace>>, Error> {
        span.attributes
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
            .transpose()
    }

    fn extract_http_data(&self, span: &SpanData) -> Http {
        let method = match span
            .attributes
            .get(&Key::new("method"))
            .map(|data| data.as_str())
            .unwrap_or_default()
            .as_ref()
        {
            "OPTIONS" => apollo_spaceport::trace::http::Method::Options,
            "GET" => apollo_spaceport::trace::http::Method::Get,
            "HEAD" => apollo_spaceport::trace::http::Method::Head,
            "POST" => apollo_spaceport::trace::http::Method::Post,
            "PUT" => apollo_spaceport::trace::http::Method::Put,
            "DELETE" => apollo_spaceport::trace::http::Method::Delete,
            "TRACE" => apollo_spaceport::trace::http::Method::Trace,
            "CONNECT" => apollo_spaceport::trace::http::Method::Connect,
            "PATCH" => apollo_spaceport::trace::http::Method::Patch,
            _ => apollo_spaceport::trace::http::Method::Unknown,
        };
        let headers = span
            .attributes
            .get(&Key::new("apollo_private_request_headers"))
            .map(|data| data.as_str())
            .unwrap_or_default();
        let request_headers = serde_json::from_str::<HashMap<String, Vec<String>>>(&headers)
            .unwrap_or_default()
            .into_iter()
            .map(|(header_name, value)| (header_name.to_lowercase(), Values { value }))
            .collect();

        Http {
            method: method.into(),
            host: Default::default(), // Do not fill in, we can't reliably get this information
            path: Default::default(), // Do not fill in, we can't reliably get this information
            request_headers,
            response_headers: Default::default(),
            status_code: 0,
            secure: Default::default(),
            protocol: Default::default(),
        }
    }
}

#[async_trait]
impl SpanExporter for Exporter {
    /// Export spans to apollo telemetry
    async fn export(&mut self, batch: Vec<SpanData>) -> ExportResult {
        // Exporting to apollo means that we must have complete trace as the entire trace must be built.
        // We do what we can, and if there are any traces that are not complete then we keep them for the next export event.
        // We may get spans that simply don't complete. These need to be cleaned up after a period. It's the price of using ftv1.

        // Note that apollo-tracing won't really work with defer/stream/live queries. In this situation it's difficult to know when a request has actually finished.
        let mut traces: Vec<(String, apollo_spaceport::Trace)> = Vec::new();
        for span in batch {
            if span.name == REQUEST_SPAN_NAME {
                let operation_signature_attr = span
                    .attributes
                    .get(&Key::new("operation.signature"))
                    .map(Value::to_string);

                if let Some(operation_signature) = operation_signature_attr {
                    match self.extract_trace(span) {
                        Ok(trace) => {
                            traces.push((operation_signature, *trace));
                        }
                        Err(Error::MultipleErrors(errors)) => {
                            if let Some(Error::DoNotSample(reason)) = errors.first() {
                                tracing::debug!(
                                    "sampling is disabled on this trace: {}, skipping",
                                    reason
                                );
                            } else {
                                tracing::error!(
                                    "failed to construct trace: {}, skipping",
                                    Error::MultipleErrors(errors)
                                );
                            }
                        }
                        Err(error) => {
                            tracing::error!("failed to construct trace: {}, skipping", error);
                        }
                    }
                }
            } else {
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
            .send(SingleReport::Traces(TracesReport { traces }));

        return ExportResult::Ok(());
    }
}
