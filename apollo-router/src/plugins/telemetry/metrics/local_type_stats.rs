use std::collections::HashMap;

use crate::graphql::ResponseVisitor;
use crate::plugins::telemetry::metrics::apollo::list_length_histogram::ListLengthHistogram;
use crate::plugins::telemetry::metrics::apollo::studio::LocalFieldStat;
use crate::plugins::telemetry::metrics::apollo::studio::LocalTypeStat;

#[derive(Clone, Debug, Default)]
pub(crate) struct LocalTypeStatRecorder {
    pub(crate) local_type_stats: HashMap<String, LocalTypeStat>,
}

impl LocalTypeStatRecorder {
    pub(crate) fn new() -> Self {
        Self {
            local_type_stats: Default::default(),
        }
    }
}

impl ResponseVisitor for LocalTypeStatRecorder {
    fn visit_field(
        &mut self,
        request: &apollo_compiler::ExecutableDocument,
        ty: &apollo_compiler::executable::NamedType,
        field: &apollo_compiler::executable::Field,
        value: &serde_json_bytes::Value,
    ) {
        match value {
            serde_json_bytes::Value::Array(items) => {
                tracing::info!(
                    "Recording {} -> {} -> {}",
                    ty.to_string(),
                    field.name.to_string(),
                    items.len()
                );
                self.local_type_stats
                    .entry(ty.to_string())
                    .or_default()
                    .local_per_field_stat
                    .entry(field.name.to_string())
                    .or_insert_with(|| LocalFieldStat {
                        return_type: field.ty().inner_named_type().to_string(),
                        list_lengths: ListLengthHistogram::new(),
                    })
                    .list_lengths
                    .record(items.len());

                for item in items {
                    self.visit_list_item(request, field.ty().inner_named_type(), field, item);
                }
            }
            serde_json_bytes::Value::Object(children) => {
                self.visit_selections(request, &field.selection_set, children);
            }
            _ => {}
        }
    }
}
