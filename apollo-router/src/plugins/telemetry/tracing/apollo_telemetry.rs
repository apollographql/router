use std::collections::HashMap;
use std::io::Cursor;
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::SystemTimeError;

use async_trait::async_trait;
use derivative::Derivative;
use futures::future::BoxFuture;
use futures::FutureExt;
use futures::TryFutureExt;
use itertools::Itertools;
use lru::LruCache;
use opentelemetry::sdk::export::trace::ExportResult;
use opentelemetry::sdk::export::trace::SpanData;
use opentelemetry::sdk::export::trace::SpanExporter;
use opentelemetry::trace::SpanId;
use opentelemetry::trace::TraceError;
use opentelemetry::Key;
use opentelemetry::Value;
use opentelemetry_semantic_conventions::trace::HTTP_METHOD;
use prost::Message;
use serde::de::DeserializeOwned;
use thiserror::Error;
use url::Url;

use crate::axum_factory::utils::REQUEST_SPAN_NAME;
use crate::plugins::telemetry;
use crate::plugins::telemetry::apollo::SingleReport;
use crate::plugins::telemetry::apollo_exporter::proto;
use crate::plugins::telemetry::apollo_exporter::proto::reports::trace::http::Values;
use crate::plugins::telemetry::apollo_exporter::proto::reports::trace::query_plan_node::ConditionNode;
use crate::plugins::telemetry::apollo_exporter::proto::reports::trace::query_plan_node::DeferNode;
use crate::plugins::telemetry::apollo_exporter::proto::reports::trace::query_plan_node::DeferNodePrimary;
use crate::plugins::telemetry::apollo_exporter::proto::reports::trace::query_plan_node::DeferredNode;
use crate::plugins::telemetry::apollo_exporter::proto::reports::trace::query_plan_node::DeferredNodeDepends;
use crate::plugins::telemetry::apollo_exporter::proto::reports::trace::query_plan_node::FetchNode;
use crate::plugins::telemetry::apollo_exporter::proto::reports::trace::query_plan_node::FlattenNode;
use crate::plugins::telemetry::apollo_exporter::proto::reports::trace::query_plan_node::Node;
use crate::plugins::telemetry::apollo_exporter::proto::reports::trace::query_plan_node::ParallelNode;
use crate::plugins::telemetry::apollo_exporter::proto::reports::trace::query_plan_node::ResponsePathElement;
use crate::plugins::telemetry::apollo_exporter::proto::reports::trace::query_plan_node::SequenceNode;
use crate::plugins::telemetry::apollo_exporter::proto::reports::trace::Details;
use crate::plugins::telemetry::apollo_exporter::proto::reports::trace::Http;
use crate::plugins::telemetry::apollo_exporter::proto::reports::trace::QueryPlanNode;
use crate::plugins::telemetry::apollo_exporter::ApolloExporter;
use crate::plugins::telemetry::config::Sampler;
use crate::plugins::telemetry::config::SamplerOption;
use crate::plugins::telemetry::tracing::apollo::TracesReport;
use crate::plugins::telemetry::tracing::BatchProcessorConfig;
use crate::plugins::telemetry::BoxError;
use crate::plugins::telemetry::ROUTER_SPAN_NAME;
use crate::plugins::telemetry::SUBGRAPH_SPAN_NAME;
use crate::plugins::telemetry::SUPERGRAPH_SPAN_NAME;
use crate::query_planner::CONDITION_ELSE_SPAN_NAME;
use crate::query_planner::CONDITION_IF_SPAN_NAME;
use crate::query_planner::CONDITION_SPAN_NAME;
use crate::query_planner::DEFER_DEFERRED_SPAN_NAME;
use crate::query_planner::DEFER_PRIMARY_SPAN_NAME;
use crate::query_planner::DEFER_SPAN_NAME;
use crate::query_planner::FETCH_SPAN_NAME;
use crate::query_planner::FLATTEN_SPAN_NAME;
use crate::query_planner::PARALLEL_SPAN_NAME;
use crate::query_planner::SEQUENCE_SPAN_NAME;

const APOLLO_PRIVATE_DURATION_NS: Key = Key::from_static_str("apollo_private.duration_ns");
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
const APOLLO_PRIVATE_FTV1: Key = Key::from_static_str("apollo_private.ftv1");
const PATH: Key = Key::from_static_str("graphql.path");
const SUBGRAPH_NAME: Key = Key::from_static_str("apollo.subgraph.name");
const CLIENT_NAME: Key = Key::from_static_str("client.name");
const CLIENT_VERSION: Key = Key::from_static_str("client.version");
const DEPENDS: Key = Key::from_static_str("graphql.depends");
const LABEL: Key = Key::from_static_str("graphql.label");
const CONDITION: Key = Key::from_static_str("graphql.condition");
const OPERATION_NAME: Key = Key::from_static_str("graphql.operation.name");

#[derive(Error, Debug)]
pub(crate) enum Error {
    #[error("subgraph protobuf decode error")]
    ProtobufDecode(#[from] crate::plugins::telemetry::apollo_exporter::DecodeError),

    #[error("subgraph trace payload was not base64")]
    Base64Decode(#[from] base64::DecodeError),

    #[error("trace parsing failed")]
    TraceParsingFailed,

    #[error("there were multiple tracing errors")]
    MultipleErrors(Vec<Error>),

    #[error("duration could not be calculated")]
    SystemTime(#[from] SystemTimeError),
}

/// A [`SpanExporter`] that writes to [`Reporter`].
///
/// [`SpanExporter`]: super::SpanExporter
/// [`Reporter`]: crate::plugins::telemetry::Reporter
#[derive(Derivative)]
#[derivative(Debug)]
pub(crate) struct Exporter {
    spans_by_parent_id: LruCache<SpanId, Vec<SpanData>>,
    #[derivative(Debug = "ignore")]
    report_exporter: Arc<ApolloExporter>,
    field_execution_weight: f64,
}

enum TreeData {
    Request(Result<Box<proto::reports::Trace>, Error>),
    Router {
        http: Box<Http>,
        client_name: Option<String>,
        client_version: Option<String>,
        duration_ns: u64,
    },
    Supergraph {
        operation_signature: String,
        operation_name: String,
        variables_json: HashMap<String, String>,
    },
    QueryPlanNode(QueryPlanNode),
    DeferPrimary(DeferNodePrimary),
    DeferDeferred(DeferredNode),
    ConditionIf(Option<QueryPlanNode>),
    ConditionElse(Option<QueryPlanNode>),
    Trace(Option<Result<Box<proto::reports::Trace>, Error>>),
}

#[buildstructor::buildstructor]
impl Exporter {
    #[builder]
    pub(crate) fn new(
        endpoint: Url,
        apollo_key: String,
        apollo_graph_ref: String,
        schema_id: String,
        buffer_size: NonZeroUsize,
        field_execution_sampler: SamplerOption,
        batch_config: BatchProcessorConfig,
    ) -> Result<Self, BoxError> {
        tracing::debug!("creating studio exporter");
        Ok(Self {
            spans_by_parent_id: LruCache::new(buffer_size),
            report_exporter: Arc::new(ApolloExporter::new(
                &endpoint,
                &batch_config,
                &apollo_key,
                &apollo_graph_ref,
                &schema_id,
            )?),
            field_execution_weight: match field_execution_sampler {
                SamplerOption::Always(Sampler::AlwaysOn) => 1.0,
                SamplerOption::Always(Sampler::AlwaysOff) => 0.0,
                SamplerOption::TraceIdRatioBased(ratio) => 1.0 / ratio,
            },
        })
    }

    fn extract_root_trace(
        &mut self,
        span: &SpanData,
        child_nodes: Vec<TreeData>,
    ) -> Result<Box<proto::reports::Trace>, Error> {
        let http = extract_http_data(span);
        let mut root_trace = proto::reports::Trace {
            start_time: Some(span.start_time.into()),
            end_time: Some(span.end_time.into()),
            duration_ns: 0,
            root: None,
            details: None,
            http: Some(http),
            ..Default::default()
        };

        for node in child_nodes {
            match node {
                TreeData::QueryPlanNode(query_plan) => {
                    root_trace.query_plan = Some(Box::new(query_plan))
                }
                TreeData::Router {
                    http,
                    client_name,
                    client_version,
                    duration_ns,
                } => {
                    let root_http = root_trace
                        .http
                        .as_mut()
                        .expect("http was extracted earlier, qed");
                    root_http.request_headers = http.request_headers;
                    root_http.response_headers = http.response_headers;
                    root_trace.client_name = client_name.unwrap_or_default();
                    root_trace.client_version = client_version.unwrap_or_default();
                    root_trace.duration_ns = duration_ns;
                }
                TreeData::Supergraph {
                    operation_signature,
                    operation_name,
                    variables_json,
                } => {
                    root_trace.field_execution_weight = self.field_execution_weight;
                    root_trace.signature = operation_signature;
                    root_trace.details = Some(Details {
                        variables_json,
                        operation_name,
                    });
                }
                _ => panic!("should never have had other node types"),
            }
        }

        Ok(Box::new(root_trace))
    }

    fn extract_trace(&mut self, span: SpanData) -> Result<Box<proto::reports::Trace>, Error> {
        self.extract_data_from_spans(&span)?
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

    fn extract_data_from_spans(&mut self, span: &SpanData) -> Result<Vec<TreeData>, Error> {
        let (mut child_nodes, errors) = self
            .spans_by_parent_id
            .pop_entry(&span.span_context.span_id())
            .map(|(_, spans)| spans)
            .unwrap_or_default()
            .into_iter()
            .map(|span| self.extract_data_from_spans(&span))
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
            PARALLEL_SPAN_NAME => vec![TreeData::QueryPlanNode(QueryPlanNode {
                node: Some(proto::reports::trace::query_plan_node::Node::Parallel(
                    ParallelNode {
                        nodes: child_nodes.remove_query_plan_nodes(),
                    },
                )),
            })],
            SEQUENCE_SPAN_NAME => vec![TreeData::QueryPlanNode(QueryPlanNode {
                node: Some(proto::reports::trace::query_plan_node::Node::Sequence(
                    SequenceNode {
                        nodes: child_nodes.remove_query_plan_nodes(),
                    },
                )),
            })],
            FETCH_SPAN_NAME => {
                let (trace_parsing_failed, trace) = match child_nodes.pop() {
                    Some(TreeData::Trace(Some(Ok(trace)))) => (false, Some(trace)),
                    Some(TreeData::Trace(Some(Err(_err)))) => (true, None),
                    _ => (false, None),
                };
                let service_name = (span
                    .attributes
                    .get(&SUBGRAPH_NAME)
                    .cloned()
                    .unwrap_or_else(|| Value::String("unknown service".into()))
                    .as_str())
                .to_string();
                vec![TreeData::QueryPlanNode(QueryPlanNode {
                    node: Some(proto::reports::trace::query_plan_node::Node::Fetch(
                        Box::new(FetchNode {
                            service_name,
                            trace_parsing_failed,
                            trace,
                            sent_time_offset: span
                                .attributes
                                .get(&APOLLO_PRIVATE_SENT_TIME_OFFSET)
                                .and_then(extract_i64)
                                .map(|f| f as u64)
                                .unwrap_or_default(),
                            sent_time: Some(span.start_time.into()),
                            received_time: Some(span.end_time.into()),
                        }),
                    )),
                })]
            }
            FLATTEN_SPAN_NAME => {
                vec![TreeData::QueryPlanNode(QueryPlanNode {
                    node: Some(proto::reports::trace::query_plan_node::Node::Flatten(
                        Box::new(FlattenNode {
                            response_path: span
                                .attributes
                                .get(&PATH)
                                .map(extract_path)
                                .unwrap_or_default(),
                            node: child_nodes.remove_first_query_plan_node().map(Box::new),
                        }),
                    )),
                })]
            }
            SUBGRAPH_SPAN_NAME => {
                vec![TreeData::Trace(
                    span.attributes
                        .get(&APOLLO_PRIVATE_FTV1)
                        .and_then(extract_ftv1_trace),
                )]
            }
            SUPERGRAPH_SPAN_NAME => {
                //Currently some data is in the supergraph span as we don't have the a request hook in plugin.
                child_nodes.push(TreeData::Supergraph {
                    operation_signature: span
                        .attributes
                        .get(&APOLLO_PRIVATE_OPERATION_SIGNATURE)
                        .and_then(extract_string)
                        .unwrap_or_default(),
                    operation_name: span
                        .attributes
                        .get(&OPERATION_NAME)
                        .and_then(extract_string)
                        .unwrap_or_default(),
                    variables_json: span
                        .attributes
                        .get(&APOLLO_PRIVATE_GRAPHQL_VARIABLES)
                        .and_then(extract_json)
                        .unwrap_or_default(),
                });
                child_nodes
            }
            REQUEST_SPAN_NAME => {
                vec![TreeData::Request(
                    self.extract_root_trace(span, child_nodes),
                )]
            }
            ROUTER_SPAN_NAME => {
                child_nodes.push(TreeData::Router {
                    http: Box::new(extract_http_data(span)),
                    client_name: span.attributes.get(&CLIENT_NAME).and_then(extract_string),
                    client_version: span
                        .attributes
                        .get(&CLIENT_VERSION)
                        .and_then(extract_string),
                    duration_ns: span
                        .attributes
                        .get(&APOLLO_PRIVATE_DURATION_NS)
                        .and_then(extract_i64)
                        .map(|e| e as u64)
                        .unwrap_or_default(),
                });
                child_nodes
            }
            DEFER_SPAN_NAME => {
                vec![TreeData::QueryPlanNode(QueryPlanNode {
                    node: Some(Node::Defer(Box::new(DeferNode {
                        primary: child_nodes.remove_first_defer_primary_node().map(Box::new),
                        deferred: child_nodes.remove_defer_deferred_nodes(),
                    }))),
                })]
            }
            DEFER_PRIMARY_SPAN_NAME => {
                vec![TreeData::DeferPrimary(DeferNodePrimary {
                    node: child_nodes.remove_first_query_plan_node().map(Box::new),
                })]
            }
            DEFER_DEFERRED_SPAN_NAME => {
                vec![TreeData::DeferDeferred(DeferredNode {
                    node: child_nodes.remove_first_query_plan_node(),
                    path: span
                        .attributes
                        .get(&PATH)
                        .map(extract_path)
                        .unwrap_or_default(),
                    // In theory we don't have to do the transformation here, but it is safer to do so.
                    depends: span
                        .attributes
                        .get(&DEPENDS)
                        .and_then(extract_json::<Vec<crate::query_planner::Depends>>)
                        .unwrap_or_default()
                        .iter()
                        .map(|d| DeferredNodeDepends {
                            id: d.id.clone(),
                            defer_label: d.defer_label.clone().unwrap_or_default(),
                        })
                        .collect(),
                    label: span
                        .attributes
                        .get(&LABEL)
                        .and_then(extract_string)
                        .unwrap_or_default(),
                })]
            }

            CONDITION_SPAN_NAME => {
                vec![TreeData::QueryPlanNode(QueryPlanNode {
                    node: Some(Node::Condition(Box::new(ConditionNode {
                        condition: span
                            .attributes
                            .get(&CONDITION)
                            .and_then(extract_string)
                            .unwrap_or_default(),
                        if_clause: child_nodes.remove_first_condition_if_node().map(Box::new),
                        else_clause: child_nodes.remove_first_condition_else_node().map(Box::new),
                    }))),
                })]
            }
            CONDITION_IF_SPAN_NAME => {
                vec![TreeData::ConditionIf(
                    child_nodes.remove_first_query_plan_node(),
                )]
            }
            CONDITION_ELSE_SPAN_NAME => {
                vec![TreeData::ConditionElse(
                    child_nodes.remove_first_query_plan_node(),
                )]
            }
            _ => child_nodes,
        })
    }
}

fn extract_json<T: DeserializeOwned>(v: &Value) -> Option<T> {
    extract_string(v)
        .map(|v| serde_json::from_str(&v))
        .transpose()
        .unwrap_or(None)
}

fn extract_string(v: &Value) -> Option<String> {
    if let Value::String(v) = v {
        Some(v.to_string())
    } else {
        None
    }
}

fn extract_path(v: &Value) -> Vec<ResponsePathElement> {
    extract_string(v)
        .map(|v| {
            v.split('/')
                .filter(|v| !v.is_empty() && *v != "@")
                .map(|v| {
                    if let Ok(index) = v.parse::<u32>() {
                        ResponsePathElement {
                            id: Some(
                                proto::reports::trace::query_plan_node::response_path_element::Id::Index(
                                    index,
                                ),
                            ),
                        }
                    } else {
                        ResponsePathElement {
                            id: Some(
                                proto::reports::trace::query_plan_node::response_path_element::Id::FieldName(
                                    v.to_string(),
                                ),
                            ),
                        }
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

fn extract_i64(v: &Value) -> Option<i64> {
    if let Value::I64(v) = v {
        Some(*v)
    } else {
        None
    }
}

fn extract_ftv1_trace(v: &Value) -> Option<Result<Box<proto::reports::Trace>, Error>> {
    if let Value::String(s) = v {
        if let Some(t) = decode_ftv1_trace(s.as_str()) {
            return Some(Ok(Box::new(t)));
        }
        return Some(Err(Error::TraceParsingFailed));
    }
    None
}

pub(crate) fn decode_ftv1_trace(string: &str) -> Option<proto::reports::Trace> {
    let bytes = base64::decode(string).ok()?;
    proto::reports::Trace::decode(Cursor::new(bytes)).ok()
}

fn extract_http_data(span: &SpanData) -> Http {
    let method = match span
        .attributes
        .get(&HTTP_METHOD)
        .map(|data| data.as_str())
        .unwrap_or_default()
        .as_ref()
    {
        "OPTIONS" => proto::reports::trace::http::Method::Options,
        "GET" => proto::reports::trace::http::Method::Get,
        "HEAD" => proto::reports::trace::http::Method::Head,
        "POST" => proto::reports::trace::http::Method::Post,
        "PUT" => proto::reports::trace::http::Method::Put,
        "DELETE" => proto::reports::trace::http::Method::Delete,
        "TRACE" => proto::reports::trace::http::Method::Trace,
        "CONNECT" => proto::reports::trace::http::Method::Connect,
        "PATCH" => proto::reports::trace::http::Method::Patch,
        _ => proto::reports::trace::http::Method::Unknown,
    };
    let request_headers = span
        .attributes
        .get(&APOLLO_PRIVATE_HTTP_REQUEST_HEADERS)
        .and_then(extract_json::<HashMap<String, Vec<String>>>)
        .unwrap_or_default()
        .into_iter()
        .map(|(header_name, value)| (header_name.to_lowercase(), Values { value }))
        .collect();
    let response_headers = span
        .attributes
        .get(&APOLLO_PRIVATE_HTTP_RESPONSE_HEADERS)
        .and_then(extract_json::<HashMap<String, Vec<String>>>)
        .unwrap_or_default()
        .into_iter()
        .map(|(header_name, value)| (header_name.to_lowercase(), Values { value }))
        .collect();

    Http {
        method: method.into(),
        request_headers,
        response_headers,
        status_code: 0,
    }
}

#[async_trait]
impl SpanExporter for Exporter {
    /// Export spans to apollo telemetry
    fn export(&mut self, batch: Vec<SpanData>) -> BoxFuture<'static, ExportResult> {
        // Exporting to apollo means that we must have complete trace as the entire trace must be built.
        // We do what we can, and if there are any traces that are not complete then we keep them for the next export event.
        // We may get spans that simply don't complete. These need to be cleaned up after a period. It's the price of using ftv1.
        let mut traces: Vec<(String, proto::reports::Trace)> = Vec::new();
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
                        tracing::error!(
                            "failed to construct trace: {}, skipping",
                            Error::MultipleErrors(errors)
                        );
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
        let mut report = telemetry::apollo::Report::default();
        report += SingleReport::Traces(TracesReport { traces });
        let exporter = self.report_exporter.clone();
        let fut = async move {
            exporter
                .submit_report(report)
                .map_err(|e| TraceError::ExportFailed(Box::new(e)))
                .await
        };
        fut.boxed()
    }
}

trait ChildNodes {
    fn remove_first_query_plan_node(&mut self) -> Option<QueryPlanNode>;
    fn remove_query_plan_nodes(&mut self) -> Vec<QueryPlanNode>;
    fn remove_first_defer_primary_node(&mut self) -> Option<DeferNodePrimary>;
    fn remove_defer_deferred_nodes(&mut self) -> Vec<DeferredNode>;
    fn remove_first_condition_if_node(&mut self) -> Option<QueryPlanNode>;
    fn remove_first_condition_else_node(&mut self) -> Option<QueryPlanNode>;
}

impl ChildNodes for Vec<TreeData> {
    fn remove_first_query_plan_node(&mut self) -> Option<QueryPlanNode> {
        if let Some((idx, _)) = self
            .iter()
            .find_position(|child| matches!(child, TreeData::QueryPlanNode(_)))
        {
            if let TreeData::QueryPlanNode(node) = self.remove(idx) {
                return Some(node);
            }
        }
        None
    }

    fn remove_query_plan_nodes(&mut self) -> Vec<QueryPlanNode> {
        let mut extracted = Vec::new();
        let mut retained = Vec::new();
        for treedata in self.drain(0..self.len()) {
            if let TreeData::QueryPlanNode(node) = treedata {
                extracted.push(node);
            } else {
                retained.push(treedata)
            }
        }
        self.append(&mut retained);
        extracted
    }

    fn remove_first_defer_primary_node(&mut self) -> Option<DeferNodePrimary> {
        if let Some((idx, _)) = self
            .iter()
            .find_position(|child| matches!(child, TreeData::DeferPrimary(_)))
        {
            if let TreeData::DeferPrimary(node) = self.remove(idx) {
                return Some(node);
            }
        }
        None
    }

    fn remove_defer_deferred_nodes(&mut self) -> Vec<DeferredNode> {
        let mut extracted = Vec::new();
        let mut retained = Vec::new();
        for treedata in self.drain(0..self.len()) {
            if let TreeData::DeferDeferred(node) = treedata {
                extracted.push(node);
            } else {
                retained.push(treedata)
            }
        }
        self.append(&mut retained);
        extracted
    }

    fn remove_first_condition_if_node(&mut self) -> Option<QueryPlanNode> {
        if let Some((idx, _)) = self
            .iter()
            .find_position(|child| matches!(child, TreeData::ConditionIf(_)))
        {
            if let TreeData::ConditionIf(node) = self.remove(idx) {
                return node;
            }
        }
        None
    }

    fn remove_first_condition_else_node(&mut self) -> Option<QueryPlanNode> {
        if let Some((idx, _)) = self
            .iter()
            .find_position(|child| matches!(child, TreeData::ConditionElse(_)))
        {
            if let TreeData::ConditionElse(node) = self.remove(idx) {
                return node;
            }
        }
        None
    }
}

#[cfg(test)]
mod test {
    use opentelemetry::Value;
    use prost::Message;
    use serde_json::json;
    use crate::plugins::telemetry::apollo_exporter::proto::reports::Trace;
    use crate::plugins::telemetry::apollo_exporter::proto::reports::trace::query_plan_node::{DeferNodePrimary, DeferredNode, ResponsePathElement};
    use crate::plugins::telemetry::apollo_exporter::proto::reports::trace::QueryPlanNode;
    use crate::plugins::telemetry::apollo_exporter::proto::reports::trace::query_plan_node::response_path_element::Id;
    use crate::plugins::telemetry::tracing::apollo_telemetry::{ChildNodes, extract_ftv1_trace, extract_i64, extract_json, extract_path, extract_string, TreeData};

    fn elements(tree_data: Vec<TreeData>) -> Vec<&'static str> {
        let mut elements = Vec::new();
        for t in tree_data {
            match t {
                TreeData::Request(_) => elements.push("request"),
                TreeData::Supergraph { .. } => elements.push("supergraph"),
                TreeData::QueryPlanNode(_) => elements.push("query_plan_node"),
                TreeData::DeferPrimary(_) => elements.push("defer_primary"),
                TreeData::DeferDeferred(_) => elements.push("defer_deferred"),
                TreeData::ConditionIf(_) => elements.push("condition_if"),
                TreeData::ConditionElse(_) => elements.push("condition_else"),
                TreeData::Trace(_) => elements.push("trace"),
                TreeData::Router { .. } => elements.push("router"),
            }
        }
        elements
    }

    #[test]
    fn remove_first_query_plan_node() {
        let mut vec = vec![
            TreeData::Trace(None),
            TreeData::QueryPlanNode(QueryPlanNode { node: None }),
            TreeData::QueryPlanNode(QueryPlanNode { node: None }),
        ];

        assert!(vec.remove_first_query_plan_node().is_some());
        assert_eq!(elements(vec), ["trace", "query_plan_node"]);
    }

    #[test]
    fn remove_query_plan_nodes() {
        let mut vec = vec![
            TreeData::Trace(None),
            TreeData::QueryPlanNode(QueryPlanNode { node: None }),
            TreeData::QueryPlanNode(QueryPlanNode { node: None }),
        ];

        assert_eq!(vec.remove_query_plan_nodes().len(), 2);
        assert_eq!(elements(vec), ["trace"]);
    }

    #[test]
    fn remove_first_defer_primary_node() {
        let mut vec = vec![
            TreeData::Trace(None),
            TreeData::DeferPrimary(DeferNodePrimary { node: None }),
            TreeData::DeferDeferred(DeferredNode {
                depends: vec![],
                label: "".to_string(),
                path: Default::default(),
                node: None,
            }),
        ];

        assert!(vec.remove_first_defer_primary_node().is_some());
        assert_eq!(elements(vec), ["trace", "defer_deferred"]);
    }

    #[test]
    fn remove_defer_deferred_nodes() {
        let mut vec = vec![
            TreeData::Trace(None),
            TreeData::DeferPrimary(DeferNodePrimary { node: None }),
            TreeData::DeferDeferred(DeferredNode {
                depends: vec![],
                label: "".to_string(),
                path: Default::default(),
                node: None,
            }),
            TreeData::DeferDeferred(DeferredNode {
                depends: vec![],
                label: "".to_string(),
                path: Default::default(),
                node: None,
            }),
        ];

        assert_eq!(vec.remove_defer_deferred_nodes().len(), 2);
        assert_eq!(elements(vec), ["trace", "defer_primary"]);
    }

    #[test]
    fn test_remove_first_condition_if_node() {
        let mut vec = vec![
            TreeData::Trace(None),
            TreeData::ConditionIf(Some(QueryPlanNode { node: None })),
            TreeData::ConditionElse(Some(QueryPlanNode { node: None })),
        ];

        assert!(vec.remove_first_condition_if_node().is_some());
        assert_eq!(elements(vec), ["trace", "condition_else"]);
    }

    #[test]
    fn test_remove_first_condition_else_node() {
        let mut vec = vec![
            TreeData::Trace(None),
            TreeData::ConditionIf(Some(QueryPlanNode { node: None })),
            TreeData::ConditionElse(Some(QueryPlanNode { node: None })),
        ];

        assert!(vec.remove_first_condition_else_node().is_some());
        assert_eq!(elements(vec), ["trace", "condition_if"]);
    }

    #[test]
    fn test_extract_json() {
        let val = json!({"hi": "there"});
        assert_eq!(
            extract_json::<serde_json::Value>(&Value::String(val.to_string().into())),
            Some(val)
        );
    }

    #[test]
    fn test_extract_string() {
        assert_eq!(
            extract_string(&Value::String("hi".into())),
            Some("hi".to_string())
        );
    }

    #[test]
    fn test_extract_path() {
        assert_eq!(
            extract_path(&Value::String("/hi/3/there".into())),
            vec![
                ResponsePathElement {
                    id: Some(Id::FieldName("hi".to_string())),
                },
                ResponsePathElement {
                    id: Some(Id::Index(3)),
                },
                ResponsePathElement {
                    id: Some(Id::FieldName("there".to_string())),
                }
            ]
        );
    }

    #[test]
    fn test_extract_i64() {
        assert_eq!(extract_i64(&Value::I64(35)), Some(35));
    }

    #[test]
    fn test_extract_ftv1_trace() {
        let trace = Trace::default();
        let encoded = base64::encode(trace.encode_to_vec());
        assert_eq!(
            *extract_ftv1_trace(&Value::String(encoded.into()))
                .expect("there was a trace here")
                .expect("the trace must be decoded"),
            trace
        );
    }
}
