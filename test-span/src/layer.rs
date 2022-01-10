use ::daggy::Dag;
use ::tracing::span;
use ::tracing::{Event, Metadata};
use indexmap::IndexMap;
use once_cell::sync::Lazy;
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;

use crate::log::LogsRecorder;
use crate::record::Recorder;
use crate::report::ALL_DAGS;
use crate::LazyMutex;

pub(crate) static ALL_SPANS: LazyMutex<IndexMap<u64, Recorder>> = Lazy::new(Default::default);
pub(crate) static ALL_LOGS: LazyMutex<LogsRecorder> = Lazy::new(Default::default);
pub(crate) static SPAN_ID_TO_ROOT_AND_NODE_INDEX: LazyMutex<
    IndexMap<u64, (u64, daggy::NodeIndex)>,
> = Lazy::new(Default::default);

#[derive(Debug, Default)]
pub struct Layer {}

impl Layer {
    fn record(&self, id: span::Id, record: &span::Record<'_>) {
        ALL_SPANS
            .lock()
            .unwrap()
            .get_mut(&id.into_u64())
            .unwrap_or_else(|| panic!("no record for id {}", id.into_u64()))
            .record(record);
    }

    fn event(&self, event: &Event<'_>, ctx: Context<'_, impl tracing::Subscriber>) {
        let current_span = ctx.current_span();
        let current_span = current_span.id().map(std::clone::Clone::clone);
        ALL_LOGS.lock().unwrap().event(current_span, event);
    }

    fn attributes(
        &self,
        span_id: span::Id,
        attributes: &span::Attributes<'_>,
        parent_id: Option<span::Id>,
    ) {
        let raw_span_id = span_id.into_u64();

        if let Some(id) = parent_id {
            // We have a parent, we can store the span in the right DAG
            let raw_parent_id = id.into_u64();

            let mut id_to_node_index = SPAN_ID_TO_ROOT_AND_NODE_INDEX.lock().unwrap();

            let (root_span_id, parent_node_index) = id_to_node_index
                .get(&raw_parent_id)
                .map(std::clone::Clone::clone)
                .unwrap_or_else(|| panic!("missing parent attributes for {}.", raw_parent_id));

            let (_, node_index) =
                if let Some(span_dag) = ALL_DAGS.lock().unwrap().get_mut(&root_span_id) {
                    span_dag.add_child(parent_node_index, (), raw_span_id)
                } else {
                    panic!("missing dag for root {}", root_span_id);
                };

            id_to_node_index.insert(raw_span_id, (root_span_id, node_index));
        } else {
            // We're dealing with a root, let's create a new DAG
            let mut new_dag: Dag<u64, ()> = Default::default();
            let root_index = new_dag.add_node(raw_span_id);

            // The span is the root here
            SPAN_ID_TO_ROOT_AND_NODE_INDEX
                .lock()
                .unwrap()
                .insert(raw_span_id, (raw_span_id, root_index));

            let mut all_dags = ALL_DAGS.lock().unwrap();
            all_dags.insert(raw_span_id, new_dag);
        }

        ALL_SPANS
            .lock()
            .unwrap()
            .entry(raw_span_id)
            .or_default()
            .attributes(span_id, attributes);
    }
}

impl<S> tracing_subscriber::Layer<S> for Layer
where
    S: tracing::Subscriber + for<'lookup> LookupSpan<'lookup>,
{
    fn register_callsite(
        &self,
        _metadata: &'static Metadata<'static>,
    ) -> tracing::subscriber::Interest {
        tracing::subscriber::Interest::always()
    }

    fn on_new_span(
        &self,
        attrs: &span::Attributes<'_>,
        id: &span::Id,
        ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let maybe_parent_id = attrs
            .parent()
            .map(std::clone::Clone::clone)
            .or_else(|| ctx.current_span().id().map(std::clone::Clone::clone));

        self.attributes(id.clone(), attrs, maybe_parent_id)
    }

    fn on_record(
        &self,
        span: &span::Id,
        values: &span::Record<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        self.record(span.clone(), values)
    }

    fn on_event(&self, event: &Event<'_>, ctx: tracing_subscriber::layer::Context<'_, S>) {
        self.event(event, ctx)
    }
}
