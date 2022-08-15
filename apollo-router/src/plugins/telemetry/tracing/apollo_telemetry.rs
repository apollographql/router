use crate::plugins::telemetry::config;
use apollo_spaceport::trace::query_plan_node::{
    FetchNode, FlattenNode, ParallelNode, SequenceNode,
};
use apollo_spaceport::trace::QueryPlanNode;
use apollo_spaceport::{Message, TracesAndStats};
use async_trait::async_trait;
use http::header::HeaderName;
use multimap::MultiMap;
use opentelemetry::sdk::export::trace::{ExportResult, SpanData, SpanExporter};
use opentelemetry::trace::SpanId;
use opentelemetry::{Key, Value};
use std::collections::HashMap;
use std::io::Cursor;
use std::time::{SystemTime, SystemTimeError};
use thiserror::Error;
use url::Url;

#[derive(Error, Debug)]
pub(crate) enum Error {
    #[error("protobuf decode error")]
    ProtobufDecodeError(#[from] apollo_spaceport::DecodeError),

    #[error("ftv1 span attribute should have been a string")]
    Ftv1SpanAttributeError,

    #[error("ftv1 span attribute should have been a string")]
    MultipleErrors(Vec<Error>),

    #[error("duration could not be calcualted")]
    SystemTimeError(#[from] SystemTimeError),
}

/// A [`SpanExporter`] that writes to [`Reporter`].
///
/// [`SpanExporter`]: super::SpanExporter
/// [`Reporter`]: apollo_spaceport::Reporter
#[derive(Debug)]
#[allow(dead_code)]
pub(crate) struct Exporter {
    trace_config: config::Trace,
    spans_by_parent_id: MultiMap<SpanId, SpanData>,
    endpoint: Url,
    apollo_key: String,
    apollo_graph_ref: String,
    client_name_header: HeaderName,
    client_version_header: HeaderName,
    schema_id: String,
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
    ) -> Self {
        Self {
            spans_by_parent_id: Default::default(),
            trace_config,
            endpoint,
            apollo_key,
            apollo_graph_ref,
            client_name_header,
            client_version_header,
            schema_id,
        }
    }

    pub(crate) fn extract_query_plan_trace(
        &mut self,
        span: SpanData,
    ) -> Result<apollo_spaceport::Trace, Error> {
        let node = self.extract_query_plan_node(&span, &span)?;
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
            query_plan: node.map(|n| Box::new(n)),
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
    ) -> Result<Option<QueryPlanNode>, Error> {
        let (child_nodes, errors) = self
            .spans_by_parent_id
            .remove(&span.span_context.span_id())
            .unwrap_or_default()
            .into_iter()
            .map(|span| self.extract_query_plan_node(root_span, &span))
            .fold((Vec::new(), Vec::new()), |(mut oks, mut errors), next| {
                match next {
                    Ok(Some(ok)) => oks.push(ok),
                    Err(err) => errors.push(err),
                    _ => {}
                }
                (oks, errors)
            });
        if !errors.is_empty() {
            return Err(Error::MultipleErrors(errors));
        }

        Ok(match span.name.as_ref() {
            "parallel" => Some(QueryPlanNode {
                node: Some(apollo_spaceport::trace::query_plan_node::Node::Parallel(
                    ParallelNode { nodes: child_nodes },
                )),
            }),
            "sequence" => Some(QueryPlanNode {
                node: Some(apollo_spaceport::trace::query_plan_node::Node::Sequence(
                    SequenceNode { nodes: child_nodes },
                )),
            }),
            "fetch" => {
                let (trace_parsing_failed, trace) = match self.extract_ftv1_trace(span) {
                    Ok(trace) => (false, trace),
                    Err(_err) => (true, None),
                };
                let service_name = (span
                    .attributes
                    .get(&Key::new("service_name"))
                    .cloned()
                    .unwrap_or_else(|| Value::String("unknown service".into()))
                    .as_str())
                .to_string();

                Some(QueryPlanNode {
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
                })
            }
            "flatten" => Some(QueryPlanNode {
                node: Some(apollo_spaceport::trace::query_plan_node::Node::Flatten(
                    Box::new(FlattenNode {
                        response_path: vec![],
                        node: None,
                    }),
                )),
            }),
            _ => None,
        })
    }

    fn extract_ftv1_trace(
        &self,
        span: &SpanData,
    ) -> Result<Option<Box<apollo_spaceport::Trace>>, Error> {
        span.attributes
            .get(&Key::new("ftv1"))
            .map(|data| {
                if let Value::String(data) = data {
                    Ok(Box::new(apollo_spaceport::Trace::decode(Cursor::new(
                        data.as_bytes(),
                    ))?))
                } else {
                    Err(Error::Ftv1SpanAttributeError)
                }
            })
            .transpose()
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
        for span in batch {
            if span.name == "router" {
                let trace = self.extract_query_plan_trace(span);
                let traces = TracesAndStats {
                    trace: vec![],
                    stats_with_context: vec![],
                    referenced_fields_by_type: Default::default(),
                    internal_traces_contributing_to_stats: vec![],
                };
                apollo_spaceport::Report {
                    header: None,
                    traces_per_query: HashMap::new(),
                    end_time: Some(SystemTime::now().into()),
                    operation_count: 0,
                };
            } else {
                // Not a root span, we may need it later so stash it.
                self.spans_by_parent_id
                    .insert(span.span_context.span_id(), span);
            }
        }

        // TODO Clean up old spans that have been knocking around for a long time? In theory as long as all spans are parented correctly then we shouldn't need to.

        // TODO send spans to spaceport
        return ExportResult::Ok(());
    }
}
