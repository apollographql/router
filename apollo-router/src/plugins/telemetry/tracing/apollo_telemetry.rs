use std::borrow::Cow;
use std::collections::HashMap;
use std::io::Cursor;
use std::time::SystemTimeError;

use async_trait::async_trait;
use derivative::Derivative;
use lru::LruCache;
use opentelemetry::sdk::export::trace::ExportResult;
use opentelemetry::sdk::export::trace::SpanData;
use opentelemetry::sdk::export::trace::SpanExporter;
use opentelemetry::trace::SpanId;
use opentelemetry::Key;
use opentelemetry::Value;
use opentelemetry_semantic_conventions::trace::HTTP_METHOD;
use thiserror::Error;
use url::Url;

use crate::axum_factory::utils::REQUEST_SPAN_NAME;
use crate::plugins::telemetry::apollo::SingleReport;
use crate::plugins::telemetry::apollo_exporter::ApolloExporter;
use crate::plugins::telemetry::apollo_exporter::Sender;
use crate::plugins::telemetry::config;
use crate::plugins::telemetry::config::Sampler;
use crate::plugins::telemetry::config::SamplerOption;
use crate::plugins::telemetry::tracing::apollo::TracesReport;
use crate::plugins::telemetry::BoxError;
use crate::plugins::telemetry::SUBGRAPH_SPAN_NAME;
use crate::plugins::telemetry::SUPERGRAPH_SPAN_NAME;
use crate::query_planner::FETCH_SPAN_NAME;
use crate::query_planner::FLATTEN_SPAN_NAME;
use crate::query_planner::PARALLEL_SPAN_NAME;
use crate::query_planner::SEQUENCE_SPAN_NAME;
use crate::spaceport::trace::http::Values;
use crate::spaceport::trace::query_plan_node::FetchNode;
use crate::spaceport::trace::query_plan_node::FlattenNode;
use crate::spaceport::trace::query_plan_node::ParallelNode;
use crate::spaceport::trace::query_plan_node::ResponsePathElement;
use crate::spaceport::trace::query_plan_node::SequenceNode;
use crate::spaceport::trace::Details;
use crate::spaceport::trace::Http;
use crate::spaceport::trace::QueryPlanNode;
use crate::spaceport::Message;

const APOLLO_PRIVATE_DURATION_NS: Key = Key::from_static_str("apollo_private.duration_ns");
const APOLLO_PRIVATE_SENT_TIME_OFFSET: Key =
    Key::from_static_str("apollo_private.sent_time_offset");
const APOLLO_PRIVATE_GRAPHQL_VARIABLES: Key =
    Key::from_static_str("apollo_private.graphql.variables");
const APOLLO_PRIVATE_HTTP_REQUEST_HEADERS: Key =
    Key::from_static_str("apollo_private.http.request_headers");
const APOLLO_PRIVATE_OPERATION_SIGNATURE: Key =
    Key::from_static_str("apollo_private.operation_signature");
const APOLLO_PRIVATE_FTV1: Key = Key::from_static_str("apollo_private.ftv1");
const APOLLO_PRIVATE_PATH: Key = Key::from_static_str("apollo_private.path");
const FTV1_DO_NOT_SAMPLE_REASON: Key = Key::from_static_str("ftv1.do_not_sample_reason");
const SUBGRAPH_NAME: Key = Key::from_static_str("apollo.subgraph.name");
const CLIENT_NAME: Key = Key::from_static_str("client.name");
const CLIENT_VERSION: Key = Key::from_static_str("client.version");

#[derive(Error, Debug)]
pub(crate) enum Error {
    #[error("subgraph protobuf decode error")]
    ProtobufDecode(#[from] crate::spaceport::DecodeError),

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
/// [`Reporter`]: crate::spaceport::Reporter
#[derive(Derivative)]
#[derivative(Debug)]
pub(crate) struct Exporter {
    trace_config: config::Trace,
    spans_by_parent_id: LruCache<SpanId, Vec<SpanData>>,
    endpoint: Url,
    schema_id: String,
    #[derivative(Debug = "ignore")]
    apollo_sender: Sender,
    field_execution_weight: f64,
}

enum TreeData {
    Request(Result<Box<crate::spaceport::Trace>, Error>),
    Supergraph {
        http: Http,
        client_name: Option<String>,
        client_version: Option<String>,
        operation_signature: String,
    },
    QueryPlan(QueryPlanNode),
    Trace(Result<Option<Box<crate::spaceport::Trace>>, Error>),
}

#[buildstructor::buildstructor]
impl Exporter {
    #[builder]
    pub(crate) fn new(
        trace_config: config::Trace,
        endpoint: Url,
        apollo_key: String,
        apollo_graph_ref: String,
        schema_id: String,
        buffer_size: usize,
        field_execution_sampler: Option<SamplerOption>,
    ) -> Result<Self, BoxError> {
        tracing::debug!("creating studio exporter");
        let apollo_exporter =
            ApolloExporter::new(&endpoint, &apollo_key, &apollo_graph_ref, &schema_id)?;
        Ok(Self {
            spans_by_parent_id: LruCache::new(buffer_size),
            trace_config,
            endpoint,
            schema_id,
            apollo_sender: apollo_exporter.provider(),
            field_execution_weight: match field_execution_sampler {
                Some(SamplerOption::Always(Sampler::AlwaysOn)) => 1.0,
                Some(SamplerOption::Always(Sampler::AlwaysOff)) => 0.0,
                Some(SamplerOption::TraceIdRatioBased(ratio)) => 1.0 / ratio,
                None => 0.0,
            },
        })
    }

    fn extract_root_trace(
        &mut self,
        span: &SpanData,
        child_nodes: Vec<TreeData>,
    ) -> Result<Box<crate::spaceport::Trace>, Error> {
        let variables = span
            .attributes
            .get(&APOLLO_PRIVATE_GRAPHQL_VARIABLES)
            .map(|data| data.as_str())
            .unwrap_or_default();
        let variables_json = if variables != "{}" {
            serde_json::from_str(&variables).unwrap_or_default()
        } else {
            HashMap::new()
        };

        let details = Details {
            variables_json,
            ..Default::default()
        };

        let http = self.extract_http_data(span);

        let mut root_trace = crate::spaceport::Trace {
            start_time: Some(span.start_time.into()),
            end_time: Some(span.end_time.into()),
            duration_ns: span
                .attributes
                .get(&APOLLO_PRIVATE_DURATION_NS)
                .and_then(Self::extract_i64)
                .map(|e| e as u64)
                .unwrap_or_default(),
            root: None,
            details: Some(details),
            http: Some(http),
            ..Default::default()
        };

        for node in child_nodes {
            match node {
                TreeData::QueryPlan(query_plan) => {
                    root_trace.query_plan = Some(Box::new(query_plan))
                }
                TreeData::Supergraph {
                    http,
                    client_name,
                    client_version,
                    operation_signature,
                } => {
                    root_trace
                        .http
                        .as_mut()
                        .expect("http was extracted earlier, qed")
                        .request_headers = http.request_headers;
                    root_trace.client_name = client_name.unwrap_or_default();
                    root_trace.client_version = client_version.unwrap_or_default();
                    root_trace.field_execution_weight = self.field_execution_weight;
                    // This will be moved out later
                    root_trace.signature = operation_signature;
                }
                _ => panic!("should never have had other node types"),
            }
        }

        Ok(Box::new(root_trace))
    }

    fn extract_trace(&mut self, span: SpanData) -> Result<Box<crate::spaceport::Trace>, Error> {
        self.extract_data_from_spans(&span, &span)?
            .pop()
            .and_then(|node| {
                if let TreeData::Request(trace) = node {
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
        if let Some(Value::String(reason)) = span.attributes.get(&FTV1_DO_NOT_SAMPLE_REASON) {
            if !reason.is_empty() {
                return Err(Error::DoNotSample(reason.clone()));
            }
        }

        Ok(match span.name.as_ref() {
            PARALLEL_SPAN_NAME => vec![TreeData::QueryPlan(QueryPlanNode {
                node: Some(crate::spaceport::trace::query_plan_node::Node::Parallel(
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
            SEQUENCE_SPAN_NAME => vec![TreeData::QueryPlan(QueryPlanNode {
                node: Some(crate::spaceport::trace::query_plan_node::Node::Sequence(
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
            FETCH_SPAN_NAME => {
                let (trace_parsing_failed, trace) = match child_nodes.pop() {
                    Some(TreeData::Trace(Ok(trace))) => (false, trace),
                    Some(TreeData::Trace(Err(_err))) => (true, None),
                    _ => (false, None),
                };
                let service_name = (span
                    .attributes
                    .get(&SUBGRAPH_NAME)
                    .cloned()
                    .unwrap_or_else(|| Value::String("unknown service".into()))
                    .as_str())
                .to_string();
                vec![TreeData::QueryPlan(QueryPlanNode {
                    node: Some(crate::spaceport::trace::query_plan_node::Node::Fetch(
                        Box::new(FetchNode {
                            service_name,
                            trace_parsing_failed,
                            trace,
                            sent_time_offset: span
                                .attributes
                                .get(&APOLLO_PRIVATE_SENT_TIME_OFFSET)
                                .and_then(Self::extract_i64)
                                .map(|f| f as u64)
                                .unwrap_or_default(),
                            sent_time: Some(span.start_time.into()),
                            received_time: Some(span.end_time.into()),
                        }),
                    )),
                })]
            }
            FLATTEN_SPAN_NAME => {
                vec![TreeData::QueryPlan(QueryPlanNode {
                    node: Some(crate::spaceport::trace::query_plan_node::Node::Flatten(
                        Box::new(FlattenNode {
                            response_path: span
                                .attributes
                                .get(&APOLLO_PRIVATE_PATH)
                                .and_then(Self::extract_string)
                                .map(|v| {
                                    v.split('/').filter(|v|!v.is_empty() && *v != "@").map(|v| {
                                        if let Ok(index) = v.parse::<u32>() {
                                            ResponsePathElement { id: Some(crate::spaceport::trace::query_plan_node::response_path_element::Id::Index(index))}
                                        } else {
                                            ResponsePathElement { id: Some(crate::spaceport::trace::query_plan_node::response_path_element::Id::FieldName(v.to_string())) }
                                        }
                                    }).collect()
                                }).unwrap_or_default(),
                            node: child_nodes
                                .into_iter()
                                .filter_map(|child| match child {
                                    TreeData::QueryPlan(node) => Some(Box::new(node)),
                                    _ => None,
                                })
                                .next(),
                        }),
                    )),
                })]
            }
            SUBGRAPH_SPAN_NAME => {
                vec![TreeData::Trace(self.find_ftv1_trace(span))]
            }
            SUPERGRAPH_SPAN_NAME => {
                //Currently some data is in the supergraph span as we don't have the a request hook in plugin.
                child_nodes.push(TreeData::Supergraph {
                    http: self.extract_http_data(span),
                    client_name: span
                        .attributes
                        .get(&CLIENT_NAME)
                        .and_then(Self::extract_string),
                    client_version: span
                        .attributes
                        .get(&CLIENT_VERSION)
                        .and_then(Self::extract_string),
                    operation_signature: span
                        .attributes
                        .get(&APOLLO_PRIVATE_OPERATION_SIGNATURE)
                        .and_then(Self::extract_string)
                        .unwrap_or_default(),
                });
                child_nodes
            }
            REQUEST_SPAN_NAME => {
                vec![TreeData::Request(
                    self.extract_root_trace(span, child_nodes),
                )]
            }
            _ => child_nodes,
        })
    }

    fn extract_string(v: &Value) -> Option<String> {
        if let Value::String(v) = v {
            Some(v.to_string())
        } else {
            None
        }
    }

    fn extract_i64(v: &Value) -> Option<i64> {
        if let Value::I64(v) = v {
            Some(*v)
        } else {
            None
        }
    }

    fn find_ftv1_trace(
        &mut self,
        span: &SpanData,
    ) -> Result<Option<Box<crate::spaceport::Trace>>, Error> {
        span.attributes
            .get(&APOLLO_PRIVATE_FTV1)
            .map(|data| {
                if let Value::String(data) = data {
                    Ok(Box::new(crate::spaceport::Trace::decode(Cursor::new(
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
            .get(&HTTP_METHOD)
            .map(|data| data.as_str())
            .unwrap_or_default()
            .as_ref()
        {
            "OPTIONS" => crate::spaceport::trace::http::Method::Options,
            "GET" => crate::spaceport::trace::http::Method::Get,
            "HEAD" => crate::spaceport::trace::http::Method::Head,
            "POST" => crate::spaceport::trace::http::Method::Post,
            "PUT" => crate::spaceport::trace::http::Method::Put,
            "DELETE" => crate::spaceport::trace::http::Method::Delete,
            "TRACE" => crate::spaceport::trace::http::Method::Trace,
            "CONNECT" => crate::spaceport::trace::http::Method::Connect,
            "PATCH" => crate::spaceport::trace::http::Method::Patch,
            _ => crate::spaceport::trace::http::Method::Unknown,
        };
        let headers = span
            .attributes
            .get(&APOLLO_PRIVATE_HTTP_REQUEST_HEADERS)
            .map(|data| data.as_str())
            .unwrap_or_default();
        let request_headers = serde_json::from_str::<HashMap<String, Vec<String>>>(&headers)
            .unwrap_or_default()
            .into_iter()
            .map(|(header_name, value)| (header_name.to_lowercase(), Values { value }))
            .collect();
        // For now, only trace_id
        // let mut response_headers = HashMap::with_capacity(1);
        // FIXME: uncomment later
        // response_headers.insert(
        //     String::from("apollo_trace_id"),
        //     Values {
        //         value: vec![span.span_context.trace_id().to_string()],
        //     },
        // );
        Http {
            method: method.into(),
            request_headers,
            response_headers: HashMap::new(),
            status_code: 0,
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
        let mut traces: Vec<(String, crate::spaceport::Trace)> = Vec::new();
        for span in batch {
            if span.name == REQUEST_SPAN_NAME {
                match self.extract_trace(span) {
                    Ok(mut trace) => {
                        let mut operation_signature = Default::default();
                        std::mem::swap(&mut trace.signature, &mut operation_signature);
                        if !operation_signature.is_empty() {
                            traces.push((operation_signature, *trace));
                        }
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
