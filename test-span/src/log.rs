use std::collections::HashSet;

use indexmap::IndexMap;
use tracing::Event;

use crate::{
    attribute::OwnedMetadata,
    record::{Record, RecordEverything},
};

#[derive(Debug, Default, Clone)]
pub struct LogsRecorder {
    recorders: IndexMap<OwnedMetadata, RecordEverything>,
}

impl LogsRecorder {
    pub fn event(&mut self, current_span_id: Option<tracing::Id>, event: &Event<'_>) {
        let metadata = OwnedMetadata::from(event.metadata());
        let metadata = if let Some(id) = current_span_id {
            metadata.with_span_id(id.into_u64())
        } else {
            metadata
        };
        event.record(self.recorders.entry(metadata).or_default())
    }

    pub fn for_spans(&self, spans: HashSet<u64>) -> Self {
        Self {
            recorders: self
                .recorders
                .iter()
                .filter_map(|(log_metadata, visitor)| match log_metadata.span_id {
                    Some(id) if spans.contains(&id) => {
                        Some((log_metadata.clone(), visitor.clone()))
                    }
                    _ => None,
                })
                .collect(),
        }
    }

    pub fn record_for_span_id_and_level(
        &self,
        span_id: u64,
        level: &tracing::Level,
    ) -> Vec<Record> {
        self.recorders
            .iter()
            .filter_map(|(log_metadata, recorders)| {
                (log_metadata.is_enabled(level) && log_metadata.span_id == Some(span_id))
                    .then(|| recorders.contents().cloned())
            })
            .flatten()
            .collect()
    }

    pub fn all_records_for_level(&self, level: &tracing::Level) -> Vec<Record> {
        self.recorders
            .iter()
            .filter_map(|(log_metadata, recorders)| {
                (log_metadata.is_enabled(level)).then(|| recorders.contents().cloned())
            })
            .flatten()
            .collect()
    }
}
